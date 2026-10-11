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

    /// Whether this identity belongs to an administrator-configured authenticated mount.
    /// This check is lexical; `resolve()` must still authenticate the physical mapping.
    fn manages(&self, _identity: &crate::SkillIdentity) -> bool {
        false
    }

    /// The uid authorized to operate this identity through an authenticated
    /// mapping, when the environment tracks one.
    ///
    /// Used only when the deployment enabled per-skill ownership isolation:
    /// a `SkillFS` mount is bound to its configured peer uid, so that uid —
    /// not the backing directory's filesystem owner — is the operator.
    /// Returns `None` for identities the environment does not manage.
    fn owner_uid(&self, _identity: &crate::SkillIdentity) -> Option<u32> {
        None
    }

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
        // The pin binds authorization to the object later privileged I/O
        // opens, instead of re-resolving the request path after lock waits.
        SkillRoot::pinned_direct(identity.path())
    }
}

/// Runs each operation on the single daemon-owned service.
#[derive(Clone)]
pub struct SkillSecExecutor {
    service: Arc<SkillSecService>,
    environment: Arc<dyn SkillEnvironment>,
    /// Whether non-root callers may only operate Skills they own. Off keeps
    /// the documented phase-one contract (every local caller may operate
    /// every managed Skill); on implements the per-user isolation the
    /// maintainers' TODO calls for.
    require_ownership: bool,
}

impl SkillSecExecutor {
    /// Reuses one service across action and background entrypoints.
    pub fn new(service: Arc<SkillSecService>) -> Self {
        Self {
            service,
            environment: Arc::new(DirectSkillEnvironment),
            require_ownership: false,
        }
    }

    /// Injects authenticated directory resolution without exposing it to clients.
    #[must_use]
    pub fn with_environment(mut self, environment: Arc<dyn SkillEnvironment>) -> Self {
        self.environment = environment;
        self
    }

    /// Restricts non-root callers to the Skills they own.
    ///
    /// Root and daemon-owned background work (startup recovery, the worker's
    /// discovery) stay unrestricted; local callers keep access to their own
    /// directories, including `SkillFS` mounts bound to their uid.
    #[must_use]
    pub fn with_require_skill_ownership(mut self, require: bool) -> Self {
        self.require_ownership = require;
        self
    }

    /// Whether `caller_uid` may mutate the Skill or inspect its private content.
    fn may_operate(&self, identity: &crate::SkillIdentity, caller_uid: u32) -> bool {
        self.authorized(identity, caller_uid, false)
    }

    /// Whether `caller_uid` may query the Skill's verdict summary.
    ///
    /// Beyond the caller's own Skills, the supported shared layout — root-owned
    /// system Skills in administrator-managed locations — stays queryable:
    /// every local user can already read that content directly, and consumers
    /// such as the Codex hook need a verdict for a consumed Skill instead of a
    /// failure that would make them fail open. Findings-bearing reads (scan,
    /// audit, export) stay owner-only.
    fn may_query(&self, identity: &crate::SkillIdentity, caller_uid: u32) -> bool {
        self.authorized(identity, caller_uid, true)
    }

    fn authorized(&self, identity: &crate::SkillIdentity, caller_uid: u32, query: bool) -> bool {
        let managed = self
            .service
            .config
            .managed_skill_dirs
            .iter()
            .any(|pattern| pattern.contains(identity))
            || self.environment.manages(identity);
        if !managed {
            return false;
        }
        if !self.require_ownership || caller_uid == 0 {
            return true;
        }
        // Per-skill isolation: only the authenticated mount's bound uid (for
        // mounted Skills — one mount is one authorization domain) or the
        // directory's filesystem owner (for ordinary managed directories) may
        // operate the Skill. Both fail closed when the owner cannot be
        // established.
        if self.environment.manages(identity) {
            self.environment
                .owner_uid(identity)
                .is_some_and(|owner| owner == caller_uid)
        } else {
            match directory_owner_uid(identity.path()) {
                Some(owner) if owner == caller_uid => true,
                // Root-owned managed directories are the shared system-Skill
                // layout; their summaries stay queryable by every caller.
                Some(0) => query,
                _ => false,
            }
        }
    }

