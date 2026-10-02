//! Skill Security extension seam.
//!
//! Layout:
//!
//! * [`policy::SecurityPolicy`] / [`policy::PermissivePolicy`] /
//!   [`policy::SkillMetaProtectionPolicy`] / [`policy::PathPolicy`] /
//!   [`policy::PolicyDecision`] — describe and decide on operations.
//! * [`event::SkillEvent`] / [`event::SkillEventKind`] /
//!   [`event::SkillEventSink`] — normalized records of FUSE-observed
//!   operations. The default sink ([`event::NoopEventSink`]) drops every
//!   event; tests can opt into [`event::InMemoryEventSink`].
//! * [`path::is_skill_meta_path`] — pure-lexical classifier for
//!   `.skill-meta/**` reserved metadata paths.
//! * [`audit::JsonlFileAuditSink`] — best-effort JSONL audit stream
//!   (Package S2). Off by default; opt in via
//!   [`crate::SkillFs::with_event_sink`].
//! * [`mode::SecurityModeConfig`] — Package M0 startup-time validation
//!   that the source/mountpoint pair satisfies the security guarantee an
//!   operator asked for. Disabled by default, so the existing normal vs.
//!   in-place mount UX is unchanged.
//! * [`drift::SourceDriftObserver`] — visibility-only seam that turns
//!   out-of-band source-tree changes into normalized
//!   [`event::SkillEventKind::SourceChanged`] records.
//! * [`lifecycle::is_reserved_lifecycle_name`] — Package S3 reservation of
//!   `.staging`, `.certified`, `.quarantine`, and `.archive` as lifecycle
//!   namespace names. S3 keeps the names hidden from ordinary
//!   `readdir`/`lookup` and rejects mutations targeting them with a
//!   deterministic permission errno; lifecycle state transitions,
//!   quarantine/scanner integration, and trusted-writer identity are all
//!   out of scope until later packages.
//! * [`lifecycle::LifecycleViewMode`] /
//!   [`lifecycle::classify_skill_name_with_mode`] — Package S3.1
//!   management-view contract. Defines the pure-API boundary between the
//!   ordinary agent-facing view (where S3 reservation applies) and a
//!   future management view that intentionally exposes the reserved
//!   roots. S3.1 only ships the contract: no FUSE callback selects
//!   [`lifecycle::LifecycleViewMode::Management`] today, and no CLI flag
//!   turns it on. Default mount behavior is exactly S3.
//! * [`ledger::LedgerAdapter`] /
//!   [`ledger::CliLedgerAdapter`] /
//!   [`ledger::DecisionCommand`] /
//!   [`ledger::LedgerResolveResult`] — contract for the external
//!   decision provider. [`ledger::DecisionCommand`] parses the
//!   operator-supplied `--decision-command <COMMAND>` value into
//!   `program + fixed_args`; the adapter then drives
//!   `<command> scan <skill_dir> --json` followed by
//!   `<command> resolve <skill_dir> --json`. Only `resolve` JSON is
//!   strictly validated. A [`ledger::StaticLedgerAdapter`] is exported
//!   for tests so no subprocess is needed.
//! * [`active::ActiveSkillResolver`] /
//!   [`active::ActiveTarget`] — D1.0 in-memory mapping that turns a
//!   parsed [`ledger::LedgerResolveResult`] into the runtime entry point
//!   `/skills/<skill>` should expose (current / snapshot / hidden).
//!   Default mount behavior is unchanged: no FUSE callback consults the
//!   resolver yet, but the type is exported so a future hook handler can
//!   wire it without touching POSIX or compiled `SKILL.md` paths.
//! * [`trusted_writer::TrustedWriterConfig`] /
//!   [`trusted_writer::ProcessIdentityResolver`] /
//!   [`trusted_writer::evaluate_trusted_writer`] — trusted writer
//!   process gate with starttime verification. Default disabled. When
//!   the operator passes `--trusted-writer <NAME>` the gate compares
//!   the configured name against the FUSE caller thread's process
//!   `comm` (TID -> TGID via `/proc/<pid>/status`, then
//!   `/proc/<tgid>/comm`) and verifies starttime to defend against
//!   PID reuse. On match, allows the otherwise-denied
//!   `.skill-meta/**` mutation. The bypass is strictly scoped to
//!   `.skill-meta/**` writes; it does not relax lifecycle
//!   reservation, virtual paths, `skill-discover`, or any other
//!   policy.
//! * [`drift_runtime`] — Package W1 runtime adapter that turns
//!   [`skillfs_core::watcher::SkillEvent`] notifications into
//!   [`drift::DriftEvent`] records and emits them through an injected
//!   [`drift::SourceDriftObserver`]. Coverage is intentionally narrow:
//!   the producer in `skillfs-core::watcher::classify_event` only surfaces
//!   `<source>/<skill>/SKILL.md` create/modify/delete and immediate
//!   skill-directory create/delete, so W1 only observes that subset.
//!   Default behavior is still no-op; nothing wires the watcher into the
//!   FUSE runtime unless an operator explicitly turns audit logging on
//!   (see the CLI `--audit-log` flag).
//!
//! Package S0 added the seam. Package S1 plugs the first real policy,
//! [`policy::SkillMetaProtectionPolicy`], into `SkillFs` as the default so
//! `.skill-meta/**` is read-visible but mutation-protected by default.
//! Package S2 adds the audit sink; the default `SkillFs` sink is still
//! [`event::NoopEventSink`]. Package M0 layers the security-mode gate on
//! top so audit/policy guarantees can be enforced by refusing to start a
//! non-in-place mount when the operator opts in. Package W1 connects the
//! existing `skillfs-core::watcher` to the W0 drift observer so out-of-band
//! source changes can surface as `SourceChanged` audit records.

