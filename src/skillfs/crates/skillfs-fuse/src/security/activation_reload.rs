//! A3: Runtime Activation Reload.
//!
//! After the daemon writes a new `activation.json` / xattr, this module
//! re-reads the activation contract and updates the in-memory
//! [`ActiveSkillResolver`] without requiring a remount.
//!
//! Design: poll-after-notify with composite freshness. When
//! [`NotifyController`] completes a `send_one` (protocol event + socket
//! notify), the reload controller starts a short-cycle poll for the
//! notified skill. The poll uses an [`ActivationFreshness`] baseline
//! captured *before* the notify is sent. The baseline records both the
//! `activation.json` file mtime and the skill directory ctime (which
//! `lsetxattr` updates on Linux). The poll considers the activation
//! "fresh" when *either* timestamp advances, so both the A2
//! xattr-primary path and the json-fallback path are covered.
//!
//! * Fresh artifact, changed target → update resolver and stop.
//! * Fresh artifact, same target → stop (`Unchanged`); daemon
//!   re-confirmed the decision.
//! * Stale freshness (daemon hasn't written yet) → keep polling.
//! * I/O error (file not yet created) → keep polling.
//! * Parse / validation error on a fresh artifact → fail-safe hidden.
//! * Timeout → keep current mapping and log a warning.
//!
//! Scope constraints:
//!
//! * Write paths are unchanged — writes still land in source/current.
//! * `notify accepted` is NOT treated as a security conclusion.
//! * We do NOT parse `latest.json`, `scanStatus`, `policy`, `findings`.
//! * Already-opened fds are pinned and unaffected; new `open` calls see
//!   the refreshed target.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use parking_lot::Mutex;
use tracing::{debug, info, warn};

use super::activation::{
    ACTIVATION_FILE, ActivationError, fail_safe_hidden, load_activation_prefer_xattr,
};
use super::active::{ActiveSkillResolver, ActiveTarget};

/// Default poll interval between activation reads (milliseconds).
pub const DEFAULT_RELOAD_INTERVAL_MS: u64 = 250;

/// Default total timeout for the poll loop (milliseconds).
pub const DEFAULT_RELOAD_TIMEOUT_MS: u64 = 5000;

// ---------------------------------------------------------------------------
// ReloadMode
// ---------------------------------------------------------------------------

/// Runtime activation reload mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReloadMode {
    /// Reload is disabled. The resolver is populated at startup only.
    #[default]
    Off,
    /// Poll activation after each notify send completes.
    Poll,
}

impl ReloadMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "off" => Some(Self::Off),
            "poll" => Some(Self::Poll),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Poll => "poll",
        }
    }
}

impl std::fmt::Display for ReloadMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// ReloadOutcome
// ---------------------------------------------------------------------------

/// Result of a single reload attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReloadOutcome {
    /// Resolver updated to a new target (different from baseline).
    Updated(ActiveTarget),
    /// The resolver kept its entry because the decision was unchanged or
    /// the skill moved to a different directory during reload.
    Unchanged,
    /// Poll timed out without observing a readable activation file.
    /// Current mapping kept.
    Timeout,
    /// Activation read failed; resolver updated to hidden (fail-safe).
    FailSafeHidden { reason: String },
}

impl ReloadOutcome {
    /// Protocol-event label for observability.
    pub fn as_protocol_label(&self) -> &'static str {
        match self {
            Self::Updated(_) => "activation_updated",
            Self::Unchanged => "activation_unchanged",
            Self::Timeout => "activation_timeout",
            Self::FailSafeHidden { .. } => "activation_invalid_hidden",
        }
    }
}

// ---------------------------------------------------------------------------
// ActivationReloadController
// ---------------------------------------------------------------------------

/// Controller that polls `activation.json` / xattr after a notify and
/// updates the [`ActiveSkillResolver`].
pub struct ActivationReloadController {
    source_root: PathBuf,
    resolver: Arc<ActiveSkillResolver>,
    interval: Duration,
    timeout: Duration,
    publication: Mutex<()>,
}

impl ActivationReloadController {
    pub fn new(
        source_root: impl Into<PathBuf>,
        resolver: Arc<ActiveSkillResolver>,
        interval: Duration,
        timeout: Duration,
    ) -> Self {
        Self {
            source_root: source_root.into(),
            resolver,
            interval,
            timeout,
            publication: Mutex::new(()),
        }
    }

    /// Build with default interval / timeout values.
    pub fn with_defaults(
        source_root: impl Into<PathBuf>,
        resolver: Arc<ActiveSkillResolver>,
    ) -> Self {
        Self::new(
            source_root,
            resolver,
            Duration::from_millis(DEFAULT_RELOAD_INTERVAL_MS),
            Duration::from_millis(DEFAULT_RELOAD_TIMEOUT_MS),
        )
    }

    pub fn source_root(&self) -> &Path {
        &self.source_root
    }

    pub(super) fn skill_dir(&self, skill_name: &str) -> PathBuf {
        self.resolver.skill_dir_under(&self.source_root, skill_name)
    }

