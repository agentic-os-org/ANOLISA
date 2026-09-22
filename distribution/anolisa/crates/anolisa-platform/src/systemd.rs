//! Injectable systemd service-management boundary.
//!
//! [`Systemd`] runs `systemctl(1)` through [`CommandRunner`], preserving
//! spawn, exit, and domain-state distinctions for callers and isolated tests.

use thiserror::Error;

use crate::command::{CommandOutput, CommandRunner, InheritedLocaleCommandRunner};

const SYSTEMCTL: &str = "systemctl";

/// Captured context for an unsuccessful `systemctl` process.
#[derive(Debug)]
pub struct SystemdCommandFailure {
    /// Operation that failed.
    pub operation: String,
    /// Unit supplied to `systemctl`, when the operation targets one.
    pub unit: Option<String>,
    /// Process exit code; `None` when terminated by a signal.
    pub code: Option<i32>,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
    /// Compatibility rendering of the captured command output.
    detail: String,
}

impl std::fmt::Display for SystemdCommandFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

/// Errors returned by systemd service operations.
#[derive(Debug, Error)]
pub enum SystemdError {
    /// `systemctl` could not be spawned.
    #[error("systemctl command failed: failed to spawn systemctl: {source}")]
    Spawn {
        /// Operation that could not be started.
        operation: String,
        /// Spawn-phase error from the operating system.
        #[source]
        source: std::io::Error,
    },
    /// The unit name is empty or typed status evidence reports it missing.
    #[error("service not found: {0}")]
    NotFound(String),
    /// `systemctl` exited unsuccessfully.
    #[error("systemctl command failed: {0}")]
    NonZeroExit(Box<SystemdCommandFailure>),
}

/// Snapshot of systemd unit state used by status/restart flows.
#[derive(Debug, PartialEq, Eq)]
pub struct UnitStatus {
    /// Whether systemd currently reports the unit as active.
    pub active: bool,
    /// Whether systemd currently reports the unit as failed.
    pub failed: bool,
    /// Whether the unit is enabled for automatic start.
    pub enabled: bool,
    /// Human-readable unit description from systemd metadata.
    pub description: String,
}

/// Systemd bridge backed by an injectable process runner.
///
/// Production code uses [`Systemd::system`]; tests use
/// [`Systemd::with_runner`] to avoid invoking the host's service manager.
pub struct Systemd<R: CommandRunner = InheritedLocaleCommandRunner> {
    runner: R,
}

impl Systemd<InheritedLocaleCommandRunner> {
    /// Build a bridge that runs the host's real `systemctl` binary.
    pub fn system() -> Self {
        Self {
            runner: InheritedLocaleCommandRunner,
        }
    }
}

impl Default for Systemd<InheritedLocaleCommandRunner> {
    fn default() -> Self {
        Self::system()
    }
}

impl<R: CommandRunner> Systemd<R> {
    /// Build a bridge backed by a custom runner.
    pub fn with_runner(runner: R) -> Self {
        Self { runner }
    }

    fn run_systemctl(&self, operation: &str, args: &[&str]) -> Result<CommandOutput, SystemdError> {
        self.runner
            .run(SYSTEMCTL, args)
            .map_err(|source| SystemdError::Spawn {
                operation: operation.to_string(),
                source,
            })
    }

    /// Query the status of a systemd unit via `systemctl show`.
    ///
    /// # Errors
    ///
    /// Returns [`SystemdError::NotFound`] for an empty or missing unit and a
    /// typed command error when `systemctl` cannot run or exits unsuccessfully.
    pub fn unit_status(&self, unit: &str) -> Result<UnitStatus, SystemdError> {
        if unit.trim().is_empty() {
            return Err(SystemdError::NotFound("<empty>".to_string()));
        }
        let out = self.run_systemctl(
            "show",
            &[
                "show",
                unit,
                "--no-pager",
                "--property=LoadState,ActiveState,UnitFileState,Description",
            ],
        )?;
        if out.code != Some(0) {
            return Err(command_failure("show", Some(unit), out));
        }

        let mut load_state = String::new();
        let mut active_state = String::new();
        let mut unit_file_state = String::new();
        let mut description = String::new();
        for line in out.stdout.lines() {
            if let Some(value) = line.strip_prefix("LoadState=") {
                load_state = value.to_string();
            } else if let Some(value) = line.strip_prefix("ActiveState=") {
                active_state = value.to_string();
            } else if let Some(value) = line.strip_prefix("UnitFileState=") {
                unit_file_state = value.to_string();
            } else if let Some(value) = line.strip_prefix("Description=") {
                description = value.to_string();
            }
        }
        if load_state == "not-found" || (load_state == "masked" && unit_file_state.is_empty()) {
            return Err(SystemdError::NotFound(unit.to_string()));
        }
        Ok(UnitStatus {
            active: active_state == "active" || active_state == "reloading",
            failed: active_state == "failed",
            enabled: matches!(
                unit_file_state.as_str(),
                "enabled" | "enabled-runtime" | "alias" | "static" | "indirect"
            ),
            description,
        })
    }

