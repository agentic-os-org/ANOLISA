//! Action-runtime execution and explicit audit projection for the shared `SkillSec` service.

use crate::service::{batch, require_batch_roots, with_key};
use crate::{InitOptions, ScanOptions, SkillRoot, SkillSecError, SkillSecService, check_deadline};
use asc_action_runtime::{AuditProjector, CapabilityExecutor, ExecutionControl};
use asc_action_types::{ActionOutcome, AuditProjection, Failure, SkillSecCommand, SkillSecRequest};
use serde_json::{Map, Value, json};
use std::sync::Arc;
use std::time::Instant;

/// Physical resolution and integration health supplied by daemon composition.
/// Implementations must authenticate mounted mappings and never fall back inside a mount.
pub trait SkillEnvironment: Send + Sync {
    /// Resolves an exact canonical identity within the request deadline.
    ///
    /// # Errors
    /// Rejects untrusted, unavailable or stale directory mappings.
    fn resolve(
        &self,
        identity: &crate::SkillIdentity,
        deadline: Instant,
    ) -> Result<SkillRoot, SkillSecError>;

    /// Optional integration health, independent of the last business result.
    fn status(&self) -> Option<Value> {
        None
    }
}

/// Direct-directory resolution when no `SkillFS` integration is configured.
pub struct DirectSkillEnvironment;

impl SkillEnvironment for DirectSkillEnvironment {
    fn resolve(
        &self,
        identity: &crate::SkillIdentity,
        deadline: Instant,
    ) -> Result<SkillRoot, SkillSecError> {
        check_deadline(deadline)?;
        SkillRoot::direct(identity.path())
    }
}

/// Runs each operation on the single daemon-owned service.
pub struct SkillSecExecutor {
    service: Arc<SkillSecService>,
    environment: Arc<dyn SkillEnvironment>,
}

impl SkillSecExecutor {
    /// Reuses one service across action and background entrypoints.
    pub fn new(service: Arc<SkillSecService>) -> Self {
        Self {
            service,
            environment: Arc::new(DirectSkillEnvironment),
        }
    }

    /// Injects authenticated directory resolution without exposing it to clients.
    #[must_use]
    pub fn with_environment(mut self, environment: Arc<dyn SkillEnvironment>) -> Self {
        self.environment = environment;
        self
    }

    fn prepare(
        &self,
        command: &SkillSecCommand,
        deadline: Instant,
    ) -> Result<Vec<SkillRoot>, SkillSecError> {
        check_deadline(deadline)?;
        let managed = match command {
            SkillSecCommand::RotateKeys {} => self.service.rotation_skills(deadline)?,
            SkillSecCommand::Init {
                force_keys: true,
                baseline,
                ..
            } => {
                let mut skills = self.service.rotation_skills(deadline)?;
                if *baseline {
                    skills.extend(self.service.managed_skills()?);
                }
                skills
            }
            SkillSecCommand::Init { baseline: true, .. }
            | SkillSecCommand::Scan { all: true, .. }
            | SkillSecCommand::Check { all: true, .. }
            | SkillSecCommand::Status { .. } => self.service.managed_skills()?,
            _ => Vec::new(),
        };
        command
            .identities(&managed)?
            .iter()
            .map(|identity| self.environment.resolve(identity, deadline))
            .collect()
    }