    fn publish_in_dir(&self, skill_name: &str, skill_dir: &Path, target: ActiveTarget) -> bool {
        // Serialize the check and publication with other reloads. A reload
        // for the new owner cannot publish between this check and set().
        // skill_dir() releases the store read lock before set() runs.
        let _publication = self.publication.lock();
        if self.skill_dir(skill_name) != skill_dir {
            return false;
        }
        self.resolver.set(skill_name, target);
        true
    }

    pub fn resolver(&self) -> &Arc<ActiveSkillResolver> {
        &self.resolver
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Reload activation for a single skill once (no polling).
    ///
    /// Reads activation via A2 (`load_activation_prefer_xattr`), updates
    /// the resolver on success, fail-safe hides on error.
    pub fn reload_skill_once(&self, skill_name: &str) -> ReloadOutcome {
        self.reload_skill_once_in_dir(skill_name, &self.skill_dir(skill_name))
    }

    pub(super) fn reload_skill_once_in_dir(
        &self,
        skill_name: &str,
        skill_dir: &Path,
    ) -> ReloadOutcome {
        match load_activation_prefer_xattr(skill_dir) {
            Ok(target) => {
                if !self.publish_in_dir(skill_name, skill_dir, target.clone()) {
                    return ReloadOutcome::Unchanged;
                }
                debug!(
                    skill = %skill_name,
                    target = %target.as_label(),
                    "activation reload: updated"
                );
                ReloadOutcome::Updated(target)
            }
            Err(e) => {
                if !self.publish_in_dir(skill_name, skill_dir, fail_safe_hidden(&e)) {
                    return ReloadOutcome::Unchanged;
                }
                warn!(
                    skill = %skill_name,
                    error = %e,
                    "activation reload: fail-safe hidden"
                );
                ReloadOutcome::FailSafeHidden {
                    reason: e.to_string(),
                }
            }
        }
    }

    /// Reload activation for multiple skills once (no polling).
    ///
    /// Returns a `Vec<(skill_name, ReloadOutcome)>` in the same order as
    /// the input. Each skill is loaded via A2
    /// (`load_activation_prefer_xattr`).
    pub fn reload_many_once(&self, skill_names: &[String]) -> Vec<(String, ReloadOutcome)> {
        skill_names
            .iter()
            .map(|name| {
                let outcome = self.reload_skill_once(name);
                (name.clone(), outcome)
            })
            .collect()
    }

    /// Reload all skills currently in the resolver (no polling).
    ///
    /// Iterates the resolver's current snapshot of keys, calls
    /// `reload_skill_once` for each, and returns the outcomes. Useful
    /// for management commands and testing.
    pub fn reload_known_skills_once(&self) -> Vec<(String, ReloadOutcome)> {
        let names: Vec<String> = self.resolver.snapshot().keys().cloned().collect();
        self.reload_many_once(&names)
    }

    /// Snapshot the activation freshness for `skill_name`. Called by
    /// `NotifyController` *before* sending the notify to the daemon, so
    /// the baseline captures the state before the daemon processes the
    /// request. Covers both `activation.json` mtime and skill dir
    /// ctime (for xattr changes).
    pub fn snapshot_freshness(&self, skill_name: &str) -> ActivationFreshness {
        let skill_dir = self.skill_dir(skill_name);
        ActivationFreshness::snapshot(&skill_dir)
    }

    /// Poll activation for `skill_name` until the activation artifact
    /// is freshly written by the daemon, or timeout expires.
    ///
    /// The caller supplies a freshness baseline taken *before* the
    /// daemon receives the notify. The poll checks both
    /// `activation.json` mtime and skill directory ctime (updated by
    /// xattr writes) on each iteration, so both the xattr-primary and
    /// json-fallback paths are covered.
    ///
    /// Semantics:
    ///
    /// * **Fresh write, target changed** → update resolver, return
    ///   `Updated`.
    /// * **Fresh write, target unchanged** → return `Unchanged`
    ///   immediately. The daemon re-confirmed the same decision.
    /// * **Stale freshness** (daemon hasn't written yet) → keep polling.
    /// * **I/O error on a fresh artifact** (race: stat'd then removed)
    ///   → keep polling.
    /// * **Parse / validation error on a fresh artifact** → fail-safe
    ///   hidden immediately.
    /// * **Directory ownership changed** → keep current mapping, return
    ///   `Unchanged`.
    /// * **Timeout** → keep current mapping, return `Timeout`.
    pub fn poll_reload_skill(
        &self,
        skill_name: &str,
        baseline_freshness: ActivationFreshness,
    ) -> ReloadOutcome {
        let skill_dir = baseline_freshness.skill_dir();
        let baseline = self.resolver.get(skill_name);
        let deadline = Instant::now() + self.timeout;

        loop {
            if self.skill_dir(skill_name) != skill_dir {
                return ReloadOutcome::Unchanged;
            }
            let current = ActivationFreshness::snapshot(skill_dir);
            let is_fresh = baseline_freshness.has_advanced(&current);

            if is_fresh {
                match load_activation_prefer_xattr(skill_dir) {
                    Ok(target) => {
                        if !targets_equal(baseline.as_ref(), &target) {
                            if !self.publish_in_dir(skill_name, skill_dir, target.clone()) {
                                return ReloadOutcome::Unchanged;
                            }
                            info!(
                                skill = %skill_name,
                                target = %target.as_label(),
                                "activation reload: new target observed"
                            );
                            return ReloadOutcome::Updated(target);
                        }
                        debug!(
                            skill = %skill_name,
                            "activation reload: daemon re-confirmed same target"
                        );
                        return ReloadOutcome::Unchanged;
                    }
                    Err(ActivationError::Io(_)) => {
                        // Race: stat'd but removed before read.
                    }
                    Err(e) => {
                        if !self.publish_in_dir(skill_name, skill_dir, fail_safe_hidden(&e)) {
                            return ReloadOutcome::Unchanged;
                        }
                        warn!(
                            skill = %skill_name,
                            error = %e,
                            "activation reload: fresh artifact invalid, fail-safe hidden"
                        );
                        return ReloadOutcome::FailSafeHidden {
                            reason: e.to_string(),
                        };
                    }
                }
            }

            if Instant::now() >= deadline {
                warn!(
                    skill = %skill_name,
                    timeout_ms = self.timeout.as_millis() as u64,
                    "activation reload: poll timeout, keeping current mapping"
                );
                return ReloadOutcome::Timeout;
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            std::thread::sleep(self.interval.min(remaining));
        }
    }
}

/// Compare two targets for equality in terms of the activation decision.
/// `None` baseline matches nothing (first load after boot).
fn targets_equal(baseline: Option<&ActiveTarget>, new: &ActiveTarget) -> bool {
    match (baseline, new) {
        (None, _) => false,
        (Some(ActiveTarget::Hidden { .. }), ActiveTarget::Hidden { .. }) => true,
        (
            Some(ActiveTarget::Snapshot {
                snapshot_dir: a, ..
            }),
            ActiveTarget::Snapshot {
                snapshot_dir: b, ..
            },
        ) => a == b,
        (
            Some(ActiveTarget::Current { source_dir: a }),
            ActiveTarget::Current { source_dir: b },
        ) => a == b,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// ActivationFreshness — composite freshness token
// ---------------------------------------------------------------------------

/// Composite freshness token covering both activation sources:
///
/// * `json_mtime` — `activation.json` file mtime. Advances when the
///   daemon writes a new `activation.json`.
/// * `dir_ctime` — skill directory ctime. Advances when an xattr is
///   set on the directory (`lsetxattr` updates `st_ctime` on Linux).
///
/// The poll considers the activation "fresh" when *either* timestamp
/// has advanced past the baseline, so both the xattr-primary and
/// json-fallback paths are covered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationFreshness {
    skill_dir: PathBuf,
    pub json_mtime: Option<SystemTime>,
    pub dir_ctime: Option<SystemTime>,
}

impl ActivationFreshness {
    /// Snapshot the current freshness state for `skill_dir`.
    pub fn snapshot(skill_dir: &Path) -> Self {
        let json_mtime = {
            let path = skill_dir.join(ACTIVATION_FILE);
            std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok())
        };
        let dir_ctime = dir_ctime_systemtime(skill_dir);
        Self {
            skill_dir: skill_dir.to_path_buf(),
            json_mtime,
            dir_ctime,
        }
    }

    /// The directory whose activation freshness was recorded.
    pub fn skill_dir(&self) -> &Path {
        &self.skill_dir
    }

    /// Whether freshness has yet to advance for this directory.
    pub fn is_stale_compared_to(&self, current: &ActivationFreshness) -> bool {
        !self.has_advanced(current)
    }

    pub fn has_advanced(&self, current: &ActivationFreshness) -> bool {
        self.skill_dir == current.skill_dir
            && (time_advanced(&self.json_mtime, &current.json_mtime)
                || time_advanced(&self.dir_ctime, &current.dir_ctime))
    }
}

fn time_advanced(baseline: &Option<SystemTime>, current: &Option<SystemTime>) -> bool {
    match (baseline, current) {
        (None, Some(_)) => true,
        (Some(b), Some(c)) => c > b,
        _ => false,
    }
}

/// Read `st_ctime` of a directory as a `SystemTime`. On Linux, xattr
/// mutations (`lsetxattr`) update `st_ctime` but not `st_mtime`.
#[cfg(target_os = "linux")]
fn dir_ctime_systemtime(path: &Path) -> Option<SystemTime> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    let ctime_secs = meta.ctime();
    let ctime_nsec = meta.ctime_nsec();
    if ctime_secs < 0 {
        return None;
    }
    let d = std::time::Duration::new(ctime_secs as u64, ctime_nsec as u32);
    Some(std::time::UNIX_EPOCH + d)
}

#[cfg(not(target_os = "linux"))]
fn dir_ctime_systemtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn setup_skill_with_activation(dir: &Path, skill: &str, activation_json: &str) {
        let skill_dir = dir.join(skill);
        let meta = skill_dir.join(".skill-meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("activation.json"), activation_json).unwrap();
    }

    /// Write activation.json with a guaranteed mtime advance from any
    /// prior write. Spins until the file's mtime actually changes,
    /// accounting for coarse filesystem mtime granularity.
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

    fn setup_skill_with_snapshot(dir: &Path, skill: &str, version: &str) {
        let skill_dir = dir.join(skill);
        let snap = skill_dir.join(format!(".skill-meta/versions/{version}.snapshot"));
        std::fs::create_dir_all(&snap).unwrap();
    }

    // ─────────────────────────────────────────────────────────────────────
    // ReloadMode
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn reload_mode_round_trip() {
        assert_eq!(ReloadMode::parse("off"), Some(ReloadMode::Off));
        assert_eq!(ReloadMode::parse("poll"), Some(ReloadMode::Poll));
        assert_eq!(ReloadMode::parse("bogus"), None);
        assert_eq!(ReloadMode::Off.as_str(), "off");
        assert_eq!(ReloadMode::Poll.as_str(), "poll");
    }

    #[test]
    fn reload_mode_default_is_off() {
        assert_eq!(ReloadMode::default(), ReloadMode::Off);
    }

    // ─────────────────────────────────────────────────────────────────────
    // reload_skill_once
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn reload_once_valid_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        setup_skill_with_snapshot(dir.path(), "alpha", "v000001");
        setup_skill_with_activation(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );

        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        let ctrl = ActivationReloadController::with_defaults(dir.path(), resolver.clone());

        let outcome = ctrl.reload_skill_once("alpha");
        match outcome {
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

    #[test]
    fn reload_once_null_target_hides() {
        let dir = tempfile::tempdir().unwrap();
        setup_skill_with_activation(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": null}"#,
        );

        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        let ctrl = ActivationReloadController::with_defaults(dir.path(), resolver.clone());

        let outcome = ctrl.reload_skill_once("alpha");
        match outcome {
            ReloadOutcome::Updated(ActiveTarget::Hidden { .. }) => {}
            other => panic!("expected Updated(Hidden), got {other:?}"),
        }
        assert!(matches!(
            resolver.get("alpha"),
            Some(ActiveTarget::Hidden { .. })
        ));
    }

    #[test]
    fn reload_once_invalid_json_failsafe_hidden() {
        let dir = tempfile::tempdir().unwrap();
        setup_skill_with_activation(dir.path(), "alpha", "INVALID JSON");

        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        let ctrl = ActivationReloadController::with_defaults(dir.path(), resolver.clone());

        let outcome = ctrl.reload_skill_once("alpha");
        assert!(
            matches!(outcome, ReloadOutcome::FailSafeHidden { .. }),
            "invalid JSON must fail-safe hidden, got {outcome:?}"
        );
        assert!(matches!(
            resolver.get("alpha"),
            Some(ActiveTarget::Hidden { .. })
        ));
    }

    #[test]
    fn reload_once_missing_activation_failsafe_hidden() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("alpha")).unwrap();

        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        let ctrl = ActivationReloadController::with_defaults(dir.path(), resolver.clone());

        let outcome = ctrl.reload_skill_once("alpha");
        assert!(
            matches!(outcome, ReloadOutcome::FailSafeHidden { .. }),
            "missing activation must fail-safe hidden, got {outcome:?}"
        );
    }