    /// Reload systemd manager configuration (`systemctl daemon-reload`).
    ///
    /// # Errors
    ///
    /// Returns a typed systemd error when the command cannot complete.
    pub fn daemon_reload(&self) -> Result<(), SystemdError> {
        self.run_operation("daemon-reload", &["daemon-reload"])
    }

    /// Enable a unit without starting it (`systemctl enable <unit>`).
    ///
    /// # Errors
    ///
    /// Returns a typed systemd error when the unit is empty or the command
    /// cannot complete.
    pub fn enable_unit_file(&self, unit: &str) -> Result<(), SystemdError> {
        self.run_unit_operation("enable", &["enable", unit], unit)
    }

    /// Start a systemd unit (`systemctl start <unit>`).
    ///
    /// # Errors
    ///
    /// Returns a typed systemd error when the unit is empty or the command
    /// cannot complete.
    pub fn start_unit(&self, unit: &str) -> Result<(), SystemdError> {
        self.run_unit_operation("start", &["start", unit], unit)
    }

    /// Restart a systemd unit (`systemctl restart <unit>`).
    ///
    /// # Errors
    ///
    /// Returns a typed systemd error when the unit is empty or the command
    /// cannot complete.
    pub fn restart_unit(&self, unit: &str) -> Result<(), SystemdError> {
        self.run_unit_operation("restart", &["restart", unit], unit)
    }

    /// Stop a systemd unit (`systemctl stop <unit>`).
    ///
    /// # Errors
    ///
    /// Returns a typed systemd error when the unit is empty or the command
    /// cannot complete.
    pub fn stop_unit(&self, unit: &str) -> Result<(), SystemdError> {
        self.run_unit_operation("stop", &["stop", unit], unit)
    }

    /// Disable a unit without stopping it (`systemctl disable <unit>`).
    ///
    /// # Errors
    ///
    /// Returns a typed systemd error when the unit is empty or the command
    /// cannot complete.
    pub fn disable_unit_file(&self, unit: &str) -> Result<(), SystemdError> {
        self.run_unit_operation("disable", &["disable", unit], unit)
    }

    /// Enable and start a systemd unit (`systemctl enable --now <unit>`).
    ///
    /// # Errors
    ///
    /// Returns a typed systemd error when the unit is empty, missing, or the
    /// command cannot complete.
    pub fn enable_unit(&self, unit: &str) -> Result<(), SystemdError> {
        self.run_unit_operation("enable", &["enable", "--now", unit], unit)
    }

    /// Disable a unit without blocking on its stop sequence.
    ///
    /// `stop --no-block` is best-effort; the following `disable` determines the
    /// operation result. This preserves the persistent opt-out path's bounded
    /// latency while preventing systemd from restarting the unit.
    ///
    /// # Errors
    ///
    /// Returns a typed systemd error when the unit is empty, missing, or the
    /// authoritative `disable` command cannot complete.
    pub fn disable_unit_deferred(&self, unit: &str) -> Result<(), SystemdError> {
        if unit.trim().is_empty() {
            return Err(SystemdError::NotFound("<empty>".to_string()));
        }
        let _ = self.run_systemctl("stop", &["stop", "--no-block", unit]);
        let result = self.run_unit_operation("disable", &["disable", unit], unit);
        if matches!(&result, Err(SystemdError::NonZeroExit(_)))
            && matches!(self.unit_status(unit), Err(SystemdError::NotFound(_)))
        {
            return Err(SystemdError::NotFound(unit.to_string()));
        }
        result
    }

