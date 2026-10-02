//! A5: Activation State Watcher And Continuous Convergence.
//!
//! Background thread that periodically checks activation freshness for
//! all known skills and reloads when changes are detected. This closes
//! the convergence gaps left by the startup-bootstrap + notify-triggered
//! poll-reload path:
//!
//! * Daemon writes activation after mount starts but before any FUSE
//!   mutation.
//! * Notify-triggered `poll_reload_skill()` times out but the daemon
//!   writes activation later.
//! * Daemon reconcile, config change, or operator action updates
//!   activation without being triggered by a FUSE mutation.
//! * Notify socket delivery fails, then the daemon repairs state
//!   through its own reconcile path.
//! * Startup reconcile sends notify but no reload is attached to
//!   that reconcile for the current mount.
//!
//! The watcher operates entirely through
//! [`ActivationReloadController`] (which holds the
//! [`ActiveSkillResolver`]). It does not interact with the FUSE event
//! loop and does not need to be threaded through `SkillFs` or
//! `MountConfig`.
//!
//! Scope constraints (matching A5 spec):
//!
//! * Does NOT parse `latest.json`, findings, policy, or scan status.
//! * Does NOT run scan/check inside SkillFS.
//! * Does NOT read activation on every FUSE read — only on periodic
//!   tick or immediate-check signal.
//! * Does NOT change fd pin semantics. Already-opened handles keep
//!   their pinned target; new `lookup`/`open`/`readdir` use the
//!   refreshed resolver.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tracing::{debug, info, warn};

use super::activation_reload::{ActivationFreshness, ActivationReloadController, ReloadOutcome};
use super::protocol_events::{ProtocolEvent, ProtocolEventWriter};

/// Default periodic interval for the activation watcher (milliseconds).
pub const DEFAULT_WATCHER_INTERVAL_MS: u64 = 30_000;

// ---------------------------------------------------------------------------
// WatcherRegistrar — lightweight trait for cross-component registration
// ---------------------------------------------------------------------------

/// Trait for registering skills with the activation watcher.
///
/// Injected into [`super::notify::NotifyController`] so that skills
/// observed through FUSE/inbox mutations are automatically tracked for
/// late-activation convergence, even when the notify-triggered poll
/// times out.
pub trait WatcherRegistrar: Send + Sync {
    fn register(&self, skill_name: &str);
}

// ---------------------------------------------------------------------------
// ActivationWatcher
// ---------------------------------------------------------------------------

/// Background convergence loop that watches activation state for all
/// known skills and reloads when the daemon writes new activation.
pub struct ActivationWatcher {
    inner: Arc<WatcherInner>,
}

struct WatcherInner {
    reload_controller: Arc<ActivationReloadController>,
    protocol_event_writer: Arc<dyn ProtocolEventWriter>,
    interval: Duration,
    /// Per-skill freshness baselines. Updated after each successful
    /// reload so only genuine freshness advances trigger re-reads.
    baselines: Mutex<HashMap<String, ActivationFreshness>>,
    /// Explicitly registered skill names (from startup bootstrap or
    /// `register_skill`). Merged with resolver keys on each tick.
    registered: Mutex<HashSet<String>>,
    shutdown_tx: Mutex<Option<std::sync::mpsc::Sender<WatcherSignal>>>,
}

#[derive(Debug)]
enum WatcherSignal {
    ImmediateCheck(Vec<String>),
    Shutdown,
}

