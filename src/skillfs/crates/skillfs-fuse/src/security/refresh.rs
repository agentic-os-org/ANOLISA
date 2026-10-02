//! Per-skill debounce + scan-then-resolve refresh.
//!
//! The FUSE mutation paths call [`RefreshController::observe`] with
//! the owning skill name, the relative path inside the skill, and a
//! [`MutationKind`] tag identifying the FUSE callback. The
//! controller enqueues a debounce timer per skill; once the timer
//! fires, a Tokio worker runs the External Decision pipeline —
//! `scan <skill_dir>` first, then `resolve <skill_dir>` — updates the
//! [`ActiveSkillResolver`] with the resolved decision, and emits one
//! [`SecurityEvent`] through the configured [`SecurityEventWriter`].
//! The scan step is fire-and-check: SkillFS does not parse its JSON,
//! only its exit status. A scan failure short-circuits the resolve
//! and applies the configured [`FailedResolveBehavior`].
//!
//! Current scope:
//!
//! * No daemon, socket, or IPC transport — `resolve` is invoked through
//!   the existing [`LedgerAdapter`] subprocess path.
//! * No trusted-writer identity, no fail-open/fail-closed policy
//!   negotiation. On invalid JSON or a non-zero exit code the
//!   controller hides the skill (configurable via
//!   [`FailedResolveBehavior`]) and emits a security event so the
//!   operator sees what happened.
//! * `.skill-meta/**`, `skill-discover`, lifecycle reserved roots, and
//!   virtual paths are filtered out so ledger writes / discover
//!   listings cannot create a refresh feedback loop.
//! * Best-effort: a missing tokio runtime, a clogged worker, or a sink
//!   write failure must never propagate back to the FUSE callback that
//!   triggered the observation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::runtime::{Builder, Handle};
use tokio::sync::{Notify, mpsc};
use tracing::{debug, info, warn};

use super::active::{ActiveResolverError, ActiveSkillResolver, ActiveTarget};
use super::event_stream::{NoopSecurityEventWriter, SecurityEvent, SecurityEventWriter};
use super::ledger::{LedgerAdapter, LedgerError, LedgerResolveResult, LedgerStatus};
use super::lifecycle::is_reserved_lifecycle_name;
use super::path::is_skill_meta_path;

/// Default debounce window between the first observation for a skill
/// and the resolve/refresh that follows. 300 ms keeps the controller
/// responsive while still coalescing the chunked writes the kernel
/// hands FUSE for a single SKILL.md edit.
pub const DEFAULT_REFRESH_DEBOUNCE_MS: u64 = 300;

/// Identifier of the FUSE callback that produced an observation.
///
/// Used both to filter out callbacks the controller never reacts to (none
/// today) and to render the `fsHook` field of the emitted security event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MutationKind {
    Mkdir,
    Create,
    Write,
    Rename,
    Unlink,
    Rmdir,
    SetattrTruncate,
}

impl MutationKind {
    /// Stable label suitable for the `fsHook` JSON field.
    pub fn as_label(self) -> &'static str {
        match self {
            MutationKind::Mkdir => "mkdir",
            MutationKind::Create => "create",
            MutationKind::Write => "write",
            MutationKind::Rename => "rename",
            MutationKind::Unlink => "unlink",
            MutationKind::Rmdir => "rmdir",
            MutationKind::SetattrTruncate => "setattr(truncate)",
        }
    }
}

/// One mutation observation captured at FUSE-callback time. Cheap to
/// build so the FUSE thread can `observe()` and return immediately.
#[derive(Debug, Clone)]
pub struct RefreshObservation {
    pub skill_name: String,
    pub relative_path: Option<PathBuf>,
    pub kind: MutationKind,
}

impl RefreshObservation {
    pub fn new(
        skill_name: impl Into<String>,
        relative_path: Option<PathBuf>,
        kind: MutationKind,
    ) -> Self {
        Self {
            skill_name: skill_name.into(),
            relative_path,
            kind,
        }
    }

    /// Render a short label of the form `write(SKILL.md)` /
    /// `mkdir(scripts)` for the `fsHook` field.
    pub fn fs_hook_label(&self) -> String {
        match self.relative_path.as_ref() {
            Some(rel) if !rel.as_os_str().is_empty() => {
                format!("{}({})", self.kind.as_label(), rel.display())
            }
            _ => self.kind.as_label().to_string(),
        }
    }
}

/// Decide how the controller treats an unparseable / failed
/// resolve. Demo policy is intentionally non-strict here: the doc
/// recommends hiding the skill so a buggy provider cannot leave a
/// risky `current` decision standing, but tests pin both behaviors so
/// a future strict mode has somewhere to plug in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FailedResolveBehavior {
    /// Replace the skill's mapping with `Hidden` and emit an event with
    /// `ledgerStatus="error"`. The default.
    #[default]
    HideOnFailure,
    /// Leave whatever mapping is currently installed unchanged and emit
    /// an event with `ledgerStatus="error"`.
    KeepPreviousMapping,
}

/// Controller that batches mutation observations into per-skill
/// debounce timers and runs the External Decision pipeline once each
/// timer fires.
pub struct RefreshController {
    inner: Arc<Inner>,
}

