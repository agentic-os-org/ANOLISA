//! D1.0 in-memory active-skill mapping.
//!
//! Background. The Skill Ledger integration plan (§5.3) wants SkillFS to
//! translate each `resolve` answer into one of three runtime entry points
//! for `/skills/<skill>`:
//!
//! * [`ActiveTarget::Current`]  — read the live source directory.
//! * [`ActiveTarget::Snapshot`] — read a trusted snapshot inside the
//!   Skill's `.skill-meta/versions/<version>/...`.
//! * [`ActiveTarget::Hidden`]   — do not expose the Skill (`readdir` and
//!   `lookup` should both pretend it is missing).
//!
//! D1.0 ships only the **pure in-memory mapping** that powers that
//! translation. There is no FUSE wiring here — nothing in this module is
//! consulted by `readdir`/`lookup`/`open`/`getattr` today. That comes in
//! a follow-up (D1.1/D1.2). Keeping the mapping separable means the
//! FUSE crate can build, run, and pass `posix_open_io_tests` /
//! `write_guard_tests` exactly as before; the mapping only becomes load-
//! bearing once a hook handler starts writing into it.
//!
//! Scope discipline (intentionally **out of scope** here):
//!
//! * watcher hot sync / source drift wiring;
//! * trusted writer identity, lifecycle state machine,
//!   `.skill-meta` write enablement;
//! * persistence / crash recovery — the resolver is purely in-memory;
//! * timeouts, retries, fail-open/fail-closed policy for
//!   [`crate::security::ledger::LedgerAdapter`] failures.
//!
//! The resolver is `Send + Sync` via an internal `RwLock`, matching the
//! pattern used by `SharedSkillStore`, so a future hook handler can keep
//! a single resolver behind an `Arc` and update it from a worker thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::RwLock;

use super::ledger::{LEDGER_SNAPSHOT_PREFIX, LedgerDecision, LedgerError, LedgerResolveResult};

/// What `/skills/<skill>` should resolve to right now.
///
/// Three-way enum on purpose so callers always handle the hidden case
/// explicitly. `Snapshot` carries both the resolved directory (joined
/// onto the live skill dir at construction time) and the ledger-supplied
/// version label so an audit consumer or UI can render the version
/// without going back to the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveTarget {
    /// Serve the live `<source>/<skill>/...` directory.
    Current { source_dir: PathBuf },
    /// Serve a trusted snapshot under
    /// `<source>/<skill>/.skill-meta/versions/<version>/...`.
    Snapshot {
        snapshot_dir: PathBuf,
        version: String,
    },
    /// Do not expose the Skill. `readdir` skips it; `lookup` should
    /// surface `ENOENT`.
    Hidden { reason: String },
}

impl ActiveTarget {
    /// Stable label used by audit / security-event consumers. Matches the
    /// shape `current` / `fallback:<version>` / `hidden:<reason>`.
    pub fn as_label(&self) -> String {
        match self {
            ActiveTarget::Current { .. } => "current".to_string(),
            ActiveTarget::Snapshot { version, .. } => format!("fallback:{version}"),
            ActiveTarget::Hidden { reason } => format!("hidden:{reason}"),
        }
    }

    /// Convenience accessor: physical directory to read from, when the
    /// target is readable. `Hidden` returns `None`.
    pub fn read_dir(&self) -> Option<&Path> {
        match self {
            ActiveTarget::Current { source_dir } => Some(source_dir),
            ActiveTarget::Snapshot { snapshot_dir, .. } => Some(snapshot_dir),
            ActiveTarget::Hidden { .. } => None,
        }
    }

    /// `true` when the target should be visible to `readdir`.
    pub fn is_visible(&self) -> bool {
        !matches!(self, ActiveTarget::Hidden { .. })
    }
}