    fn run(
        &self,
        request: &SkillSecRequest,
        roots: &[SkillRoot],
        deadline: Instant,
    ) -> Result<(Value, i64), SkillSecError> {
        check_deadline(deadline)?;
        let root = || required_root(roots);
        let service = &self.service;
        match &request.command {
            SkillSecCommand::Init {
                baseline,
                force_keys,
                scanners,
                ..
            } => service.init(
                roots,
                &InitOptions {
                    baseline: *baseline,
                    force_keys: *force_keys,
                    scanners: scanners.clone(),
                },
                request.caller_uid,
                deadline,
            ),
            SkillSecCommand::Scan {
                all,
                force,
                scanners,
                ..
            } => {
                let options = ScanOptions {
                    scanners: scanners.clone(),
                    force: *force,
                };
                if *all {
                    service.scan_batch(roots, &options, deadline)
                } else {
                    Ok((service.scan(root()?, &options, deadline)?, 0))
                }
            }
            SkillSecCommand::Certify {
                scanner,
                scanner_version,
                findings,
                ..
            } => service
                .certify(
                    root()?,
                    scanner,
                    scanner_version.as_deref(),
                    findings,
                    deadline,
                )
                .map(|value| (value, 0)),
            SkillSecCommand::Analyze { .. } => analyze(root()?, deadline),
            SkillSecCommand::Check { all, .. } => self.check(roots, *all, deadline),
            SkillSecCommand::Status { verbose } => self.status(roots, *verbose, deadline),
            SkillSecCommand::ListScanners {} => Ok(self.list_scanners()),
            SkillSecCommand::Audit {
                verify_snapshots, ..
            } => {
                let value = service.audit(root()?, *verify_snapshots, deadline)?;
                let code = i64::from(value["valid"] != true);
                Ok((value, code))
            }
            SkillSecCommand::Decide {
                action,
                version,
                reason,
                clear,
                ..
            } => {
                let key = service.initialize_with_deadline(deadline)?;
                let value = if *clear {
                    service.clear_decision(root()?, deadline)?
                } else {
                    service.decide(
                        root()?,
                        action.ok_or_else(|| {
                            SkillSecError::Invalid("decision action required".into())
                        })?,
                        version.as_deref(),
                        reason.as_deref(),
                        deadline,
                    )?
                };
                Ok((with_key(value, &key), 0))
            }
            SkillSecCommand::Show { .. } => Ok((service.show(root()?, deadline)?, 0)),
            SkillSecCommand::Export {
                version, output, ..
            } => Ok((
                service.export(root()?, version, output, request.caller_uid, deadline)?,
                0,
            )),
            SkillSecCommand::Activate { .. } => Ok((service.activate(root()?, deadline)?, 0)),
            SkillSecCommand::Reconcile { .. } => Ok((service.reconcile(root()?, deadline)?, 0)),
            SkillSecCommand::RotateKeys {} => {
                Ok((service.rotate_keys(roots, request.caller_uid, deadline)?, 0))
            }
        }
    }
    fn list_scanners(&self) -> (Value, i64) {
        let scanners: Vec<_> = self.service.scanners().scanners().iter().map(|s| json!({"name":s.name,"type":s.invocation,"parser":s.parser,"enabled":s.enabled,"autoInvocable":s.enabled && s.invocation == "builtin","description":s.description})).collect();
        (json!({"command":"list-scanners","scanners":scanners}), 0)
    }

    fn check(
        &self,
        roots: &[SkillRoot],
        all: bool,
        deadline: Instant,
    ) -> Result<(Value, i64), SkillSecError> {
        if all {
            require_batch_roots(roots)?;
            let (results, failed) =
                batch(roots, deadline, |root| self.service.check(root, deadline))?;
            let critical = failed || results.iter().any(critical);
            Ok((json!({"results":results}), i64::from(critical)))
        } else {
            let root = required_root(roots)?;
            let value = self.service.check(root, deadline)?;
            let code = i64::from(critical(&value));
            Ok((value, code))
        }
    }

    fn status(
        &self,
        roots: &[SkillRoot],
        verbose: bool,
        deadline: Instant,
    ) -> Result<(Value, i64), SkillSecError> {
        let service = &self.service;

        let keys = service.key_status(deadline)?;
        let (results, _) = batch(roots, deadline, |root| service.check(root, deadline))?;
        let mut breakdown =
            json!({"pass":0,"none":0,"drifted":0,"warn":0,"deny":0,"tampered":0,"error":0});
        for value in &results {
            let status = value["status"]
                .as_str()
                .filter(|s| breakdown.get(s).is_some())
                .unwrap_or("error");
            breakdown[status] = json!(breakdown[status].as_u64().unwrap_or(0) + 1);
        }
        let health = if results.is_empty() {
            "empty"
        } else if results.iter().any(critical) {
            "critical"
        } else if results
            .iter()
            .any(|v| matches!(v["status"].as_str(), Some("warn" | "drifted")))
        {
            "attention"
        } else if results.iter().all(|v| v["status"] == "none") {
            "unscanned"
        } else {
            "healthy"
        };
        let mut value = json!({"command":"status","keys":keys,"config":{"managedSkillDirPatterns":roots.len(),"registeredScanners":service.scanners().scanners().iter().map(|s| &s.name).collect::<Vec<_>>()},"skills":{"discovered":results.len(),"breakdown":breakdown,"health":health}});
        if verbose {
            value["results"] = json!(results);
        }
        Ok((value, 0))
    }
}