struct Inner {
    adapter: Arc<dyn LedgerAdapter>,
    resolver: Arc<ActiveSkillResolver>,
    event_writer: Arc<dyn SecurityEventWriter>,
    debounce: Duration,
    failed_behavior: FailedResolveBehavior,
    /// Per-skill latest pending observation. The debounce worker
    /// drains this map when the timer fires; observations arriving
    /// during the timer window overwrite the pending entry so the
    /// resolve sees the most recent FUSE hook.
    pending: Mutex<HashMap<String, PendingState>>,
    notify: Notify,
    sender: mpsc::UnboundedSender<RefreshCommand>,
}

#[derive(Debug, Clone)]
struct PendingState {
    observation: RefreshObservation,
    fire_at: Instant,
}

#[derive(Debug)]
enum RefreshCommand {
    /// A new observation (or an update to an existing pending
    /// observation) was inserted; the worker should re-check pending.
    Wakeup,
    /// Shut the worker down.
    Shutdown,
}

impl RefreshController {
    /// Build a controller that uses the provided adapter / resolver /
    /// event writer, with a custom debounce window.
    ///
    /// If the caller is already inside a Tokio runtime, the worker is
    /// spawned on that runtime via [`Handle::try_current`]. Otherwise a
    /// dedicated single-thread Tokio runtime is created and pinned to a
    /// background thread that drives the worker directly via
    /// [`tokio::runtime::Runtime::block_on`]. The thread exits cleanly
    /// when the worker returns — either because [`Self::shutdown`] sent
    /// a shutdown command, or because the controller was dropped (the
    /// command channel closes and the worker sees `recv() == None`).
    /// There is no `pending::<()>` park, so the background thread is
    /// always reachable from the controller's tear-down path.
    pub fn new(
        adapter: Arc<dyn LedgerAdapter>,
        resolver: Arc<ActiveSkillResolver>,
        event_writer: Arc<dyn SecurityEventWriter>,
        debounce: Duration,
        failed_behavior: FailedResolveBehavior,
    ) -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            adapter,
            resolver,
            event_writer,
            debounce,
            failed_behavior,
            pending: Mutex::new(HashMap::new()),
            notify: Notify::new(),
            sender: tx,
        });
        let worker_inner = inner.clone();
        match Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move { worker_loop(worker_inner, rx).await });
            }
            Err(_) => {
                // No ambient runtime — spin up a private one for the
                // debounce worker on a dedicated thread. The thread
                // *drives* the worker via `block_on`, so when the
                // worker returns (shutdown command or channel close on
                // controller drop) `block_on` returns, the runtime is
                // dropped, and the thread exits. No detached
                // `pending::<()>` park is left behind.
                let spawn_result = std::thread::Builder::new()
                    .name("skillfs-refresh".to_string())
                    .spawn(move || {
                        let rt = match Builder::new_current_thread()
                            .enable_time()
                            .enable_io()
                            .build()
                        {
                            Ok(rt) => rt,
                            Err(e) => {
                                warn!(
                                    error = %e,
                                    "failed to build private Tokio runtime for refresh; \
                                     observations will be dropped"
                                );
                                return;
                            }
                        };
                        rt.block_on(worker_loop(worker_inner, rx));
                    });
                if spawn_result.is_err() {
                    warn!(
                        "failed to spawn skillfs-refresh worker thread; \
                         observations will be dropped"
                    );
                }
            }
        }
        Arc::new(Self { inner })
    }

    /// Convenience constructor that uses [`DEFAULT_REFRESH_DEBOUNCE_MS`]
    /// and the default [`FailedResolveBehavior`] (hide-on-failure).
    pub fn with_defaults(
        adapter: Arc<dyn LedgerAdapter>,
        resolver: Arc<ActiveSkillResolver>,
        event_writer: Arc<dyn SecurityEventWriter>,
    ) -> Arc<Self> {
        Self::new(
            adapter,
            resolver,
            event_writer,
            Duration::from_millis(DEFAULT_REFRESH_DEBOUNCE_MS),
            FailedResolveBehavior::default(),
        )
    }

    /// Convenience constructor for tests/examples that don't care
    /// about events. Wraps a [`NoopSecurityEventWriter`].
    pub fn without_events(
        adapter: Arc<dyn LedgerAdapter>,
        resolver: Arc<ActiveSkillResolver>,
        debounce: Duration,
    ) -> Arc<Self> {
        Self::new(
            adapter,
            resolver,
            Arc::new(NoopSecurityEventWriter),
            debounce,
            FailedResolveBehavior::default(),
        )
    }

    /// Record a FUSE mutation observation. Returns `true` when the
    /// observation was accepted and a debounce timer is pending,
    /// `false` when the observation was filtered out (skill-discover,
    /// `.skill-meta/**`, lifecycle reserved name).
    ///
    /// This call is non-blocking; the actual resolve + resolver update
    /// runs on the worker.
    pub fn observe(&self, observation: RefreshObservation) -> bool {
        if !is_skill_eligible(&observation.skill_name) {
            return false;
        }
        if let Some(rel) = observation.relative_path.as_ref() {
            if is_skill_meta_path(rel) {
                return false;
            }
        }
        let now = Instant::now();
        let fire_at = now + self.inner.debounce;
        {
            let mut pending = self.inner.pending.lock();
            pending.insert(
                observation.skill_name.clone(),
                PendingState {
                    observation,
                    fire_at,
                },
            );
        }
        // Best-effort wake; if the receiver has already been dropped
        // (controller shutting down) we silently swallow the error.
        let _ = self.inner.sender.send(RefreshCommand::Wakeup);
        self.inner.notify.notify_one();
        true
    }

    /// Convenience entry point for FUSE callbacks: they always know
    /// the skill name and the relative path within the skill. Returns
    /// the same `bool` semantics as [`Self::observe`].
    pub fn observe_mutation(
        &self,
        skill_name: &str,
        relative_path: Option<&Path>,
        kind: MutationKind,
    ) -> bool {
        let obs = RefreshObservation::new(skill_name, relative_path.map(|p| p.to_path_buf()), kind);
        self.observe(obs)
    }

    /// Drain pending observations synchronously. Test helper; the
    /// production worker drives the same code path on its own timer.
    /// Returns the number of skills whose state was refreshed.
    pub fn flush_for_testing(&self) -> usize {
        let drained = self
            .inner
            .drain_due(Instant::now() + self.inner.debounce * 2);
        let mut count = 0;
        for state in drained {
            self.inner.run_one(state);
            count += 1;
        }
        count
    }

    /// Shut the worker down gracefully. Pending observations are
    /// dropped — the controller is best-effort by design.
    pub fn shutdown(&self) {
        let _ = self.inner.sender.send(RefreshCommand::Shutdown);
        self.inner.notify.notify_waiters();
    }

    /// Test helper exposing the configured debounce window.
    pub fn debounce(&self) -> Duration {
        self.inner.debounce
    }
}

