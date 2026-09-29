//! Service-unit lifecycle executor.
//!
//! Alpha contract: ANOLISA needs to start owned service units after a
//! component installs, stop them before disable / uninstall, and surface
//! systemctl status on `anolisa restart`. This module wraps the minimum
//! amount of `systemctl` we need without a dbus dependency.
//!
//! The trait surface is small on purpose. A non-Linux host or a detected
//! container runtime gets a [`NotSupportedServiceManager`] whose ops
//! succeed with `state: NotSupported, supported: false, changed: false`;
//! callers treat that as a quiet skip rather than a warning. Install mode
//! selects the *scope* instead of disabling services outright: system mode
//! drives system units via `systemctl`, user mode drives the caller's user
//! units via `systemctl --user`. A manager only acts on requests of its
//! own scope (see [`ServiceManager::handles_scope`]); a request for the
//! other scope is a documented skip.
//!
//! `FakeServiceManager` is the executor used by `service.rs`'s own unit
//! tests and by integration tests that need to assert which ops the
//! orchestrators dispatched.

use std::collections::HashSet;
use std::sync::Mutex;

use anolisa_env::EnvFacts;
use anolisa_platform::command::{CommandOutput, CommandRunner, InheritedLocaleCommandRunner};

use crate::manifest::ServiceScope;

/// One operation issued against a service manager. Used both to drive
/// systemctl and to record what a [`FakeServiceManager`] saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServiceOp {
    /// Query service state.
    Probe,
    /// Start a stopped service.
    Start,
    /// Stop a running service.
    Stop,
    /// Restart a service.
    Restart,
    /// Enable service startup.
    Enable,
    /// Disable service startup.
    Disable,
    /// Reload the manager's unit database (`systemctl daemon-reload`).
    /// Not tied to a specific unit; run once after new unit files land so
    /// they become loadable.
    DaemonReload,
}

impl ServiceOp {
    /// Wire label, matched by `systemctl <op> <unit>` and audit logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Probe => "probe",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::DaemonReload => "daemon-reload",
        }
    }
}

/// Coarse runtime state of a service unit. Mirrors the subset of
/// `systemctl is-active` answers callers can act on, plus
/// `NotSupported` for hosts where ANOLISA refuses to drive systemctl.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    /// Unit is active.
    Active,
    /// Unit is inactive.
    Inactive,
    /// Unit is failed.
    Failed,
    /// Unit is starting.
    Activating,
    /// Unit is stopping.
    Deactivating,
    /// Unit is not installed or not known to the manager.
    NotInstalled,
    /// Host/mode deliberately does not support this service manager.
    NotSupported,
    /// Manager returned a state outside the modeled vocabulary.
    Unknown,
}

impl ServiceState {
    /// Stable lowercase wire label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Inactive => "inactive",
            Self::Failed => "failed",
            Self::Activating => "activating",
            Self::Deactivating => "deactivating",
            Self::NotInstalled => "not_installed",
            Self::NotSupported => "not_supported",
            Self::Unknown => "unknown",
        }
    }
}

/// What [`ServiceManager`] returns from a single op.
#[derive(Debug, Clone)]
pub struct ServiceOutcome {
    /// Backend label, e.g. `"systemd"`, `"not-supported"`, or a custom
    /// fake-manager name.
    pub manager: String,
    /// Service unit name (e.g. `"agentsight.service"`).
    pub unit: String,
    /// Op the caller requested.
    pub op: ServiceOp,
    /// State of the unit after the op.
    pub state: ServiceState,
    /// `false` when the backend is a deliberate skip (e.g. user mode).
    pub supported: bool,
    /// `true` when the op caused a state change. Always `false` for
    /// probe and for unsupported backends.
    pub changed: bool,
    /// Human-readable description, used for logs / warnings.
    pub message: String,
}

/// Failure surface for a single service op. Errors are non-fatal at
/// the lifecycle layer — orchestrators surface them as warnings.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// `systemctl` could not be spawned.
    #[error("spawning systemctl failed: {source}")]
    Spawn {
        /// Underlying spawn error.
        #[source]
        source: std::io::Error,
    },
    /// Native manager command returned a non-zero exit code.
    #[error("systemctl {op} {unit} exited with status {code}: {stderr}")]
    NonZeroExit {
        /// Operation attempted.
        op: String,
        /// Unit targeted by the operation.
        unit: String,
        /// Process exit code.
        code: i32,
        /// Captured stderr.
        stderr: String,
    },
}

/// Backend trait that every service-manager impl satisfies.
pub trait ServiceManager: Send + Sync {
    /// Wire label written into `ServiceRef.manager` and log records.
    fn manager(&self) -> &str;
    /// `false` when this backend is a deliberate skip — orchestrators
    /// short-circuit so they don't emit per-unit warnings.
    fn supported(&self) -> bool;
    /// Reason text when `supported() == false`. `None` for active backends.
    fn unsupported_reason(&self) -> Option<&str> {
        None
    }

    /// Whether this manager drives units of `scope`. A systemd backend is
    /// bound to a single scope by install mode — `systemctl` for
    /// [`ServiceScope::System`], `systemctl --user` for
    /// [`ServiceScope::User`] — and orchestrators skip any request whose
    /// scope it does not handle. Default `true` for scope-agnostic
    /// backends (the not-supported skip and the test fake's default).
    fn handles_scope(&self, _scope: ServiceScope) -> bool {
        true
    }

    /// Reload the manager's unit database so a freshly-installed unit file
    /// becomes loadable (`systemctl daemon-reload`). Default is a no-op
    /// for backends that don't drive systemd; override only where a real
    /// reload applies. Not tied to a unit, so the returned outcome's
    /// `unit` is empty.
    fn daemon_reload(&self) -> Result<ServiceOutcome, ServiceError> {
        Ok(ServiceOutcome {
            manager: self.manager().to_string(),
            unit: String::new(),
            op: ServiceOp::DaemonReload,
            state: ServiceState::NotSupported,
            supported: false,
            changed: false,
            message: "daemon-reload skipped: manager does not drive systemd".to_string(),
        })
    }

    /// Probe the service without attempting mutation.
    fn probe_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError>;
    /// Start the service.
    fn start_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError>;
    /// Stop the service.
    fn stop_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError>;
    /// Restart the service.
    fn restart_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError>;
    /// Enable startup for the service.
    fn enable_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError>;
    /// Disable startup for the service.
    fn disable_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError>;
}

/// Pick the **system-scope** backend for the current host + install mode.
/// Linux + system mode + no container → real `systemctl` driver;
/// everything else (non-Linux, user mode, container) → quiet skip. This is
/// the manager used to enable/stop system units and to probe systemd
/// health checks, so user mode is deliberately unsupported here — a
/// user-mode install cannot manage system units. User-scope activation has
/// its own factory, [`user_service_for_install_mode`].
pub fn for_install_mode(install_mode: &str, env: &EnvFacts) -> Box<dyn ServiceManager> {
    if env.os != "linux" {
        return Box::new(NotSupportedServiceManager::new(format!(
            "service manager unsupported on os '{}'",
            env.os,
        )));
    }
    if install_mode != "system" {
        return Box::new(NotSupportedServiceManager::new(
            "service manager unsupported in install_mode='user' (alpha)".to_string(),
        ));
    }
    if let Some(rt) = env.container.as_deref() {
        return Box::new(NotSupportedServiceManager::new(format!(
            "container runtime '{rt}' detected — refusing to drive systemctl from inside a container",
        )));
    }
    Box::new(SystemdServiceManager::new())
}

/// Pick the **user-scope** backend (`systemctl --user`) for activating user
/// units. Unlike [`for_install_mode`], user units are auto-activated only in
/// a **user**-mode install: that install runs as the owning user with their
/// session bus, so `systemctl --user enable/start` targets the right
/// manager. A system-mode install *places* a user unit (under
/// `{userunitdir}`) for all users but leaves activation to the user — root
/// has no single target user session — so it is a quiet skip here.
///
/// Linux + user mode + no container → user-scope `systemctl --user` driver;
/// everything else → quiet skip.
pub fn user_service_for_install_mode(
    install_mode: &str,
    env: &EnvFacts,
) -> Box<dyn ServiceManager> {
    if env.os != "linux" {
        return Box::new(NotSupportedServiceManager::new(format!(
            "user-scope service manager unsupported on os '{}'",
            env.os,
        )));
    }
    if install_mode != "user" {
        return Box::new(NotSupportedServiceManager::new(format!(
            "user-scope service not auto-activated in install_mode='{install_mode}' — \
             unit is placed; enable per-user with `systemctl --user enable`",
        )));
    }
    if let Some(rt) = env.container.as_deref() {
        return Box::new(NotSupportedServiceManager::new(format!(
            "container runtime '{rt}' detected — refusing to drive systemctl --user from inside a container",
        )));
    }
    Box::new(SystemdServiceManager::with_scope(ServiceScope::User))
}

/// Quiet-skip backend. Every op succeeds with `state: NotSupported`.
pub struct NotSupportedServiceManager {
    reason: String,
}