/// Errors building an [`ActiveTarget`] from a [`LedgerResolveResult`].
///
/// All variants reflect an invariant that the strict ledger validator
/// upstream **should** have caught. They are kept as explicit errors
/// (rather than panics or `unwrap`s) because the conversion is the only
/// place where the ledger contract meets a real on-disk `source_root` —
/// e.g. the ledger could send a perfectly-shaped `target` whose parent
/// `source_root` is empty. Centralized here so the consumer can render
/// a single class of "ledger said X but we cannot honor it" errors.
#[derive(Debug)]
pub enum ActiveMappingError {
    /// Ledger returned `decision=fallback` but did not include a target.
    /// In practice the strict ledger parser catches this; the variant
    /// exists so callers do not have to assume it.
    MissingFallbackTarget { skill_name: String },
    /// `source_root` was empty so we cannot build an absolute source dir.
    EmptySourceRoot,
}

impl std::fmt::Display for ActiveMappingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActiveMappingError::MissingFallbackTarget { skill_name } => {
                write!(
                    f,
                    "ledger decision was fallback for skill '{skill_name}' but no target was provided"
                )
            }
            ActiveMappingError::EmptySourceRoot => {
                write!(f, "cannot build active target with empty source root")
            }
        }
    }
}

impl std::error::Error for ActiveMappingError {}

/// Error surface for the checked
/// [`ActiveSkillResolver::set_from_resolve_for_expected`] API.
///
/// Two orthogonal failures can prevent a resolve result from being
/// installed:
///
/// * `SkillNameMismatch` — the result's `skillName` does not equal the
///   canonical SkillFS identity (`basename(skill_dir)`) the caller asked
///   about. This is the N1/D1.6 contract violation; SkillFS must reject
///   the response before it ever reaches the in-memory map so a buggy
///   provider cannot rename `/skills/<canonical>` into a different key.
/// * `Mapping` — the response was structurally accepted (`skillName`
///   matches) but the [`ActiveTarget`] could not be built (e.g. empty
///   source root). This is the same surface
///   [`ActiveSkillResolver::set_from_resolve`] returns.
///
/// The variants are explicit (rather than collapsed into a single
/// `LedgerError`) so callers can keep their existing security-event /
/// failure-policy plumbing per branch without losing the underlying
/// expected/actual pair.
#[derive(Debug)]
pub enum ActiveResolverError {
    /// The result's `skillName` does not equal `expected`.
    SkillNameMismatch { expected: String, actual: String },
    /// Building the [`ActiveTarget`] from the resolve failed.
    Mapping(ActiveMappingError),
}

impl ActiveResolverError {
    /// Render this error as a [`LedgerError`] so callers that already
    /// have a audit pipeline keyed on the ledger error surface
    /// can keep using a single error vocabulary.
    ///
    /// `SkillNameMismatch` round-trips into
    /// [`LedgerError::SkillNameMismatch`]; `Mapping` is rendered as a
    /// best-effort [`LedgerError::InvalidField`] that carries the
    /// underlying display so the operator-facing message is preserved.
    pub fn to_ledger_error(&self) -> LedgerError {
        match self {
            ActiveResolverError::SkillNameMismatch { expected, actual } => {
                LedgerError::SkillNameMismatch {
                    expected: expected.clone(),
                    actual: actual.clone(),
                }
            }
            ActiveResolverError::Mapping(m) => LedgerError::InvalidField {
                field: "active_target",
                reason: m.to_string(),
            },
        }
    }
}

impl std::fmt::Display for ActiveResolverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActiveResolverError::SkillNameMismatch { expected, actual } => {
                write!(f, "skillName mismatch: expected {expected}, got {actual}")
            }
            ActiveResolverError::Mapping(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for ActiveResolverError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ActiveResolverError::Mapping(m) => Some(m),
            _ => None,
        }
    }
}

impl From<ActiveMappingError> for ActiveResolverError {
    fn from(value: ActiveMappingError) -> Self {
        ActiveResolverError::Mapping(value)
    }
}

/// Pure in-memory `skill name -> ActiveTarget` mapping.
///
/// Concurrent reads are cheap (`RwLock` read-locked snapshot via
/// [`ActiveSkillResolver::snapshot`]); writes are serialized. No
/// background tasks, no persistence, no observers — D1.0 keeps it that
/// boring on purpose.
pub struct ActiveSkillResolver {
    source_root: PathBuf,
    entries: RwLock<HashMap<String, ActiveTarget>>,
    /// Test-only counter of [`Self::get`] calls, used to assert the
    /// pinned-resolution boundary consults the resolver exactly once. Compiled
    /// out of production builds so `get` stays a plain read on the hot path.
    #[cfg(test)]
    get_calls: AtomicU64,
    /// Test-only counter of [`Self::get`] calls made **inside an `open`
    /// decision scope** (see [`open_decision_scope`]). Unlike [`Self::get_calls`]
    /// this ignores reads from `lookup`/`getattr`/`read`, so a test can assert
    /// the real `open` boundary consults the resolver exactly once without any
    /// dependence on kernel cache timing. Production builds omit it.
    #[cfg(test)]
    open_decision_reads: AtomicU64,
}