    #[test]
    fn reload_once_snapshot_not_found_failsafe_hidden() {
        let dir = tempfile::tempdir().unwrap();
        setup_skill_with_activation(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000099.snapshot"}"#,
        );

        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        let ctrl = ActivationReloadController::with_defaults(dir.path(), resolver.clone());

        let outcome = ctrl.reload_skill_once("alpha");
        assert!(matches!(outcome, ReloadOutcome::FailSafeHidden { .. }));
    }

    // ─────────────────────────────────────────────────────────────────────
    // poll_reload_skill
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn old_owner_poll_cannot_replace_watchers_new_owner_decision() {
        use crate::security::{ActivationWatcher, NoopProtocolEventWriter};
        use parking_lot::RwLock;
        use skillfs_core::{parser::parse_skill_file, store::SkillStore};

        const FIRST: &str =
            r#"{"schemaVersion":1,"target":".skill-meta/versions/v000001.snapshot"}"#;
        const SECOND: &str =
            r#"{"schemaVersion":1,"target":".skill-meta/versions/v000002.snapshot"}"#;
        for old_update in [SECOND, "invalid activation"] {
            let root = tempfile::tempdir().unwrap();
            let cat_a = root.path().join("cat_a");
            let cat_b = root.path().join("cat_b");
            for category in [&cat_a, &cat_b] {
                setup_skill_with_snapshot(category, "demo", "v000001");
                setup_skill_with_snapshot(category, "demo", "v000002");
                setup_skill_with_activation(category, "demo", FIRST);
                std::fs::write(
                    category.join("demo/SKILL.md"),
                    "---\nname: demo\ndescription: fixture\n---\n",
                )
                .unwrap();
            }
            let store = Arc::new(RwLock::new(SkillStore::new()));
            let select = |category: &Path| {
                let manifest = category.join("demo/SKILL.md");
                store.write().upsert(parse_skill_file(&manifest).unwrap());
            };
            select(&cat_a);
            let resolver =
                Arc::new(ActiveSkillResolver::new(root.path()).with_store(store.clone()));
            let ctrl = Arc::new(ActivationReloadController::new(
                root.path(),
                resolver.clone(),
                Duration::from_millis(20),
                Duration::from_secs(2),
            ));
            let watcher =
                ActivationWatcher::with_defaults(ctrl.clone(), Arc::new(NoopProtocolEventWriter));
            assert!(matches!(
                ctrl.reload_skill_once("demo"),
                ReloadOutcome::Updated(_)
            ));
            let before = resolver.get_call_count();
            let freshness = ctrl.snapshot_freshness("demo");
            let poll_ctrl = ctrl.clone();
            let poll_freshness = freshness.clone();
            let poll =
                std::thread::spawn(move || poll_ctrl.poll_reload_skill("demo", poll_freshness));
            let deadline = Instant::now() + Duration::from_secs(1);
            while resolver.get_call_count() == before && Instant::now() < deadline {
                std::thread::yield_now();
            }
            assert!(resolver.get_call_count() > before, "poll did not start");

            select(&cat_b);
            let updates = watcher.tick_for_testing();
            let [(id, ReloadOutcome::Updated(new_target))] = updates.as_slice() else {
                panic!("watcher must activate the new owner: {updates:?}");
            };
            assert_eq!(id, "demo");
            assert_eq!(
                new_target.read_dir(),
                Some(
                    cat_b
                        .join("demo/.skill-meta/versions/v000001.snapshot")
                        .as_path()
                )
            );
            update_activation_fresh(&cat_a, "demo", old_update);
            assert_eq!(poll.join().unwrap(), ReloadOutcome::Unchanged);
            assert_eq!(
                ctrl.poll_reload_skill("demo", freshness),
                ReloadOutcome::Unchanged
            );
            assert_eq!(
                ctrl.reload_skill_once_in_dir("demo", &cat_a.join("demo")),
                ReloadOutcome::Unchanged
            );
            assert_eq!(resolver.get("demo").as_ref(), Some(new_target));
            assert!(watcher.tick_for_testing().is_empty());
        }
    }