impl NotSupportedServiceManager {
    /// Build a quiet-skip backend with a stable reason string.
    pub fn new(reason: String) -> Self {
        Self { reason }
    }
    fn outcome(&self, unit: &str, op: ServiceOp) -> ServiceOutcome {
        ServiceOutcome {
            manager: "not-supported".to_string(),
            unit: unit.to_string(),
            op,
            state: ServiceState::NotSupported,
            supported: false,
            changed: false,
            message: self.reason.clone(),
        }
    }
}

impl ServiceManager for NotSupportedServiceManager {
    fn manager(&self) -> &str {
        "not-supported"
    }
    fn supported(&self) -> bool {
        false
    }
    fn unsupported_reason(&self) -> Option<&str> {
        Some(&self.reason)
    }
    fn probe_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
        Ok(self.outcome(unit, ServiceOp::Probe))
    }
    fn start_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
        Ok(self.outcome(unit, ServiceOp::Start))
    }
    fn stop_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
        Ok(self.outcome(unit, ServiceOp::Stop))
    }
    fn restart_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
        Ok(self.outcome(unit, ServiceOp::Restart))
    }
    fn enable_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
        Ok(self.outcome(unit, ServiceOp::Enable))
    }
    fn disable_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
        Ok(self.outcome(unit, ServiceOp::Disable))
    }
}

/// Real `systemctl` backend. Resolves the binary off `PATH`, and
/// returns spawn errors as `ServiceError::Spawn` so the caller can
/// downgrade them to warnings.
pub struct SystemdServiceManager<R = InheritedLocaleCommandRunner> {
    runner: R,
    /// System vs user manager. A user-scoped instance prefixes every
    /// invocation with `--user`, so ops target the caller's `systemd
    /// --user` instance instead of the system manager. Set from install
    /// mode by [`for_install_mode`].
    scope: ServiceScope,
}

impl SystemdServiceManager<InheritedLocaleCommandRunner> {
    /// Build a **system**-scope manager that invokes `systemctl` from
    /// `PATH`.
    pub fn new() -> Self {
        Self::with_scope(ServiceScope::System)
    }

    /// Build a manager bound to `scope`. A [`ServiceScope::User`] manager
    /// prefixes every `systemctl` call with `--user`.
    pub fn with_scope(scope: ServiceScope) -> Self {
        Self::with_runner(scope, InheritedLocaleCommandRunner)
    }
}

impl<R: CommandRunner> SystemdServiceManager<R> {
    /// Build a scope-bound backend with injectable process execution.
    pub fn with_runner(scope: ServiceScope, runner: R) -> Self {
        Self { runner, scope }
    }

    fn run(&self, args: &[&str]) -> Result<CommandOutput, ServiceError> {
        let mut scoped_args = Vec::with_capacity(args.len() + 1);
        if self.scope == ServiceScope::User {
            scoped_args.push("--user");
        }
        scoped_args.extend_from_slice(args);
        self.runner
            .run("systemctl", &scoped_args)
            .map_err(|source| ServiceError::Spawn { source })
    }

    fn probe_state(&self, unit: &str) -> Result<ServiceState, ServiceError> {
        let output = self.run(&["is-active", unit])?;
        let stdout = output.stdout.trim().to_lowercase();
        // is-active also uses nonzero exits for domain states. A failed
        // query with empty output must not masquerade as a missing unit.
        let observed_state = matches!(output.code, Some(3 | 4))
            && output.stderr.trim().is_empty()
            && matches!(
                stdout.as_str(),
                "active"
                    | "reloading"
                    | "activating"
                    | "deactivating"
                    | "inactive"
                    | "failed"
                    | "unknown"
                    | "maintenance"
            );
        if output.code != Some(0) && !observed_state {
            return Err(ServiceError::NonZeroExit {
                op: "is-active".to_string(),
                unit: unit.to_string(),
                code: output.code.unwrap_or(-1),
                stderr: output.stderr.trim().to_string(),
            });
        }
        Ok(match stdout.as_str() {
            "active" => ServiceState::Active,
            "reloading" | "activating" => ServiceState::Activating,
            "deactivating" => ServiceState::Deactivating,
            "inactive" => ServiceState::Inactive,
            "failed" => ServiceState::Failed,
            "unknown" | "" => ServiceState::NotInstalled,
            _ => ServiceState::Unknown,
        })
    }
}

impl<R: CommandRunner + Send + Sync> SystemdServiceManager<R> {
    fn run_op(&self, op: ServiceOp, unit: &str) -> Result<ServiceOutcome, ServiceError> {
        let prior = self.probe_state(unit)?;
        if matches!(op, ServiceOp::Probe) {
            return Ok(ServiceOutcome {
                manager: self.manager().to_string(),
                unit: unit.to_string(),
                op,
                state: prior,
                supported: true,
                changed: false,
                message: format!("systemctl is-active reported {}", prior.as_str()),
            });
        }
        let output = self.run(&[op.as_str(), unit])?;
        if output.code != Some(0) {
            let code = output.code.unwrap_or(-1);
            let stderr = output.stderr.trim().to_string();
            return Err(ServiceError::NonZeroExit {
                op: op.as_str().to_string(),
                unit: unit.to_string(),
                code,
                stderr,
            });
        }
        let post = self.probe_state(unit)?;
        let changed = match op {
            ServiceOp::Start => prior != ServiceState::Active && post == ServiceState::Active,
            ServiceOp::Stop => prior == ServiceState::Active && post != ServiceState::Active,
            ServiceOp::Restart => true,
            ServiceOp::Enable | ServiceOp::Disable => true,
            ServiceOp::Probe | ServiceOp::DaemonReload => false,
        };
        Ok(ServiceOutcome {
            manager: self.manager().to_string(),
            unit: unit.to_string(),
            op,
            state: post,
            supported: true,
            changed,
            message: format!(
                "systemctl {} {} ok (state={})",
                op.as_str(),
                unit,
                post.as_str()
            ),
        })
    }
}

impl Default for SystemdServiceManager<InheritedLocaleCommandRunner> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: CommandRunner + Send + Sync> ServiceManager for SystemdServiceManager<R> {
    fn manager(&self) -> &str {
        // Report the scope in the label (a user-scoped instance drives
        // `systemctl --user`, a distinct namespace). Shared with the install
        // state writers via `ServiceScope::manager_label` so the running
        // manager and the persisted `ServiceRef.manager` never diverge.
        self.scope.manager_label()
    }
    fn supported(&self) -> bool {
        true
    }
    fn handles_scope(&self, scope: ServiceScope) -> bool {
        self.scope == scope
    }
    fn daemon_reload(&self) -> Result<ServiceOutcome, ServiceError> {
        let output = self.run(&["daemon-reload"])?;
        if output.code != Some(0) {
            let code = output.code.unwrap_or(-1);
            let stderr = output.stderr.trim().to_string();
            return Err(ServiceError::NonZeroExit {
                op: "daemon-reload".to_string(),
                unit: String::new(),
                code,
                stderr,
            });
        }
        Ok(ServiceOutcome {
            manager: self.manager().to_string(),
            unit: String::new(),
            op: ServiceOp::DaemonReload,
            // daemon-reload doesn't target a unit, so there is no post-state.
            state: ServiceState::Unknown,
            supported: true,
            changed: true,
            message: "systemctl daemon-reload ok".to_string(),
        })
    }
    fn probe_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
        self.run_op(ServiceOp::Probe, unit)
    }
    fn start_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
        self.run_op(ServiceOp::Start, unit)
    }
    fn stop_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
        self.run_op(ServiceOp::Stop, unit)
    }
    fn restart_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
        self.run_op(ServiceOp::Restart, unit)
    }
    fn enable_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
        self.run_op(ServiceOp::Enable, unit)
    }
    fn disable_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
        self.run_op(ServiceOp::Disable, unit)
    }
}

/// Test executor. Records every (op, unit) call and lets tests assert
/// which units the orchestrators tried to drive without actually
/// invoking systemctl.
pub struct FakeServiceManager {
    manager_name: String,
    supported: bool,
    /// Scope this fake claims to drive (see [`ServiceManager::handles_scope`]).
    /// Defaults to [`ServiceScope::System`] so existing system-mode tests
    /// drive their requests and user-scope requests are skipped.
    scope: ServiceScope,
    state: Mutex<ServiceState>,
    calls: Mutex<Vec<(ServiceOp, String)>>,
    fail_ops: Mutex<HashSet<(ServiceOp, String)>>,
}