    /// Authoritative ownership check on the resolved object, not the path.
    ///
    /// The checks before resolution are a prefilter; this decision binds to
    /// the pinned directory the service actually opens after its lock wait, so
    /// a caller with parent rename rights cannot substitute another owner's
    /// directory between authorization and the privileged operation.
    fn root_authorized(&self, root: &SkillRoot, caller_uid: u32, query: bool) -> bool {
        if self.environment.manages(&root.identity) {
            return self
                .environment
                .owner_uid(&root.identity)
                .is_some_and(|owner| owner == caller_uid);
        }
        match root.pinned_owner_uid() {
            Some(owner) if owner == caller_uid => true,
            Some(0) => query,
            _ => false,
        }
    }

    /// Discovers authorized ordinary Skills and registered mounts without traversing FUSE.
    /// Shared by aggregate commands and the daemon's asynchronous startup scan.
    ///
    /// `caller_uid` is the kernel-authenticated caller (0 for daemon-owned
    /// background discovery); with ownership isolation on, only the Skills
    /// that caller may operate are returned, and foreign-owned subtrees are
    /// pruned during traversal so a foreign user's unreadable, non-UTF-8 or
    /// over-deep tree cannot abort the caller's aggregate.
    ///
    /// # Errors
    /// Reports invalid or unreadable configured roots, corrupt registration and expired deadlines.
    pub fn discover(
        &self,
        caller_uid: u32,
        deadline: Instant,
    ) -> Result<Vec<crate::SkillIdentity>, SkillSecError> {
        let mut skills: std::collections::BTreeSet<_> = self
            .service
            .managed_skills()?
            .into_iter()
            .filter(|identity| self.may_operate(identity, caller_uid))
            .collect();
        let isolate = self.require_ownership && caller_uid != 0;
        for pattern in &self.service.config.managed_skill_dirs {
            // Mounted Skills are discovered by authenticated notifications, never by traversing FUSE.
            if !self.environment.manages(&pattern.root) {
                skills.extend(
                    crate::discovery::discover(
                        pattern,
                        deadline,
                        |id| self.environment.manages(id),
                        |path: &std::path::Path| {
                            isolate && directory_owner_uid(path) != Some(caller_uid)
                        },
                    )?
                    .into_iter()
                    .filter(|identity| self.may_operate(identity, caller_uid)),
                );
            }
        }
        Ok(skills.into_iter().collect())
    }