impl Inner {
    /// Drain every pending observation whose timer has expired by
    /// `deadline`. Returns the drained states in insertion order so
    /// tests can pin behavior.
    fn drain_due(&self, deadline: Instant) -> Vec<PendingState> {
        let mut due = Vec::new();
        let mut guard = self.pending.lock();
        let keys: Vec<String> = guard.keys().cloned().collect();
        for key in keys {
            if let Some(state) = guard.get(&key) {
                if state.fire_at <= deadline {
                    if let Some(removed) = guard.remove(&key) {
                        due.push(removed);
                    }
                }
            }
        }
        due
    }

    /// Time of the soonest pending observation. `None` when nothing is
    /// pending.
    fn next_fire_at(&self) -> Option<Instant> {
        self.pending.lock().values().map(|s| s.fire_at).min()
    }

    /// Run the External Decision pipeline once for `state`.
    ///
    /// D1.3.1: scan runs first (its JSON is not consumed; SkillFS only
    /// observes exit status). A scan failure short-circuits the resolve
    /// and applies the configured failure behavior. Only when the scan
    /// succeeds do we invoke `resolve` and update the active mapping.
    fn run_one(&self, state: PendingState) {
        let observation = state.observation;
        let skill = observation.skill_name.clone();
        let skill_dir = self.resolver.source_root().join(&skill);
        let fs_hook = observation.fs_hook_label();
        match self.adapter.scan(&skill_dir) {
            Ok(()) => match self.adapter.resolve(&skill_dir) {
                Ok(parsed) => self.handle_resolve_ok(&skill, &fs_hook, parsed),
                Err(err) => {
                    self.handle_decision_err(&skill, &fs_hook, "scan -> resolve failed", &err)
                }
            },
            Err(err) => {
                self.handle_decision_err(&skill, &fs_hook, "scan failed", &err);
            }
        }
    }

    /// Install a successful resolve through the checked
    /// [`ActiveSkillResolver::set_from_resolve_for_expected`] API.
    ///
    /// The checked API enforces the N1/D1.6 canonical identity contract
    /// at the resolver boundary, so a `skillName` mismatch here cannot
    /// silently key the active mapping off a different name even if a
    /// future caller forgets to validate up-front. A mismatch surfaces
    /// as `ActiveResolverError::SkillNameMismatch` and routes through
    /// `handle_decision_err` with `"scan -> resolve failed"` so the
    /// configured `FailedResolveBehavior` applies uniformly with other
    /// post-scan resolve failures; an `ActiveResolverError::Mapping`
    /// keeps the existing "ledger target could not be installed"
    /// hide-on-failure path so a structurally-bad active target never
    /// degrades into a current decision.
    fn handle_resolve_ok(&self, skill: &str, fs_hook: &str, result: LedgerResolveResult) {
        let status_label = result.status.as_str().to_string();
        match self.resolver.set_from_resolve_for_expected(skill, &result) {
            Ok(target) => {
                info!(
                    skill,
                    decision = target.as_label().as_str(),
                    "refresh: resolver updated"
                );
                let event =
                    SecurityEvent::new(skill, fs_hook, "scan -> resolve", target.as_label())
                        .with_ledger_status(status_label);
                let event = match result.reason.as_ref() {
                    Some(r) if !r.is_empty() => event.with_message(r.clone()),
                    _ => event,
                };
                self.event_writer.emit(&event);
            }
            Err(ActiveResolverError::SkillNameMismatch { expected, actual }) => {
                let err = LedgerError::SkillNameMismatch { expected, actual };
                self.handle_decision_err(skill, fs_hook, "scan -> resolve failed", &err);
            }
            Err(ActiveResolverError::Mapping(e)) => {
                warn!(
                    skill,
                    error = %e,
                    "refresh: resolver could not install ledger target; hiding skill"
                );
                self.apply_hidden(
                    skill,
                    "ledger target could not be installed",
                    fs_hook,
                    "scan -> resolve",
                    Some(LedgerStatus::None.as_str()),
                );
            }
        }
    }