impl ActivationWatcher {
    pub fn new(
        reload_controller: Arc<ActivationReloadController>,
        protocol_event_writer: Arc<dyn ProtocolEventWriter>,
        interval: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(WatcherInner {
                reload_controller,
                protocol_event_writer,
                interval,
                baselines: Mutex::new(HashMap::new()),
                registered: Mutex::new(HashSet::new()),
                shutdown_tx: Mutex::new(None),
            }),
        }
    }

    /// Build with the default interval.
    pub fn with_defaults(
        reload_controller: Arc<ActivationReloadController>,
        protocol_event_writer: Arc<dyn ProtocolEventWriter>,
    ) -> Self {
        Self::new(
            reload_controller,
            protocol_event_writer,
            Duration::from_millis(DEFAULT_WATCHER_INTERVAL_MS),
        )
    }

    /// Register a skill name for tracking. The watcher will check this
    /// skill on every tick even if it is not yet in the resolver.
    pub fn register_skill(&self, name: &str) {
        self.inner.registered.lock().insert(name.to_string());
    }

    /// Bulk-register skill names.
    pub fn register_skills(&self, names: &[String]) {
        let mut registered = self.inner.registered.lock();
        for name in names {
            registered.insert(name.clone());
        }
    }

    /// Wake the watcher early and check the specified skills on the
    /// next tick. Non-blocking; if the signal channel is full or the
    /// watcher is not running, this is silently dropped.
    pub fn schedule_immediate_check(&self, names: Vec<String>) {
        let guard = self.inner.shutdown_tx.lock();
        if let Some(ref tx) = *guard {
            let _ = tx.send(WatcherSignal::ImmediateCheck(names));
        }
    }

    /// Spawn the background watcher thread. Idempotent — calling
    /// `start` twice has no effect if the thread is already running.
    pub fn start(&self) {
        let (tx, rx) = std::sync::mpsc::channel();
        {
            let mut guard = self.inner.shutdown_tx.lock();
            if guard.is_some() {
                return;
            }
            *guard = Some(tx);
        }
        let inner = self.inner.clone();
        let spawn_result = std::thread::Builder::new()
            .name("skillfs-activation-watcher".to_string())
            .spawn(move || {
                watcher_loop(inner, rx);
            });
        if let Err(e) = spawn_result {
            warn!(
                error = %e,
                "activation watcher: failed to spawn background thread"
            );
            *self.inner.shutdown_tx.lock() = None;
        }
    }

    /// Signal the background thread to stop.
    pub fn shutdown(&self) {
        let tx = self.inner.shutdown_tx.lock().take();
        if let Some(tx) = tx {
            let _ = tx.send(WatcherSignal::Shutdown);
        }
    }

    /// Run one convergence tick synchronously. Test helper — checks all
    /// tracked skills and returns the outcomes.
    pub fn tick_for_testing(&self) -> Vec<(String, ReloadOutcome)> {
        self.inner.run_tick(None)
    }

    pub fn interval(&self) -> Duration {
        self.inner.interval
    }
}

impl WatcherRegistrar for ActivationWatcher {
    fn register(&self, skill_name: &str) {
        self.register_skill(skill_name);
    }
}

impl Drop for ActivationWatcher {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ---------------------------------------------------------------------------
// Background loop
// ---------------------------------------------------------------------------

fn watcher_loop(inner: Arc<WatcherInner>, rx: std::sync::mpsc::Receiver<WatcherSignal>) {
    info!(
        interval_ms = inner.interval.as_millis() as u64,
        "activation watcher: starting"
    );

    loop {
        match rx.recv_timeout(inner.interval) {
            Ok(WatcherSignal::Shutdown) => {
                debug!("activation watcher: shutdown signal received");
                return;
            }
            Ok(WatcherSignal::ImmediateCheck(names)) => {
                debug!(
                    count = names.len(),
                    "activation watcher: immediate check requested"
                );
                inner.run_tick(Some(&names));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                inner.run_tick(None);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                debug!("activation watcher: channel disconnected, stopping");
                return;
            }
        }
    }
}

impl WatcherInner {
    /// Run one convergence tick. When `only_skills` is `Some`, only
    /// those skills are checked; otherwise all tracked skills are
    /// checked. Returns outcomes for skills that were reloaded.
    fn run_tick(&self, only_skills: Option<&[String]>) -> Vec<(String, ReloadOutcome)> {
        let skills = match only_skills {
            Some(names) => names.to_vec(),
            None => self.tracked_skills(),
        };

        if skills.is_empty() {
            return Vec::new();
        }

        let source_root = self.reload_controller.source_root();
        let mut outcomes = Vec::new();

        for name in &skills {
            let skill_dir = source_root.join(name);
            let current = ActivationFreshness::snapshot(&skill_dir);
            let needs_reload = {
                let baselines = self.baselines.lock();
                match baselines.get(name) {
                    None => true,
                    Some(baseline) => baseline.has_advanced(&current),
                }
            };

            if needs_reload {
                let outcome = self.reload_controller.reload_skill_once(name);
                debug!(
                    skill = %name,
                    outcome = outcome.as_protocol_label(),
                    "activation watcher: reload"
                );

                let skill_dir_str = skill_dir.to_string_lossy().to_string();
                let reload_event = ProtocolEvent::with_reload_outcome(
                    &skill_dir_str,
                    name,
                    outcome.as_protocol_label(),
                );
                self.protocol_event_writer.emit(&reload_event);

                self.baselines
                    .lock()
                    .insert(name.clone(), ActivationFreshness::snapshot(&skill_dir));
                outcomes.push((name.clone(), outcome));
            }
        }

        if !outcomes.is_empty() {
            debug!(
                reloaded = outcomes.len(),
                "activation watcher: tick complete"
            );
        }

        outcomes
    }