pub mod activation;
pub mod activation_reload;
pub mod activation_watcher;
pub mod active;
pub mod audit;
pub(crate) mod auth;
pub mod backing_root;
pub mod config;
pub mod control_socket;
pub mod drift;
pub mod drift_runtime;
pub mod event;
pub mod event_stream;
pub mod inbox;
pub mod install;
pub mod ledger;
pub mod lifecycle;
pub mod mode;
pub mod notify;
pub mod path;
pub mod policy;
pub mod protocol_events;
pub mod refresh;
pub mod resolver;
pub mod runtime_metrics;
pub mod session_stats;
pub mod session_stats_writer;
mod skill_dir;
pub mod telemetry_gate;
pub mod trusted_writer;

pub use activation::{
    ACTIVATION_FILE, ACTIVATION_SCHEMA_VERSION, ACTIVATION_XATTR, ActivationError, ActivationMode,
    ActivationRecord, XattrReadOutcome, bootstrap_activation, classify_xattr_errno,
    enumerate_hermes_skill_ids, enumerate_hermes_skill_leaves, enumerate_hermes_top_level_skills,
    fail_safe_hidden, load_activation, load_activation_prefer_xattr, read_activation_xattr,
    resolve_activation,
};
pub use activation_reload::{
    ActivationFreshness, ActivationReloadController, DEFAULT_RELOAD_INTERVAL_MS,
    DEFAULT_RELOAD_TIMEOUT_MS, ReloadMode, ReloadOutcome,
};
pub use activation_watcher::{ActivationWatcher, DEFAULT_WATCHER_INTERVAL_MS, WatcherRegistrar};
pub use active::{ActiveMappingError, ActiveResolverError, ActiveSkillResolver, ActiveTarget};
pub use audit::{
    AuditConfig, AuditPathError, AuditRuntimeConfig, DEFAULT_AUDIT_QUEUE_CAPACITY,
    JsonlFileAuditSink, event_action_str, event_kind_str, event_to_json, serialize_event_jsonl,
};
pub use backing_root::{BackingRootError, LedgerBackingRoot};
pub use config::{ConfigError, SecurityConfig};
pub use control_socket::{
    CONTROL_SCHEMA_VERSION, ControlError, ControlRequest, ControlResponse, ControlSocketConfig,
    ControlSocketContext, ControlSocketHandle, ControlSocketServer, PeerCredentials, PeerIdentity,
    PeerVerifyResult, SocketPreflightError, TrustedPeerConfig, dispatch_request, identify_peer,
    parse_request, parse_request_with_raw, preflight_socket_path, verify_peer,
};
pub use drift::{
    DriftChangeKind, DriftEvent, DriftScope, SourceDriftObserver, classify_drift_path,
};
pub use drift_runtime::{
    DriftWatcherHandle, core_event_to_drift_event, drive_drift_watcher, spawn_drift_watcher,
};
pub use event::{
    InMemoryEventSink, NoopEventSink, SkillEvent, SkillEventAction, SkillEventKind, SkillEventSink,
};
pub use event_stream::{
    DEFAULT_EVENT_QUEUE_CAPACITY, InMemorySecurityEventWriter, JsonlSecurityEventWriter,
    NoopSecurityEventWriter, SecurityEvent, SecurityEventSink, SecurityEventWriter,
    resolve_events_path,
};
pub use inbox::{
    INBOX_DIR_NAME, INBOX_SKILL_NAME_MAX_LEN, INSTALL_COMPLETE_SENTINEL, is_inbox_dir_name,
    is_install_complete_path, is_valid_inbox_skill_name,
};
pub use install::{
    InstallerStagingController, PendingInstallController, PostPublishGraceController,
    PostPublishSessionKind, PostPublishWritePattern, QuietTimeoutController, StagingConfig,
    StagingMatcher, StagingPattern, UnactivatedVisibility, is_valid_staging_rename_target,
    validate_post_publish_patterns, validate_staging_patterns,
};
pub use ledger::{
    CliLedgerAdapter, DecisionCommand, LEDGER_SCHEMA_VERSION, LEDGER_SNAPSHOT_PREFIX,
    LedgerAdapter, LedgerDecision, LedgerError, LedgerResolveResult, LedgerStatus,
    LedgerTargetKind, MAX_SKILL_NAME_LEN, StaticAdapterCall, StaticLedgerAdapter,
};
pub use lifecycle::{
    LIFECYCLE_ARCHIVE, LIFECYCLE_CERTIFIED, LIFECYCLE_QUARANTINE, LIFECYCLE_RESERVED_NAMES,
    LIFECYCLE_STAGING, LifecycleAccess, LifecycleNameClass, LifecycleViewMode,
    classify_skill_name as classify_lifecycle_skill_name,
    classify_skill_name_with_mode as classify_lifecycle_skill_name_with_mode,
    is_lifecycle_name_mutable, is_lifecycle_name_visible, is_reserved_lifecycle_name,
};
pub use mode::{SecurityModeConfig, SecurityModeError};
pub use notify::{
    CapturedNotify, DEFAULT_NOTIFY_DEBOUNCE_MS, DEFAULT_NOTIFY_TIMEOUT_MS, FailingNotifyClient,
    InMemoryNotifyClient, MAX_NOTIFY_PATHS, NOTIFY_METHOD, NOTIFY_SCHEMA_VERSION, NoopNotifyClient,
    NotifyChangeEvent, NotifyClient, NotifyController, NotifyError, NotifyEventKind,
    NotifyMetricsSnapshot, NotifyParams, NotifyRetryClass, RECONCILE_AMBIGUOUS_RETRY_LIMIT,
    RECONCILE_RETRY_BASE_MS, RECONCILE_RETRY_MAX_MS, ScriptedNotifyClient, ScriptedNotifyFailure,
    SlowNotifyClient, UnixSocketNotifyClient,
};
pub use path::{SKILL_META_DIR, is_skill_meta_path};
pub use policy::{
    PathPolicy, PermissivePolicy, PolicyDecision, SecurityPolicy, SkillMetaProtectionPolicy,
};
pub use protocol_events::{
    DEFAULT_PROTOCOL_EVENT_QUEUE_CAPACITY, InMemoryProtocolEventWriter, JsonlProtocolEventWriter,
    NoopProtocolEventWriter, PROTOCOL_EVENT_SCHEMA_VERSION, ProtocolEvent, ProtocolEventWriter,
    ProtocolEventsPathError, resolve_protocol_events_path, serialize_protocol_event,
    validate_protocol_events_path_outside_source,
};
pub use refresh::{
    DEFAULT_REFRESH_DEBOUNCE_MS, FailedResolveBehavior, MutationKind, RefreshController,
    RefreshObservation,
};
pub use runtime_metrics::{
    RuntimeMetricRecord, RuntimeMetricsSink, RuntimeMetricsWriter, SKILLFS_RUNTIME_METRICS_LOG_PATH,
};
pub use session_stats::{
    RuntimeDecisionOutcome, SkillfsSessionStats, SkillfsSessionSummary, serialize_session_summary,
};
pub use session_stats_writer::{
    SKILLFS_SESSION_METRICS_LOG_PATH, SessionStatsWriter, SummaryWriteOutcome,
};
pub use telemetry_gate::{TELEMETRY_DISABLED_SENTINEL, telemetry_allowed, telemetry_allowed_at};
pub use trusted_writer::{
    FileId, LinuxProcCommResolver, ProcessIdentity, ProcessIdentityResolver, TrustedWriterConfig,
    TrustedWriterDecision, default_identity_resolver, evaluate_trusted_writer, read_comm_file,
};