impl FakeServiceManager {
    /// Build a supported, **system**-scope fake manager with an initially
    /// inactive unit state and no injected failures.
    pub fn new() -> Self {
        Self {
            manager_name: "fake".to_string(),
            supported: true,
            scope: ServiceScope::System,
            state: Mutex::new(ServiceState::Inactive),
            calls: Mutex::new(Vec::new()),
            fail_ops: Mutex::new(HashSet::new()),
        }
    }
    /// Build a fake bound to `scope`, for exercising user-scope routing.
    pub fn with_scope(scope: ServiceScope) -> Self {
        Self {
            scope,
            ..Self::new()
        }
    }
    /// Snapshot of every call recorded so far, in dispatch order.
    pub fn calls(&self) -> Vec<(ServiceOp, String)> {
        self.calls.lock().expect("poisoned").clone()
    }
    /// Override the unit's reported state before the next op. Useful
    /// for testing reactivation / probe semantics.
    pub fn set_state(&self, state: ServiceState) {
        *self.state.lock().expect("poisoned") = state;
    }
    /// Cause a specific (op, unit) pair to return `NonZeroExit` so
    /// tests can assert orchestrator warning paths.
    pub fn fail(&self, op: ServiceOp, unit: &str) {
        self.fail_ops
            .lock()
            .expect("poisoned")
            .insert((op, unit.to_string()));
    }
    fn record(&self, op: ServiceOp, unit: &str) -> Result<ServiceOutcome, ServiceError> {
        self.calls
            .lock()
            .expect("poisoned")
            .push((op, unit.to_string()));
        if self
            .fail_ops
            .lock()
            .expect("poisoned")
            .contains(&(op, unit.to_string()))
        {
            return Err(ServiceError::NonZeroExit {
                op: op.as_str().to_string(),
                unit: unit.to_string(),
                code: 1,
                stderr: "fake forced failure".to_string(),
            });
        }
        let prior = *self.state.lock().expect("poisoned");
        let next = match op {
            ServiceOp::Start | ServiceOp::Restart => ServiceState::Active,
            ServiceOp::Stop => ServiceState::Inactive,
            _ => prior,
        };
        *self.state.lock().expect("poisoned") = next;
        Ok(ServiceOutcome {
            manager: self.manager_name.clone(),
            unit: unit.to_string(),
            op,
            state: next,
            supported: self.supported,
            changed: prior != next,
            message: format!("fake {} {} ok", op.as_str(), unit),
        })
    }
}

impl Default for FakeServiceManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceManager for FakeServiceManager {
    fn manager(&self) -> &str {
        &self.manager_name
    }
    fn supported(&self) -> bool {
        self.supported
    }
    fn handles_scope(&self, scope: ServiceScope) -> bool {
        self.scope == scope
    }
    fn daemon_reload(&self) -> Result<ServiceOutcome, ServiceError> {
        self.record(ServiceOp::DaemonReload, "")
    }
    fn probe_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
        self.record(ServiceOp::Probe, unit)
    }
    fn start_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
        self.record(ServiceOp::Start, unit)
    }
    fn stop_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
        self.record(ServiceOp::Stop, unit)
    }
    fn restart_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
        self.record(ServiceOp::Restart, unit)
    }
    fn enable_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
        self.record(ServiceOp::Enable, unit)
    }
    fn disable_service(&self, unit: &str) -> Result<ServiceOutcome, ServiceError> {
        self.record(ServiceOp::Disable, unit)
    }
}

/// Append a [`crate::central_log::LogKind::Component`] central-log record describing one
/// service op (start / stop / restart / probe / enable / disable). Pairs
/// each `manager.{start,stop}_service` call with an audit line so
/// `anolisa logs --op-id <id>` can reconstruct what happened to the
/// owned units, not just the surrounding verb. Logs are best-effort:
/// failures are swallowed because the parent verb has already committed.
// Service audit records intentionally spell out operation, actor, mode,
// and unit fields so lifecycle call sites show the full audit context.
#[allow(clippy::too_many_arguments)]
pub fn record_service_op(
    log: Option<&crate::central_log::CentralLog>,
    op: ServiceOp,
    component: &str,
    unit: &str,
    operation_id: &str,
    actor: &str,
    install_mode: &str,
    error: Option<&str>,
) {
    let Some(log) = log else {
        return;
    };
    use crate::central_log::{LogKind, LogRecord, Severity};
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let (severity, message) = match error {
        None => (
            Severity::Info,
            format!("service {} ok for {}/{}", op.as_str(), component, unit),
        ),
        Some(err) => (
            Severity::Warn,
            format!(
                "service {} skipped for {}/{}: {}",
                op.as_str(),
                component,
                unit,
                err
            ),
        ),
    };
    let _ = log.append(&LogRecord {
        kind: LogKind::Component,
        operation_id: Some(operation_id.to_string()),
        command: format!("service:{}", op.as_str()),
        source: "anolisa-core".to_string(),
        component: Some(component.to_string()),
        severity,
        message,
        actor: actor.to_string(),
        install_mode: Some(install_mode.to_string()),
        started_at: now.clone(),
        finished_at: Some(now),
        status: None,
        objects: vec![component.to_string()],
        backup_ids: Vec::new(),
        warnings: Vec::new(),
        details: serde_json::json!({"unit": unit}),
    });
}

/// Append a [`crate::central_log::LogKind::Component`] record describing a service op that
/// was *skipped* because the resolved [`ServiceManager`] reports
/// `supported() == false` (typical on macOS dev machines, `--install-mode
/// user`, or an unsupported container runtime). Without this audit line
/// the verb appears to have silently never touched any units, which
/// makes it hard to tell "no services declared" from "services declared
/// but no manager available".
///
/// Severity is `Info` because this is the expected, advertised behavior
/// of the unsupported manager — operators reading logs should see it as
/// a documented skip rather than a fault. The `details` payload carries
/// `supported = false` plus the manager-supplied `unsupported_reason`
/// (when available) so machine consumers can disambiguate platforms.
// Unsupported-service audit lines need the same context plus manager details;
// keeping them explicit avoids a throwaway builder with weaker invariants.
#[allow(clippy::too_many_arguments)]
pub fn record_service_op_unsupported(
    log: Option<&crate::central_log::CentralLog>,
    op: ServiceOp,
    component: &str,
    unit: &str,
    operation_id: &str,
    actor: &str,
    install_mode: &str,
    manager_name: &str,
    unsupported_reason: Option<&str>,
) {
    let Some(log) = log else {
        return;
    };
    use crate::central_log::{LogKind, LogRecord, Severity};
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let reason_str = unsupported_reason.unwrap_or("manager not supported on this platform");
    let message = format!(
        "service {} skipped for {}/{}: {} ({})",
        op.as_str(),
        component,
        unit,
        manager_name,
        reason_str,
    );
    let _ = log.append(&LogRecord {
        kind: LogKind::Component,
        operation_id: Some(operation_id.to_string()),
        command: format!("service:{}", op.as_str()),
        source: "anolisa-core".to_string(),
        component: Some(component.to_string()),
        severity: Severity::Info,
        message,
        actor: actor.to_string(),
        install_mode: Some(install_mode.to_string()),
        started_at: now.clone(),
        finished_at: Some(now),
        status: None,
        objects: vec![component.to_string()],
        backup_ids: Vec::new(),
        warnings: Vec::new(),
        details: serde_json::json!({
            "unit": unit,
            "supported": false,
            "manager": manager_name,
            "reason": reason_str,
        }),
    });
}

/// One resolved service activation. `unit` is the effective unit name
/// (template instance already substituted by the caller); `scope`,
/// `enable`, and `start` are carried verbatim from the component's
/// `[[component.services]]` contract.
#[derive(Debug, Clone)]
pub struct ServiceRequest {
    /// Effective systemd unit name (e.g. `agentsight.service` or
    /// `anolisa-memory@alice.service`).
    pub unit: String,
    /// `system` drives `systemctl`; `user` is a documented skip for now.
    pub scope: ServiceScope,
    /// Enable the unit (persistent across boots) when true.
    pub enable: bool,
    /// Start the unit now when true. On upgrade this becomes a restart —
    /// see [`ServiceActivation`].
    pub start: bool,
}

/// How [`apply_services`] brings a unit up: a fresh install starts it; an
/// upgrade restarts it so the replaced binary is reloaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceActivation {
    /// Fresh install — `systemctl start`.
    Start,
    /// Upgrade — `systemctl restart` to pick up the new binary.
    Restart,
}

/// Aggregate result of [`apply_services`]. Activation is best-effort, so
/// there is no abort field — every request is attempted and failures are
/// collected as warnings.
#[derive(Debug, Default)]
pub struct ServiceRunOutcome {
    /// Units successfully enabled — used to backfill `ServiceRef.enabled`.
    pub enabled_units: Vec<String>,
    /// Units successfully started or restarted.
    pub started_units: Vec<String>,
    /// Per-op warnings from tolerated (best-effort) failures.
    pub warnings: Vec<String>,
}