    /// Collect the union of explicitly registered skills and current
    /// resolver keys. This ensures skills added by other paths (e.g.
    /// inbox install, notify reload) are picked up automatically.
    fn tracked_skills(&self) -> Vec<String> {
        let mut skills: HashSet<String> = self.registered.lock().clone();
        for key in self.reload_controller.resolver().snapshot().keys() {
            skills.insert(key.clone());
        }
        let mut sorted: Vec<String> = skills.into_iter().collect();
        sorted.sort();
        sorted
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::activation_reload::ActivationReloadController;
    use crate::security::active::{ActiveSkillResolver, ActiveTarget};
    use crate::security::protocol_events::InMemoryProtocolEventWriter;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn setup_skill_with_snapshot(dir: &Path, skill: &str, version: &str) {
        let skill_dir = dir.join(skill);
        let snap = skill_dir.join(format!(".skill-meta/versions/{version}.snapshot"));
        std::fs::create_dir_all(&snap).unwrap();
    }

    fn setup_skill_with_activation(dir: &Path, skill: &str, activation_json: &str) {
        let skill_dir = dir.join(skill);
        let meta = skill_dir.join(".skill-meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("activation.json"), activation_json).unwrap();
    }

    fn update_activation_fresh(dir: &Path, skill: &str, activation_json: &str) {
        let skill_dir = dir.join(skill);
        let before = ActivationFreshness::snapshot(&skill_dir);
        loop {
            std::thread::sleep(Duration::from_millis(15));
            setup_skill_with_activation(dir, skill, activation_json);
            let after = ActivationFreshness::snapshot(&skill_dir);
            if before.has_advanced(&after) {
                break;
            }
        }
    }

    fn make_watcher(
        dir: &Path,
        interval: Duration,
    ) -> (
        ActivationWatcher,
        Arc<ActiveSkillResolver>,
        Arc<InMemoryProtocolEventWriter>,
    ) {
        let resolver = Arc::new(ActiveSkillResolver::new(dir));
        let reload_ctrl = Arc::new(ActivationReloadController::new(
            dir,
            resolver.clone(),
            Duration::from_millis(50),
            Duration::from_millis(500),
        ));
        let writer = Arc::new(InMemoryProtocolEventWriter::new());
        let watcher = ActivationWatcher::new(reload_ctrl, writer.clone(), interval);
        (watcher, resolver, writer)
    }

    // ─────────────────────────────────────────────────────────────────
    // Basic detection
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn watcher_detects_freshness_advance() {
        let dir = tempfile::tempdir().unwrap();
        setup_skill_with_snapshot(dir.path(), "alpha", "v000001");
        setup_skill_with_activation(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );

        let (watcher, resolver, writer) = make_watcher(dir.path(), Duration::from_secs(60));
        watcher.register_skill("alpha");

        // First tick: loads activation, sets baseline.
        let outcomes = watcher.tick_for_testing();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(
            outcomes[0].1,
            ReloadOutcome::Updated(ActiveTarget::Snapshot { .. })
        ));
        assert!(matches!(
            resolver.get("alpha"),
            Some(ActiveTarget::Snapshot { ref version, .. }) if version == "v000001.snapshot"
        ));

        // No change — second tick should produce no outcomes.
        let outcomes2 = watcher.tick_for_testing();
        assert!(
            outcomes2.is_empty(),
            "no freshness advance should produce no reloads, got {outcomes2:?}"
        );

        // Now update activation to v2.
        setup_skill_with_snapshot(dir.path(), "alpha", "v000002");
        update_activation_fresh(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000002.snapshot"}"#,
        );

        let outcomes3 = watcher.tick_for_testing();
        assert_eq!(outcomes3.len(), 1);
        match &outcomes3[0].1 {
            ReloadOutcome::Updated(ActiveTarget::Snapshot { version, .. }) => {
                assert_eq!(version, "v000002.snapshot");
            }
            other => panic!("expected Updated(Snapshot v2), got {other:?}"),
        }
        assert!(matches!(
            resolver.get("alpha"),
            Some(ActiveTarget::Snapshot { ref version, .. }) if version == "v000002.snapshot"
        ));

        // Protocol events should have been emitted.
        assert!(writer.len() >= 2, "expected at least 2 protocol events");
        let events = writer.events();
        assert!(events.iter().all(|e| e.event_kind == "reload"));
        assert!(events.iter().all(|e| e.reload_outcome.is_some()));
    }

    #[test]
    fn watcher_no_change_no_reload() {
        let dir = tempfile::tempdir().unwrap();
        setup_skill_with_snapshot(dir.path(), "alpha", "v000001");
        setup_skill_with_activation(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );

        let (watcher, _resolver, writer) = make_watcher(dir.path(), Duration::from_secs(60));
        watcher.register_skill("alpha");

        // First tick sets baseline.
        watcher.tick_for_testing();
        let initial_events = writer.len();

        // Multiple ticks without change.
        for _ in 0..3 {
            let outcomes = watcher.tick_for_testing();
            assert!(outcomes.is_empty());
        }
        assert_eq!(
            writer.len(),
            initial_events,
            "no freshness advance must not emit more protocol events"
        );
    }

    // ─────────────────────────────────────────────────────────────────
    // Registration
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn watcher_register_skill_adds_to_tracked() {
        let dir = tempfile::tempdir().unwrap();
        setup_skill_with_snapshot(dir.path(), "alpha", "v000001");
        setup_skill_with_activation(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );

        let (watcher, resolver, _writer) = make_watcher(dir.path(), Duration::from_secs(60));

        // Not registered yet — tick finds nothing.
        let outcomes = watcher.tick_for_testing();
        assert!(outcomes.is_empty());
        assert!(resolver.get("alpha").is_none());

        // Register and tick.
        watcher.register_skill("alpha");
        let outcomes = watcher.tick_for_testing();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(
            resolver.get("alpha"),
            Some(ActiveTarget::Snapshot { .. })
        ));
    }

