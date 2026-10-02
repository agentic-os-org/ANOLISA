//! Policy evaluation plus unified audit storage, query, and retention.

pub mod action;
pub mod builtin;
pub mod config;
pub mod evaluate;
pub mod export;
pub mod glob;
pub mod log;
pub mod policy;
pub mod query;
pub mod reader;
pub mod redact;
pub mod retention;
pub mod state;
pub mod store;

pub use action::{parse_action_string, split_compound_command, ParseError};
pub use builtin::BuiltinPreset;
pub use evaluate::evaluate;
pub use policy::{LoadedPolicy, PolicySource};

use chrono::Utc;
use cosh_types::audit::{
    Action, AuditActor, AuditActorKind, AuditComponent, AuditComponentName, AuditDecisionData,
    AuditEventOutcome, AuditEventType, AuditEventV1, AuditIdentity, AuditMode, AuditOutcomeStatus,
    AuditRedaction, AuditRedactionStatus, AuditSubject, Decision, KnownAuditEventType, LogSource,
    Outcome,
};
use cosh_types::error::CoshError;
use uuid::Uuid;

use self::store::{AuditDurability, AuditSegmentWriter};

#[cfg(test)]
/// Private temporary directory used by no-follow audit storage tests.
pub(crate) struct AuditTestDir {
    _directory: tempfile::TempDir,
    path: std::path::PathBuf,
}

#[cfg(test)]
impl AuditTestDir {
    /// Creates a private test directory and resolves platform-managed path aliases.
    pub(crate) fn create() -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.path().canonicalize().unwrap();
        Self {
            _directory: directory,
            path,
        }
    }

    /// Returns the real path used by no-follow storage tests.
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }
}

/// Call-site identity captured for a policy audit event.
#[derive(Debug, Clone)]
pub struct CallerInfo {
    /// Shell/terminal correlation identifier, with a process-scoped fallback.
    pub session_id: String,
    /// Local user label used only by legacy call sites, never persisted in v1.
    pub user: String,
    /// Real user identifier.
    pub uid: u32,
    /// Effective user identifier.
    pub euid: u32,
    /// Sudo user label used only by legacy call sites, never persisted in v1.
    pub sudo_user: Option<String>,
    /// Current process identifier.
    pub pid: u32,
}