    fn run_unit_operation(
        &self,
        operation: &str,
        args: &[&str],
        unit: &str,
    ) -> Result<(), SystemdError> {
        if unit.trim().is_empty() {
            return Err(SystemdError::NotFound("<empty>".to_string()));
        }
        let out = self.run_systemctl(operation, args)?;
        if out.code == Some(0) {
            return Ok(());
        }
        Err(command_failure(operation, Some(unit), out))
    }

    fn run_operation(&self, operation: &str, args: &[&str]) -> Result<(), SystemdError> {
        let out = self.run_systemctl(operation, args)?;
        if out.code == Some(0) {
            return Ok(());
        }
        Err(command_failure(operation, None, out))
    }
}

fn command_failure(operation: &str, unit: Option<&str>, out: CommandOutput) -> SystemdError {
    let combined = format!("{}{}", out.stderr, out.stdout);
    let detail = if combined.trim().is_empty() {
        format!(
            "systemctl exited with {}",
            out.code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_string())
        )
    } else {
        combined.trim().to_string()
    };
    let failure = Box::new(SystemdCommandFailure {
        operation: operation.to_string(),
        unit: unit.map(str::to_string),
        code: out.code,
        stdout: out.stdout,
        stderr: out.stderr,
        detail,
    });
    SystemdError::NonZeroExit(failure)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::io;

    use super::*;

    enum FakeOutcome {
        Ok(CommandOutput),
        Err(io::ErrorKind),
    }

    struct FakeCall {
        args: Vec<String>,
        outcome: FakeOutcome,
    }

    struct FakeCommandRunner {
        calls: RefCell<VecDeque<FakeCall>>,
    }

    impl CommandRunner for FakeCommandRunner {
        fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
            assert_eq!(program, SYSTEMCTL);
            let call = self
                .calls
                .borrow_mut()
                .pop_front()
                .expect("unexpected systemctl call");
            assert_eq!(args, call.args);
            match call.outcome {
                FakeOutcome::Ok(out) => Ok(out),
                FakeOutcome::Err(kind) => Err(io::Error::new(kind, "fake systemctl spawn failure")),
            }
        }
    }

    fn call(args: &[&str], code: Option<i32>, stdout: &str, stderr: &str) -> FakeCall {
        FakeCall {
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            outcome: FakeOutcome::Ok(CommandOutput {
                code,
                stdout: stdout.to_string(),
                stderr: stderr.to_string(),
            }),
        }
    }

    fn spawn_error(args: &[&str], kind: io::ErrorKind) -> FakeCall {
        FakeCall {
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            outcome: FakeOutcome::Err(kind),
        }
    }

    fn systemd(calls: Vec<FakeCall>) -> Systemd<FakeCommandRunner> {
        Systemd::with_runner(FakeCommandRunner {
            calls: RefCell::new(calls.into()),
        })
    }

    fn assert_finished(systemd: &Systemd<FakeCommandRunner>) {
        assert!(systemd.runner.calls.borrow().is_empty());
    }

    #[test]
    fn empty_unit_operations_never_invoke_systemctl() {
        let systemd = systemd(Vec::new());
        assert!(matches!(
            systemd.unit_status(" "),
            Err(SystemdError::NotFound(unit)) if unit == "<empty>"
        ));
        assert!(matches!(
            systemd.enable_unit(" "),
            Err(SystemdError::NotFound(unit)) if unit == "<empty>"
        ));
        assert!(matches!(
            systemd.disable_unit_deferred(" "),
            Err(SystemdError::NotFound(unit)) if unit == "<empty>"
        ));
        assert!(matches!(
            systemd.start_unit(" "),
            Err(SystemdError::NotFound(unit)) if unit == "<empty>"
        ));
        assert!(matches!(
            systemd.restart_unit(" "),
            Err(SystemdError::NotFound(unit)) if unit == "<empty>"
        ));
        assert!(matches!(
            systemd.stop_unit(" "),
            Err(SystemdError::NotFound(unit)) if unit == "<empty>"
        ));
        assert!(matches!(
            systemd.enable_unit_file(" "),
            Err(SystemdError::NotFound(unit)) if unit == "<empty>"
        ));
        assert!(matches!(
            systemd.disable_unit_file(" "),
            Err(SystemdError::NotFound(unit)) if unit == "<empty>"
        ));
        assert_finished(&systemd);
    }

    #[test]
    fn service_lifecycle_operations_preserve_systemctl_argv() {
        let systemd = systemd(vec![
            call(&["daemon-reload"], Some(0), "", ""),
            call(&["enable", "anolisa.service"], Some(0), "", ""),
            call(&["start", "anolisa.service"], Some(0), "", ""),
            call(&["restart", "anolisa.service"], Some(0), "", ""),
            call(&["stop", "anolisa.service"], Some(0), "", ""),
            call(&["disable", "anolisa.service"], Some(0), "", ""),
        ]);

        systemd.daemon_reload().expect("reload should succeed");
        systemd
            .enable_unit_file("anolisa.service")
            .expect("enable should succeed");
        systemd
            .start_unit("anolisa.service")
            .expect("start should succeed");
        systemd
            .restart_unit("anolisa.service")
            .expect("restart should succeed");
        systemd
            .stop_unit("anolisa.service")
            .expect("stop should succeed");
        systemd
            .disable_unit_file("anolisa.service")
            .expect("disable should succeed");
        assert_finished(&systemd);
    }

    #[test]
    fn stop_non_zero_exit_does_not_probe_status() {
        let systemd = systemd(vec![call(
            &["stop", "missing.service"],
            Some(5),
            "",
            "arbitrary localized diagnostic\n",
        )]);

        assert!(matches!(
            systemd.stop_unit("missing.service"),
            Err(SystemdError::NonZeroExit(failure))
                if failure.operation == "stop"
                    && failure.unit.as_deref() == Some("missing.service")
                    && failure.code == Some(5)
        ));
        assert_finished(&systemd);
    }

    #[test]
    fn unit_status_parses_machine_readable_properties() {
        let systemd = systemd(vec![call(
            &[
                "show",
                "anolisa.service",
                "--no-pager",
                "--property=LoadState,ActiveState,UnitFileState,Description",
            ],
            Some(0),
            "LoadState=loaded\nActiveState=reloading\nUnitFileState=enabled-runtime\nDescription=ANOLISA\n",
            "",
        )]);

        let status = systemd
            .unit_status("anolisa.service")
            .expect("status should parse");
        assert_eq!(
            status,
            UnitStatus {
                active: true,
                failed: false,
                enabled: true,
                description: "ANOLISA".to_string(),
            }
        );
        assert_finished(&systemd);
    }

    #[test]
    fn spawn_failure_is_distinct_from_process_exit() {
        let systemd = systemd(vec![spawn_error(
            &["enable", "--now", "anolisa.service"],
            io::ErrorKind::PermissionDenied,
        )]);

        let err = systemd
            .enable_unit("anolisa.service")
            .expect_err("spawn should fail");
        assert!(matches!(
            &err,
            SystemdError::Spawn { operation, source }
                if operation == "enable" && source.kind() == io::ErrorKind::PermissionDenied
        ));
        assert_eq!(
            err.to_string(),
            "systemctl command failed: failed to spawn systemctl: fake systemctl spawn failure"
        );
        assert_finished(&systemd);
    }

    #[test]
    fn missing_unit_has_typed_domain_error() {
        let systemd = systemd(vec![call(
            &[
                "show",
                "missing.service",
                "--no-pager",
                "--property=LoadState,ActiveState,UnitFileState,Description",
            ],
            Some(0),
            "LoadState=not-found\nActiveState=inactive\nUnitFileState=\nDescription=missing.service\n",
            "",
        )]);

        assert!(matches!(
            systemd.unit_status("missing.service"),
            Err(SystemdError::NotFound(unit)) if unit == "missing.service"
        ));
        assert_finished(&systemd);
    }

    #[test]
    fn unit_status_derives_failed_from_active_state() {
        let systemd = systemd(vec![call(
            &[
                "show",
                "broken.service",
                "--no-pager",
                "--property=LoadState,ActiveState,UnitFileState,Description",
            ],
            Some(0),
            "LoadState=loaded\nActiveState=failed\nUnitFileState=enabled\nDescription=Broken\n",
            "",
        )]);

        let status = systemd
            .unit_status("broken.service")
            .expect("failed is a valid unit state");
        assert!(!status.active);
        assert!(status.failed);
        assert!(status.enabled);
        assert_eq!(status.description, "Broken");
        assert_finished(&systemd);
    }

    #[test]
    fn bus_error_with_missing_path_is_non_zero_exit() {
        let systemd = systemd(vec![call(
            &[
                "show",
                "anolisa.service",
                "--no-pager",
                "--property=LoadState,ActiveState,UnitFileState,Description",
            ],
            Some(1),
            "",
            "Failed to connect to bus: No such file or directory\n",
        )]);

        let err = systemd
            .unit_status("anolisa.service")
            .expect_err("bus failure is not a missing unit");
        assert!(matches!(
            err,
            SystemdError::NonZeroExit(failure)
                if failure.operation == "show"
                    && failure.unit.as_deref() == Some("anolisa.service")
                    && failure.stderr.contains("No such file or directory")
        ));
        assert_finished(&systemd);
    }

    #[test]
    fn bus_error_with_failed_text_is_non_zero_exit() {
        let systemd = systemd(vec![call(
            &[
                "show",
                "anolisa.service",
                "--no-pager",
                "--property=LoadState,ActiveState,UnitFileState,Description",
            ],
            Some(1),
            "",
            "Failed to connect to bus: Host is down\n",
        )]);

        let err = systemd
            .unit_status("anolisa.service")
            .expect_err("diagnostic text is not a failed unit state");
        assert!(matches!(
            err,
            SystemdError::NonZeroExit(failure)
                if failure.operation == "show"
                    && failure.unit.as_deref() == Some("anolisa.service")
                    && failure.stderr.contains("Host is down")
        ));
        assert_finished(&systemd);
    }

    #[test]
    fn generic_non_zero_exit_keeps_status_and_streams() {
        let systemd = systemd(vec![call(
            &["disable", "anolisa.service"],
            Some(4),
            "out\n",
            "permission denied\n",
        )]);

        let err = systemd
            .disable_unit_file("anolisa.service")
            .expect_err("disable should fail");
        assert!(matches!(
            &err,
            SystemdError::NonZeroExit(failure)
                if failure.operation == "disable"
                    && failure.unit.as_deref() == Some("anolisa.service")
                    && failure.code == Some(4)
                    && failure.stdout == "out\n"
                    && failure.stderr == "permission denied\n"
        ));
        assert_eq!(
            err.to_string(),
            "systemctl command failed: permission denied\nout"
        );
        assert_finished(&systemd);
    }

    #[test]
    fn deferred_disable_ignores_stop_failure_but_checks_disable() {
        let systemd = systemd(vec![
            spawn_error(
                &["stop", "--no-block", "anolisa.service"],
                io::ErrorKind::NotFound,
            ),
            call(&["disable", "anolisa.service"], Some(0), "", ""),
        ]);

        systemd
            .disable_unit_deferred("anolisa.service")
            .expect("disable is authoritative");
        assert_finished(&systemd);
    }

    #[test]
    fn deferred_disable_confirms_missing_unit_from_status() {
        let systemd = systemd(vec![
            call(
                &["stop", "--no-block", "missing.service"],
                Some(5),
                "",
                "Unit missing.service not loaded.\n",
            ),
            call(
                &["disable", "missing.service"],
                Some(1),
                "",
                "Failed to disable unit: Unit file missing.service does not exist.\n",
            ),
            call(
                &[
                    "show",
                    "missing.service",
                    "--no-pager",
                    "--property=LoadState,ActiveState,UnitFileState,Description",
                ],
                Some(0),
                "LoadState=not-found\nActiveState=inactive\nUnitFileState=\nDescription=missing.service\n",
                "",
            ),
        ]);

        assert!(matches!(
            systemd.disable_unit_deferred("missing.service"),
            Err(SystemdError::NotFound(unit)) if unit == "missing.service"
        ));
        assert_finished(&systemd);
    }
}