impl CapabilityExecutor for SkillSecExecutor {
    type Request = SkillSecRequest;

    fn execute(&self, control: &ExecutionControl, request: &Self::Request) -> ActionOutcome {
        let counter = &self.service.active_requests;
        if counter
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |n| (n < 2).then_some(n + 1),
            )
            .is_err()
        {
            return error_outcome(&SkillSecError::Busy);
        }
        let _admission = Admission(counter);
        let mut skill_count = 0;
        let result = if control.cancelled {
            Err(SkillSecError::Timeout)
        } else {
            self.prepare(&request.command, control.deadline)
                .and_then(|roots| {
                    skill_count = roots.len();
                    self.run(request, &roots, control.deadline)
                })
        };
        let mut outcome = match result {
            Ok((data, exit_code)) => {
                let failed_execution = exit_code != 0
                    && (data["status"] == "error"
                        || data["results"]
                            .as_array()
                            .is_some_and(|items| items.iter().any(|v| v["status"] == "error")));
                ActionOutcome {
                    success: exit_code == 0,
                    exit_code,
                    error: None,
                    error_type: if failed_execution {
                        "SkillLedgerError".into()
                    } else {
                        String::new()
                    },
                    data: Map::from_iter([("output".into(), data)]),
                }
            }
            Err(error) => error_outcome(&error),
        };
        outcome.data.insert("skillCount".into(), json!(skill_count));
        if matches!(request.command, SkillSecCommand::Status { .. })
            && let Some(status) = self.environment.status()
        {
            outcome.data["output"]["skillfs"] = status;
        }
        outcome
    }
}

// Preserve the V1 security-events consumer's command-specific verdict projection. A successful
// scan invocation can still yield deny; non-judgment operations do not invent a security verdict.
fn event_verdict(command: &SkillSecCommand, output: &Value) -> Option<&'static str> {
    const VERDICTS: &[&str] = &[
        "pass",
        "none",
        "warn",
        "unmanaged",
        "drifted",
        "deny",
        "tampered",
        "error",
    ];
    let known = |value: &Value| {
        VERDICTS
            .iter()
            .copied()
            .find(|v| value.as_str() == Some(*v))
    };
    match command {
        SkillSecCommand::Init { .. }
        | SkillSecCommand::Scan { .. }
        | SkillSecCommand::Check { .. }
            if output["results"].is_array() =>
        {
            output["results"]
                .as_array()?
                .iter()
                .flat_map(|item| [known(&item["status"]), known(&item["scanStatus"])])
                .flatten()
                .max_by_key(|v| VERDICTS.iter().position(|known| known == v))
        }
        SkillSecCommand::Check { .. } | SkillSecCommand::Analyze { .. } => known(&output["status"]),
        SkillSecCommand::Scan { .. } | SkillSecCommand::Certify { .. } => {
            known(&output["scanStatus"])
        }
        SkillSecCommand::Show { .. } => known(&output["latestStatus"]),
        SkillSecCommand::Decide { .. } => {
            known(&output["currentStatus"]).or_else(|| known(&output["scanStatus"]))
        }
        _ => None,
    }
}

fn analyze(root: &SkillRoot, deadline: Instant) -> Result<(Value, i64), SkillSecError> {
    root.verify_mapping()?;
    let result = crate::scanner::analyze(&root.io_dir, deadline)?;
    root.verify_mapping()?;
    Ok((result.data, i64::from(result.exit_code)))
}

fn required_root(roots: &[SkillRoot]) -> Result<&SkillRoot, SkillSecError> {
    roots
        .first()
        .ok_or_else(|| SkillSecError::Invalid("Skill root is required".into()))
}

fn critical(value: &Value) -> bool {
    matches!(
        value["status"].as_str(),
        Some("deny" | "tampered" | "error")
    )
}