impl CallerInfo {
    /// Detects process identity without failing the policy decision path.
    pub fn detect() -> Self {
        let user = std::env::var("USER")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| {
                std::env::var("LOGNAME")
                    .ok()
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or_else(|| "unknown".to_string());
        let session_id = detected_shell_session_id()
            .unwrap_or_else(|| format!("p{}-t{}", std::process::id(), Utc::now().timestamp()));
        let sudo_user = std::env::var("SUDO_USER")
            .ok()
            .filter(|value| !value.is_empty());
        Self {
            session_id,
            user,
            uid: nix::unistd::Uid::current().as_raw(),
            euid: nix::unistd::Uid::effective().as_raw(),
            sudo_user,
            pid: std::process::id(),
        }
    }
}

fn detected_shell_session_id() -> Option<String> {
    std::env::var("COSH_SESSION_ID")
        .ok()
        .filter(|value| !value.is_empty())
}

/// Runs a full policy evaluation and records its decision when possible.
///
/// # Errors
///
/// Returns an audit error with the produced decision in safe structured
/// details when a deny or approval-required decision cannot be persisted.
/// Allowed decisions remain available in best-effort mode if recording fails.
pub fn check(
    action: Action,
    source: LogSource,
    loaded: &LoadedPolicy,
) -> Result<Decision, CoshError> {
    let settings = config::load_audit_settings(None)?.settings;
    check_with_mode(action, source, loaded, settings.mode)
}

fn check_with_mode(
    action: Action,
    source: LogSource,
    loaded: &LoadedPolicy,
    mode: AuditMode,
) -> Result<Decision, CoshError> {
    let decision = evaluate(&action, loaded);
    match record_to_log(action, &decision, source) {
        Ok(()) => Ok(decision),
        Err(error) if decision.outcome == Outcome::Allow && mode == AuditMode::BestEffort => {
            tracing::warn!(
                target: "cosh_audit",
                decision = ?decision,
                audit_mode = ?mode,
                error = %error,
                "failed to persist allowed audit decision in best-effort mode; allowing action"
            );
            Ok(decision)
        }
        Err(mut error) => {
            if let Ok(value) = serde_json::to_value(&decision) {
                error = error.with_details(serde_json::json!({ "decision": value }));
            }
            Err(error)
        }
    }
}

/// Records a pre-decided policy result without evaluating it again.
///
/// # Errors
///
/// Returns a stable audit error when the version 1 record cannot be durably
/// persisted.
pub fn record_decision(
    action: Action,
    decision: &Decision,
    source: LogSource,
) -> Result<(), CoshError> {
    record_to_log(action, decision, source)
}

/// Evaluates a policy without recording an execution event.
pub fn classify(action: &Action, loaded: &LoadedPolicy) -> Decision {
    evaluate(action, loaded)
}

fn record_to_log(
    mut action: Action,
    decision: &Decision,
    source: LogSource,
) -> Result<(), CoshError> {
    let redacted = redact::redact_action(&mut action);
    let shell_session_id = detected_shell_session_id();
    let caller = CallerInfo::detect();
    let root = config::resolve_audit_root()?;
    let component = match source {
        LogSource::Cli => AuditComponentName::CoshCli,
        LogSource::Tui { .. } => AuditComponentName::CoshShell,
        LogSource::External { .. } => AuditComponentName::CoshCore,
    };
    let outcome = match decision.outcome {
        Outcome::Allow => AuditOutcomeStatus::Allowed,
        Outcome::Deny => AuditOutcomeStatus::Denied,
        Outcome::RequireApproval => AuditOutcomeStatus::Started,
    };
    let decision_name = match decision.outcome {
        Outcome::Allow => "allow",
        Outcome::Deny => "deny",
        Outcome::RequireApproval => "require_approval",
    };
    let payload = AuditDecisionData {
        decision: decision_name.to_string(),
        reason_code: Some(if decision.matched_rule.is_some() {
            "matched_rule".to_string()
        } else {
            "default_policy".to_string()
        }),
        policy_version: Some(decision.policy_version.clone()),
        duration_ms: None,
    };
    let now = Utc::now();
    let mut event = AuditEventV1::new(
        Uuid::new_v4().to_string(),
        AuditEventType::from(KnownAuditEventType::PolicyDecision),
        now,
        now,
        0,
        AuditComponent {
            name: component.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        AuditIdentity {
            shell_session_id,
            ..AuditIdentity::default()
        },
        AuditActor {
            kind: AuditActorKind::User,
            uid: Some(caller.uid),
            euid: Some(caller.euid),
        },
        AuditEventOutcome {
            status: outcome,
            code: None,
            retryable: false,
        },
        AuditSubject {
            kind: "policy_action".to_string(),
            name: Some(action.operation),
        },
        &payload,
        AuditRedaction {
            policy_version: "audit-redaction-v1".to_string(),
            status: if redacted {
                AuditRedactionStatus::Redacted
            } else {
                AuditRedactionStatus::Clean
            },
            fields: Vec::new(),
        },
    )
    .map_err(|error| {
        cosh_types::error::CoshError::new(
            cosh_types::error::ErrorCode::AuditLogError,
            format!("build policy audit event: {error}"),
            "audit",
        )
    })?;
    let mut writer = AuditSegmentWriter::create(&root.path, component)?;
    writer.append(&mut event, AuditDurability::SecurityBoundary)?;
    writer.close()
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use super::*;
    use cosh_types::audit::{ActionSubsystem, AuditMode, Outcome};

    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    struct AuditEnvGuard {
        directory: AuditTestDir,
        original_audit_dir: Option<std::ffi::OsString>,
        original_shell_session_id: Option<std::ffi::OsString>,
        _lock: MutexGuard<'static, ()>,
    }

    fn temp_audit_env() -> AuditEnvGuard {
        let lock = env_lock();
        let directory = AuditTestDir::create();
        let original_audit_dir = std::env::var_os("COSH_AUDIT_DIR");
        let original_shell_session_id = std::env::var_os("COSH_SESSION_ID");
        std::env::set_var("COSH_AUDIT_DIR", directory.path());
        AuditEnvGuard {
            directory,
            original_audit_dir,
            original_shell_session_id,
            _lock: lock,
        }
    }

    fn failing_audit_env() -> AuditEnvGuard {
        let guard = temp_audit_env();
        let file_root = guard.directory.path().join("not-a-directory");
        std::fs::write(&file_root, []).unwrap();
        std::env::set_var("COSH_AUDIT_DIR", file_root);
        guard
    }

    impl Drop for AuditEnvGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.original_audit_dir {
                std::env::set_var("COSH_AUDIT_DIR", value);
            } else {
                std::env::remove_var("COSH_AUDIT_DIR");
            }
            if let Some(value) = &self.original_shell_session_id {
                std::env::set_var("COSH_SESSION_ID", value);
            } else {
                std::env::remove_var("COSH_SESSION_ID");
            }
        }
    }