/// Enable/start (or restart) each requested unit, recording one audit line
/// per attempted op. Activation is **best-effort**: a failed enable or
/// start degrades to a warning and the run continues — service failures
/// never abort or roll back the install (a component's files are still
/// usable, and operators can fix the unit out of band).
///
/// Skips, in priority order, each producing a documented `Info` audit line
/// and no warning:
/// - `!manager.supported()`: non-Linux or container host.
/// - `!manager.handles_scope(req.scope)`: the request's scope is not the
///   one this install mode drives (e.g. a user-scope unit in a system
///   install) — the unit is placed but not activated here.
#[allow(clippy::too_many_arguments)]
pub fn apply_services(
    manager: &dyn ServiceManager,
    requests: &[ServiceRequest],
    activation: ServiceActivation,
    log: Option<&crate::central_log::CentralLog>,
    component: &str,
    operation_id: &str,
    actor: &str,
    install_mode: &str,
) -> ServiceRunOutcome {
    let mut outcome = ServiceRunOutcome::default();

    // Freshly-installed unit files aren't loadable until the manager reloads
    // its database, so a unit placed this run would otherwise fail `start`
    // with "not found". Reload once up-front when we'll actually drive
    // systemd: a supported backend with at least one request whose scope it
    // handles. A user-scope manager reloads via `systemctl --user`; requests
    // of the scope it does not handle are skipped in the loop below.
    if manager.supported()
        && requests
            .iter()
            .any(|req| manager.handles_scope(req.scope) && (req.enable || req.start))
    {
        match manager.daemon_reload() {
            Ok(_) => record_service_op(
                log,
                ServiceOp::DaemonReload,
                component,
                "",
                operation_id,
                actor,
                install_mode,
                None,
            ),
            Err(err) => {
                let msg = err.to_string();
                record_service_op(
                    log,
                    ServiceOp::DaemonReload,
                    component,
                    "",
                    operation_id,
                    actor,
                    install_mode,
                    Some(&msg),
                );
                outcome
                    .warnings
                    .push(format!("daemon-reload failed: {msg}"));
            }
        }
    }

    for req in requests {
        // Representative op for skip records: prefer the first op we would
        // have run so the audit line names a meaningful action.
        let primary_op = if req.enable {
            ServiceOp::Enable
        } else {
            ServiceOp::Start
        };

        if !manager.supported() {
            record_service_op_unsupported(
                log,
                primary_op,
                component,
                &req.unit,
                operation_id,
                actor,
                install_mode,
                manager.manager(),
                manager.unsupported_reason(),
            );
            continue;
        }

        // The manager is bound to one scope by install mode. A unit of the
        // other scope is placed but not activated here: a user-scope unit in
        // a system install is left for `systemctl --user enable`; a
        // system-scope unit in a user install needs a system-mode install.
        if !manager.handles_scope(req.scope) {
            let reason = match req.scope {
                ServiceScope::User => {
                    "user-scope service not activated in this install mode — \
                     unit placed; enable per-user with `systemctl --user enable`"
                }
                ServiceScope::System => {
                    "system-scope service not activated in user install mode — \
                     needs a system-mode install"
                }
            };
            record_service_op_unsupported(
                log,
                primary_op,
                component,
                &req.unit,
                operation_id,
                actor,
                install_mode,
                manager.manager(),
                Some(reason),
            );
            continue;
        }

        if req.enable {
            match manager.enable_service(&req.unit) {
                Ok(_) => {
                    record_service_op(
                        log,
                        ServiceOp::Enable,
                        component,
                        &req.unit,
                        operation_id,
                        actor,
                        install_mode,
                        None,
                    );
                    outcome.enabled_units.push(req.unit.clone());
                }
                Err(err) => {
                    let msg = err.to_string();
                    record_service_op(
                        log,
                        ServiceOp::Enable,
                        component,
                        &req.unit,
                        operation_id,
                        actor,
                        install_mode,
                        Some(&msg),
                    );
                    outcome
                        .warnings
                        .push(format!("enable {} failed: {msg}", req.unit));
                }
            }
        }

        if req.start {
            let op = match activation {
                ServiceActivation::Start => ServiceOp::Start,
                ServiceActivation::Restart => ServiceOp::Restart,
            };
            let result = match activation {
                ServiceActivation::Start => manager.start_service(&req.unit),
                ServiceActivation::Restart => manager.restart_service(&req.unit),
            };
            match result {
                Ok(_) => {
                    record_service_op(
                        log,
                        op,
                        component,
                        &req.unit,
                        operation_id,
                        actor,
                        install_mode,
                        None,
                    );
                    outcome.started_units.push(req.unit.clone());
                }
                Err(err) => {
                    let msg = err.to_string();
                    record_service_op(
                        log,
                        op,
                        component,
                        &req.unit,
                        operation_id,
                        actor,
                        install_mode,
                        Some(&msg),
                    );
                    outcome
                        .warnings
                        .push(format!("{} {} failed: {msg}", op.as_str(), req.unit));
                }
            }
        }
    }
    outcome
}

/// Aggregate result of [`deactivate_services`]. Like [`ServiceRunOutcome`]
/// for the install side, deactivation is best-effort: every unit is
/// attempted and failures are collected as warnings rather than aborting
/// the uninstall.
#[derive(Debug, Default)]
pub struct DeactivationOutcome {
    /// Units successfully stopped.
    pub stopped: Vec<String>,
    /// Units successfully disabled.
    pub disabled: Vec<String>,
    /// Per-op warnings from tolerated (best-effort) failures.
    pub warnings: Vec<String>,
}