// Deprecated compat aliases — remove in next major version.
#[deprecated(note = "renamed to RefreshController")]
pub type DemoRefreshController = RefreshController;
#[deprecated(note = "renamed to RefreshObservation")]
pub type DemoRefreshObservation = RefreshObservation;
#[deprecated(note = "renamed to MutationKind")]
pub type DemoMutationKind = MutationKind;
#[deprecated(note = "renamed to SecurityEvent")]
pub type DemoEvent = SecurityEvent;
#[deprecated(note = "renamed to SecurityEventWriter")]
pub type DemoEventWriter = dyn SecurityEventWriter;
#[deprecated(note = "renamed to SecurityEventSink")]
pub type DemoEventSink = dyn SecurityEventWriter;
#[deprecated(note = "renamed to JsonlSecurityEventWriter")]
pub type JsonlDemoEventWriter = JsonlSecurityEventWriter;
#[deprecated(note = "renamed to NoopSecurityEventWriter")]
pub type NoopDemoEventWriter = NoopSecurityEventWriter;
#[deprecated(note = "renamed to InMemorySecurityEventWriter")]
pub type InMemoryDemoEventWriter = InMemorySecurityEventWriter;
#[deprecated(note = "renamed to DEFAULT_EVENT_QUEUE_CAPACITY")]
pub const DEFAULT_DEMO_EVENT_QUEUE_CAPACITY: usize = DEFAULT_EVENT_QUEUE_CAPACITY;
#[deprecated(note = "renamed to DEFAULT_REFRESH_DEBOUNCE_MS")]
pub const DEFAULT_DEMO_REFRESH_DEBOUNCE_MS: u64 = DEFAULT_REFRESH_DEBOUNCE_MS;
#[deprecated(note = "renamed to resolve_events_path")]
pub fn resolve_demo_events_path(path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    resolve_events_path(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn permissive_policy_allows_every_kind() {
        let policy = PermissivePolicy;
        for kind in [
            SkillEventKind::Open,
            SkillEventKind::Read,
            SkillEventKind::Write,
            SkillEventKind::Create,
            SkillEventKind::Delete,
            SkillEventKind::Rename,
            SkillEventKind::Metadata,
            SkillEventKind::Readlink,
            SkillEventKind::SymlinkAttempt,
            SkillEventKind::HardlinkAttempt,
            SkillEventKind::PolicyDecision,
            SkillEventKind::PolicyDenied,
            SkillEventKind::SourceChanged,
        ] {
            let ctx = PathPolicy::new(kind)
                .with_skill_name(Some("alpha"))
                .with_relative_path(Some(Path::new("scripts/run.sh")));
            assert_eq!(policy.check_path(&ctx), PolicyDecision::Allow);
            assert!(policy.check_path(&ctx).is_allowed());
        }
    }

    #[test]
    fn policy_decision_constructors() {
        assert_eq!(PolicyDecision::allow(), PolicyDecision::Allow);
        let deny = PolicyDecision::deny(libc::EACCES, "test");
        assert!(!deny.is_allowed());
        match deny {
            PolicyDecision::Deny { errno, reason } => {
                assert_eq!(errno, libc::EACCES);
                assert_eq!(reason, "test");
            }
            _ => panic!("expected Deny"),
        }
    }

    #[test]
    fn noop_sink_does_not_panic_or_record() {
        let sink = NoopEventSink;
        let event = SkillEvent::new(SkillEventKind::Read)
            .with_skill_name("alpha")
            .with_bytes(64);
        // Repeated emit must not fail.
        for _ in 0..16 {
            sink.emit(&event);
        }
    }

    #[test]
    fn in_memory_sink_records_events() {
        let sink = InMemoryEventSink::new();
        assert!(sink.is_empty());
        sink.emit(
            &SkillEvent::new(SkillEventKind::Readlink)
                .with_skill_name("alpha")
                .with_relative_path("link")
                .with_action(SkillEventAction::Allowed),
        );
        sink.emit(
            &SkillEvent::new(SkillEventKind::SymlinkAttempt)
                .with_skill_name("alpha")
                .with_action(SkillEventAction::Rejected)
                .with_errno(libc::EROFS),
        );

        assert_eq!(sink.len(), 2);
        let recorded = sink.events();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].kind, SkillEventKind::Readlink);
        assert_eq!(recorded[1].kind, SkillEventKind::SymlinkAttempt);
        assert_eq!(recorded[1].errno, Some(libc::EROFS));
        assert_eq!(recorded[1].action, Some(SkillEventAction::Rejected));

        let symlink_attempts = sink.of_kind(SkillEventKind::SymlinkAttempt);
        assert_eq!(symlink_attempts.len(), 1);
        assert_eq!(symlink_attempts[0].skill_name.as_deref(), Some("alpha"));
    }

    #[test]
    fn event_normalization_preserves_skill_name_and_path() {
        let event = SkillEvent::new(SkillEventKind::Delete)
            .with_optional_skill_name(Some("alpha"))
            .with_optional_relative_path(Some(Path::new("scripts/run.sh")))
            .with_caller(1000, 1000)
            .with_errno(libc::ENOENT);

        assert_eq!(event.skill_name.as_deref(), Some("alpha"));
        assert_eq!(
            event.relative_path.as_deref(),
            Some(Path::new("scripts/run.sh"))
        );
        assert_eq!(event.uid, Some(1000));
        assert_eq!(event.gid, Some(1000));
        assert_eq!(event.errno, Some(libc::ENOENT));
        assert_eq!(event.bytes, None);
    }

    #[test]
    fn event_optional_setters_can_clear_or_skip() {
        let event = SkillEvent::new(SkillEventKind::Metadata)
            .with_optional_skill_name::<String>(None)
            .with_optional_relative_path::<&Path>(None);
        assert!(event.skill_name.is_none());
        assert!(event.relative_path.is_none());
    }
}