    #[test]
    fn watcher_picks_up_resolver_keys() {
        let dir = tempfile::tempdir().unwrap();
        setup_skill_with_snapshot(dir.path(), "alpha", "v000001");
        setup_skill_with_activation(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );

        let (watcher, resolver, _writer) = make_watcher(dir.path(), Duration::from_secs(60));

        // Add skill to resolver directly (simulating notify reload).
        resolver.set(
            "alpha".to_string(),
            ActiveTarget::Hidden {
                reason: "initial".to_string(),
            },
        );

        // Watcher should pick it up via resolver keys, not registration.
        let outcomes = watcher.tick_for_testing();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(
            resolver.get("alpha"),
            Some(ActiveTarget::Snapshot { .. })
        ));
    }

    // ─────────────────────────────────────────────────────────────────
    // Immediate check
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn watcher_schedule_immediate_wakes_early() {
        let dir = tempfile::tempdir().unwrap();
        setup_skill_with_snapshot(dir.path(), "alpha", "v000001");
        setup_skill_with_activation(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );

        let (watcher, resolver, _writer) = make_watcher(dir.path(), Duration::from_secs(300));
        watcher.register_skill("alpha");
        watcher.start();

        // Schedule an immediate check — should not wait 300s.
        watcher.schedule_immediate_check(vec!["alpha".to_string()]);

        // Wait a bit for the watcher to process.
        std::thread::sleep(Duration::from_millis(500));

        assert!(
            matches!(resolver.get("alpha"), Some(ActiveTarget::Snapshot { .. })),
            "immediate check must reload without waiting for full interval"
        );

        watcher.shutdown();
    }