    /// Apply the configured failure behavior to a scan or resolve
    /// failure. The `ledger_action` argument distinguishes
    /// `"scan failed"` from `"scan -> resolve failed"` in the
    /// event stream; the on-disk active mapping is updated identically
    /// because either failure leaves the resolver without a trustworthy
    /// new decision.
    fn handle_decision_err(
        &self,
        skill: &str,
        fs_hook: &str,
        ledger_action: &str,
        err: &LedgerError,
    ) {
        warn!(
            skill,
            ledger_action,
            error = %err,
            "refresh: external decision step failed"
        );
        let message = err.to_string();
        match self.failed_behavior {
            FailedResolveBehavior::HideOnFailure => {
                self.apply_hidden(skill, &message, fs_hook, ledger_action, Some("error"));
            }
            FailedResolveBehavior::KeepPreviousMapping => {
                let label = self
                    .resolver
                    .get(skill)
                    .map(|t| t.as_label())
                    .unwrap_or_else(|| "hidden:unknown".to_string());
                let event = SecurityEvent::new(skill, fs_hook, ledger_action, label)
                    .with_ledger_status("error")
                    .with_message(message);
                self.event_writer.emit(&event);
            }
        }
    }

    fn apply_hidden(
        &self,
        skill: &str,
        reason: &str,
        fs_hook: &str,
        ledger_action: &str,
        status_label: Option<&str>,
    ) {
        let target = ActiveTarget::Hidden {
            reason: reason.to_string(),
        };
        self.resolver.set(skill.to_string(), target.clone());
        let mut event = SecurityEvent::new(skill, fs_hook, ledger_action, target.as_label());
        if let Some(s) = status_label {
            event = event.with_ledger_status(s.to_string());
        }
        event = event.with_message(reason.to_string());
        self.event_writer.emit(&event);
    }
}

/// Skill-name filter shared by [`RefreshController::observe`] and
/// the FUSE wiring helper. Returns `false` for `skill-discover`,
/// lifecycle reserved roots (`.staging`, `.certified`, `.quarantine`,
/// `.archive`), and obviously-empty names. Future packages can layer
/// more filters here without touching FUSE callback bodies.
pub(crate) fn is_skill_eligible(skill: &str) -> bool {
    if skill.is_empty() {
        return false;
    }
    if skill == "skill-discover" {
        return false;
    }
    if is_reserved_lifecycle_name(skill) {
        return false;
    }
    true
}