/// Converts a domain failure without conflating a risk verdict with an execution error.
pub fn error_outcome(error: &SkillSecError) -> ActionOutcome {
    let kind = match error {
        SkillSecError::PermissionDenied => "PermissionDenied",
        SkillSecError::Busy => "Busy",
        SkillSecError::RotationPending => "RotationPending",
        SkillSecError::Timeout => "TimeoutError",
        SkillSecError::Invalid(_) | SkillSecError::Json(_) => "ValueError",
        SkillSecError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
            "FileNotFoundError"
        }
        SkillSecError::Io { .. } => "OSError",
        SkillSecError::Integrity(_) => "IntegrityError",
        SkillSecError::Key => "KeyError",
        SkillSecError::Scanner(_) => "ScannerError",
    };
    let message = error.to_string();
    ActionOutcome {
        success: false,
        exit_code: 1,
        error: Some(message.clone()),
        error_type: kind.into(),
        data: Map::from_iter([("output".into(), json!({"status":"error","error":message}))]),
    }
}

/// Audits bounded business metadata, excluding findings, source, manual reasons and key material.
#[derive(Default)]
pub struct SkillSecAuditProjector;

impl AuditProjector for SkillSecAuditProjector {
    type Request = SkillSecRequest;

    fn project(&self, request: &Self::Request, outcome: &ActionOutcome) -> AuditProjection {
        let audited_request = Map::from_iter([
            ("command".into(), json!(request.command.name())),
            (
                "skillCount".into(),
                outcome.data.get("skillCount").cloned().unwrap_or(json!(0)),
            ),
        ]);
        let output = &outcome.data["output"];
        let mut result = Map::from_iter([
            ("success".into(), json!(outcome.success)),
            ("exitCode".into(), json!(outcome.exit_code)),
        ]);
        for field in [
            "status",
            "scanStatus",
            "versionId",
            "newVersion",
            "valid",
            "versions_checked",
            "coverage_complete",
            "rotated",
            "trustRebuildRequired",
        ] {
            if let Some(value) = output.get(field) {
                result.insert(field.into(), value.clone());
            }
        }
        if let Some(value) = output.pointer("/activation/activationPending") {
            result.insert("activationPending".into(), value.clone());
        }
        if let Some(items) = output["results"].as_array() {
            result.insert("resultCount".into(), json!(items.len()));
        }
        if let Some(verdict) = event_verdict(&request.command, output) {
            result.insert("verdict".into(), json!(verdict));
        }
        AuditProjection::Completed {
            request: audited_request,
            result,
            failure: (!outcome.error_type.is_empty()).then(|| Failure {
                // Domain errors can contain source paths and scanner-supplied text. The public
                // event retains only the controlled error class; business output remains complete.
                error: Some("SkillSec operation failed".into()),
                error_type: outcome.error_type.clone(),
                exit_code: outcome.exit_code,
            }),
        }
    }
}