    // ─────────────────────────────────────────────────────────────────
    // Shutdown
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn watcher_shutdown_stops_thread() {
        let dir = tempfile::tempdir().unwrap();
        let (watcher, _resolver, _writer) = make_watcher(dir.path(), Duration::from_millis(50));
        watcher.start();
        std::thread::sleep(Duration::from_millis(100));
        watcher.shutdown();

        // Verify the watcher does not spin after shutdown.
        let start = std::time::Instant::now();
        drop(watcher);
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "watcher drop after shutdown took too long: {elapsed:?}"
        );
    }

    #[test]
    fn watcher_start_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let (watcher, _resolver, _writer) = make_watcher(dir.path(), Duration::from_secs(60));
        watcher.start();
        watcher.start(); // second call should be no-op
        watcher.shutdown();
    }

    // ─────────────────────────────────────────────────────────────────
    // Fail-safe
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn watcher_invalid_activation_hides() {
        let dir = tempfile::tempdir().unwrap();
        setup_skill_with_activation(dir.path(), "alpha", "CORRUPTED");

        let (watcher, resolver, _writer) = make_watcher(dir.path(), Duration::from_secs(60));
        watcher.register_skill("alpha");

        let outcomes = watcher.tick_for_testing();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(
            outcomes[0].1,
            ReloadOutcome::FailSafeHidden { .. }
        ));
        assert!(matches!(
            resolver.get("alpha"),
            Some(ActiveTarget::Hidden { .. })
        ));
    }

    // ─────────────────────────────────────────────────────────────────
    // Missing then created
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn watcher_missing_then_created() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("alpha")).unwrap();

        let (watcher, resolver, _writer) = make_watcher(dir.path(), Duration::from_secs(60));
        watcher.register_skill("alpha");

        // First tick: activation missing -> fail-safe hidden.
        let outcomes = watcher.tick_for_testing();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(
            outcomes[0].1,
            ReloadOutcome::FailSafeHidden { .. }
        ));
        assert!(matches!(
            resolver.get("alpha"),
            Some(ActiveTarget::Hidden { .. })
        ));

        // Now write valid activation.
        setup_skill_with_snapshot(dir.path(), "alpha", "v000001");
        update_activation_fresh(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );

        // Next tick: freshness advanced -> reload picks up snapshot.
        let outcomes = watcher.tick_for_testing();
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0].1 {
            ReloadOutcome::Updated(ActiveTarget::Snapshot { version, .. }) => {
                assert_eq!(version, "v000001.snapshot");
            }
            other => panic!("expected Updated(Snapshot), got {other:?}"),
        }
        assert!(matches!(
            resolver.get("alpha"),
            Some(ActiveTarget::Snapshot { .. })
        ));
    }

    // ─────────────────────────────────────────────────────────────────
    // xattr freshness (conditional on filesystem support)
    // ─────────────────────────────────────────────────────────────────

    fn user_xattr_supported(dir: &Path) -> bool {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c_path = match CString::new(dir.as_os_str().as_bytes()) {
            Ok(c) => c,
            Err(_) => return false,
        };
        let c_name = match CString::new("user.skillfs.probe") {
            Ok(c) => c,
            Err(_) => return false,
        };
        let rc = unsafe {
            libc::lsetxattr(
                c_path.as_ptr(),
                c_name.as_ptr(),
                b"1".as_ptr() as *const libc::c_void,
                1,
                0,
            )
        };
        if rc != 0 {
            return false;
        }
        unsafe {
            libc::lremovexattr(c_path.as_ptr(), c_name.as_ptr());
        }
        true
    }

    fn set_activation_xattr(dir: &Path, value: &str) {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c_path = CString::new(dir.as_os_str().as_bytes()).unwrap();
        let c_name = CString::new(super::super::activation::ACTIVATION_XATTR).unwrap();
        let rc = unsafe {
            libc::lsetxattr(
                c_path.as_ptr(),
                c_name.as_ptr(),
                value.as_ptr() as *const libc::c_void,
                value.len(),
                0,
            )
        };
        assert_eq!(
            rc,
            0,
            "lsetxattr failed: {}",
            std::io::Error::last_os_error()
        );
    }

    fn xattr_capable_tempdir() -> Option<tempfile::TempDir> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(env_path) = std::env::var("SKILLFS_XATTR_TEST_ROOT") {
            if !env_path.is_empty() {
                candidates.push(PathBuf::from(env_path));
            }
        }
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for ancestor in manifest_dir.ancestors() {
            if ancestor.join("Cargo.lock").exists() {
                candidates.push(ancestor.join("target").join("xattr-tests"));
                break;
            }
        }
        if let Some(home) = std::env::var_os("HOME") {
            let mut path = PathBuf::from(home);
            path.push(".cache");
            path.push("skillfs-xattr-tests");
            candidates.push(path);
        }
        for cand in candidates {
            if std::fs::create_dir_all(&cand).is_err() {
                continue;
            }
            let td = match tempfile::Builder::new()
                .prefix("a5-xattr-")
                .tempdir_in(&cand)
            {
                Ok(d) => d,
                Err(_) => continue,
            };
            if user_xattr_supported(td.path()) {
                return Some(td);
            }
        }
        None
    }

    #[test]
    fn watcher_xattr_only_update_via_ctime() {
        let td = match xattr_capable_tempdir() {
            Some(d) => d,
            None => {
                eprintln!("SKIP: no xattr-capable filesystem for A5 xattr watcher test");
                return;
            }
        };
        let dir = td.path();
        setup_skill_with_snapshot(dir, "alpha", "v000001");
        setup_skill_with_snapshot(dir, "alpha", "v000002");

        let skill_dir = dir.join("alpha");
        std::fs::create_dir_all(skill_dir.join(".skill-meta")).unwrap();
        set_activation_xattr(
            &skill_dir,
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );

        let resolver = Arc::new(ActiveSkillResolver::new(dir));
        let reload_ctrl = Arc::new(ActivationReloadController::new(
            dir,
            resolver.clone(),
            Duration::from_millis(50),
            Duration::from_millis(500),
        ));
        let writer = Arc::new(InMemoryProtocolEventWriter::new());
        let watcher = ActivationWatcher::new(reload_ctrl, writer.clone(), Duration::from_secs(60));
        watcher.register_skill("alpha");

        // First tick: loads v1.
        let outcomes = watcher.tick_for_testing();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(
            resolver.get("alpha"),
            Some(ActiveTarget::Snapshot { ref version, .. }) if version == "v000001.snapshot"
        ));

        // Update xattr only (no json file) — wait for ctime advance.
        let baseline = ActivationFreshness::snapshot(&skill_dir);
        loop {
            std::thread::sleep(Duration::from_millis(15));
            set_activation_xattr(
                &skill_dir,
                r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000002.snapshot"}"#,
            );
            let current = ActivationFreshness::snapshot(&skill_dir);
            if baseline.has_advanced(&current) {
                break;
            }
        }

        // Next tick: detects ctime advance and reloads.
        let outcomes = watcher.tick_for_testing();
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0].1 {
            ReloadOutcome::Updated(ActiveTarget::Snapshot { version, .. }) => {
                assert_eq!(version, "v000002.snapshot");
            }
            other => panic!("expected Updated(Snapshot v2) from xattr-only change, got {other:?}"),
        }
    }

    // ─────────────────────────────────────────────────────────────────
    // Accessor
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn watcher_interval_accessor() {
        let dir = tempfile::tempdir().unwrap();
        let (watcher, _, _) = make_watcher(dir.path(), Duration::from_millis(42));
        assert_eq!(watcher.interval(), Duration::from_millis(42));
    }
}