    #[test]
    fn poll_reload_detects_change_from_snapshot_a_to_b() {
        let dir = tempfile::tempdir().unwrap();
        setup_skill_with_snapshot(dir.path(), "alpha", "v000001");
        setup_skill_with_snapshot(dir.path(), "alpha", "v000002");

        setup_skill_with_activation(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );
        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        let ctrl = ActivationReloadController::new(
            dir.path(),
            resolver.clone(),
            Duration::from_millis(50),
            Duration::from_millis(2000),
        );

        ctrl.reload_skill_once("alpha");

        // Snapshot mtime BEFORE daemon writes, then fresh-write.
        let baseline_freshness = ctrl.snapshot_freshness("alpha");
        update_activation_fresh(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000002.snapshot"}"#,
        );

        let outcome = ctrl.poll_reload_skill("alpha", baseline_freshness);
        match outcome {
            ReloadOutcome::Updated(ActiveTarget::Snapshot { version, .. }) => {
                assert_eq!(version, "v000002.snapshot");
            }
            other => panic!("expected Updated(Snapshot v2), got {other:?}"),
        }
        match resolver.get("alpha") {
            Some(ActiveTarget::Snapshot { version, .. }) => {
                assert_eq!(version, "v000002.snapshot");
            }
            other => panic!("expected resolver to have v2, got {other:?}"),
        }
    }