    fn package_install() -> Action {
        Action {
            subsystem: ActionSubsystem::Pkg,
            operation: "install".to_string(),
            target: Some("nginx".to_string()),
            args: Vec::new(),
            raw: Some("pkg install nginx".to_string()),
        }
    }

    #[test]
    fn check_records_one_v1_policy_event() {
        let guard = temp_audit_env();
        let loaded = builtin::balanced();
        let decision = check_with_mode(
            package_install(),
            LogSource::Cli,
            &loaded,
            AuditMode::BestEffort,
        )
        .unwrap();
        assert_eq!(decision.outcome, Outcome::RequireApproval);
        let read = reader::read_all(guard.directory.path(), false).unwrap();
        assert_eq!(read.events.len(), 1);
        assert_eq!(read.events[0].event.event_type.as_str(), "policy.decision");
    }

    #[test]
    fn check_allows_allowed_action_when_audit_persistence_fails() {
        let _guard = failing_audit_env();

        let decision = check_with_mode(
            parse_action_string("echo hello").unwrap(),
            LogSource::Cli,
            &builtin::balanced(),
            AuditMode::BestEffort,
        )
        .unwrap();

        assert_eq!(decision.outcome, Outcome::Allow);
    }

    #[test]
    fn check_fails_closed_for_non_allow_actions_when_audit_persistence_fails() {
        let _guard = failing_audit_env();

        for (action_string, expected_outcome) in [
            ("rm /tmp/test", Outcome::Deny),
            ("touch /tmp/test", Outcome::RequireApproval),
        ] {
            let error = check_with_mode(
                parse_action_string(action_string).unwrap(),
                LogSource::Cli,
                &builtin::balanced(),
                AuditMode::BestEffort,
            )
            .unwrap_err();
            let details = error
                .details
                .expect("audit error must include its decision");

            assert_eq!(
                details["decision"]["outcome"],
                serde_json::to_value(expected_outcome).unwrap()
            );
        }
    }

    #[test]
    fn check_fails_closed_for_allowed_action_when_audit_is_required() {
        let _guard = failing_audit_env();

        let error = check_with_mode(
            parse_action_string("echo hello").unwrap(),
            LogSource::Cli,
            &builtin::balanced(),
            AuditMode::Required,
        )
        .unwrap_err();
        let details = error
            .details
            .expect("audit error must include its decision");

        assert_eq!(details["decision"]["outcome"], "Allow");
    }

    #[test]
    fn policy_event_records_cosh_session_as_shell_identity() {
        let guard = temp_audit_env();
        std::env::set_var("COSH_SESSION_ID", "raw-session-identity-test");

        check_with_mode(
            package_install(),
            LogSource::Cli,
            &builtin::balanced(),
            AuditMode::BestEffort,
        )
        .unwrap();

        let read = reader::read_all(guard.directory.path(), false).unwrap();
        let identity = &read.events[0].event.identity;
        assert_eq!(
            identity.shell_session_id.as_deref(),
            Some("raw-session-identity-test")
        );
        assert_eq!(identity.provider_session_id, None);
    }

    #[test]
    fn caller_info_detection_is_infallible() {
        let _guard = temp_audit_env();
        std::env::remove_var("COSH_SESSION_ID");

        let info = CallerInfo::detect();

        assert!(!info.session_id.is_empty());
        assert!(!info.user.is_empty());
    }

    #[test]
    fn policy_event_omits_missing_session_identities() {
        let guard = temp_audit_env();
        std::env::remove_var("COSH_SESSION_ID");

        check_with_mode(
            package_install(),
            LogSource::Cli,
            &builtin::balanced(),
            AuditMode::BestEffort,
        )
        .unwrap();

        let read = reader::read_all(guard.directory.path(), false).unwrap();
        let identity = &read.events[0].event.identity;
        assert_eq!(identity.shell_session_id, None);
        assert_eq!(identity.provider_session_id, None);
    }

    #[test]
    fn redacted_policy_event_does_not_persist_secret() {
        let guard = temp_audit_env();
        let loaded = builtin::balanced();
        let action = Action {
            subsystem: ActionSubsystem::Pkg,
            operation: "install".to_string(),
            target: Some("nginx".to_string()),
            args: vec![("password".to_string(), "hunter2".to_string())],
            raw: None,
        };
        check_with_mode(action, LogSource::Cli, &loaded, AuditMode::BestEffort).unwrap();
        let files = std::fs::read_dir(guard.directory.path().join("v1/segments"))
            .unwrap()
            .flat_map(|date| std::fs::read_dir(date.unwrap().path()).unwrap())
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        let bytes = std::fs::read(&files[0]).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("hunter2"));
    }
}