/// Stop and disable each owned unit before its files are removed, recording
/// one audit line per attempted op. The uninstall-side mirror of
/// [`apply_services`]: stopping releases the running daemon so its binary
/// can be unlinked cleanly, disabling removes the boot-time symlink so an
/// uninstalled component leaves no orphan `enabled` unit behind.
///
/// `disable` is idempotent (a no-op on a unit that was never enabled), so
/// each unit is stopped *and* disabled unconditionally — the executor does
/// not need to know which units were enabled at install time.
///
/// A bare service template is stopped as `name@*.service` so every loaded
/// instance is deactivated; disabling still targets the declared template.
///
/// **Best-effort**: a failed stop still proceeds to disable, and neither
/// failure aborts or rolls back the uninstall — warnings surface on the
/// verb's outcome instead. An `!manager.supported()` host produces a
/// documented `Info` skip line per op (stop and disable) and no warning.
#[allow(clippy::too_many_arguments)]
pub fn deactivate_services(
    manager: &dyn ServiceManager,
    units: &[(String, String)],
    log: Option<&crate::central_log::CentralLog>,
    operation_id: &str,
    actor: &str,
    install_mode: &str,
) -> DeactivationOutcome {
    let mut outcome = DeactivationOutcome::default();
    for (component, unit) in units {
        if !manager.supported() {
            for op in [ServiceOp::Stop, ServiceOp::Disable] {
                record_service_op_unsupported(
                    log,
                    op,
                    component,
                    unit,
                    operation_id,
                    actor,
                    install_mode,
                    manager.manager(),
                    manager.unsupported_reason(),
                );
            }
            continue;
        }

        // A template name cannot be stopped directly. systemctl expands this
        // pattern against loaded units, covering every live instance.
        let stop_target = unit
            .strip_suffix("@.service")
            .map_or_else(|| unit.clone(), |prefix| format!("{prefix}@*.service"));
        match manager.stop_service(&stop_target) {
            Ok(_) => {
                record_service_op(
                    log,
                    ServiceOp::Stop,
                    component,
                    &stop_target,
                    operation_id,
                    actor,
                    install_mode,
                    None,
                );
                outcome.stopped.push(stop_target.clone());
            }
            Err(err) => {
                let msg = err.to_string();
                record_service_op(
                    log,
                    ServiceOp::Stop,
                    component,
                    &stop_target,
                    operation_id,
                    actor,
                    install_mode,
                    Some(&msg),
                );
                outcome
                    .warnings
                    .push(format!("stop {stop_target} failed: {msg}"));
            }
        }

        // Disable runs even when stop failed: a still-running unit can
        // still have its boot symlink removed, and leaving it enabled is
        // exactly the orphan we are here to prevent.
        match manager.disable_service(unit) {
            Ok(_) => {
                record_service_op(
                    log,
                    ServiceOp::Disable,
                    component,
                    unit,
                    operation_id,
                    actor,
                    install_mode,
                    None,
                );
                outcome.disabled.push(unit.clone());
            }
            Err(err) => {
                let msg = err.to_string();
                record_service_op(
                    log,
                    ServiceOp::Disable,
                    component,
                    unit,
                    operation_id,
                    actor,
                    install_mode,
                    Some(&msg),
                );
                outcome
                    .warnings
                    .push(format!("disable {unit} failed: {msg}"));
            }
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io;
    use std::path::PathBuf;

    struct ScriptedRunner {
        calls: Mutex<VecDeque<(Vec<String>, io::Result<CommandOutput>)>>,
    }

    impl ScriptedRunner {
        fn new(scope: ServiceScope, steps: Vec<(&str, io::Result<CommandOutput>)>) -> Self {
            Self {
                calls: Mutex::new(
                    steps
                        .into_iter()
                        .map(|(op, result)| {
                            let mut args = Vec::new();
                            if scope == ServiceScope::User {
                                args.push("--user".to_string());
                            }
                            args.push(op.to_string());
                            if op != "daemon-reload" {
                                args.push("test.service".to_string());
                            }
                            (args, result)
                        })
                        .collect(),
                ),
            }
        }

        fn assert_finished(&self) {
            assert!(self.calls.lock().unwrap().is_empty());
        }
    }

    impl CommandRunner for ScriptedRunner {
        fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
            assert_eq!(program, "systemctl");
            let (expected, result) = self
                .calls
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected service call");
            assert_eq!(args, expected);
            result
        }
    }

    fn command_output(code: Option<i32>, stdout: &str, stderr: &str) -> io::Result<CommandOutput> {
        Ok(CommandOutput {
            code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        })
    }

    #[test]
    fn systemd_probe_bus_failure_is_not_missing() {
        let runner = ScriptedRunner::new(
            ServiceScope::System,
            vec![(
                "is-active",
                command_output(Some(1), "", "Failed to connect to bus: Host is down\n"),
            )],
        );
        let manager = SystemdServiceManager::with_runner(ServiceScope::System, runner);
        let error = manager.probe_service("test.service").unwrap_err();
        assert_eq!(
            error.to_string(),
            "systemctl is-active test.service exited with status 1: Failed to connect to bus: Host is down"
        );
        assert!(matches!(error, ServiceError::NonZeroExit { code: 1, .. }));
        manager.runner.assert_finished();
    }

    #[test]
    fn systemd_probe_preserves_supported_states_and_success_parsing() {
        let states = [
            ("active", ServiceState::Active),
            ("reloading", ServiceState::Activating),
            ("activating", ServiceState::Activating),
            ("deactivating", ServiceState::Deactivating),
            ("inactive", ServiceState::Inactive),
            ("failed", ServiceState::Failed),
            ("unknown", ServiceState::NotInstalled),
            ("maintenance", ServiceState::Unknown),
        ];
        for scope in [ServiceScope::System, ServiceScope::User] {
            for code in [0, 3, 4] {
                for (token, state) in states {
                    let stdout = format!(" \t{}\n", token.to_uppercase());
                    let runner = ScriptedRunner::new(
                        scope,
                        vec![("is-active", command_output(Some(code), &stdout, " \t\n"))],
                    );
                    let manager = SystemdServiceManager::with_runner(scope, runner);
                    let outcome = manager.probe_service("test.service").unwrap();
                    assert_eq!(outcome.state, state);
                    assert_eq!(outcome.manager, scope.manager_label());
                    assert!(outcome.supported);
                    assert!(!outcome.changed);
                    assert!(manager.handles_scope(scope));
                    manager.runner.assert_finished();
                }
            }
            for (stdout, state) in [
                ("", ServiceState::NotInstalled),
                ("new-state", ServiceState::Unknown),
            ] {
                let manager = SystemdServiceManager::with_runner(
                    scope,
                    ScriptedRunner::new(
                        scope,
                        vec![("is-active", command_output(Some(0), stdout, "warning"))],
                    ),
                );
                assert_eq!(manager.probe_service("test.service").unwrap().state, state);
                manager.runner.assert_finished();
            }
        }
    }

    #[test]
    fn systemd_probe_rejects_failed_and_ambiguous_observations() {
        for code in [Some(1), Some(2), Some(3), Some(4), Some(5), None] {
            for (stdout, stderr) in [
                ("", ""),
                (" \n", "\t"),
                ("inactive", " warning \n"),
                ("unknown", "Failed to connect to bus"),
                ("new-state", ""),
                ("inactive\nfailed", ""),
                ("unit inactive", ""),
            ] {
                let runner = ScriptedRunner::new(
                    ServiceScope::System,
                    vec![("is-active", command_output(code, stdout, stderr))],
                );
                let manager = SystemdServiceManager::with_runner(ServiceScope::System, runner);
                let error = manager.probe_service("test.service").unwrap_err();
                match error {
                    ServiceError::NonZeroExit {
                        op,
                        unit,
                        code: actual,
                        stderr: detail,
                    } => {
                        assert_eq!(op, "is-active");
                        assert_eq!(unit, "test.service");
                        assert_eq!(actual, code.unwrap_or(-1));
                        assert_eq!(detail, stderr.trim());
                    }
                    other => panic!("unexpected error: {other}"),
                }
                manager.runner.assert_finished();
            }
        }
        for code in [Some(1), Some(2), Some(5), None] {
            let manager = SystemdServiceManager::with_runner(
                ServiceScope::System,
                ScriptedRunner::new(
                    ServiceScope::System,
                    vec![("is-active", command_output(code, "active", ""))],
                ),
            );
            assert!(matches!(
                manager.probe_service("test.service"),
                Err(ServiceError::NonZeroExit { .. })
            ));
            manager.runner.assert_finished();
        }
    }

    fn dispatch_service(
        manager: &dyn ServiceManager,
        op: ServiceOp,
    ) -> Result<ServiceOutcome, ServiceError> {
        match op {
            ServiceOp::Start => manager.start_service("test.service"),
            ServiceOp::Stop => manager.stop_service("test.service"),
            ServiceOp::Restart => manager.restart_service("test.service"),
            ServiceOp::Enable => manager.enable_service("test.service"),
            ServiceOp::Disable => manager.disable_service("test.service"),
            ServiceOp::Probe => manager.probe_service("test.service"),
            ServiceOp::DaemonReload => manager.daemon_reload(),
        }
    }

    #[test]
    fn systemd_operations_preserve_scope_sequence_and_changes() {
        for scope in [ServiceScope::System, ServiceScope::User] {
            for op in [
                ServiceOp::Start,
                ServiceOp::Stop,
                ServiceOp::Restart,
                ServiceOp::Enable,
                ServiceOp::Disable,
            ] {
                let (prior, post, post_code) = if op == ServiceOp::Stop {
                    ("active", "inactive", 3)
                } else {
                    ("inactive", "active", 0)
                };
                let runner = ScriptedRunner::new(
                    scope,
                    vec![
                        (
                            "is-active",
                            command_output(Some(if prior == "active" { 0 } else { 3 }), prior, ""),
                        ),
                        (op.as_str(), command_output(Some(0), "", "")),
                        ("is-active", command_output(Some(post_code), post, "")),
                    ],
                );
                let manager = SystemdServiceManager::with_runner(scope, runner);
                let outcome = dispatch_service(&manager, op).unwrap();
                assert_eq!(outcome.op, op);
                assert_eq!(outcome.unit, "test.service");
                assert_eq!(outcome.manager, scope.manager_label());
                assert!(outcome.changed);
                assert_eq!(
                    outcome.state,
                    if op == ServiceOp::Stop {
                        ServiceState::Inactive
                    } else {
                        ServiceState::Active
                    }
                );
                assert_eq!(
                    outcome.message,
                    format!(
                        "systemctl {} test.service ok (state={})",
                        op.as_str(),
                        outcome.state.as_str()
                    )
                );
                manager.runner.assert_finished();
            }
            let manager = SystemdServiceManager::with_runner(
                scope,
                ScriptedRunner::new(
                    scope,
                    vec![("daemon-reload", command_output(Some(0), "", ""))],
                ),
            );
            let outcome = manager.daemon_reload().unwrap();
            assert_eq!(outcome.state, ServiceState::Unknown);
            assert!(outcome.changed);
            assert_eq!(outcome.unit, "");
            assert_eq!(outcome.message, "systemctl daemon-reload ok");
            manager.runner.assert_finished();
        }
    }

    #[test]
    fn systemd_maintenance_observation_does_not_block_operations() {
        for scope in [ServiceScope::System, ServiceScope::User] {
            for op in [
                ServiceOp::Start,
                ServiceOp::Stop,
                ServiceOp::Restart,
                ServiceOp::Enable,
                ServiceOp::Disable,
            ] {
                let manager = SystemdServiceManager::with_runner(
                    scope,
                    ScriptedRunner::new(
                        scope,
                        vec![
                            ("is-active", command_output(Some(3), "maintenance\n", "")),
                            (op.as_str(), command_output(Some(0), "", "")),
                            ("is-active", command_output(Some(3), "maintenance\n", "")),
                        ],
                    ),
                );
                let outcome = dispatch_service(&manager, op).unwrap();
                assert_eq!(outcome.state, ServiceState::Unknown);
                assert_eq!(outcome.op, op);
                assert_eq!(
                    outcome.changed,
                    matches!(
                        op,
                        ServiceOp::Restart | ServiceOp::Enable | ServiceOp::Disable
                    )
                );
                manager.runner.assert_finished();
            }
        }
    }

    #[test]
    fn systemd_failures_stop_at_the_failing_phase() {
        for scope in [ServiceScope::System, ServiceScope::User] {
            for op in [
                ServiceOp::Start,
                ServiceOp::Stop,
                ServiceOp::Restart,
                ServiceOp::Enable,
                ServiceOp::Disable,
                ServiceOp::Probe,
                ServiceOp::DaemonReload,
            ] {
                let phase_count = if matches!(op, ServiceOp::Probe | ServiceOp::DaemonReload) {
                    1
                } else {
                    3
                };
                for phase in 0..phase_count {
                    for failure in 0..3 {
                        let mut steps = Vec::new();
                        if phase > 0 {
                            steps.push(("is-active", command_output(Some(0), "active", "")));
                        }
                        if phase > 1 {
                            steps.push((op.as_str(), command_output(Some(0), "", "")));
                        }
                        let failing_op = if op == ServiceOp::DaemonReload || phase == 1 {
                            op.as_str()
                        } else {
                            "is-active"
                        };
                        let output = match failure {
                            0 => Err(io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                "spawn denied",
                            )),
                            1 => command_output(Some(1), "ignored stdout", " failure detail \n"),
                            _ => command_output(None, "active", " failure detail \n"),
                        };
                        steps.push((failing_op, output));
                        let manager = SystemdServiceManager::with_runner(
                            scope,
                            ScriptedRunner::new(scope, steps),
                        );
                        match dispatch_service(&manager, op).unwrap_err() {
                            ServiceError::Spawn { source } => {
                                assert_eq!(failure, 0);
                                assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
                                assert_eq!(source.to_string(), "spawn denied");
                            }
                            ServiceError::NonZeroExit {
                                op: actual,
                                unit,
                                code,
                                stderr,
                            } => {
                                assert_eq!(actual, failing_op);
                                assert_eq!(
                                    unit,
                                    if op == ServiceOp::DaemonReload {
                                        ""
                                    } else {
                                        "test.service"
                                    }
                                );
                                assert_eq!(code, if failure == 1 { 1 } else { -1 });
                                assert_eq!(stderr, "failure detail");
                            }
                        }
                        manager.runner.assert_finished();
                    }
                }
            }
        }
    }

    fn fake_env(os: &str, container: Option<&str>) -> EnvFacts {
        EnvFacts {
            os: os.to_string(),
            arch: "x86_64".to_string(),
            libc: None,
            kernel: None,
            pkg_base: None,
            os_id: None,
            os_id_like: None,
            os_version: None,
            btf: None,
            cap_bpf: None,
            container: container.map(|s| s.to_string()),
            user: "tester".to_string(),
            uid: 1000,
            home: PathBuf::from("/home/tester"),
        }
    }

    #[test]
    fn fake_manager_records_calls_in_order() {
        let m = FakeServiceManager::new();
        m.start_service("a.service").unwrap();
        m.stop_service("a.service").unwrap();
        m.probe_service("b.service").unwrap();
        let calls = m.calls();
        assert_eq!(
            calls,
            vec![
                (ServiceOp::Start, "a.service".to_string()),
                (ServiceOp::Stop, "a.service".to_string()),
                (ServiceOp::Probe, "b.service".to_string()),
            ]
        );
    }

    #[test]
    fn fake_manager_tracks_state_transitions() {
        let m = FakeServiceManager::new();
        let started = m.start_service("a.service").unwrap();
        assert_eq!(started.state, ServiceState::Active);
        assert!(started.changed);
        // Second start is a no-op transition (already active).
        let again = m.start_service("a.service").unwrap();
        assert_eq!(again.state, ServiceState::Active);
        assert!(!again.changed);
        let stopped = m.stop_service("a.service").unwrap();
        assert_eq!(stopped.state, ServiceState::Inactive);
        assert!(stopped.changed);
    }

    #[test]
    fn fake_manager_can_force_failure_per_op() {
        let m = FakeServiceManager::new();
        m.fail(ServiceOp::Start, "a.service");
        let err = m.start_service("a.service").unwrap_err();
        match err {
            ServiceError::NonZeroExit { op, unit, code, .. } => {
                assert_eq!(op, "start");
                assert_eq!(unit, "a.service");
                assert_eq!(code, 1);
            }
            other => panic!("expected NonZeroExit, got {other:?}"),
        }
        // Other ops on the same unit still succeed.
        assert!(m.stop_service("a.service").is_ok());
    }

    #[test]
    fn not_supported_manager_short_circuits_every_op() {
        let m = NotSupportedServiceManager::new("test reason".to_string());
        for outcome in [
            m.probe_service("x.service").unwrap(),
            m.start_service("x.service").unwrap(),
            m.stop_service("x.service").unwrap(),
            m.restart_service("x.service").unwrap(),
            m.enable_service("x.service").unwrap(),
            m.disable_service("x.service").unwrap(),
        ] {
            assert_eq!(outcome.state, ServiceState::NotSupported);
            assert!(!outcome.supported);
            assert!(!outcome.changed);
            assert_eq!(outcome.manager, "not-supported");
            assert_eq!(outcome.message, "test reason");
        }
    }

    #[test]
    fn factory_returns_systemd_only_for_linux_system_no_container() {
        let m = for_install_mode("system", &fake_env("linux", None));
        assert_eq!(m.manager(), "systemd");
        assert!(m.supported());
        // System mode drives system units, not user units.
        assert!(m.handles_scope(ServiceScope::System));
        assert!(!m.handles_scope(ServiceScope::User));
    }

    #[test]
    fn factory_skips_user_install_mode_on_linux() {
        // The system-scope factory stays unsupported in user mode: a
        // user-mode install cannot manage system units (this also keeps
        // systemd health checks degrading to not_supported in user mode).
        let m = for_install_mode("user", &fake_env("linux", None));
        assert!(!m.supported());
        assert_eq!(m.manager(), "not-supported");
        assert!(m.unsupported_reason().unwrap().contains("install_mode"));
    }

    #[test]
    fn user_factory_drives_user_scope_only_in_user_mode() {
        // The user-scope factory drives `systemctl --user` in user mode,
        // handling only user-scope requests, and labels itself `systemd-user`
        // so diagnostics distinguish it from the system manager.
        let m = user_service_for_install_mode("user", &fake_env("linux", None));
        assert!(m.supported());
        assert_eq!(m.manager(), "systemd-user");
        assert!(m.handles_scope(ServiceScope::User));
        assert!(!m.handles_scope(ServiceScope::System));
    }

    #[test]
    fn user_factory_skips_system_mode_as_place_only() {
        // System mode places the user unit but does not auto-activate it.
        let m = user_service_for_install_mode("system", &fake_env("linux", None));
        assert!(!m.supported());
        assert!(m.unsupported_reason().unwrap().contains("placed"));
    }

    #[test]
    fn user_factory_skips_inside_containers() {
        let m = user_service_for_install_mode("user", &fake_env("linux", Some("docker")));
        assert!(!m.supported());
        assert!(m.unsupported_reason().unwrap().contains("docker"));
    }

    #[test]
    fn factory_skips_non_linux_hosts() {
        let m = for_install_mode("system", &fake_env("darwin", None));
        assert!(!m.supported());
        assert!(m.unsupported_reason().unwrap().contains("darwin"));
    }

    #[test]
    fn factory_skips_inside_containers() {
        let m = for_install_mode("system", &fake_env("linux", Some("docker")));
        assert!(!m.supported());
        assert!(m.unsupported_reason().unwrap().contains("docker"));
    }

    /// `record_service_op` must emit one `LogKind::Component` line per
    /// call with `command: "service:<op>"`, the component + unit
    /// captured, install_mode stamped, and the parent operation_id
    /// threaded through. Severity is Info on success and Warn on a
    /// reported error so audit consumers can grep failed teardowns
    /// without having to parse the message text.
    #[test]
    fn record_service_op_writes_kind_component_lines_with_correct_severity() {
        use crate::central_log::CentralLog;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("central.log");
        let log = CentralLog::open(path.clone());

        record_service_op(
            Some(&log),
            ServiceOp::Start,
            "agentsight",
            "agentsight.service",
            "op-test-001",
            "tester",
            "system",
            None,
        );
        record_service_op(
            Some(&log),
            ServiceOp::Stop,
            "agentsight",
            "agentsight.service",
            "op-test-001",
            "tester",
            "system",
            Some("systemctl exited 1"),
        );

        let content = std::fs::read_to_string(&path).expect("read log");
        let lines: Vec<serde_json::Value> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("parse line"))
            .collect();
        assert_eq!(lines.len(), 2, "expected one record per op");

        let ok = &lines[0];
        assert_eq!(ok.get("kind").and_then(|v| v.as_str()), Some("component"));
        assert_eq!(
            ok.get("command").and_then(|v| v.as_str()),
            Some("service:start"),
        );
        assert_eq!(
            ok.get("severity").and_then(|v| v.as_str()),
            Some("info"),
            "successful service ops must be Info",
        );
        assert_eq!(
            ok.get("component").and_then(|v| v.as_str()),
            Some("agentsight"),
        );
        assert_eq!(
            ok.get("install_mode").and_then(|v| v.as_str()),
            Some("system"),
        );
        assert_eq!(
            ok.get("operation_id").and_then(|v| v.as_str()),
            Some("op-test-001"),
        );
        assert_eq!(
            ok.get("details")
                .and_then(|v| v.get("unit"))
                .and_then(|v| v.as_str()),
            Some("agentsight.service"),
        );

        let err = &lines[1];
        assert_eq!(
            err.get("command").and_then(|v| v.as_str()),
            Some("service:stop"),
        );
        assert_eq!(
            err.get("severity").and_then(|v| v.as_str()),
            Some("warn"),
            "service ops that errored must be Warn so audit pipelines can grep",
        );
        let msg = err.get("message").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            msg.contains("systemctl exited 1") && msg.contains("agentsight.service"),
            "warn message must carry the underlying error and unit: {msg}",
        );
    }

    /// `record_service_op` is a no-op when no log handle is provided.
    /// Pins the contract so callers that want to skip auditing (e.g.
    /// in-tree service-start fallbacks for hosts where the central log
    /// is read-only) don't accidentally pay an empty-string write.
    #[test]
    fn record_service_op_with_no_log_handle_is_a_noop() {
        record_service_op(
            None,
            ServiceOp::Start,
            "agentsight",
            "agentsight.service",
            "op-test-002",
            "tester",
            "system",
            None,
        );
        // No assertion needed — the contract is "do not panic, do not
        // touch disk". A future regression that introduces an unwrap on
        // the optional log would surface here.
    }

    /// When the resolved `ServiceManager` is the not-supported stub
    /// (macOS dev, `--install-mode user`, container), the verb still
    /// must leave an audit trail so operators can tell "no services
    /// declared" from "services declared but no manager available".
    /// Pins the wire contract: kind=component, command=service:<op>,
    /// severity=Info (this is documented behaviour, not a fault),
    /// details.supported=false, details.reason carries the manager's
    /// own unsupported reason verbatim.
    #[test]
    fn record_service_op_unsupported_writes_supported_false_details() {
        use crate::central_log::CentralLog;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("central.log");
        let log = CentralLog::open(path.clone());

        record_service_op_unsupported(
            Some(&log),
            ServiceOp::Start,
            "agentsight",
            "agentsight.service",
            "op-test-003",
            "tester",
            "user",
            "not-supported",
            Some("install_mode=user is not supported by systemd manager"),
        );

        let content = std::fs::read_to_string(&path).expect("read log");
        let lines: Vec<serde_json::Value> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("parse line"))
            .collect();
        assert_eq!(lines.len(), 1);
        let rec = &lines[0];
        assert_eq!(rec.get("kind").and_then(|v| v.as_str()), Some("component"));
        assert_eq!(
            rec.get("command").and_then(|v| v.as_str()),
            Some("service:start"),
        );
        assert_eq!(
            rec.get("severity").and_then(|v| v.as_str()),
            Some("info"),
            "unsupported skip is documented behaviour, not a fault — must be Info",
        );
        assert_eq!(
            rec.get("install_mode").and_then(|v| v.as_str()),
            Some("user"),
        );
        let details = rec.get("details").expect("details present");
        assert_eq!(
            details.get("supported").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            details.get("manager").and_then(|v| v.as_str()),
            Some("not-supported"),
        );
        assert!(
            details
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .contains("install_mode=user"),
        );
    }

    /// `record_service_op_unsupported` is a no-op without a log handle.
    /// Mirrors `record_service_op_with_no_log_handle_is_a_noop` so both
    /// helpers share the "do not panic on `None`" contract.
    #[test]
    fn record_service_op_unsupported_with_no_log_handle_is_a_noop() {
        record_service_op_unsupported(
            None,
            ServiceOp::Stop,
            "agentsight",
            "agentsight.service",
            "op-test-004",
            "tester",
            "system",
            "not-supported",
            Some("nope"),
        );
    }

    fn svc_req(unit: &str, enable: bool, start: bool) -> ServiceRequest {
        ServiceRequest {
            unit: unit.to_string(),
            scope: ServiceScope::System,
            enable,
            start,
        }
    }

    #[test]
    fn apply_services_enables_then_starts_in_order() {
        let m = FakeServiceManager::new();
        let reqs = vec![svc_req("a.service", true, true)];
        let out = apply_services(
            &m,
            &reqs,
            ServiceActivation::Start,
            None,
            "comp",
            "op1",
            "cli",
            "system",
        );
        assert_eq!(
            m.calls(),
            vec![
                (ServiceOp::DaemonReload, "".to_string()),
                (ServiceOp::Enable, "a.service".to_string()),
                (ServiceOp::Start, "a.service".to_string()),
            ]
        );
        assert_eq!(out.enabled_units, vec!["a.service".to_string()]);
        assert_eq!(out.started_units, vec!["a.service".to_string()]);
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn apply_services_skips_enable_when_not_requested() {
        let m = FakeServiceManager::new();
        let reqs = vec![svc_req("a.service", false, true)];
        let out = apply_services(
            &m,
            &reqs,
            ServiceActivation::Start,
            None,
            "comp",
            "op1",
            "cli",
            "system",
        );
        assert_eq!(
            m.calls(),
            vec![
                (ServiceOp::DaemonReload, "".to_string()),
                (ServiceOp::Start, "a.service".to_string()),
            ]
        );
        assert!(out.enabled_units.is_empty());
        assert_eq!(out.started_units, vec!["a.service".to_string()]);
    }

    #[test]
    fn apply_services_skips_start_when_not_requested() {
        let m = FakeServiceManager::new();
        let reqs = vec![svc_req("a.service", true, false)];
        let out = apply_services(
            &m,
            &reqs,
            ServiceActivation::Start,
            None,
            "comp",
            "op1",
            "cli",
            "system",
        );
        assert_eq!(
            m.calls(),
            vec![
                (ServiceOp::DaemonReload, "".to_string()),
                (ServiceOp::Enable, "a.service".to_string()),
            ]
        );
        assert_eq!(out.enabled_units, vec!["a.service".to_string()]);
        assert!(out.started_units.is_empty());
    }

    #[test]
    fn apply_services_restart_activation_uses_restart_not_start() {
        let m = FakeServiceManager::new();
        let reqs = vec![svc_req("a.service", false, true)];
        let out = apply_services(
            &m,
            &reqs,
            ServiceActivation::Restart,
            None,
            "comp",
            "op1",
            "cli",
            "system",
        );
        assert_eq!(
            m.calls(),
            vec![
                (ServiceOp::DaemonReload, "".to_string()),
                (ServiceOp::Restart, "a.service".to_string()),
            ]
        );
        assert_eq!(out.started_units, vec!["a.service".to_string()]);
    }

    #[test]
    fn apply_services_enable_failure_warns_and_still_starts() {
        let m = FakeServiceManager::new();
        m.fail(ServiceOp::Enable, "a.service");
        let reqs = vec![svc_req("a.service", true, true)];
        let out = apply_services(
            &m,
            &reqs,
            ServiceActivation::Start,
            None,
            "comp",
            "op1",
            "cli",
            "system",
        );
        // Best-effort: enable failed but start was still attempted.
        assert_eq!(
            m.calls(),
            vec![
                (ServiceOp::DaemonReload, "".to_string()),
                (ServiceOp::Enable, "a.service".to_string()),
                (ServiceOp::Start, "a.service".to_string()),
            ]
        );
        assert!(out.enabled_units.is_empty());
        assert_eq!(out.started_units, vec!["a.service".to_string()]);
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("a.service"));
    }

    #[test]
    fn apply_services_start_failure_warns_without_aborting() {
        let m = FakeServiceManager::new();
        m.fail(ServiceOp::Start, "a.service");
        let reqs = vec![svc_req("a.service", true, true)];
        let out = apply_services(
            &m,
            &reqs,
            ServiceActivation::Start,
            None,
            "comp",
            "op1",
            "cli",
            "system",
        );
        assert_eq!(out.enabled_units, vec!["a.service".to_string()]);
        assert!(out.started_units.is_empty());
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("a.service"));
    }

    #[test]
    fn apply_services_unsupported_manager_is_quiet_skip() {
        let m = NotSupportedServiceManager::new("install_mode=user".to_string());
        let reqs = vec![svc_req("a.service", true, true)];
        let out = apply_services(
            &m,
            &reqs,
            ServiceActivation::Start,
            None,
            "comp",
            "op1",
            "cli",
            "user",
        );
        assert!(out.enabled_units.is_empty());
        assert!(out.started_units.is_empty());
        // Unsupported is a documented skip, not a fault — no warnings.
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn apply_services_user_scope_is_skipped_by_a_system_manager() {
        // Default fake is system-scope (mirrors a system-mode install). A
        // user-scope request is not handled, so the manager is never called
        // — not even a daemon-reload, which only precedes work it handles.
        let m = FakeServiceManager::new();
        let reqs = vec![ServiceRequest {
            unit: "anolisa-memory@alice.service".to_string(),
            scope: ServiceScope::User,
            enable: true,
            start: true,
        }];
        let out = apply_services(
            &m,
            &reqs,
            ServiceActivation::Start,
            None,
            "comp",
            "op1",
            "cli",
            "system",
        );
        assert!(m.calls().is_empty());
        assert!(out.enabled_units.is_empty());
        assert!(out.started_units.is_empty());
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn apply_services_drives_user_scope_when_manager_handles_it() {
        // A user-scope manager (user-mode install) drives a user-scope unit:
        // reload precedes enable+start, all recorded against it.
        let m = FakeServiceManager::with_scope(ServiceScope::User);
        let reqs = vec![ServiceRequest {
            unit: "anolisa-memory@alice.service".to_string(),
            scope: ServiceScope::User,
            enable: true,
            start: true,
        }];
        let out = apply_services(
            &m,
            &reqs,
            ServiceActivation::Start,
            None,
            "comp",
            "op1",
            "cli",
            "user",
        );
        let calls = m.calls();
        assert_eq!(calls[0], (ServiceOp::DaemonReload, String::new()));
        assert!(calls.contains(&(
            ServiceOp::Enable,
            "anolisa-memory@alice.service".to_string()
        )));
        assert!(calls.contains(&(ServiceOp::Start, "anolisa-memory@alice.service".to_string())));
        assert_eq!(
            out.enabled_units,
            vec!["anolisa-memory@alice.service".to_string()]
        );
        assert_eq!(
            out.started_units,
            vec!["anolisa-memory@alice.service".to_string()]
        );
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn apply_services_daemon_reload_failure_warns_but_still_activates() {
        let m = FakeServiceManager::new();
        m.fail(ServiceOp::DaemonReload, "");
        let reqs = vec![svc_req("a.service", true, true)];
        let out = apply_services(
            &m,
            &reqs,
            ServiceActivation::Start,
            None,
            "comp",
            "op1",
            "cli",
            "system",
        );
        // Reload is attempted first and fails, but activation continues.
        assert_eq!(
            m.calls(),
            vec![
                (ServiceOp::DaemonReload, "".to_string()),
                (ServiceOp::Enable, "a.service".to_string()),
                (ServiceOp::Start, "a.service".to_string()),
            ]
        );
        assert_eq!(out.enabled_units, vec!["a.service".to_string()]);
        assert_eq!(out.started_units, vec!["a.service".to_string()]);
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("daemon-reload"));
    }

    #[test]
    fn apply_services_logs_info_on_enable_and_warn_on_start_failure() {
        use crate::central_log::CentralLog;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("central.log");
        let log = CentralLog::open(path.clone());

        let m = FakeServiceManager::new();
        m.fail(ServiceOp::Start, "agentsight.service");
        let reqs = vec![svc_req("agentsight.service", true, true)];
        apply_services(
            &m,
            &reqs,
            ServiceActivation::Start,
            Some(&log),
            "agentsight",
            "op-svc-1",
            "tester",
            "system",
        );

        let content = std::fs::read_to_string(&path).expect("read log");
        let lines: Vec<serde_json::Value> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("parse line"))
            .collect();
        assert_eq!(
            lines.len(),
            3,
            "one record per attempted op: daemon-reload, enable, start"
        );

        // A new unit file landed this run, so a daemon-reload precedes
        // activation — recorded as an Info audit line.
        assert_eq!(
            lines[0].get("command").and_then(|v| v.as_str()),
            Some("service:daemon-reload"),
        );
        assert_eq!(
            lines[0].get("severity").and_then(|v| v.as_str()),
            Some("info"),
        );
        assert_eq!(
            lines[1].get("command").and_then(|v| v.as_str()),
            Some("service:enable"),
        );
        assert_eq!(
            lines[1].get("severity").and_then(|v| v.as_str()),
            Some("info"),
        );
        assert_eq!(
            lines[2].get("command").and_then(|v| v.as_str()),
            Some("service:start"),
        );
        assert_eq!(
            lines[2].get("severity").and_then(|v| v.as_str()),
            Some("warn"),
            "a failed start must be Warn so audit pipelines can grep",
        );
        let msg = lines[2]
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(
            msg.contains("agentsight.service"),
            "warn must name the unit: {msg}"
        );
    }

    #[test]
    fn apply_services_unsupported_logs_supported_false_details() {
        use crate::central_log::CentralLog;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("central.log");
        let log = CentralLog::open(path.clone());

        let m = NotSupportedServiceManager::new("install_mode=user is not supported".to_string());
        let reqs = vec![svc_req("agentsight.service", true, true)];
        apply_services(
            &m,
            &reqs,
            ServiceActivation::Start,
            Some(&log),
            "agentsight",
            "op-svc-2",
            "tester",
            "user",
        );

        let content = std::fs::read_to_string(&path).expect("read log");
        let lines: Vec<serde_json::Value> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("parse line"))
            .collect();
        assert_eq!(lines.len(), 1, "one skip record per request");
        let rec = &lines[0];
        assert_eq!(
            rec.get("severity").and_then(|v| v.as_str()),
            Some("info"),
            "unsupported skip is documented behaviour, not a fault",
        );
        let details = rec.get("details").expect("details present");
        assert_eq!(
            details.get("supported").and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    fn unit(component: &str, name: &str) -> (String, String) {
        (component.to_string(), name.to_string())
    }

    #[test]
    fn deactivate_services_stops_then_disables_in_order() {
        let m = FakeServiceManager::new();
        let units = vec![unit("agentsight", "agentsight.service")];
        let out = deactivate_services(&m, &units, None, "op1", "cli", "system");
        assert_eq!(
            m.calls(),
            vec![
                (ServiceOp::Stop, "agentsight.service".to_string()),
                (ServiceOp::Disable, "agentsight.service".to_string()),
            ]
        );
        assert_eq!(out.stopped, vec!["agentsight.service".to_string()]);
        assert_eq!(out.disabled, vec!["agentsight.service".to_string()]);
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn deactivate_services_stops_template_instances_before_disabling_template() {
        let m = FakeServiceManager::new();
        let units = vec![unit("cosh-ng", "cosh-gateway@.service")];
        let out = deactivate_services(&m, &units, None, "op1", "cli", "system");
        assert_eq!(
            m.calls(),
            vec![
                (ServiceOp::Stop, "cosh-gateway@*.service".to_string()),
                (ServiceOp::Disable, "cosh-gateway@.service".to_string()),
            ]
        );
        assert_eq!(out.stopped, vec!["cosh-gateway@*.service".to_string()]);
        assert_eq!(out.disabled, vec!["cosh-gateway@.service".to_string()]);
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn deactivate_services_stop_failure_warns_and_still_disables() {
        let m = FakeServiceManager::new();
        m.fail(ServiceOp::Stop, "a.service");
        let units = vec![unit("comp", "a.service")];
        let out = deactivate_services(&m, &units, None, "op1", "cli", "system");
        // Best-effort: stop failed but disable was still attempted so the
        // boot symlink is removed regardless.
        assert_eq!(
            m.calls(),
            vec![
                (ServiceOp::Stop, "a.service".to_string()),
                (ServiceOp::Disable, "a.service".to_string()),
            ]
        );
        assert!(out.stopped.is_empty());
        assert_eq!(out.disabled, vec!["a.service".to_string()]);
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("a.service"));
    }

    #[test]
    fn deactivate_services_disable_failure_warns_without_aborting() {
        let m = FakeServiceManager::new();
        m.fail(ServiceOp::Disable, "a.service");
        let units = vec![unit("comp", "a.service")];
        let out = deactivate_services(&m, &units, None, "op1", "cli", "system");
        assert_eq!(out.stopped, vec!["a.service".to_string()]);
        assert!(out.disabled.is_empty());
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("a.service"));
    }

    #[test]
    fn deactivate_services_unsupported_manager_is_quiet_skip() {
        let m = NotSupportedServiceManager::new("install_mode=user".to_string());
        let units = vec![unit("comp", "a.service")];
        let out = deactivate_services(&m, &units, None, "op1", "cli", "user");
        // Unsupported never touches a manager method and never warns.
        assert!(out.stopped.is_empty());
        assert!(out.disabled.is_empty());
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn deactivate_services_logs_stop_and_disable_records() {
        use crate::central_log::CentralLog;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("central.log");
        let log = CentralLog::open(path.clone());

        let m = FakeServiceManager::new();
        let units = vec![unit("agentsight", "agentsight.service")];
        deactivate_services(&m, &units, Some(&log), "op-deact-1", "tester", "system");

        let content = std::fs::read_to_string(&path).expect("read log");
        let lines: Vec<serde_json::Value> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("parse line"))
            .collect();
        assert_eq!(lines.len(), 2, "one record per attempted op");
        assert_eq!(
            lines[0].get("command").and_then(|v| v.as_str()),
            Some("service:stop"),
        );
        assert_eq!(
            lines[1].get("command").and_then(|v| v.as_str()),
            Some("service:disable"),
        );
        for rec in &lines {
            assert_eq!(rec.get("severity").and_then(|v| v.as_str()), Some("info"));
            assert_eq!(rec.get("kind").and_then(|v| v.as_str()), Some("component"));
        }
    }

    #[test]
    fn deactivate_services_with_no_log_handle_is_a_noop() {
        let m = FakeServiceManager::new();
        let units = vec![unit("comp", "a.service")];
        // None log handle must not panic and must still drive the manager.
        let out = deactivate_services(&m, &units, None, "op1", "cli", "system");
        assert_eq!(out.stopped.len() + out.disabled.len(), 2);
    }
}