    #[test]
    fn poll_reload_unchanged_returns_immediately_on_fresh_write() {
        let dir = tempfile::tempdir().unwrap();
        setup_skill_with_snapshot(dir.path(), "alpha", "v000001");
        setup_skill_with_activation(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );

        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        let ctrl = ActivationReloadController::new(
            dir.path(),
            resolver.clone(),
            Duration::from_millis(50),
            Duration::from_millis(5000),
        );

        ctrl.reload_skill_once("alpha");

        let baseline_freshness = ctrl.snapshot_freshness("alpha");
        update_activation_fresh(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );

        let start = Instant::now();
        let outcome = ctrl.poll_reload_skill("alpha", baseline_freshness);
        let elapsed = start.elapsed();
        assert_eq!(outcome, ReloadOutcome::Unchanged);
        assert!(
            elapsed < Duration::from_millis(500),
            "unchanged poll must not block; took {elapsed:?}"
        );

        assert!(matches!(
            resolver.get("alpha"),
            Some(ActiveTarget::Snapshot { ref version, .. }) if version == "v000001.snapshot"
        ));
    }

    #[test]
    fn poll_stale_mtime_does_not_return_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        setup_skill_with_snapshot(dir.path(), "alpha", "v000001");
        setup_skill_with_activation(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );

        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        let ctrl = ActivationReloadController::new(
            dir.path(),
            resolver.clone(),
            Duration::from_millis(30),
            Duration::from_millis(100),
        );

        ctrl.reload_skill_once("alpha");