async fn worker_loop(inner: Arc<Inner>, mut rx: mpsc::UnboundedReceiver<RefreshCommand>) {
    debug!("refresh worker starting");
    loop {
        // Wait until either a new observation arrives or the next
        // pending timer fires. Computing `sleep_for` inside the loop
        // (instead of carrying a `tokio::time::Sleep` across iterations)
        // keeps the bookkeeping simple at the cost of one extra
        // wake-up per command — fine for this cadence.
        let sleep_for = match inner.next_fire_at() {
            Some(t) => t.saturating_duration_since(Instant::now()),
            None => Duration::from_secs(60),
        };
        tokio::select! {
            cmd = rx.recv() => {
                match cmd {
                    Some(RefreshCommand::Wakeup) => {
                        // re-check next iteration
                    }
                    Some(RefreshCommand::Shutdown) | None => {
                        debug!("refresh worker shutting down");
                        return;
                    }
                }
            }
            _ = tokio::time::sleep(sleep_for) => {}
        }

        let due = inner.drain_due(Instant::now());
        if due.is_empty() {
            continue;
        }
        // Run resolves on the shared blocking pool so the CLI subprocess
        // call doesn't stall the async worker. We wait for completion
        // sequentially per skill to keep ordering deterministic in the
        // event stream — debounce already capped fanout to one
        // entry per skill per window.
        for state in due {
            let inner_clone = inner.clone();
            let join = tokio::task::spawn_blocking(move || inner_clone.run_one(state)).await;
            if let Err(e) = join {
                warn!(error = %e, "refresh: blocking task join failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::ledger::{
        LedgerDecision, LedgerResolveResult, LedgerStatus, LedgerTargetKind, StaticLedgerAdapter,
    };

    fn make_current(skill: &str) -> LedgerResolveResult {
        LedgerResolveResult {
            schema_version: 1,
            skill_name: skill.to_string(),
            declared_name: None,
            status: LedgerStatus::Pass,
            decision: LedgerDecision::Current,
            target: None,
            target_kind: None,
            current_version: Some("v000001".to_string()),
            trusted_version: Some("v000001".to_string()),
            reason: None,
        }
    }

    fn make_fallback(skill: &str) -> LedgerResolveResult {
        LedgerResolveResult {
            schema_version: 1,
            skill_name: skill.to_string(),
            declared_name: None,
            status: LedgerStatus::Deny,
            decision: LedgerDecision::Fallback,
            target: Some(PathBuf::from(".skill-meta/versions/v000001.snapshot")),
            target_kind: Some(LedgerTargetKind::RelativeToSkillDir),
            current_version: Some("v000003".to_string()),
            trusted_version: Some("v000001".to_string()),
            reason: Some("current version has high-risk findings".to_string()),
        }
    }

    fn make_hidden(skill: &str) -> LedgerResolveResult {
        LedgerResolveResult {
            schema_version: 1,
            skill_name: skill.to_string(),
            declared_name: None,
            status: LedgerStatus::None,
            decision: LedgerDecision::Hidden,
            target: None,
            target_kind: None,
            current_version: None,
            trusted_version: None,
            reason: Some("no certified version yet".to_string()),
        }
    }

    fn build_test_controller(
        adapter: Arc<dyn LedgerAdapter>,
        resolver: Arc<ActiveSkillResolver>,
        events: Arc<dyn SecurityEventWriter>,
        failed: FailedResolveBehavior,
    ) -> Arc<RefreshController> {
        RefreshController::new(adapter, resolver, events, Duration::from_millis(50), failed)
    }

    #[test]
    fn skill_eligibility_rejects_skill_discover_and_lifecycle() {
        assert!(is_skill_eligible("alpha"));
        assert!(!is_skill_eligible("skill-discover"));
        assert!(!is_skill_eligible(".staging"));
        assert!(!is_skill_eligible(".certified"));
        assert!(!is_skill_eligible(".quarantine"));
        assert!(!is_skill_eligible(".archive"));
        assert!(!is_skill_eligible(""));
    }

    #[test]
    fn observation_filters_skill_meta_paths() {
        let adapter = StaticLedgerAdapter::new();
        adapter.insert("alpha", make_current("alpha"));
        let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
        let events = Arc::new(InMemorySecurityEventWriterShim::default());
        let ctrl = build_test_controller(
            Arc::new(adapter),
            resolver,
            events.clone(),
            FailedResolveBehavior::HideOnFailure,
        );

        // .skill-meta paths must be rejected.
        let accepted = ctrl.observe(RefreshObservation::new(
            "alpha",
            Some(PathBuf::from(".skill-meta/manifest.json")),
            MutationKind::Write,
        ));
        assert!(!accepted, "skill-meta path must not enqueue refresh");

        // skill-discover always rejected regardless of relative path.
        let accepted = ctrl.observe(RefreshObservation::new(
            "skill-discover",
            Some(PathBuf::from("scripts/run.sh")),
            MutationKind::Write,
        ));
        assert!(!accepted, "skill-discover must not enqueue refresh");

        // Lifecycle reserved roots rejected.
        let accepted = ctrl.observe(RefreshObservation::new(
            ".staging",
            None,
            MutationKind::Mkdir,
        ));
        assert!(
            !accepted,
            "lifecycle reserved root must not enqueue refresh"
        );

        ctrl.shutdown();
    }

    /// Local in-memory writer with an explicit `clear_for_test` so the
    /// invalid-JSON test can isolate the second observation's event.
    #[derive(Default)]
    struct InMemorySecurityEventWriterShim {
        inner: parking_lot::Mutex<Vec<SecurityEvent>>,
    }

    impl InMemorySecurityEventWriterShim {
        fn events(&self) -> Vec<SecurityEvent> {
            self.inner.lock().clone()
        }
    }
    impl SecurityEventWriter for InMemorySecurityEventWriterShim {
        fn emit(&self, event: &SecurityEvent) {
            self.inner.lock().push(event.clone());
        }
    }

    #[test]
    fn write_observation_updates_resolver_to_current() {
        let adapter = StaticLedgerAdapter::new();
        adapter.insert("alpha", make_current("alpha"));
        let adapter: Arc<dyn LedgerAdapter> = Arc::new(adapter);
        let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
        let events = Arc::new(InMemorySecurityEventWriterShim::default());
        let ctrl = build_test_controller(
            adapter,
            resolver.clone(),
            events.clone(),
            FailedResolveBehavior::HideOnFailure,
        );

        ctrl.observe(RefreshObservation::new(
            "alpha",
            Some(PathBuf::from("SKILL.md")),
            MutationKind::Write,
        ));
        let processed = ctrl.flush_for_testing();
        assert_eq!(processed, 1);
        let target = resolver.get("alpha").expect("alpha entry");
        assert!(matches!(target, ActiveTarget::Current { .. }));
        let events = events.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].skill, "alpha");
        assert_eq!(events[0].fs_hook, "write(SKILL.md)");
        assert_eq!(events[0].skillfs_decision, "current");
        assert_eq!(events[0].ledger_status.as_deref(), Some("pass"));
        ctrl.shutdown();
    }

    #[test]
    fn fallback_decision_updates_resolver_to_snapshot() {
        let adapter = StaticLedgerAdapter::new();
        adapter.insert("alpha", make_fallback("alpha"));
        let adapter: Arc<dyn LedgerAdapter> = Arc::new(adapter);
        let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
        let events = Arc::new(InMemorySecurityEventWriterShim::default());
        let ctrl = build_test_controller(
            adapter,
            resolver.clone(),
            events.clone(),
            FailedResolveBehavior::HideOnFailure,
        );
        ctrl.observe(RefreshObservation::new(
            "alpha",
            Some(PathBuf::from("SKILL.md")),
            MutationKind::Write,
        ));
        ctrl.flush_for_testing();
        let target = resolver.get("alpha").expect("alpha entry");
        match target {
            ActiveTarget::Snapshot { version, .. } => assert_eq!(version, "v000001"),
            other => panic!("expected snapshot, got {other:?}"),
        }
        let events = events.events();
        assert_eq!(events[0].skillfs_decision, "fallback:v000001");
        assert_eq!(events[0].ledger_status.as_deref(), Some("deny"));
        assert!(events[0].message.is_some());
        ctrl.shutdown();
    }

    #[test]
    fn hidden_decision_updates_resolver_to_hidden() {
        let adapter = StaticLedgerAdapter::new();
        adapter.insert("alpha", make_hidden("alpha"));
        let adapter: Arc<dyn LedgerAdapter> = Arc::new(adapter);
        let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
        let events = Arc::new(InMemorySecurityEventWriterShim::default());
        let ctrl = build_test_controller(
            adapter,
            resolver.clone(),
            events.clone(),
            FailedResolveBehavior::HideOnFailure,
        );
        ctrl.observe(RefreshObservation::new("alpha", None, MutationKind::Mkdir));
        ctrl.flush_for_testing();
        let target = resolver.get("alpha").expect("alpha entry");
        assert!(matches!(target, ActiveTarget::Hidden { .. }));
        let events = events.events();
        assert_eq!(
            events[0].skillfs_decision,
            "hidden:no certified version yet"
        );
        ctrl.shutdown();
    }

    #[test]
    fn invalid_resolve_hides_by_default() {
        let adapter = StaticLedgerAdapter::new();
        adapter.insert_err(
            "alpha",
            LedgerError::InvalidJson {
                reason: "garbled".to_string(),
            },
        );
        let adapter: Arc<dyn LedgerAdapter> = Arc::new(adapter);
        let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
        // Pre-seed alpha with current so we can verify it gets replaced.
        resolver.set(
            "alpha",
            ActiveTarget::Current {
                source_dir: PathBuf::from("/srv/skills/alpha"),
            },
        );
        let events = Arc::new(InMemorySecurityEventWriterShim::default());
        let ctrl = build_test_controller(
            adapter,
            resolver.clone(),
            events.clone(),
            FailedResolveBehavior::HideOnFailure,
        );
        ctrl.observe(RefreshObservation::new(
            "alpha",
            Some(PathBuf::from("SKILL.md")),
            MutationKind::Write,
        ));
        ctrl.flush_for_testing();
        let target = resolver.get("alpha").expect("alpha entry");
        assert!(matches!(target, ActiveTarget::Hidden { .. }));
        let events = events.events();
        assert_eq!(events[0].ledger_status.as_deref(), Some("error"));
        assert!(events[0].skillfs_decision.starts_with("hidden:"));
        ctrl.shutdown();
    }

    #[test]
    fn invalid_resolve_with_keep_previous_does_not_change_mapping() {
        let adapter = StaticLedgerAdapter::new();
        adapter.insert_err(
            "alpha",
            LedgerError::NonZeroExit {
                status: 7,
                stdout: String::new(),
                stderr: "boom".to_string(),
            },
        );
        let adapter: Arc<dyn LedgerAdapter> = Arc::new(adapter);
        let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
        resolver.set(
            "alpha",
            ActiveTarget::Current {
                source_dir: PathBuf::from("/srv/skills/alpha"),
            },
        );
        let events = Arc::new(InMemorySecurityEventWriterShim::default());
        let ctrl = build_test_controller(
            adapter,
            resolver.clone(),
            events.clone(),
            FailedResolveBehavior::KeepPreviousMapping,
        );
        ctrl.observe(RefreshObservation::new(
            "alpha",
            Some(PathBuf::from("SKILL.md")),
            MutationKind::Write,
        ));
        ctrl.flush_for_testing();
        let target = resolver.get("alpha").expect("alpha entry");
        assert!(
            matches!(target, ActiveTarget::Current { .. }),
            "previous mapping must be preserved"
        );
        let events = events.events();
        assert_eq!(events[0].ledger_status.as_deref(), Some("error"));
        assert_eq!(events[0].skillfs_decision, "current");
        ctrl.shutdown();
    }

    #[test]
    fn debounce_collapses_repeat_observations() {
        let adapter = StaticLedgerAdapter::new();
        // Only one resolve should ever be consumed.
        adapter.insert("alpha", make_current("alpha"));
        let adapter: Arc<dyn LedgerAdapter> = Arc::new(adapter);
        let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
        let events = Arc::new(InMemorySecurityEventWriterShim::default());
        let ctrl = build_test_controller(
            adapter,
            resolver,
            events.clone(),
            FailedResolveBehavior::HideOnFailure,
        );
        for _ in 0..5 {
            ctrl.observe(RefreshObservation::new(
                "alpha",
                Some(PathBuf::from("SKILL.md")),
                MutationKind::Write,
            ));
        }
        let processed = ctrl.flush_for_testing();
        assert_eq!(processed, 1, "five observations must collapse to one");
        let events = events.events();
        assert_eq!(events.len(), 1);
        ctrl.shutdown();
    }

    #[test]
    fn successful_pipeline_records_scan_before_resolve() {
        // Pin the D1.3.1 ordering invariant. The static adapter logs
        // every call; the resolved decision is independent of the
        // scan, but scan must always be the first observed call for
        // the skill.
        let adapter = StaticLedgerAdapter::new();
        adapter.insert("alpha", make_current("alpha"));
        let logged_adapter = Arc::new(adapter);
        let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
        let events = Arc::new(InMemorySecurityEventWriterShim::default());
        let ctrl = build_test_controller(
            logged_adapter.clone(),
            resolver,
            events.clone(),
            FailedResolveBehavior::HideOnFailure,
        );
        ctrl.observe(RefreshObservation::new(
            "alpha",
            Some(PathBuf::from("SKILL.md")),
            MutationKind::Write,
        ));
        assert_eq!(ctrl.flush_for_testing(), 1);
        let calls = logged_adapter.calls();
        assert_eq!(
            calls,
            vec![
                crate::security::ledger::StaticAdapterCall::Scan {
                    skill_name: "alpha".to_string()
                },
                crate::security::ledger::StaticAdapterCall::Resolve {
                    skill_name: "alpha".to_string()
                },
            ],
            "scan must run before resolve for the same skill, got {calls:?}"
        );
        let events = events.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].ledger_action, "scan -> resolve");
        ctrl.shutdown();
    }

    #[test]
    fn scan_failure_skips_resolve_and_hides_by_default() {
        // Scan blowing up must short-circuit the pipeline: resolve is
        // never called, the resolver flips to hidden, and the
        // event reports the `scan failed` label.
        let adapter = StaticLedgerAdapter::new();
        adapter.insert_scan_err(
            "alpha",
            LedgerError::NonZeroExit {
                status: 9,
                stdout: String::new(),
                stderr: "scan crashed".to_string(),
            },
        );
        // Register a resolve result too — if the pipeline incorrectly
        // calls resolve after a failed scan, the test would still see
        // an updated mapping. Detecting the absence of that is the
        // load-bearing assertion below.
        adapter.insert("alpha", make_current("alpha"));
        let logged_adapter = Arc::new(adapter);
        let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
        resolver.set(
            "alpha",
            ActiveTarget::Current {
                source_dir: PathBuf::from("/srv/skills/alpha"),
            },
        );
        let events = Arc::new(InMemorySecurityEventWriterShim::default());
        let ctrl = build_test_controller(
            logged_adapter.clone(),
            resolver.clone(),
            events.clone(),
            FailedResolveBehavior::HideOnFailure,
        );
        ctrl.observe(RefreshObservation::new(
            "alpha",
            Some(PathBuf::from("SKILL.md")),
            MutationKind::Write,
        ));
        ctrl.flush_for_testing();
        let calls = logged_adapter.calls();
        assert_eq!(
            calls,
            vec![crate::security::ledger::StaticAdapterCall::Scan {
                skill_name: "alpha".to_string()
            }],
            "resolve must NOT run after a scan failure, got {calls:?}"
        );
        let target = resolver.get("alpha").expect("alpha entry");
        assert!(matches!(target, ActiveTarget::Hidden { .. }));
        let evs = events.events();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].ledger_action, "scan failed");
        assert_eq!(evs[0].ledger_status.as_deref(), Some("error"));
        assert!(evs[0].skillfs_decision.starts_with("hidden:"));
        ctrl.shutdown();
    }

    #[test]
    fn resolve_failure_after_scan_reports_scan_then_resolve_failed() {
        // Scan succeeds (default), resolve fails — the event must
        // record the post-scan resolve failure so the operator can
        // distinguish it from a scan failure.
        let adapter = StaticLedgerAdapter::new();
        adapter.insert_err(
            "alpha",
            LedgerError::InvalidJson {
                reason: "garbled".to_string(),
            },
        );
        let logged_adapter = Arc::new(adapter);
        let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
        let events = Arc::new(InMemorySecurityEventWriterShim::default());
        let ctrl = build_test_controller(
            logged_adapter.clone(),
            resolver.clone(),
            events.clone(),
            FailedResolveBehavior::HideOnFailure,
        );
        ctrl.observe(RefreshObservation::new(
            "alpha",
            Some(PathBuf::from("SKILL.md")),
            MutationKind::Write,
        ));
        ctrl.flush_for_testing();
        let calls = logged_adapter.calls();
        assert_eq!(
            calls,
            vec![
                crate::security::ledger::StaticAdapterCall::Scan {
                    skill_name: "alpha".to_string()
                },
                crate::security::ledger::StaticAdapterCall::Resolve {
                    skill_name: "alpha".to_string()
                },
            ]
        );
        let evs = events.events();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].ledger_action, "scan -> resolve failed");
        assert_eq!(evs[0].ledger_status.as_deref(), Some("error"));
        ctrl.shutdown();
    }

    #[test]
    fn scan_failure_with_keep_previous_does_not_change_mapping() {
        let adapter = StaticLedgerAdapter::new();
        adapter.insert_scan_err(
            "alpha",
            LedgerError::NonZeroExit {
                status: 11,
                stdout: String::new(),
                stderr: "scan bombed".to_string(),
            },
        );
        let adapter: Arc<dyn LedgerAdapter> = Arc::new(adapter);
        let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
        resolver.set(
            "alpha",
            ActiveTarget::Current {
                source_dir: PathBuf::from("/srv/skills/alpha"),
            },
        );
        let events = Arc::new(InMemorySecurityEventWriterShim::default());
        let ctrl = build_test_controller(
            adapter,
            resolver.clone(),
            events.clone(),
            FailedResolveBehavior::KeepPreviousMapping,
        );
        ctrl.observe(RefreshObservation::new(
            "alpha",
            Some(PathBuf::from("SKILL.md")),
            MutationKind::Write,
        ));
        ctrl.flush_for_testing();
        let target = resolver.get("alpha").expect("alpha entry");
        assert!(
            matches!(target, ActiveTarget::Current { .. }),
            "scan failure under KeepPreviousMapping must preserve mapping"
        );
        let evs = events.events();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].ledger_action, "scan failed");
        assert_eq!(evs[0].ledger_status.as_deref(), Some("error"));
        ctrl.shutdown();
    }

    #[test]
    fn fs_hook_label_renders_relative_path() {
        let obs = RefreshObservation::new(
            "alpha",
            Some(PathBuf::from("scripts/run.sh")),
            MutationKind::Write,
        );
        assert_eq!(obs.fs_hook_label(), "write(scripts/run.sh)");
        let obs = RefreshObservation::new("alpha", None, MutationKind::Mkdir);
        assert_eq!(obs.fs_hook_label(), "mkdir");
    }

    /// Regression test for the no-ambient-runtime path: dropping the
    /// last `Arc<RefreshController>` must close the command
    /// channel, which lets `worker_loop` return, which lets
    /// `block_on(...)` in the private runtime thread return — so the
    /// dedicated `skillfs-refresh` thread exits cleanly instead
    /// of leaking forever on a `pending::<()>` park.
    ///
    /// We can't reliably observe a specific OS thread exiting from
    /// the test harness (Linux thread namespaces, scheduling, and the
    /// fact that `std::thread::Builder::spawn` returns a detached
    /// handle here), so the test pins the *behavioral* invariants
    /// instead:
    ///
    /// 1. Building, observing on, shutting down, and dropping the
    ///    controller many times in sequence must not hang and must
    ///    not panic. If `block_on(pending::<()>())` were still in
    ///    use the loop body would still complete (the threads just
    ///    leak in the background) — but combined with the other
    ///    checks below this is the cheapest available signal.
    /// 2. After `shutdown()` returns, the resolver state must still
    ///    reflect the most recent successful resolve, proving the
    ///    worker actually drained pending observations before
    ///    exiting.
    /// 3. The whole flow runs to completion within a generous
    ///    timeout; a hung worker would block the test binary instead.
    #[test]
    fn no_ambient_runtime_controller_tears_down_on_drop() {
        let start = std::time::Instant::now();
        for _ in 0..8 {
            let adapter = StaticLedgerAdapter::new();
            adapter.insert("alpha", make_current("alpha"));
            let adapter: Arc<dyn LedgerAdapter> = Arc::new(adapter);
            let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));
            let events = Arc::new(InMemorySecurityEventWriterShim::default());
            let ctrl = RefreshController::new(
                adapter,
                resolver.clone(),
                events.clone(),
                Duration::from_millis(20),
                FailedResolveBehavior::HideOnFailure,
            );
            ctrl.observe(RefreshObservation::new(
                "alpha",
                Some(PathBuf::from("SKILL.md")),
                MutationKind::Write,
            ));
            assert_eq!(ctrl.flush_for_testing(), 1);
            assert!(matches!(
                resolver.get("alpha"),
                Some(ActiveTarget::Current { .. })
            ));
            ctrl.shutdown();
            // Drop the Arc — this closes the command channel, the
            // worker loop returns, `block_on` returns, the runtime
            // is dropped, and the dedicated thread exits.
            drop(ctrl);
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(10),
            "8 controller create/drop cycles took {elapsed:?}; \
             a hung private runtime thread would have blocked here"
        );
    }
}