#[cfg(test)]
thread_local! {
    /// Set while a FUSE `open` callback is resolving its activation decision, so
    /// [`ActiveSkillResolver::get`] can attribute reads to the `open` boundary
    /// specifically. FUSE callbacks run on the session thread, so a thread-local
    /// cleanly scopes counting to the `open` handler and excludes the separate
    /// `lookup`/`getattr`/`read` callbacks.
    static IN_OPEN_DECISION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test-only RAII guard marking the current thread as inside an `open` decision.
/// While held, [`ActiveSkillResolver::get`] increments
/// `open_decision_reads`. `open_impl` holds one across its whole body so a
/// regression that reads the resolver twice is counted as two.
#[cfg(test)]
pub(crate) struct OpenDecisionScope;

#[cfg(test)]
pub(crate) fn open_decision_scope() -> OpenDecisionScope {
    IN_OPEN_DECISION.with(|c| c.set(true));
    OpenDecisionScope
}

#[cfg(test)]
impl Drop for OpenDecisionScope {
    fn drop(&mut self) {
        IN_OPEN_DECISION.with(|c| c.set(false));
    }
}

impl ActiveSkillResolver {
    /// Build an empty resolver rooted at `source_root`. Every later
    /// [`ActiveSkillResolver::set_from_resolve`] call joins the ledger's
    /// relative target onto `<source_root>/<skill_name>`.
    pub fn new(source_root: impl Into<PathBuf>) -> Self {
        Self {
            source_root: source_root.into(),
            entries: RwLock::new(HashMap::new()),
            #[cfg(test)]
            get_calls: AtomicU64::new(0),
            #[cfg(test)]
            open_decision_reads: AtomicU64::new(0),
        }
    }

    /// Source root the resolver was built against. Useful for tests and
    /// for the event handler that wants to render absolute paths.
    pub fn source_root(&self) -> &Path {
        &self.source_root
    }

    /// Insert or replace the target for `skill_name`. Pure setter — no
    /// ledger interpretation. Provided for tests, seeding, and the
    /// future hook handler that already has a typed `ActiveTarget`.
    pub fn set(&self, skill_name: impl Into<String>, target: ActiveTarget) {
        self.entries.write().insert(skill_name.into(), target);
    }

    /// **Low-level / unchecked**: translate a [`LedgerResolveResult`]
    /// into an [`ActiveTarget`] and install it under
    /// `result.skill_name`. This helper does **not** verify that
    /// `result.skill_name` equals the canonical SkillFS identity for
    /// the request that produced the response.
    ///
    /// Production runtime callers (the CLI bootstrap, the refresh
    /// controller, the inbox install pipeline) MUST go through
    /// [`Self::set_from_resolve_for_expected`] so a buggy or hostile
    /// provider cannot key the active mapping off a different name.
    /// This method is kept public only for seeding, tests, and
    /// future hook handlers that already have a typed `ActiveTarget` /
    /// validated `LedgerResolveResult` in hand.
    ///
    /// Returns the resulting target so the caller can log / emit a demo
    /// event without going back to the resolver.
    pub fn set_from_resolve(
        &self,
        result: &LedgerResolveResult,
    ) -> Result<ActiveTarget, ActiveMappingError> {
        let target = self.target_from_resolve(result)?;
        self.entries
            .write()
            .insert(result.skill_name.clone(), target.clone());
        Ok(target)
    }

    /// Checked variant of [`Self::set_from_resolve`] that enforces the
    /// N1/D1.6 canonical skill identity contract: the result's
    /// `skillName` must equal `expected` (which the caller derived from
    /// `basename(skill_dir)` for the request that produced the
    /// response). On mismatch the resolver is **not** mutated; on
    /// success the target is installed under the canonical key just
    /// like [`Self::set_from_resolve`].
    ///
    /// This is the only entry point production runtime code paths
    /// should use to consume a resolve result. It guarantees that:
    ///
    /// * `/skills/<expected>` is the only path key the response can
    ///   ever produce — a mismatched `skillName` cannot create
    ///   `/skills/<actual>` as an alias.
    /// * the resolver's existing entry for `expected` (if any) is
    ///   preserved when the response is rejected.
    pub fn set_from_resolve_for_expected(
        &self,
        expected: &str,
        result: &LedgerResolveResult,
    ) -> Result<ActiveTarget, ActiveResolverError> {
        if result.skill_name != expected {
            return Err(ActiveResolverError::SkillNameMismatch {
                expected: expected.to_string(),
                actual: result.skill_name.clone(),
            });
        }
        let target = self.set_from_resolve(result)?;
        Ok(target)
    }

    /// Compute the `ActiveTarget` that **would** be installed, without
    /// mutating the map. Useful for dry-run validation in tests.
    pub fn target_from_resolve(
        &self,
        result: &LedgerResolveResult,
    ) -> Result<ActiveTarget, ActiveMappingError> {
        if self.source_root.as_os_str().is_empty() {
            return Err(ActiveMappingError::EmptySourceRoot);
        }
        let skill_dir = self.source_root.join(&result.skill_name);
        match result.decision {
            LedgerDecision::Current => Ok(ActiveTarget::Current {
                source_dir: skill_dir,
            }),
            LedgerDecision::Fallback => {
                let rel = result.target.as_ref().ok_or_else(|| {
                    ActiveMappingError::MissingFallbackTarget {
                        skill_name: result.skill_name.clone(),
                    }
                })?;
                // `rel` was already validated by the ledger parser to be
                // a clean relative path under `.skill-meta/versions/`.
                let snapshot_dir = skill_dir.join(rel);
                let version = derive_snapshot_version(rel, result.trusted_version.as_deref());
                Ok(ActiveTarget::Snapshot {
                    snapshot_dir,
                    version,
                })
            }
            LedgerDecision::Hidden => Ok(ActiveTarget::Hidden {
                reason: result
                    .reason
                    .clone()
                    .unwrap_or_else(|| "hidden by ledger".to_string()),
            }),
        }
    }

    /// Remove the entry for `skill_name`. Pure helper; D1.0 does not call
    /// this from the FUSE side.
    pub fn forget(&self, skill_name: &str) -> Option<ActiveTarget> {
        self.entries.write().remove(skill_name)
    }

    /// Snapshot copy of the current mapping. Cheap clone; not a live view.
    pub fn snapshot(&self) -> HashMap<String, ActiveTarget> {
        self.entries.read().clone()
    }

    /// Look up the active target for `skill_name`, cloned out of the map.
    pub fn get(&self, skill_name: &str) -> Option<ActiveTarget> {
        #[cfg(test)]
        {
            self.get_calls.fetch_add(1, Ordering::Relaxed);
            if IN_OPEN_DECISION.with(|c| c.get()) {
                self.open_decision_reads.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.entries.read().get(skill_name).cloned()
    }

    /// Test-only count of [`Self::get`] calls since construction. Used by the
    /// pinned-resolution unit tests to assert a single resolver read; not part
    /// of the production API.
    #[cfg(test)]
    pub(crate) fn get_call_count(&self) -> u64 {
        self.get_calls.load(Ordering::Relaxed)
    }

    /// Test-only count of [`Self::get`] calls made inside an `open` decision
    /// scope (i.e. attributable to the `open` callback, not `lookup`/`getattr`).
    #[cfg(test)]
    pub(crate) fn open_decision_reads(&self) -> u64 {
        self.open_decision_reads.load(Ordering::Relaxed)
    }

    /// Number of entries currently mapped.
    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Pick a sensible `version` label for an `ActiveTarget::Snapshot`.
///
/// Preference order:
///
/// 1. `trusted_version` from the ledger response (the ledger's
///    canonical label for the snapshot).
/// 2. The first non-prefix path component of `rel_target` (so
///    `.skill-meta/versions/v000001.snapshot` yields `v000001.snapshot`,
///    matching the §6.4 display).
/// 3. The full stringified relative target, as a last resort.
fn derive_snapshot_version(rel_target: &Path, trusted_version: Option<&str>) -> String {
    if let Some(v) = trusted_version {
        if !v.is_empty() {
            return v.to_string();
        }
    }
    let prefix = Path::new(LEDGER_SNAPSHOT_PREFIX);
    if let Ok(rest) = rel_target.strip_prefix(prefix) {
        if let Some(first) = rest.components().next() {
            return first.as_os_str().to_string_lossy().into_owned();
        }
    }
    rel_target.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::ledger::{LedgerError, LedgerResolveResult, LedgerStatus};

    fn make_current() -> LedgerResolveResult {
        LedgerResolveResult::from_json_str(
            r#"{
                "schemaVersion": 1,
                "skillName": "demo-weather",
                "status": "pass",
                "decision": "current",
                "currentVersion": "v000001",
                "trustedVersion": "v000001"
            }"#,
        )
        .unwrap()
    }

    fn make_fallback() -> LedgerResolveResult {
        LedgerResolveResult::from_json_str(
            r#"{
                "schemaVersion": 1,
                "skillName": "demo-weather",
                "status": "deny",
                "decision": "fallback",
                "currentVersion": "v000003",
                "trustedVersion": "v000001",
                "target": ".skill-meta/versions/v000001.snapshot",
                "targetKind": "relative_to_skill_dir"
            }"#,
        )
        .unwrap()
    }

    fn make_hidden(reason: bool) -> LedgerResolveResult {
        let json = if reason {
            r#"{
                "schemaVersion": 1,
                "skillName": "demo-weather",
                "status": "none",
                "decision": "hidden",
                "reason": "no certified version yet"
            }"#
        } else {
            r#"{
                "schemaVersion": 1,
                "skillName": "demo-weather",
                "status": "none",
                "decision": "hidden"
            }"#
        };
        LedgerResolveResult::from_json_str(json).unwrap()
    }

    #[test]
    fn default_resolver_is_empty() {
        let r = ActiveSkillResolver::new("/srv/skills");
        assert!(r.is_empty());
        assert_eq!(r.source_root(), Path::new("/srv/skills"));
        assert!(r.get("demo-weather").is_none());
    }

    #[test]
    fn set_from_resolve_current_points_to_live_skill_dir() {
        let r = ActiveSkillResolver::new("/srv/skills");
        let target = r.set_from_resolve(&make_current()).unwrap();
        assert_eq!(
            target,
            ActiveTarget::Current {
                source_dir: PathBuf::from("/srv/skills/demo-weather"),
            }
        );
        assert_eq!(r.get("demo-weather").unwrap(), target);
        assert_eq!(target.as_label(), "current");
        assert_eq!(
            target.read_dir(),
            Some(Path::new("/srv/skills/demo-weather"))
        );
        assert!(target.is_visible());
    }

    #[test]
    fn set_from_resolve_fallback_points_to_snapshot_inside_skill() {
        let r = ActiveSkillResolver::new("/srv/skills");
        let target = r.set_from_resolve(&make_fallback()).unwrap();
        match &target {
            ActiveTarget::Snapshot {
                snapshot_dir,
                version,
            } => {
                assert_eq!(
                    snapshot_dir,
                    Path::new("/srv/skills/demo-weather/.skill-meta/versions/v000001.snapshot")
                );
                assert_eq!(version, "v000001"); // trustedVersion wins
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
        assert_eq!(target.as_label(), "fallback:v000001");
        assert!(target.is_visible());
    }

    #[test]
    fn fallback_version_falls_back_to_first_path_component() {
        // Drop trustedVersion from the JSON so the path-derived label
        // shows up. Build the payload manually to keep the test honest.
        let raw = r#"{
            "schemaVersion": 1,
            "skillName": "demo-weather",
            "status": "deny",
            "decision": "fallback",
            "target": ".skill-meta/versions/v000002.snapshot",
            "targetKind": "relative_to_skill_dir"
        }"#;
        let result = LedgerResolveResult::from_json_str(raw).unwrap();
        let r = ActiveSkillResolver::new("/srv/skills");
        let target = r.set_from_resolve(&result).unwrap();
        match target {
            ActiveTarget::Snapshot { version, .. } => assert_eq!(version, "v000002.snapshot"),
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    #[test]
    fn set_from_resolve_hidden_uses_reason_when_provided() {
        let r = ActiveSkillResolver::new("/srv/skills");
        let target = r.set_from_resolve(&make_hidden(true)).unwrap();
        assert_eq!(
            target,
            ActiveTarget::Hidden {
                reason: "no certified version yet".to_string(),
            }
        );
        assert_eq!(target.as_label(), "hidden:no certified version yet");
        assert!(!target.is_visible());
        assert!(target.read_dir().is_none());
    }

    #[test]
    fn set_from_resolve_hidden_falls_back_to_default_reason() {
        let r = ActiveSkillResolver::new("/srv/skills");
        let target = r.set_from_resolve(&make_hidden(false)).unwrap();
        assert_eq!(
            target,
            ActiveTarget::Hidden {
                reason: "hidden by ledger".to_string(),
            }
        );
    }

    #[test]
    fn set_from_resolve_overwrites_existing_entry() {
        let r = ActiveSkillResolver::new("/srv/skills");
        r.set_from_resolve(&make_current()).unwrap();
        r.set_from_resolve(&make_fallback()).unwrap();
        let current = r.get("demo-weather").unwrap();
        assert!(matches!(current, ActiveTarget::Snapshot { .. }));
    }

    #[test]
    fn forget_removes_entry() {
        let r = ActiveSkillResolver::new("/srv/skills");
        r.set_from_resolve(&make_current()).unwrap();
        assert!(r.forget("demo-weather").is_some());
        assert!(r.get("demo-weather").is_none());
        assert!(r.forget("demo-weather").is_none());
    }

    #[test]
    fn set_direct_target_works_without_ledger() {
        let r = ActiveSkillResolver::new("/srv/skills");
        r.set(
            "alpha",
            ActiveTarget::Hidden {
                reason: "test".to_string(),
            },
        );
        assert!(matches!(r.get("alpha"), Some(ActiveTarget::Hidden { .. })));
    }

    #[test]
    fn snapshot_returns_independent_copy() {
        let r = ActiveSkillResolver::new("/srv/skills");
        r.set_from_resolve(&make_current()).unwrap();
        let snap1 = r.snapshot();
        r.forget("demo-weather");
        let snap2 = r.snapshot();
        assert_eq!(snap1.len(), 1);
        assert_eq!(snap2.len(), 0);
    }

    #[test]
    fn empty_source_root_is_rejected() {
        let r = ActiveSkillResolver::new("");
        let err = r.set_from_resolve(&make_current()).unwrap_err();
        assert!(matches!(err, ActiveMappingError::EmptySourceRoot));
    }

    #[test]
    fn set_from_resolve_for_expected_accepts_matching_skill_name() {
        let r = ActiveSkillResolver::new("/srv/skills");
        let target = r
            .set_from_resolve_for_expected("demo-weather", &make_current())
            .expect("matching skillName must install");
        assert_eq!(
            target,
            ActiveTarget::Current {
                source_dir: PathBuf::from("/srv/skills/demo-weather"),
            }
        );
        assert_eq!(r.get("demo-weather").unwrap(), target);
    }

    #[test]
    fn set_from_resolve_for_expected_rejects_mismatch_and_preserves_state() {
        let r = ActiveSkillResolver::new("/srv/skills");
        // Pre-seed `weather` so we can confirm a mismatched response
        // does not mutate the resolver.
        r.set(
            "weather",
            ActiveTarget::Current {
                source_dir: PathBuf::from("/srv/skills/weather"),
            },
        );

        // Provider returned a result for a different skill.
        let bad = make_current(); // skillName = demo-weather
        let err = r
            .set_from_resolve_for_expected("weather", &bad)
            .unwrap_err();
        match err {
            ActiveResolverError::SkillNameMismatch { expected, actual } => {
                assert_eq!(expected, "weather");
                assert_eq!(actual, "demo-weather");
            }
            other => panic!("expected SkillNameMismatch, got {other:?}"),
        }

        // Resolver state is unchanged: weather still current, no
        // demo-weather alias was installed.
        match r.get("weather") {
            Some(ActiveTarget::Current { .. }) => {}
            other => panic!("expected weather current preserved, got {other:?}"),
        }
        assert!(
            r.get("demo-weather").is_none(),
            "mismatched skillName must not produce an alias key"
        );
    }

    #[test]
    fn active_resolver_error_renders_back_into_ledger_error() {
        let mismatch = ActiveResolverError::SkillNameMismatch {
            expected: "weather".to_string(),
            actual: "calculator".to_string(),
        };
        match mismatch.to_ledger_error() {
            LedgerError::SkillNameMismatch { expected, actual } => {
                assert_eq!(expected, "weather");
                assert_eq!(actual, "calculator");
            }
            other => panic!("expected LedgerError::SkillNameMismatch, got {other:?}"),
        }

        let mapping = ActiveResolverError::Mapping(ActiveMappingError::EmptySourceRoot);
        let rendered = mapping.to_ledger_error();
        assert!(matches!(
            rendered,
            LedgerError::InvalidField {
                field: "active_target",
                ..
            }
        ));
    }

    #[test]
    fn set_from_resolve_for_expected_propagates_mapping_errors() {
        // Empty source root makes ActiveTarget construction fail. The
        // checked API must surface that as ActiveResolverError::Mapping
        // (not as a SkillNameMismatch).
        let r = ActiveSkillResolver::new("");
        let err = r
            .set_from_resolve_for_expected("demo-weather", &make_current())
            .unwrap_err();
        match err {
            ActiveResolverError::Mapping(ActiveMappingError::EmptySourceRoot) => {}
            other => panic!("expected Mapping(EmptySourceRoot), got {other:?}"),
        }
    }

    #[test]
    fn pinned_status_round_trip() {
        // Sanity check that the ledger status -> demo-mapping reasoning
        // stays the way the plan documents it. The mapping table in
        // `docs/security-ledger-integration-plan.md` §4.3 lists `pass`
        // -> current, `deny` -> fallback, `none` -> hidden; the
        // adapter is responsible for that decision, the mapping here
        // just follows the `decision` field. Pin both directions so a
        // future refactor that loses the decision field shows up
        // immediately.
        let pass = make_current();
        assert_eq!(pass.status, LedgerStatus::Pass);
        assert_eq!(pass.decision, LedgerDecision::Current);

        let deny = make_fallback();
        assert_eq!(deny.status, LedgerStatus::Deny);
        assert_eq!(deny.decision, LedgerDecision::Fallback);

        let none = make_hidden(false);
        assert_eq!(none.status, LedgerStatus::None);
        assert_eq!(none.decision, LedgerDecision::Hidden);
    }

    #[test]
    fn concurrent_read_write_no_panic_no_deadlock() {
        use std::sync::Arc;

        let resolver = Arc::new(ActiveSkillResolver::new("/srv/skills"));

        const WRITERS: usize = 4;
        const READERS: usize = 4;
        const ITERS: usize = 200;

        let mut handles = Vec::new();

        for w in 0..WRITERS {
            let r = resolver.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..ITERS {
                    let name = format!("skill-{}", (w * ITERS + i) % 8);
                    match i % 3 {
                        0 => {
                            r.set(
                                name,
                                ActiveTarget::Hidden {
                                    reason: "test".to_string(),
                                },
                            );
                        }
                        1 => {
                            let _ = r.set_from_resolve(&make_current());
                        }
                        _ => {
                            r.forget(&name);
                        }
                    }
                }
            }));
        }

        for _ in 0..READERS {
            let r = resolver.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..ITERS {
                    let name = format!("skill-{}", i % 8);
                    let _ = r.get(&name);
                    let _ = r.snapshot();
                }
            }));
        }

        for h in handles {
            h.join().expect("thread must not panic");
        }

        resolver.set(
            "post-stress",
            ActiveTarget::Current {
                source_dir: PathBuf::from("/srv/skills/post-stress"),
            },
        );
        assert!(resolver.get("post-stress").is_some());
        let _ = resolver.snapshot();
    }
}