// Two concurrent bounded content captures protect the shared daemon's memory. Per-Skill locks
// remain the consistency boundary; a busy response is finalized through the same public sink.
struct Admission<'a>(&'a std::sync::atomic::AtomicUsize);
impl Drop for Admission<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::tests::{deadline, fixture, uninitialized_fixture};
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct RejectingEnvironment(AtomicUsize);

    impl SkillEnvironment for RejectingEnvironment {
        fn resolve(
            &self,
            _: &crate::SkillIdentity,
            _: Instant,
        ) -> Result<SkillRoot, SkillSecError> {
            self.0.fetch_add(1, Ordering::AcqRel);
            Err(SkillSecError::Integrity(
                "unavailable authenticated mapping".into(),
            ))
        }
    }

    #[test]
    fn failed_preparation_never_falls_back_to_a_readable_source_or_changes_keys() {
        let (_temporary, service, root) = uninitialized_fixture();
        let environment = Arc::new(RejectingEnvironment(AtomicUsize::new(0)));
        let executor = SkillSecExecutor::new(service.clone()).with_environment(environment.clone());
        let request = SkillSecRequest {
            command: SkillSecCommand::Init {
                baseline: true,
                force_keys: true,
                skill_dirs: vec![root.identity.clone()],
                scanners: None,
            },
            caller_uid: 0,
        };
        let outcome = executor.execute(
            &ExecutionControl {
                deadline: deadline(),
                cancelled: false,
            },
            &request,
        );
        assert_eq!(outcome.error_type, "IntegrityError");
        assert_eq!(environment.0.load(Ordering::Acquire), 1);
        assert_eq!(service.active_requests.load(Ordering::Acquire), 0);
        assert!(!service.config.state_dir.join("signing-key.pk8").exists());
        assert!(!root.io_dir.join(".skill-meta").exists());
        let audit = SkillSecAuditProjector
            .project(&request, &outcome)
            .into_details();
        assert_eq!(audit["error_type"], "IntegrityError");
        assert_eq!(audit["request"]["skillCount"], 0);
        assert!(
            !serde_json::to_string(&audit)
                .unwrap()
                .contains("unavailable authenticated mapping")
        );
    }

    #[test]
    fn invalid_selectors_and_expired_or_cancelled_requests_do_not_resolve_or_mutate() {
        let (_temporary, service, root) = uninitialized_fixture();
        let environment = Arc::new(RejectingEnvironment(AtomicUsize::new(0)));
        let executor = SkillSecExecutor::new(service.clone()).with_environment(environment.clone());
        let mut request = SkillSecRequest {
            command: SkillSecCommand::Scan {
                skill_dir: Some(root.identity.clone()),
                all: true,
                skill_dirs: vec![],
                force: false,
                scanners: None,
            },
            caller_uid: 0,
        };
        let invalid = executor.execute(
            &ExecutionControl {
                deadline: deadline(),
                cancelled: false,
            },
            &request,
        );
        assert_eq!(invalid.error_type, "ValueError");
        request.command = SkillSecCommand::Init {
            baseline: true,
            force_keys: true,
            skill_dirs: vec![root.identity.clone()],
            scanners: None,
        };
        for control in [
            ExecutionControl {
                deadline: Instant::now(),
                cancelled: false,
            },
            ExecutionControl {
                deadline: deadline(),
                cancelled: true,
            },
        ] {
            let outcome = executor.execute(&control, &request);
            assert_eq!(outcome.error_type, "TimeoutError");
        }
        assert_eq!(environment.0.load(Ordering::Acquire), 0);
        assert_eq!(service.active_requests.load(Ordering::Acquire), 0);
        assert!(!service.config.state_dir.join("signing-key.pk8").exists());
        assert!(!root.io_dir.join(".skill-meta").exists());
    }

    struct MappedEnvironment(SkillRoot);

    impl SkillEnvironment for MappedEnvironment {
        fn resolve(
            &self,
            identity: &crate::SkillIdentity,
            deadline: Instant,
        ) -> Result<SkillRoot, SkillSecError> {
            check_deadline(deadline)?;
            assert_eq!(identity, &self.0.identity);
            Ok(self.0.clone())
        }

        fn status(&self) -> Option<Value> {
            Some(json!({"enabled": true, "healthy": true}))
        }
    }

    #[test]
    fn authenticated_mapping_keeps_the_source_identity_and_status_health() {
        let (_temporary, service, physical) = uninitialized_fixture();
        let identity =
            crate::SkillIdentity::new(physical.io_dir.with_file_name("source-alias")).unwrap();
        let root = SkillRoot::resolved(identity.clone(), physical.io_dir).unwrap();
        let executor = SkillSecExecutor::new(service.clone())
            .with_environment(Arc::new(MappedEnvironment(root.clone())));
        let control = ExecutionControl {
            deadline: deadline(),
            cancelled: false,
        };
        let request = SkillSecRequest {
            command: SkillSecCommand::Certify {
                skill_dir: identity.clone(),
                scanner: "custom".into(),
                scanner_version: None,
                findings: json!([]),
            },
            caller_uid: 1001,
        };
        let outcome = executor.execute(&control, &request);
        assert!(outcome.success, "{outcome:?}");
        assert_eq!(outcome.data["skillCount"], 1);
        assert_eq!(service.managed_skills().unwrap(), vec![identity]);
        assert_eq!(service.check(&root, deadline()).unwrap()["status"], "pass");
        let status = executor.execute(
            &control,
            &SkillSecRequest {
                command: SkillSecCommand::Status { verbose: true },
                caller_uid: 1001,
            },
        );
        assert!(status.success, "{status:?}");
        assert_eq!(
            status.data["output"]["skillfs"],
            json!({"enabled": true, "healthy": true})
        );
        assert_eq!(status.data["skillCount"], 1);
    }

    #[test]
    fn expired_content_scan_releases_admission_for_the_next_request() {
        let (_temporary, service, root) = fixture();
        for index in 0..8 {
            std::fs::write(
                root.io_dir.join(format!("comments-{index}.js")),
                "/**/".repeat(249_999),
            )
            .unwrap();
        }
        let executor = SkillSecExecutor::new(service.clone());
        let mut request = SkillSecRequest {
            command: SkillSecCommand::Scan {
                skill_dir: Some(root.identity.clone()),
                all: false,
                skill_dirs: vec![],
                force: false,
                scanners: Some(vec!["static-scanner".into()]),
            },
            caller_uid: 1001,
        };
        let result = executor.execute(
            &ExecutionControl {
                deadline: Instant::now() + std::time::Duration::from_millis(1),
                cancelled: false,
            },
            &request,
        );
        assert_eq!(result.error_type, "TimeoutError");
        assert_eq!(
            service
                .active_requests
                .load(std::sync::atomic::Ordering::Acquire),
            0
        );
        request.command = SkillSecCommand::ListScanners {};
        assert!(
            executor
                .execute(
                    &ExecutionControl {
                        deadline: deadline(),
                        cancelled: false
                    },
                    &request
                )
                .success
        );
    }

    #[test]
    fn outcomes_keep_risk_exit_codes_and_audit_excludes_sensitive_details() {
        let (_temporary, service, root) = fixture();
        let executor = SkillSecExecutor::new(service.clone());
        let request = SkillSecRequest {
            command: SkillSecCommand::Certify {
                skill_dir: root.identity.clone(),
                scanner: "fixture".into(),
                scanner_version: None,
                findings: json!([{"rule":"private-rule","level":"deny","message":"PRIVATE_SOURCE_MARKER","file":"private.py"}]),
            },
            caller_uid: 1001,
        };
        let control = ExecutionControl {
            deadline: deadline(),
            cancelled: false,
        };
        let certified = executor.execute(&control, &request);
        assert_eq!(certified.exit_code, 0);
        assert_eq!(certified.data["output"]["keyCreated"], false);
        let check = SkillSecRequest {
            command: SkillSecCommand::Check {
                skill_dir: Some(root.identity.clone()),
                all: false,
                skill_dirs: Vec::new(),
            },
            caller_uid: 1001,
        };
        let outcome = executor.execute(&control, &check);
        assert_eq!(outcome.exit_code, 1);
        assert!(outcome.error_type.is_empty());
        assert!(
            serde_json::to_string(&outcome.data)
                .unwrap()
                .contains("PRIVATE_SOURCE_MARKER")
        );
        let audit = SkillSecAuditProjector
            .project(&check, &outcome)
            .into_details();
        assert!(
            !serde_json::to_string(&audit)
                .unwrap()
                .contains("PRIVATE_SOURCE_MARKER")
        );
        assert!(!audit.contains_key("error_type"));
        assert_eq!(audit["result"]["status"], "deny");
        assert_eq!(audit["result"]["verdict"], "deny");
        let rejected = executor.execute(
            &ExecutionControl {
                deadline: deadline(),
                cancelled: true,
            },
            &check,
        );
        assert_eq!(rejected.error_type, "TimeoutError");
        assert_eq!(
            service
                .active_requests
                .load(std::sync::atomic::Ordering::Acquire),
            0
        );
        service
            .active_requests
            .store(2, std::sync::atomic::Ordering::Release);
        assert_eq!(executor.execute(&control, &check).error_type, "Busy");
    }

    #[test]
    fn baseline_and_batch_keep_successful_skills_and_classify_execution_failures() {
        let (_temporary, service, root) = fixture();
        let bad_path = root.io_dir.with_file_name("invalid-skill");
        std::fs::create_dir(&bad_path).unwrap();
        let bad_root = SkillRoot::direct(bad_path).unwrap();
        let identities = vec![bad_root.identity, root.identity.clone()];
        let executor = SkillSecExecutor::new(service.clone());
        let control = ExecutionControl {
            deadline: deadline(),
            cancelled: false,
        };
        let mut request = SkillSecRequest {
            command: SkillSecCommand::Init {
                baseline: true,
                force_keys: false,
                skill_dirs: identities.clone(),
                scanners: None,
            },
            caller_uid: 1001,
        };
        let outcome = executor.execute(&control, &request);
        assert_eq!(outcome.exit_code, 1);
        assert_eq!(outcome.error_type, "SkillLedgerError");
        assert_eq!(outcome.data["output"]["results"][0]["status"], "error");
        assert_eq!(outcome.data["output"]["results"][1]["versionId"], "v000001");
        assert_eq!(service.check(&root, deadline()).unwrap()["status"], "pass");
        let audit = SkillSecAuditProjector
            .project(&request, &outcome)
            .into_details();
        assert_eq!(audit["error_type"], "SkillLedgerError");
        assert_eq!(audit["result"]["verdict"], "error");
        request.command = SkillSecCommand::Scan {
            skill_dir: None,
            all: true,
            skill_dirs: identities.clone(),
            force: false,
            scanners: None,
        };
        let outcome = executor.execute(&control, &request);
        assert_eq!(outcome.data["output"]["results"][1]["status"], "noop");
        assert_eq!(outcome.error_type, "SkillLedgerError");
        request.command = SkillSecCommand::Check {
            skill_dir: None,
            all: true,
            skill_dirs: identities.clone(),
        };
        let outcome = executor.execute(&control, &request);
        assert_eq!(outcome.data["output"]["results"][1]["status"], "pass");
        assert_eq!(outcome.exit_code, 1);
    }

    #[test]
    fn empty_aggregate_is_not_success_and_does_not_initialize_keys() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let service = Arc::new(
            SkillSecService::new(
                crate::SkillSecConfig {
                    state_dir: directory.path().into(),
                    managed_skill_dirs: Vec::new(),
                },
                crate::scanner::ScannerRegistry::default(),
            )
            .unwrap(),
        );
        let executor = SkillSecExecutor::new(service.clone());
        for command in [
            SkillSecCommand::Check {
                skill_dir: None,
                all: true,
                skill_dirs: Vec::new(),
            },
            SkillSecCommand::Scan {
                skill_dir: None,
                all: true,
                skill_dirs: Vec::new(),
                force: false,
                scanners: None,
            },
        ] {
            let outcome = executor.execute(
                &ExecutionControl {
                    deadline: deadline(),
                    cancelled: false,
                },
                &SkillSecRequest {
                    command,
                    caller_uid: 1001,
                },
            );
            assert_eq!(outcome.error_type, "FileNotFoundError");
            assert_eq!(outcome.exit_code, 1);
        }
        assert_eq!(
            service.key_status(deadline()).unwrap()["initialized"],
            false
        );
        let key = service.initialize().unwrap();
        assert_eq!(key["keyCreated"], true);
        assert_eq!(service.initialize().unwrap()["keyCreated"], false);
        assert_eq!(
            event_verdict(
                &SkillSecCommand::Status { verbose: true },
                &json!({"status":"pass"})
            ),
            None
        );
        assert_eq!(
            event_verdict(
                &SkillSecCommand::Show {
                    skill_dir: crate::SkillIdentity::new("/synthetic/skill").unwrap()
                },
                &json!({"latestStatus":"drifted"})
            ),
            Some("drifted")
        );
    }

    #[test]
    fn protocol_model_refuses_claimed_identity_and_invalid_selector_combinations() {
        for value in [
            json!({"command":"rotate-keys","uid":0}),
            json!({"command":"list-scanners","uid":0}),
            json!({"command":"status","ioDir":"/private"}),
            json!({"command":"unknown"}),
        ] {
            assert!(serde_json::from_value::<SkillSecCommand>(value).is_err());
        }
        let command: SkillSecCommand = serde_json::from_value(
            json!({"command":"scan","all":true,"skillDir":"/synthetic/skill"}),
        )
        .unwrap();
        assert!(command.identities(&[]).is_err());
    }
}