        // Snapshot mtime, then do NOT rewrite the file.
        let baseline_freshness = ctrl.snapshot_freshness("alpha");
        let outcome = ctrl.poll_reload_skill("alpha", baseline_freshness);
        assert_eq!(
            outcome,
            ReloadOutcome::Timeout,
            "stale activation must timeout, not return Unchanged"
        );
    }

    #[test]
    fn poll_reload_timeout_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("alpha")).unwrap();

        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        let ctrl = ActivationReloadController::new(
            dir.path(),
            resolver.clone(),
            Duration::from_millis(30),
            Duration::from_millis(100),
        );

        let baseline_freshness = ctrl.snapshot_freshness("alpha");
        let outcome = ctrl.poll_reload_skill("alpha", baseline_freshness);
        assert_eq!(outcome, ReloadOutcome::Timeout);
    }

    #[test]
    fn poll_reload_invalid_activation_failsafe_hidden() {
        let dir = tempfile::tempdir().unwrap();
        setup_skill_with_snapshot(dir.path(), "alpha", "v000001");
        setup_skill_with_activation(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );

        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        let ctrl = ActivationReloadController::new(
            dir.path(),
            resolver.clone(),
            Duration::from_millis(50),
            Duration::from_millis(2000),
        );

        ctrl.reload_skill_once("alpha");

        let baseline_freshness = ctrl.snapshot_freshness("alpha");
        update_activation_fresh(dir.path(), "alpha", "CORRUPTED");

        let outcome = ctrl.poll_reload_skill("alpha", baseline_freshness);
        assert!(
            matches!(outcome, ReloadOutcome::FailSafeHidden { .. }),
            "corrupted activation must fail-safe hidden, got {outcome:?}"
        );
        assert!(matches!(
            resolver.get("alpha"),
            Some(ActiveTarget::Hidden { .. })
        ));
    }

    #[test]
    fn poll_reload_target_null_hides() {
        let dir = tempfile::tempdir().unwrap();
        setup_skill_with_snapshot(dir.path(), "alpha", "v000001");
        setup_skill_with_activation(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );

        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        let ctrl = ActivationReloadController::new(
            dir.path(),
            resolver.clone(),
            Duration::from_millis(50),
            Duration::from_millis(2000),
        );

        ctrl.reload_skill_once("alpha");

        let baseline_freshness = ctrl.snapshot_freshness("alpha");
        update_activation_fresh(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": null}"#,
        );

        let outcome = ctrl.poll_reload_skill("alpha", baseline_freshness);
        match outcome {
            ReloadOutcome::Updated(ActiveTarget::Hidden { .. }) => {}
            other => panic!("expected Updated(Hidden), got {other:?}"),
        }
        assert!(matches!(
            resolver.get("alpha"),
            Some(ActiveTarget::Hidden { .. })
        ));
    }

    #[test]
    fn poll_reload_no_baseline_file_created_during_poll() {
        let dir = tempfile::tempdir().unwrap();
        setup_skill_with_snapshot(dir.path(), "alpha", "v000001");
        std::fs::create_dir_all(dir.path().join("alpha/.skill-meta")).unwrap();

        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        let dir_path = dir.path().to_path_buf();
        let ctrl = ActivationReloadController::new(
            dir.path(),
            resolver.clone(),
            Duration::from_millis(30),
            Duration::from_millis(1000),
        );

        // No activation file yet — json_mtime is None.
        let baseline_freshness = ctrl.snapshot_freshness("alpha");
        assert!(baseline_freshness.json_mtime.is_none());

        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            setup_skill_with_activation(
                &dir_path,
                "alpha",
                r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
            );
        });

        let outcome = ctrl.poll_reload_skill("alpha", baseline_freshness);
        writer.join().unwrap();
        match outcome {
            ReloadOutcome::Updated(ActiveTarget::Snapshot { version, .. }) => {
                assert_eq!(version, "v000001.snapshot");
            }
            other => panic!("expected Updated(Snapshot), got {other:?}"),
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // targets_equal
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn targets_equal_none_is_never_equal() {
        let snap = ActiveTarget::Snapshot {
            snapshot_dir: PathBuf::from("/a/b"),
            version: "v1".to_string(),
        };
        assert!(!targets_equal(None, &snap));
    }

    #[test]
    fn targets_equal_same_snapshot_is_equal() {
        let a = ActiveTarget::Snapshot {
            snapshot_dir: PathBuf::from("/a/b"),
            version: "v1".to_string(),
        };
        let b = ActiveTarget::Snapshot {
            snapshot_dir: PathBuf::from("/a/b"),
            version: "v2".to_string(),
        };
        assert!(targets_equal(Some(&a), &b));
    }

    #[test]
    fn targets_equal_different_snapshot_dirs() {
        let a = ActiveTarget::Snapshot {
            snapshot_dir: PathBuf::from("/a/v1"),
            version: "v1".to_string(),
        };
        let b = ActiveTarget::Snapshot {
            snapshot_dir: PathBuf::from("/a/v2"),
            version: "v2".to_string(),
        };
        assert!(!targets_equal(Some(&a), &b));
    }

    #[test]
    fn targets_equal_hidden_to_hidden() {
        let a = ActiveTarget::Hidden {
            reason: "r1".to_string(),
        };
        let b = ActiveTarget::Hidden {
            reason: "r2".to_string(),
        };
        assert!(targets_equal(Some(&a), &b));
    }

    #[test]
    fn targets_equal_different_variants() {
        let a = ActiveTarget::Hidden {
            reason: "r".to_string(),
        };
        let b = ActiveTarget::Snapshot {
            snapshot_dir: PathBuf::from("/x"),
            version: "v".to_string(),
        };
        assert!(!targets_equal(Some(&a), &b));
    }

    // ─────────────────────────────────────────────────────────────────────
    // Controller accessors
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn controller_accessors() {
        let dir = tempfile::tempdir().unwrap();
        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        let ctrl = ActivationReloadController::new(
            dir.path(),
            resolver,
            Duration::from_millis(100),
            Duration::from_millis(2000),
        );
        assert_eq!(ctrl.source_root(), dir.path());
        assert_eq!(ctrl.interval(), Duration::from_millis(100));
        assert_eq!(ctrl.timeout(), Duration::from_millis(2000));
    }

    #[test]
    fn with_defaults_uses_default_values() {
        let dir = tempfile::tempdir().unwrap();
        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        let ctrl = ActivationReloadController::with_defaults(dir.path(), resolver);
        assert_eq!(
            ctrl.interval(),
            Duration::from_millis(DEFAULT_RELOAD_INTERVAL_MS)
        );
        assert_eq!(
            ctrl.timeout(),
            Duration::from_millis(DEFAULT_RELOAD_TIMEOUT_MS)
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // P2 regressions: unchanged decisions must not block
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn poll_baseline_hidden_target_null_returns_unchanged_on_fresh_write() {
        let dir = tempfile::tempdir().unwrap();
        setup_skill_with_activation(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": null}"#,
        );

        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        resolver.set(
            "alpha".to_string(),
            ActiveTarget::Hidden {
                reason: "initial".to_string(),
            },
        );

        let ctrl = ActivationReloadController::new(
            dir.path(),
            resolver.clone(),
            Duration::from_millis(50),
            Duration::from_millis(5000),
        );

        let baseline_freshness = ctrl.snapshot_freshness("alpha");
        update_activation_fresh(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": null}"#,
        );

        let start = Instant::now();
        let outcome = ctrl.poll_reload_skill("alpha", baseline_freshness);
        let elapsed = start.elapsed();

        assert_eq!(outcome, ReloadOutcome::Unchanged);
        assert!(
            elapsed < Duration::from_millis(500),
            "hidden→hidden poll must not block; took {elapsed:?}"
        );
    }

    #[test]
    fn poll_baseline_snapshot_same_snapshot_returns_unchanged_on_fresh_write() {
        let dir = tempfile::tempdir().unwrap();
        setup_skill_with_snapshot(dir.path(), "alpha", "v000001");
        setup_skill_with_activation(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );

        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        let ctrl = ActivationReloadController::new(
            dir.path(),
            resolver.clone(),
            Duration::from_millis(50),
            Duration::from_millis(5000),
        );

        ctrl.reload_skill_once("alpha");

        let baseline_freshness = ctrl.snapshot_freshness("alpha");
        update_activation_fresh(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );

        let start = Instant::now();
        let outcome = ctrl.poll_reload_skill("alpha", baseline_freshness);
        let elapsed = start.elapsed();

        assert_eq!(outcome, ReloadOutcome::Unchanged);
        assert!(
            elapsed < Duration::from_millis(500),
            "same-snapshot poll must not block subsequent notify; took {elapsed:?}"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // P3 regression: interval > timeout sleep is clamped
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn poll_interval_larger_than_timeout_does_not_oversleep() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("alpha")).unwrap();

        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        let ctrl = ActivationReloadController::new(
            dir.path(),
            resolver.clone(),
            Duration::from_millis(60_000),
            Duration::from_millis(200),
        );

        let baseline_freshness = ctrl.snapshot_freshness("alpha");
        let start = Instant::now();
        let outcome = ctrl.poll_reload_skill("alpha", baseline_freshness);
        let elapsed = start.elapsed();

        assert_eq!(outcome, ReloadOutcome::Timeout);
        assert!(
            elapsed < Duration::from_secs(2),
            "poll with interval>timeout must clamp sleep; took {elapsed:?}"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // xattr freshness
    // ─────────────────────────────────────────────────────────────────────

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
                .prefix("a3-xattr-")
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
    fn poll_xattr_only_update_detected_via_dir_ctime() {
        let td = match xattr_capable_tempdir() {
            Some(d) => d,
            None => {
                eprintln!("SKIP: no xattr-capable filesystem for A3 xattr reload test");
                return;
            }
        };
        let dir = td.path();
        setup_skill_with_snapshot(dir, "alpha", "v000001");
        setup_skill_with_snapshot(dir, "alpha", "v000002");

        // Bootstrap with xattr pointing to v1 (no activation.json).
        let skill_dir = dir.join("alpha");
        std::fs::create_dir_all(skill_dir.join(".skill-meta")).unwrap();
        set_activation_xattr(
            &skill_dir,
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );

        let resolver = Arc::new(ActiveSkillResolver::new(dir));
        let ctrl = ActivationReloadController::new(
            dir,
            resolver.clone(),
            Duration::from_millis(30),
            Duration::from_millis(2000),
        );

        ctrl.reload_skill_once("alpha");
        assert!(matches!(
            resolver.get("alpha"),
            Some(ActiveTarget::Snapshot { ref version, .. }) if version == "v000001.snapshot"
        ));

        // Snapshot freshness, then update xattr only (no json file).
        let baseline_freshness = ctrl.snapshot_freshness("alpha");
        // Wait for ctime to advance.
        loop {
            std::thread::sleep(Duration::from_millis(15));
            set_activation_xattr(
                &skill_dir,
                r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000002.snapshot"}"#,
            );
            let current = ActivationFreshness::snapshot(&skill_dir);
            if baseline_freshness.has_advanced(&current) {
                break;
            }
        }

        let outcome = ctrl.poll_reload_skill("alpha", baseline_freshness);
        match outcome {
            ReloadOutcome::Updated(ActiveTarget::Snapshot { version, .. }) => {
                assert_eq!(version, "v000002.snapshot");
            }
            other => panic!("expected Updated(Snapshot v2) from xattr-only change, got {other:?}"),
        }
        match resolver.get("alpha") {
            Some(ActiveTarget::Snapshot { version, .. }) => {
                assert_eq!(version, "v000002.snapshot");
            }
            other => panic!("expected resolver to have v2 from xattr, got {other:?}"),
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // A4: ReloadOutcome protocol labels
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn reload_outcome_protocol_labels() {
        let updated = ReloadOutcome::Updated(ActiveTarget::Hidden {
            reason: "test".to_string(),
        });
        assert_eq!(updated.as_protocol_label(), "activation_updated");

        assert_eq!(
            ReloadOutcome::Unchanged.as_protocol_label(),
            "activation_unchanged"
        );
        assert_eq!(
            ReloadOutcome::Timeout.as_protocol_label(),
            "activation_timeout"
        );

        let hidden = ReloadOutcome::FailSafeHidden {
            reason: "bad json".to_string(),
        };
        assert_eq!(hidden.as_protocol_label(), "activation_invalid_hidden");
    }

    // ─────────────────────────────────────────────────────────────────────
    // A4: reload_many_once
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn reload_many_once_loads_multiple_skills() {
        let dir = tempfile::tempdir().unwrap();
        setup_skill_with_snapshot(dir.path(), "alpha", "v000001");
        setup_skill_with_activation(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );
        setup_skill_with_activation(
            dir.path(),
            "beta",
            r#"{"schemaVersion": 1, "target": null}"#,
        );

        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        let ctrl = ActivationReloadController::with_defaults(dir.path(), resolver.clone());

        let results = ctrl.reload_many_once(&["alpha".to_string(), "beta".to_string()]);
        assert_eq!(results.len(), 2);

        let (name0, outcome0) = &results[0];
        assert_eq!(name0, "alpha");
        assert!(matches!(
            outcome0,
            ReloadOutcome::Updated(ActiveTarget::Snapshot { .. })
        ));

        let (name1, outcome1) = &results[1];
        assert_eq!(name1, "beta");
        assert!(matches!(
            outcome1,
            ReloadOutcome::Updated(ActiveTarget::Hidden { .. })
        ));

        assert!(matches!(
            resolver.get("alpha"),
            Some(ActiveTarget::Snapshot { .. })
        ));
        assert!(matches!(
            resolver.get("beta"),
            Some(ActiveTarget::Hidden { .. })
        ));
    }

    #[test]
    fn reload_many_once_mixed_valid_and_invalid() {
        let dir = tempfile::tempdir().unwrap();
        setup_skill_with_snapshot(dir.path(), "alpha", "v000001");
        setup_skill_with_activation(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );
        setup_skill_with_activation(dir.path(), "beta", "INVALID");

        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        let ctrl = ActivationReloadController::with_defaults(dir.path(), resolver.clone());

        let results = ctrl.reload_many_once(&["alpha".to_string(), "beta".to_string()]);

        assert!(matches!(
            results[0].1,
            ReloadOutcome::Updated(ActiveTarget::Snapshot { .. })
        ));
        assert!(matches!(results[1].1, ReloadOutcome::FailSafeHidden { .. }));
    }

    // ─────────────────────────────────────────────────────────────────────
    // A4: reload_known_skills_once
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn reload_known_skills_once_reloads_resolver_keys() {
        let dir = tempfile::tempdir().unwrap();
        setup_skill_with_snapshot(dir.path(), "alpha", "v000001");
        setup_skill_with_activation(
            dir.path(),
            "alpha",
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );

        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        resolver.set(
            "alpha".to_string(),
            ActiveTarget::Current {
                source_dir: dir.path().join("alpha"),
            },
        );

        let ctrl = ActivationReloadController::with_defaults(dir.path(), resolver.clone());

        let results = ctrl.reload_known_skills_once();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "alpha");
        assert!(matches!(
            results[0].1,
            ReloadOutcome::Updated(ActiveTarget::Snapshot { .. })
        ));
    }

    #[test]
    fn reload_known_skills_once_empty_resolver() {
        let dir = tempfile::tempdir().unwrap();
        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        let ctrl = ActivationReloadController::with_defaults(dir.path(), resolver);

        let results = ctrl.reload_known_skills_once();
        assert!(results.is_empty());
    }

    // ─────────────────────────────────────────────────────────────────────
    // xattr freshness
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn freshness_detects_xattr_ctime_change() {
        let td = match xattr_capable_tempdir() {
            Some(d) => d,
            None => {
                eprintln!("SKIP: no xattr-capable filesystem for ctime freshness test");
                return;
            }
        };
        let dir = td.path();
        let before = ActivationFreshness::snapshot(dir);
        std::thread::sleep(Duration::from_millis(15));

        set_activation_xattr(dir, r#"{"schemaVersion": 1, "target": null}"#);

        let after = ActivationFreshness::snapshot(dir);
        assert!(
            before.has_advanced(&after),
            "xattr write must advance dir ctime; before={before:?} after={after:?}"
        );
    }
}