    fn prepare(
        &self,
        request: &SkillSecRequest,
        deadline: Instant,
    ) -> Result<Vec<SkillRoot>, SkillSecError> {
        check_deadline(deadline)?;
        let command = &request.command;
        let rotates = matches!(
            command,
            SkillSecCommand::RotateKeys {}
                | SkillSecCommand::Init {
                    force_keys: true,
                    ..
                }
        );
        if rotates && request.caller_uid != 0 {
            return Err(SkillSecError::PermissionDenied);
        }
        // Explicitly forbidden targets are rejected as a whole; query commands
        // (check, show) additionally accept the shared system-Skill layout.
        let query = matches!(
            command,
            SkillSecCommand::Check { .. } | SkillSecCommand::Show { .. }
        );
        let authorized = |identity: &crate::SkillIdentity| {
            if query {
                self.may_query(identity, request.caller_uid)
            } else {
                self.may_operate(identity, request.caller_uid)
            }
        };
        // Validate caller paths before discovery or resolution performs any target I/O.
        let mut identities = command.identities(&[])?;
        for identity in &identities {
            if !authorized(identity) {
                return Err(SkillSecError::ScopeDenied(identity.clone()));
            }
        }
        let recovery =
            if rotates && !matches!(command, SkillSecCommand::Init { baseline: true, .. }) {
                self.service.rotation_recovery_skills(deadline)?
            } else {
                None
            };
        let mut managed = if rotates {
            self.service.rotation_skills(deadline)?
        } else {
            Vec::new()
        };
        if matches!(
            command,
            SkillSecCommand::Init { baseline: true, .. }
                | SkillSecCommand::Scan { all: true, .. }
                | SkillSecCommand::Check { all: true, .. }
                | SkillSecCommand::Status { .. }
        ) {
            managed.extend(self.discover(request.caller_uid, deadline)?);
        }
        identities.extend(managed);
        identities.sort();
        identities.dedup();
        for identity in &identities {
            // Only root can finish an already authorized, private rotation intent after reconfiguration.
            // Registration alone never grants this exception to a new operation.
            if !authorized(identity)
                && !recovery
                    .as_ref()
                    .is_some_and(|skills| skills.contains(identity))
            {
                return Err(SkillSecError::ScopeDenied(identity.clone()));
            }
        }
        let mut roots = Vec::with_capacity(identities.len());
        for identity in &identities {
            roots.push(self.environment.resolve(identity, deadline)?);
        }
        // Authoritative per-root ownership on the resolved, pinned object: the
        // pre-resolution checks are a prefilter, and the directory opened for
        // the privileged operation must be the one that was authorized.
        if self.require_ownership && request.caller_uid != 0 {
            for root in &roots {
                if !self.root_authorized(root, request.caller_uid, query) {
                    return Err(SkillSecError::ScopeDenied(root.identity.clone()));
                }
            }
        }
        Ok(roots)
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
        let mut value = json!({"command":"status","keys":keys,"config":{"managedSkillDirPatterns":service.config.managed_skill_dirs.len(),"registeredScanners":service.scanners().scanners().iter().map(|s| &s.name).collect::<Vec<_>>()},"skills":{"discovered":results.len(),"breakdown":breakdown,"health":health}});
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
            self.prepare(request, control.deadline).and_then(|roots| {
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

/// The filesystem owner of one ordinary managed Skill directory.
///
/// Returns `None` when the directory cannot be inspected, so per-skill
/// ownership isolation fails closed instead of trusting an unowned path.
#[cfg(unix)]
fn directory_owner_uid(path: &std::path::Path) -> Option<u32> {
    std::fs::metadata(path)
        .ok()
        .filter(std::fs::Metadata::is_dir)
        .map(|metadata| {
            use std::os::unix::fs::MetadataExt as _;
            metadata.uid()
        })
}

#[cfg(not(unix))]
fn directory_owner_uid(_path: &std::path::Path) -> Option<u32> {
    None
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
        SkillSecError::PermissionDenied | SkillSecError::ScopeDenied(_) => "PermissionDenied",
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
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
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
        fn manages(&self, identity: &crate::SkillIdentity) -> bool {
            identity == &self.0.identity
        }
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
        let (_temporary, mut service, physical) = uninitialized_fixture();
        Arc::get_mut(&mut service)
            .unwrap()
            .config
            .managed_skill_dirs
            .clear();
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
                    state_dir: directory.path().canonicalize().unwrap(),
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

    /// Fixture uids for ownership-isolation tests.
    ///
    /// Under a root test container the fixture's real owner is root, and an
    /// owner derived from metadata would silently exercise the root exemption
    /// instead of the ordinary owner's success path. Re-own the fixture to a
    /// non-root uid when the process can; non-root development machines keep
    /// the real owner and a foreign neighbor uid.
    #[cfg(unix)]
    fn isolation_uids(io_dir: &std::path::Path) -> (u32, u32) {
        if std::os::unix::fs::chown(io_dir, Some(1001), None).is_ok() {
            (1001, 1002)
        } else {
            let owner = std::fs::metadata(io_dir).unwrap().uid();
            (owner, owner.wrapping_add(1))
        }
    }

    /// A managed `parent/<scope>` tree with initialized keys and separate state.
    #[cfg(unix)]
    fn skill_tree(
        scope: &str,
    ) -> (
        tempfile::TempDir,
        tempfile::TempDir,
        Arc<SkillSecService>,
        std::path::PathBuf,
    ) {
        let tree = tempfile::tempdir().unwrap();
        let parent = tree.path().canonicalize().unwrap();
        let state = tempfile::tempdir().unwrap();
        let state_dir = state.path().canonicalize().unwrap();
        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let service = Arc::new(
            SkillSecService::new(
                crate::SkillSecConfig {
                    state_dir: state_dir.clone(),
                    managed_skill_dirs: vec![
                        crate::ManagedSkillDir::new(parent.join(scope)).unwrap(),
                    ],
                },
                crate::scanner::ScannerRegistry::default(),
            )
            .unwrap(),
        );
        service.initialize().unwrap();
        (tree, state, service, parent)
    }

    #[cfg(unix)]
    fn write_skill(directory: &std::path::Path, name: &str) {
        std::fs::create_dir(directory).unwrap();
        std::fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {name}\n---\nSafe"),
        )
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ownership_isolation_restricts_direct_skills_to_their_owner() {
        let (_temporary, service, root) = fixture();
        let (owner, foreign) = isolation_uids(&root.io_dir);
        let command = || SkillSecCommand::Certify {
            skill_dir: root.identity.clone(),
            scanner: "fixture".into(),
            scanner_version: None,
            findings: json!([]),
        };
        let control = ExecutionControl {
            deadline: deadline(),
            cancelled: false,
        };
        let isolated = SkillSecExecutor::new(service.clone()).with_require_skill_ownership(true);

        // The owner may still operate their own Skill.
        let owner_call = isolated.execute(
            &control,
            &SkillSecRequest {
                command: command(),
                caller_uid: owner,
            },
        );
        assert!(owner_call.success, "{owner_call:?}");

        // A foreign non-root caller is refused before any business side effect.
        let denied = isolated.execute(
            &control,
            &SkillSecRequest {
                command: command(),
                caller_uid: foreign,
            },
        );
        assert_eq!(denied.error_type, "PermissionDenied", "{denied:?}");

        // Root stays unrestricted.
        let root_call = isolated.execute(
            &control,
            &SkillSecRequest {
                command: command(),
                caller_uid: 0,
            },
        );
        assert!(root_call.success, "{root_call:?}");

        // With isolation off, the documented phase-one contract is unchanged:
        // every local caller may operate every managed Skill.
        let phase_one = SkillSecExecutor::new(service.clone());
        let anyone = phase_one.execute(
            &control,
            &SkillSecRequest {
                command: command(),
                caller_uid: foreign,
            },
        );
        assert!(anyone.success, "{anyone:?}");
    }

    #[cfg(unix)]
    #[test]
    fn ownership_isolation_filters_discovery_to_owned_skills() {
        let (_temporary, service, root) = fixture();
        let (owner, foreign) = isolation_uids(&root.io_dir);
        let executor = SkillSecExecutor::new(service).with_require_skill_ownership(true);

        let included = executor.discover(owner, deadline()).unwrap();
        assert!(
            included.contains(&root.identity),
            "the owner's Skill must stay discoverable: {included:?}"
        );
        let excluded = executor.discover(foreign, deadline()).unwrap();
        assert!(
            !excluded.contains(&root.identity),
            "a foreign caller must not discover another user's Skill: {excluded:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn isolated_aggregates_process_owned_skills_and_skip_shared_system_skills() {
        let (_tree, _state, service, parent) = skill_tree("*");
        let own = parent.join("own-skill");
        let system = parent.join("system-skill");
        write_skill(&own, "own");
        write_skill(&system, "system");
        // The supported two-uid layout: a root-owned shared system Skill and a
        // caller-owned Skill under one managed parent.
        if std::os::unix::fs::chown(&system, Some(0), None).is_err()
            || std::os::unix::fs::chown(&own, Some(1001), None).is_err()
        {
            // Only a root test container can provision this layout.
            return;
        }
        let system_identity = crate::SkillIdentity::new(&system).unwrap();
        let own_identity = crate::SkillIdentity::new(&own).unwrap();
        let executor = SkillSecExecutor::new(service).with_require_skill_ownership(true);
        let control = ExecutionControl {
            deadline: deadline(),
            cancelled: false,
        };

        // Root establishes the baseline for both Skills.
        let baseline = executor.execute(
            &control,
            &SkillSecRequest {
                command: SkillSecCommand::Scan {
                    skill_dir: None,
                    all: true,
                    skill_dirs: Vec::new(),
                    force: false,
                    scanners: None,
                },
                caller_uid: 0,
            },
        );
        assert!(baseline.success, "{baseline:?}");
        assert_eq!(
            baseline.data["output"]["results"].as_array().map(Vec::len),
            Some(2)
        );

        // The caller's aggregate covers exactly their own Skill: the CLI sends
        // no client-side discovery, and the filtered daemon discovery decides
        // the batch, so a root-owned system Skill can no longer reject the
        // whole batch before the caller's Skills are processed.
        let aggregate = executor.execute(
            &control,
            &SkillSecRequest {
                command: SkillSecCommand::Check {
                    skill_dir: None,
                    all: true,
                    skill_dirs: Vec::new(),
                },
                caller_uid: 1001,
            },
        );
        assert!(aggregate.success, "{aggregate:?}");
        let results = aggregate.data["output"]["results"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(results.len(), 1, "{results:?}");
        assert_eq!(results[0]["canonicalSkillDir"], json!(own_identity));

        // Explicit mutation of the shared system Skill stays owner-only.
        let scan_denied = executor.execute(
            &control,
            &SkillSecRequest {
                command: SkillSecCommand::Scan {
                    skill_dir: Some(system_identity.clone()),
                    all: false,
                    skill_dirs: Vec::new(),
                    force: false,
                    scanners: None,
                },
                caller_uid: 1001,
            },
        );
        assert_eq!(
            scan_denied.error_type, "PermissionDenied",
            "{scan_denied:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn shared_system_skills_stay_queryable_but_not_operable() {
        let (_tree, _state, service, parent) = skill_tree("*");
        let system = parent.join("system-skill");
        let foreign = parent.join("foreign-skill");
        write_skill(&system, "system");
        write_skill(&foreign, "foreign");
        if std::os::unix::fs::chown(&system, Some(0), None).is_err()
            || std::os::unix::fs::chown(&foreign, Some(1002), None).is_err()
        {
            // Only a root test container can provision the two-uid layout.
            return;
        }
        let system_identity = crate::SkillIdentity::new(&system).unwrap();
        let foreign_identity = crate::SkillIdentity::new(&foreign).unwrap();
        let executor = SkillSecExecutor::new(service).with_require_skill_ownership(true);
        let control = ExecutionControl {
            deadline: deadline(),
            cancelled: false,
        };

        // Root scans the shared system Skill so consumers get a real verdict.
        let baseline = executor.execute(
            &control,
            &SkillSecRequest {
                command: SkillSecCommand::Scan {
                    skill_dir: Some(system_identity.clone()),
                    all: false,
                    skill_dirs: Vec::new(),
                    force: false,
                    scanners: None,
                },
                caller_uid: 0,
            },
        );
        assert!(baseline.success, "{baseline:?}");

        // A consumer may query the shared Skill's summary — the Codex hook
        // needs a valid verdict instead of a PermissionDenied that would make
        // it fail open.
        let checked = executor.execute(
            &control,
            &SkillSecRequest {
                command: SkillSecCommand::Check {
                    skill_dir: Some(system_identity.clone()),
                    all: false,
                    skill_dirs: Vec::new(),
                },
                caller_uid: 1001,
            },
        );
        assert!(checked.success, "{checked:?}");
        assert_eq!(
            checked.data["output"]["status"],
            json!("pass"),
            "{checked:?}"
        );
        let shown = executor.execute(
            &control,
            &SkillSecRequest {
                command: SkillSecCommand::Show {
                    skill_dir: system_identity.clone(),
                },
                caller_uid: 1001,
            },
        );
        assert!(shown.success, "{shown:?}");
        assert_eq!(
            shown.data["output"]["latestStatus"],
            json!("pass"),
            "{shown:?}"
        );

        // Findings-bearing operations on the same Skill stay owner-only.
        for command in [
            SkillSecCommand::Analyze {
                skill_dir: system_identity.clone(),
            },
            SkillSecCommand::Audit {
                skill_dir: system_identity.clone(),
                verify_snapshots: false,
            },
            SkillSecCommand::Export {
                skill_dir: system_identity.clone(),
                version: "latest".into(),
                output: parent.join("export"),
            },
        ] {
            let denied = executor.execute(
                &control,
                &SkillSecRequest {
                    command,
                    caller_uid: 1001,
                },
            );
            assert_eq!(denied.error_type, "PermissionDenied", "{denied:?}");
        }

        // Another user's private Skill is not queryable at all.
        let denied = executor.execute(
            &control,
            &SkillSecRequest {
                command: SkillSecCommand::Check {
                    skill_dir: Some(foreign_identity),
                    all: false,
                    skill_dirs: Vec::new(),
                },
                caller_uid: 1001,
            },
        );
        assert_eq!(denied.error_type, "PermissionDenied", "{denied:?}");
    }

    #[cfg(unix)]
    #[test]
    fn isolated_discovery_prunes_foreign_subtrees_instead_of_walking_them() {
        use std::os::unix::ffi::OsStringExt as _;
        let (_tree, _state, service, parent) = skill_tree("**");
        let own = parent.join("own-skill");
        let foreign = parent.join("foreign-home");
        write_skill(&own, "own");
        std::fs::create_dir(&foreign).unwrap();
        // A non-UTF-8 entry that `Directory::names` rejects, planted in the
        // foreign user's own tree.
        std::fs::File::create(foreign.join(std::ffi::OsString::from_vec(vec![0xff]))).unwrap();
        if std::os::unix::fs::chown(&foreign, Some(1002), None).is_err()
            || std::os::unix::fs::chown(&own, Some(1001), None).is_err()
        {
            // Only a root test container can provision the two-uid layout.
            return;
        }
        let executor = SkillSecExecutor::new(service).with_require_skill_ownership(true);

        // The caller's aggregate is decided without walking the foreign tree,
        // so the planted entry cannot abort it.
        let discovered = executor.discover(1001, deadline()).unwrap();
        assert!(
            discovered.contains(&crate::SkillIdentity::new(&own).unwrap()),
            "{discovered:?}"
        );
        assert!(
            !discovered
                .iter()
                .any(|identity| identity.path().starts_with(&foreign)),
            "{discovered:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn pinned_direct_refuses_a_replaced_directory() {
        let (_temporary, _service, root) = fixture();
        let pinned = SkillRoot::pinned_direct(&root.io_dir).unwrap();
        let moved = root.io_dir.with_file_name("moved-away");
        std::fs::rename(&root.io_dir, &moved).unwrap();
        std::fs::create_dir(&root.io_dir).unwrap();
        assert!(matches!(
            pinned.open_verified(),
            Err(SkillSecError::Integrity(_))
        ));
        // The moved original still matches a pin taken against itself.
        let re_pinned = SkillRoot::pinned_direct(&moved).unwrap();
        assert!(re_pinned.open_verified().is_ok());
        // Unpinned direct roots keep the historical resolve-then-open shape;
        // the pin is what refuses the replacement.
        assert!(
            SkillRoot::direct(&root.io_dir)
                .unwrap()
                .open_verified()
                .is_ok()
        );
    }

    #[cfg(unix)]
    #[test]
    fn authorization_binds_to_the_resolved_directory() {
        struct SwappingEnvironment {
            parked: std::path::PathBuf,
        }
        impl SkillEnvironment for SwappingEnvironment {
            fn resolve(
                &self,
                identity: &crate::SkillIdentity,
                deadline: Instant,
            ) -> Result<SkillRoot, SkillSecError> {
                check_deadline(deadline)?;
                let root = SkillRoot::pinned_direct(identity.path())?;
                // A racing caller with parent rename rights substitutes a
                // fresh directory after the pin, before the privileged open.
                std::fs::rename(identity.path(), &self.parked).unwrap();
                std::fs::create_dir(identity.path()).unwrap();
                Ok(root)
            }
        }
        let (_temporary, service, root) = fixture();
        let (owner, _) = isolation_uids(&root.io_dir);
        let parked = root.io_dir.with_file_name("parked");
        let executor = SkillSecExecutor::new(service)
            .with_environment(Arc::new(SwappingEnvironment {
                parked: parked.clone(),
            }))
            .with_require_skill_ownership(true);
        let outcome = executor.execute(
            &ExecutionControl {
                deadline: deadline(),
                cancelled: false,
            },
            &SkillSecRequest {
                command: SkillSecCommand::Certify {
                    skill_dir: root.identity.clone(),
                    scanner: "fixture".into(),
                    scanner_version: None,
                    findings: json!([]),
                },
                caller_uid: owner,
            },
        );
        // The substitution is refused on the pinned object, and the parked
        // original never receives any ledger metadata.
        assert_eq!(outcome.error_type, "PermissionDenied", "{outcome:?}");
        assert!(!parked.join(".skill-meta").exists());
    }

    #[test]
    fn ownership_isolation_honors_authenticated_mount_owners() {
        struct OwnedEnvironment(SkillRoot);
        impl SkillEnvironment for OwnedEnvironment {
            fn manages(&self, identity: &crate::SkillIdentity) -> bool {
                identity == &self.0.identity
            }
            fn owner_uid(&self, identity: &crate::SkillIdentity) -> Option<u32> {
                self.manages(identity).then_some(1001)
            }
            fn resolve(
                &self,
                identity: &crate::SkillIdentity,
                deadline: Instant,
            ) -> Result<SkillRoot, SkillSecError> {
                check_deadline(deadline)?;
                assert_eq!(identity, &self.0.identity);
                Ok(self.0.clone())
            }
        }
        let (_temporary, mut service, physical) = uninitialized_fixture();
        Arc::get_mut(&mut service)
            .unwrap()
            .config
            .managed_skill_dirs
            .clear();
        let identity =
            crate::SkillIdentity::new(physical.io_dir.with_file_name("source-alias")).unwrap();
        let root = SkillRoot::resolved(identity.clone(), physical.io_dir).unwrap();
        // The mount is bound to uid 1001; the backing directory happens to
        // belong to the test runner, so the authenticated binding — not the
        // filesystem owner — must decide.
        let executor = SkillSecExecutor::new(service)
            .with_environment(Arc::new(OwnedEnvironment(root)))
            .with_require_skill_ownership(true);
        let control = ExecutionControl {
            deadline: deadline(),
            cancelled: false,
        };
        let command = SkillSecCommand::Certify {
            skill_dir: identity,
            scanner: "custom".into(),
            scanner_version: None,
            findings: json!([]),
        };

        let owner = executor.execute(
            &control,
            &SkillSecRequest {
                command: command.clone(),
                caller_uid: 1001,
            },
        );
        assert!(owner.success, "{owner:?}");

        let foreign = executor.execute(
            &control,
            &SkillSecRequest {
                command,
                caller_uid: 1002,
            },
        );
        assert_eq!(foreign.error_type, "PermissionDenied", "{foreign:?}");
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
