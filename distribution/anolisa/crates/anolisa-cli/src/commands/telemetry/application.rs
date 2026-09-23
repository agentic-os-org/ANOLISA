//! Application orchestration for telemetry mutations.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anolisa_core::execution::{CommandOutcome, CommandOutcomeStatus, ExecutionIntent};
use anolisa_core::{
    RegistrationManager, TelemetryChannel, Uploader, generate_link_id, require_root,
};
use anolisa_platform::command::CommandRunner;
use anolisa_platform::fs_layout::FsLayout;
use anolisa_platform::systemd::{Systemd, SystemdError};

use crate::context::{CliContext, InstallMode};
use crate::response::CliError;

use super::{SERVICE_NAME, UNIT_FILENAME};

/// Typed input for one telemetry mutation.
pub(super) enum TelemetryRequest {
    /// Enables telemetry collection.
    Enable {
        /// Selects read-only preview or state-changing application.
        intent: ExecutionIntent,
    },
    /// Disables telemetry collection.
    Disable {
        /// Selects read-only preview or state-changing application.
        intent: ExecutionIntent,
    },
    /// Links the host to named reporting.
    Link {
        /// Selects read-only preview or state-changing application.
        intent: ExecutionIntent,
    },
    /// Removes the named-reporting link and cached identity.
    Unlink {
        /// Selects read-only preview or state-changing application.
        intent: ExecutionIntent,
    },
    /// Runs the uploader once or continuously.
    Upload {
        /// Whether to run the continuous upload loop.
        loop_flag: bool,
        /// Selects read-only preview or state-changing application.
        intent: ExecutionIntent,
    },
    /// Initializes the telemetry operations channel.
    Init {
        /// Selects read-only preview or state-changing application.
        intent: ExecutionIntent,
    },
}

impl TelemetryRequest {
    fn intent(&self) -> ExecutionIntent {
        match self {
            Self::Enable { intent }
            | Self::Disable { intent }
            | Self::Link { intent }
            | Self::Unlink { intent }
            | Self::Upload { intent, .. }
            | Self::Init { intent } => *intent,
        }
    }

    fn command(&self) -> &'static str {
        match self {
            Self::Enable { .. } => "telemetry enable",
            Self::Disable { .. } => "telemetry disable",
            Self::Link { .. } => "telemetry link",
            Self::Unlink { .. } => "telemetry unlink",
            Self::Upload { .. } => "telemetry upload",
            Self::Init { .. } => "telemetry init",
        }
    }

    fn preview_message(&self) -> &'static str {
        match self {
            Self::Enable { .. } => "would enable telemetry collection and start the uploader",
            Self::Disable { .. } => "would disable telemetry collection and stop the uploader",
            Self::Link { .. } => "would link this instance to named reporting",
            Self::Unlink { .. } => "would unlink this instance and erase the cached identity",
            Self::Upload {
                loop_flag: true, ..
            } => "would start the continuous telemetry upload loop",
            Self::Upload {
                loop_flag: false, ..
            } => "would upload buffered telemetry once",
            Self::Init { .. } => "would initialize the telemetry operations channel",
        }
    }
}

/// Typed postcondition reported by an applied telemetry mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TelemetryChange {
    /// Collection is enabled.
    CollectionEnabled,
    /// Collection is disabled.
    CollectionDisabled,
    /// Named reporting is linked with the generated identifier.
    LinkCreated {
        /// Opaque link identifier persisted for the uploader.
        link_id: String,
    },
    /// Named reporting is unlinked.
    LinkCleared,
    /// The selected uploader mode completed successfully.
    UploadCompleted {
        /// Whether the continuous loop rather than a single upload ran.
        loop_flag: bool,
    },
    /// The telemetry operations channel is initialized.
    OpsChannelInitialized,
}

/// Read-only telemetry result rendered by the command layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TelemetryPreview {
    /// Stable command label used by existing JSON and error output.
    pub(super) command: &'static str,
    /// Existing human and JSON preview message.
    pub(super) message: &'static str,
}

/// Applied telemetry result needed by the compatibility renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TelemetryApplied {
    /// Collection was enabled.
    Enabled,
    /// Collection was disabled.
    Disabled,
    /// Named reporting is linked or was already linked.
    Linked {
        /// Effective link identifier.
        link_id: String,
        /// Whether apply found the host already linked and performed no write.
        already_linked: bool,
    },
    /// Named reporting was unlinked.
    Unlinked,
    /// The uploader completed in the requested mode.
    Uploaded {
        /// Whether the continuous loop rather than a single upload ran.
        loop_flag: bool,
    },
    /// The telemetry operations channel was initialized.
    Initialized,
}

impl TelemetryApplied {
    /// Returns the stable command label for error projection.
    pub(super) fn command(&self) -> &'static str {
        match self {
            Self::Enabled => "telemetry enable",
            Self::Disabled => "telemetry disable",
            Self::Linked { .. } => "telemetry link",
            Self::Unlinked => "telemetry unlink",
            Self::Uploaded { .. } => "telemetry upload",
            Self::Initialized => "telemetry init",
        }
    }
}

/// Typed application result separating previews from applied mutations.
#[derive(Debug)]
pub(super) enum TelemetryApplicationOutcome {
    /// Plan-only result produced without entering the effect executor.
    Preview(TelemetryPreview),
    /// Applied result paired with terminal status, changes, and warnings.
    Applied {
        /// Command-specific facts needed by the existing renderer.
        result: TelemetryApplied,
        /// Completed outcome; operation ID remains absent for telemetry.
        outcome: CommandOutcome<TelemetryChange>,
    },
}

struct TelemetryExecution {
    result: TelemetryApplied,
    changes: Vec<TelemetryChange>,
    warnings: Vec<String>,
}

trait TelemetryEffects {
    fn require_root(&self) -> Result<(), String>;
    fn read_link_id(&self) -> Option<String>;
    fn enable_collection(&self, linked: bool) -> Result<(), String>;
    fn disable_collection(&self) -> Result<(), String>;
    fn systemd_available(&self) -> bool;
    fn install_and_enable_service(&self, ctx: &CliContext) -> Result<(), String>;
    fn disable_service(&self) -> Result<(), String>;
    fn spawn_uploader(&self) -> Result<(), String>;
    fn generate_link_id(&self) -> String;
    fn link(&self, link_id: &str) -> Result<(), String>;
    fn collection_enabled(&self) -> bool;
    fn append_instance_snapshot(&self) -> Result<(), String>;
    fn unlink(&self) -> Result<(), String>;
    fn forget_identity(&self) -> Result<(), String>;
    fn upload(&self, loop_flag: bool) -> Result<(), String>;
    fn init_ops_channel(&self, linked: bool) -> Result<(), String>;
}

struct SystemTelemetryEffects;

impl TelemetryEffects for SystemTelemetryEffects {
    fn require_root(&self) -> Result<(), String> {
        require_root().map_err(|error| error.to_string())
    }

    fn read_link_id(&self) -> Option<String> {
        RegistrationManager::new().read_link_id()
    }

    fn enable_collection(&self, linked: bool) -> Result<(), String> {
        TelemetryChannel::new()
            .enable_collection(linked)
            .map_err(|error| error.to_string())
    }

    fn disable_collection(&self) -> Result<(), String> {
        TelemetryChannel::new()
            .disable_collection()
            .map_err(|error| error.to_string())
    }

    fn systemd_available(&self) -> bool {
        Path::new("/run/systemd/system").exists()
    }

    fn install_and_enable_service(&self, ctx: &CliContext) -> Result<(), String> {
        install_and_enable_service(ctx).map_err(|error| error.to_string())
    }

    fn disable_service(&self) -> Result<(), String> {
        disable_service_with(&Systemd::system())
    }

    fn spawn_uploader(&self) -> Result<(), String> {
        Uploader::default()
            .ensure_running()
            .map_err(|error| error.to_string())
    }

    fn generate_link_id(&self) -> String {
        generate_link_id()
    }

    fn link(&self, link_id: &str) -> Result<(), String> {
        RegistrationManager::new()
            .do_link(link_id)
            .map_err(|error| error.to_string())
    }

    fn collection_enabled(&self) -> bool {
        TelemetryChannel::new().is_enabled()
    }

    fn append_instance_snapshot(&self) -> Result<(), String> {
        TelemetryChannel::new()
            .append_instance_snapshot(true)
            .map_err(|error| error.to_string())
    }

    fn unlink(&self) -> Result<(), String> {
        RegistrationManager::new()
            .do_unlink()
            .map_err(|error| error.to_string())
    }

    fn forget_identity(&self) -> Result<(), String> {
        TelemetryChannel::new()
            .forget_identity()
            .map_err(|error| error.to_string())
    }

    fn upload(&self, loop_flag: bool) -> Result<(), String> {
        let uploader = Uploader::default();
        let result = if loop_flag {
            uploader.run_loop()
        } else {
            uploader.run_once()
        };
        result.map_err(|error| error.to_string())
    }

    fn init_ops_channel(&self, linked: bool) -> Result<(), String> {
        TelemetryChannel::new()
            .ensure_ops_channel(linked)
            .map_err(|error| error.to_string())
    }
}

/// Runs one telemetry mutation against production system boundaries.
///
/// # Errors
///
/// Returns the existing CLI error when scope validation or application fails.
pub(super) fn run(
    request: TelemetryRequest,
    ctx: &CliContext,
) -> Result<TelemetryApplicationOutcome, CliError> {
    run_with_effects(request, ctx, &SystemTelemetryEffects)
}

fn run_with_effects(
    request: TelemetryRequest,
    ctx: &CliContext,
    effects: &dyn TelemetryEffects,
) -> Result<TelemetryApplicationOutcome, CliError> {
    if matches!(request.intent(), ExecutionIntent::Plan) {
        if ctx.install_mode != InstallMode::System {
            let command = request.command();
            return Err(CliError::InvalidArgument {
                command: command.to_string(),
                reason: format!(
                    "command '{command}' operates on system scope; use `--install-mode system` to preview it"
                ),
            });
        }
        return Ok(TelemetryApplicationOutcome::Preview(TelemetryPreview {
            command: request.command(),
            message: request.preview_message(),
        }));
    }

    let execution = execute_with_effects(request, ctx, effects)?;
    Ok(TelemetryApplicationOutcome::Applied {
        result: execution.result,
        outcome: CommandOutcome::new(
            CommandOutcomeStatus::Completed,
            None,
            execution.changes,
            execution.warnings,
        ),
    })
}

fn execute_with_effects(
    request: TelemetryRequest,
    ctx: &CliContext,
    effects: &dyn TelemetryEffects,
) -> Result<TelemetryExecution, CliError> {
    match request {
        TelemetryRequest::Enable { .. } => execute_enable(ctx, effects),
        TelemetryRequest::Disable { .. } => execute_disable(effects),
        TelemetryRequest::Link { .. } => execute_link(effects),
        TelemetryRequest::Unlink { .. } => execute_unlink(effects),
        TelemetryRequest::Upload { loop_flag, .. } => execute_upload(loop_flag, effects),
        TelemetryRequest::Init { .. } => execute_init(effects),
    }
}

fn execute_enable(
    ctx: &CliContext,
    effects: &dyn TelemetryEffects,
) -> Result<TelemetryExecution, CliError> {
    const COMMAND: &str = "telemetry enable";
    effects
        .require_root()
        .map_err(|error| runtime(COMMAND, error))?;
    let linked = effects.read_link_id().is_some();
    effects
        .enable_collection(linked)
        .map_err(|error| runtime(COMMAND, error))?;

    let mut warnings = Vec::new();
    if effects.systemd_available() {
        if let Err(error) = effects.install_and_enable_service(ctx) {
            warnings.push(format!(
                "could not enable {UNIT_FILENAME} ({error}); falling back to lazy start"
            ));
            if let Err(error) = effects.spawn_uploader() {
                warnings.push(format!(
                    "telemetry enabled, but uploader failed to start: {error}"
                ));
            }
        }
    } else if let Err(error) = effects.spawn_uploader() {
        warnings.push(format!(
            "telemetry enabled, but uploader failed to start: {error}"
        ));
    }

    Ok(TelemetryExecution {
        result: TelemetryApplied::Enabled,
        changes: vec![TelemetryChange::CollectionEnabled],
        warnings,
    })
}

fn execute_disable(effects: &dyn TelemetryEffects) -> Result<TelemetryExecution, CliError> {
    const COMMAND: &str = "telemetry disable";
    effects
        .require_root()
        .map_err(|error| runtime(COMMAND, error))?;
    effects
        .disable_collection()
        .map_err(|error| runtime(COMMAND, error))?;

    let warnings = if effects.systemd_available() {
        effects
            .disable_service()
            .err()
            .map(|error| format!("failed to disable {UNIT_FILENAME}: {error}"))
            .into_iter()
            .collect()
    } else {
        Vec::new()
    };
    Ok(TelemetryExecution {
        result: TelemetryApplied::Disabled,
        changes: vec![TelemetryChange::CollectionDisabled],
        warnings,
    })
}

fn execute_link(effects: &dyn TelemetryEffects) -> Result<TelemetryExecution, CliError> {
    const COMMAND: &str = "telemetry link";
    effects
        .require_root()
        .map_err(|error| runtime(COMMAND, error))?;
    if let Some(link_id) = effects.read_link_id() {
        return Ok(TelemetryExecution {
            result: TelemetryApplied::Linked {
                link_id,
                already_linked: true,
            },
            changes: Vec::new(),
            warnings: Vec::new(),
        });
    }

    let link_id = effects.generate_link_id();
    effects
        .link(&link_id)
        .map_err(|error| runtime(COMMAND, error))?;
    let warnings = if effects.collection_enabled() {
        effects
            .append_instance_snapshot()
            .err()
            .map(|error| format!("linked, but failed to record instance snapshot: {error}"))
            .into_iter()
            .collect()
    } else {
        Vec::new()
    };

    Ok(TelemetryExecution {
        result: TelemetryApplied::Linked {
            link_id: link_id.clone(),
            already_linked: false,
        },
        changes: vec![TelemetryChange::LinkCreated { link_id }],
        warnings,
    })
}

fn execute_unlink(effects: &dyn TelemetryEffects) -> Result<TelemetryExecution, CliError> {
    const COMMAND: &str = "telemetry unlink";
    effects
        .require_root()
        .map_err(|error| runtime(COMMAND, error))?;
    effects.unlink().map_err(|error| runtime(COMMAND, error))?;
    let warnings = effects
        .forget_identity()
        .err()
        .map(|error| format!("unlinked, but failed to erase identity cache: {error}"))
        .into_iter()
        .collect();

    Ok(TelemetryExecution {
        result: TelemetryApplied::Unlinked,
        changes: vec![TelemetryChange::LinkCleared],
        warnings,
    })
}

fn execute_upload(
    loop_flag: bool,
    effects: &dyn TelemetryEffects,
) -> Result<TelemetryExecution, CliError> {
    const COMMAND: &str = "telemetry upload";
    effects
        .upload(loop_flag)
        .map_err(|error| runtime(COMMAND, error))?;
    Ok(TelemetryExecution {
        result: TelemetryApplied::Uploaded { loop_flag },
        changes: vec![TelemetryChange::UploadCompleted { loop_flag }],
        warnings: Vec::new(),
    })
}

fn execute_init(effects: &dyn TelemetryEffects) -> Result<TelemetryExecution, CliError> {
    const COMMAND: &str = "telemetry init";
    effects
        .require_root()
        .map_err(|error| runtime(COMMAND, error))?;
    let linked = effects.read_link_id().is_some();
    effects
        .init_ops_channel(linked)
        .map_err(|error| runtime(COMMAND, error))?;
    Ok(TelemetryExecution {
        result: TelemetryApplied::Initialized,
        changes: vec![TelemetryChange::OpsChannelInitialized],
        warnings: Vec::new(),
    })
}

fn runtime(command: &str, error: impl std::fmt::Display) -> CliError {
    CliError::Runtime {
        command: command.to_string(),
        reason: error.to_string(),
    }
}

fn install_and_enable_service(ctx: &CliContext) -> Result<(), CliError> {
    install_and_enable_service_with(ctx, &Systemd::system(), std::env::current_exe)
}

fn install_and_enable_service_with<R, F>(
    ctx: &CliContext,
    systemd: &Systemd<R>,
    current_exe: F,
) -> Result<(), CliError>
where
    R: CommandRunner,
    F: FnOnce() -> io::Result<PathBuf>,
{
    const UNIT_TEMPLATE: &str = include_str!("../../../../../systemd/anolisa-telemetry.service.in");

    const COMMAND: &str = "telemetry enable";
    let executable = current_exe().map_err(|error| runtime(COMMAND, error))?;
    let unit_content = UNIT_TEMPLATE.replace("@@ANOLISA_BIN@@", &executable.display().to_string());
    let layout = FsLayout::system(ctx.prefix.clone());
    let unit_path = layout.systemd_unit_dir.join(UNIT_FILENAME);
    if let Some(parent) = unit_path.parent() {
        fs::create_dir_all(parent).map_err(|error| runtime(COMMAND, error))?;
    }
    fs::write(&unit_path, unit_content).map_err(|error| runtime(COMMAND, error))?;

    systemd.daemon_reload().map_err(|error| match error {
        SystemdError::Spawn { source, .. } => runtime(COMMAND, source),
        // Reload historically displays only trimmed stderr, unlike enable's
        // combined-output SystemdError display. Preserve that warning contract.
        SystemdError::NonZeroExit(failure) => runtime(
            COMMAND,
            format!("systemctl daemon-reload failed: {}", failure.stderr.trim()),
        ),
        error => runtime(COMMAND, error),
    })?;

    systemd
        .enable_unit(SERVICE_NAME)
        .map_err(|error| runtime(COMMAND, error))
}

fn disable_service_with<R: CommandRunner>(systemd: &Systemd<R>) -> Result<(), String> {
    match systemd.disable_unit_deferred(SERVICE_NAME) {
        Ok(()) | Err(SystemdError::NotFound(_)) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::path::Path;
    use std::rc::Rc;

    use anolisa_platform::command::CommandOutput;

    use super::*;
    use crate::test_support::{TestContextOptions, TestSandbox, context_for_root};

    type ServiceCalls = Rc<RefCell<Vec<Vec<String>>>>;

    struct ScriptedRunner {
        calls: ServiceCalls,
        outputs: RefCell<VecDeque<io::Result<CommandOutput>>>,
        expected_unit: Option<(PathBuf, String)>,
    }

    impl ScriptedRunner {
        fn new(outputs: Vec<io::Result<CommandOutput>>) -> Self {
            Self {
                calls: Default::default(),
                outputs: RefCell::new(outputs.into()),
                expected_unit: None,
            }
        }
    }

    impl CommandRunner for ScriptedRunner {
        fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
            assert_eq!(program, "systemctl");
            if let Some((path, content)) = &self.expected_unit {
                assert_eq!(fs::read_to_string(path).unwrap(), *content);
            }
            self.calls
                .borrow_mut()
                .push(args.iter().map(|arg| (*arg).to_string()).collect());
            self.outputs
                .borrow_mut()
                .pop_front()
                .expect("unexpected systemctl call")
        }
    }

    fn output(code: Option<i32>, stdout: &str, stderr: &str) -> io::Result<CommandOutput> {
        Ok(CommandOutput {
            code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        })
    }

    fn service_effects(outputs: Vec<io::Result<CommandOutput>>) -> (FakeEffects, ServiceCalls) {
        let runner = ScriptedRunner::new(outputs);
        let calls = runner.calls.clone();
        (
            FakeEffects {
                systemd: true,
                service: Some(Systemd::with_runner(runner)),
                ..Default::default()
            },
            calls,
        )
    }

    #[derive(Default)]
    struct FakeEffects {
        calls: RefCell<Vec<&'static str>>,
        root_error: Option<&'static str>,
        link_id: Option<String>,
        enable_error: Option<&'static str>,
        disable_error: Option<&'static str>,
        systemd: bool,
        service: Option<Systemd<ScriptedRunner>>,
        install_error: Option<&'static str>,
        disable_service_error: Option<&'static str>,
        spawn_error: Option<&'static str>,
        generated_link_id: Option<String>,
        link_error: Option<&'static str>,
        collection_enabled: bool,
        snapshot_error: Option<&'static str>,
        unlink_error: Option<&'static str>,
        forget_error: Option<&'static str>,
        upload_error: Option<&'static str>,
        init_error: Option<&'static str>,
    }

    impl FakeEffects {
        fn record(&self, call: &'static str) {
            self.calls.borrow_mut().push(call);
        }

        fn result(error: Option<&'static str>) -> Result<(), String> {
            error.map_or(Ok(()), |reason| Err(reason.to_string()))
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.borrow().clone()
        }
    }

    impl TelemetryEffects for FakeEffects {
        fn require_root(&self) -> Result<(), String> {
            self.record("require_root");
            Self::result(self.root_error)
        }

        fn read_link_id(&self) -> Option<String> {
            self.record("read_link_id");
            self.link_id.clone()
        }

        fn enable_collection(&self, linked: bool) -> Result<(), String> {
            self.record(if linked {
                "enable_collection_linked"
            } else {
                "enable_collection_anonymous"
            });
            Self::result(self.enable_error)
        }

        fn disable_collection(&self) -> Result<(), String> {
            self.record("disable_collection");
            Self::result(self.disable_error)
        }

        fn systemd_available(&self) -> bool {
            self.record("systemd_available");
            self.systemd
        }

        fn install_and_enable_service(&self, ctx: &CliContext) -> Result<(), String> {
            self.record("install_and_enable_service");
            if let Some(systemd) = &self.service {
                install_and_enable_service_with(ctx, systemd, || {
                    self.record("current_exe");
                    Ok(ctx.layout().bin_dir.join("anolisa"))
                })
                .map_err(|error| error.to_string())
            } else {
                Self::result(self.install_error)
            }
        }

        fn disable_service(&self) -> Result<(), String> {
            self.record("disable_service");
            if let Some(systemd) = &self.service {
                disable_service_with(systemd)
            } else {
                Self::result(self.disable_service_error)
            }
        }

        fn spawn_uploader(&self) -> Result<(), String> {
            self.record("spawn_uploader");
            Self::result(self.spawn_error)
        }

        fn generate_link_id(&self) -> String {
            self.record("generate_link_id");
            self.generated_link_id
                .clone()
                .unwrap_or_else(|| "generated-link".to_string())
        }

        fn link(&self, _link_id: &str) -> Result<(), String> {
            self.record("link");
            Self::result(self.link_error)
        }

        fn collection_enabled(&self) -> bool {
            self.record("collection_enabled");
            self.collection_enabled
        }

        fn append_instance_snapshot(&self) -> Result<(), String> {
            self.record("append_instance_snapshot");
            Self::result(self.snapshot_error)
        }

        fn unlink(&self) -> Result<(), String> {
            self.record("unlink");
            Self::result(self.unlink_error)
        }

        fn forget_identity(&self) -> Result<(), String> {
            self.record("forget_identity");
            Self::result(self.forget_error)
        }

        fn upload(&self, loop_flag: bool) -> Result<(), String> {
            self.record(if loop_flag {
                "upload_loop"
            } else {
                "upload_once"
            });
            Self::result(self.upload_error)
        }

        fn init_ops_channel(&self, linked: bool) -> Result<(), String> {
            self.record(if linked {
                "init_ops_channel_linked"
            } else {
                "init_ops_channel_anonymous"
            });
            Self::result(self.init_error)
        }
    }

    fn ctx(mode: InstallMode) -> CliContext {
        context_for_root(
            Path::new("/tmp/anolisa-telemetry-application"),
            mode,
            None,
            TestContextOptions::default(),
        )
    }

    fn plan_requests() -> Vec<(TelemetryRequest, &'static str, &'static str)> {
        vec![
            (
                TelemetryRequest::Enable {
                    intent: ExecutionIntent::Plan,
                },
                "telemetry enable",
                "would enable telemetry collection and start the uploader",
            ),
            (
                TelemetryRequest::Disable {
                    intent: ExecutionIntent::Plan,
                },
                "telemetry disable",
                "would disable telemetry collection and stop the uploader",
            ),
            (
                TelemetryRequest::Link {
                    intent: ExecutionIntent::Plan,
                },
                "telemetry link",
                "would link this instance to named reporting",
            ),
            (
                TelemetryRequest::Unlink {
                    intent: ExecutionIntent::Plan,
                },
                "telemetry unlink",
                "would unlink this instance and erase the cached identity",
            ),
            (
                TelemetryRequest::Upload {
                    loop_flag: false,
                    intent: ExecutionIntent::Plan,
                },
                "telemetry upload",
                "would upload buffered telemetry once",
            ),
            (
                TelemetryRequest::Upload {
                    loop_flag: true,
                    intent: ExecutionIntent::Plan,
                },
                "telemetry upload",
                "would start the continuous telemetry upload loop",
            ),
            (
                TelemetryRequest::Init {
                    intent: ExecutionIntent::Plan,
                },
                "telemetry init",
                "would initialize the telemetry operations channel",
            ),
        ]
    }

    #[test]
    fn every_plan_returns_the_existing_message_without_calling_executor() {
        for (request, command, message) in plan_requests() {
            let effects = FakeEffects::default();
            let outcome = run_with_effects(request, &ctx(InstallMode::System), &effects)
                .expect("telemetry preview");
            assert!(effects.calls().is_empty());
            let TelemetryApplicationOutcome::Preview(preview) = outcome else {
                panic!("expected preview");
            };
            assert_eq!(preview.command, command);
            assert_eq!(preview.message, message);
        }
    }

    #[test]
    fn user_mode_plan_rejects_before_calling_executor() {
        let effects = FakeEffects::default();
        let error = run_with_effects(
            TelemetryRequest::Enable {
                intent: ExecutionIntent::Plan,
            },
            &ctx(InstallMode::User),
            &effects,
        )
        .expect_err("user-mode preview must fail");
        assert_eq!(error.code(), "INVALID_ARGUMENT");
        assert!(effects.calls().is_empty());
    }

    #[test]
    fn apply_returns_completed_outcome_with_changes_and_warnings() {
        let effects = FakeEffects {
            spawn_error: Some("uploader unavailable"),
            ..Default::default()
        };
        let outcome = run_with_effects(
            TelemetryRequest::Enable {
                intent: ExecutionIntent::Apply,
            },
            &ctx(InstallMode::System),
            &effects,
        )
        .expect("telemetry apply");
        assert_eq!(
            effects.calls(),
            vec![
                "require_root",
                "read_link_id",
                "enable_collection_anonymous",
                "systemd_available",
                "spawn_uploader",
            ]
        );
        let TelemetryApplicationOutcome::Applied { result, outcome } = outcome else {
            panic!("expected applied outcome");
        };
        assert_eq!(result, TelemetryApplied::Enabled);
        assert_eq!(outcome.status(), &CommandOutcomeStatus::Completed);
        assert_eq!(outcome.changes(), &[TelemetryChange::CollectionEnabled]);
        assert_eq!(
            outcome.warnings(),
            &["telemetry enabled, but uploader failed to start: uploader unavailable".to_string()]
        );
        assert!(outcome.operation_id().is_none());
    }

    #[test]
    fn already_linked_apply_carries_no_change() {
        let effects = FakeEffects {
            link_id: Some("existing-link".to_string()),
            ..Default::default()
        };
        let outcome = run_with_effects(
            TelemetryRequest::Link {
                intent: ExecutionIntent::Apply,
            },
            &ctx(InstallMode::System),
            &effects,
        )
        .expect("already linked apply");
        let TelemetryApplicationOutcome::Applied { outcome, .. } = outcome else {
            panic!("expected applied outcome");
        };
        assert_eq!(outcome.status(), &CommandOutcomeStatus::Completed);
        assert!(outcome.changes().is_empty());
        assert_eq!(effects.calls(), vec!["require_root", "read_link_id"]);
    }

    #[test]
    fn enable_service_failure_falls_back_and_keeps_both_warnings() {
        let effects = FakeEffects {
            systemd: true,
            install_error: Some("daemon reload failed"),
            spawn_error: Some("spawn failed"),
            ..Default::default()
        };
        let outcome = run_with_effects(
            TelemetryRequest::Enable {
                intent: ExecutionIntent::Apply,
            },
            &ctx(InstallMode::System),
            &effects,
        )
        .expect("fallback remains successful");
        let TelemetryApplicationOutcome::Applied { outcome, .. } = outcome else {
            panic!("expected applied outcome");
        };
        assert_eq!(
            outcome.warnings(),
            &[
                "could not enable anolisa-telemetry.service (daemon reload failed); falling back to lazy start".to_string(),
                "telemetry enabled, but uploader failed to start: spawn failed".to_string(),
            ]
        );
        assert_eq!(
            effects.calls(),
            vec![
                "require_root",
                "read_link_id",
                "enable_collection_anonymous",
                "systemd_available",
                "install_and_enable_service",
                "spawn_uploader",
            ]
        );
    }

    #[test]
    fn disable_service_failure_is_a_non_terminal_warning() {
        let effects = FakeEffects {
            systemd: true,
            disable_service_error: Some("bus unavailable"),
            ..Default::default()
        };
        let outcome = run_with_effects(
            TelemetryRequest::Disable {
                intent: ExecutionIntent::Apply,
            },
            &ctx(InstallMode::System),
            &effects,
        )
        .expect("disable remains successful");
        let TelemetryApplicationOutcome::Applied { outcome, .. } = outcome else {
            panic!("expected applied outcome");
        };
        assert_eq!(outcome.status(), &CommandOutcomeStatus::Completed);
        assert_eq!(
            outcome.warnings(),
            &["failed to disable anolisa-telemetry.service: bus unavailable".to_string()]
        );
        assert_eq!(
            effects.calls(),
            vec![
                "require_root",
                "disable_collection",
                "systemd_available",
                "disable_service",
            ]
        );
    }

    #[test]
    fn new_link_keeps_snapshot_failure_as_warning() {
        let effects = FakeEffects {
            generated_link_id: Some("new-link".to_string()),
            collection_enabled: true,
            snapshot_error: Some("snapshot write failed"),
            ..Default::default()
        };
        let outcome = run_with_effects(
            TelemetryRequest::Link {
                intent: ExecutionIntent::Apply,
            },
            &ctx(InstallMode::System),
            &effects,
        )
        .expect("link remains successful");
        let TelemetryApplicationOutcome::Applied { result, outcome } = outcome else {
            panic!("expected applied outcome");
        };
        assert_eq!(
            result,
            TelemetryApplied::Linked {
                link_id: "new-link".to_string(),
                already_linked: false,
            }
        );
        assert_eq!(
            outcome.changes(),
            &[TelemetryChange::LinkCreated {
                link_id: "new-link".to_string(),
            }]
        );
        assert_eq!(
            outcome.warnings(),
            &["linked, but failed to record instance snapshot: snapshot write failed".to_string()]
        );
    }

    #[test]
    fn unlink_keeps_identity_cleanup_failure_as_warning() {
        let effects = FakeEffects {
            forget_error: Some("cache removal failed"),
            ..Default::default()
        };
        let outcome = run_with_effects(
            TelemetryRequest::Unlink {
                intent: ExecutionIntent::Apply,
            },
            &ctx(InstallMode::System),
            &effects,
        )
        .expect("unlink remains successful");
        let TelemetryApplicationOutcome::Applied { outcome, .. } = outcome else {
            panic!("expected applied outcome");
        };
        assert_eq!(outcome.changes(), &[TelemetryChange::LinkCleared]);
        assert_eq!(
            outcome.warnings(),
            &["unlinked, but failed to erase identity cache: cache removal failed".to_string()]
        );
        assert_eq!(
            effects.calls(),
            vec!["require_root", "unlink", "forget_identity"]
        );
    }

    #[test]
    fn upload_modes_and_init_return_typed_changes() {
        for loop_flag in [false, true] {
            let effects = FakeEffects::default();
            let outcome = run_with_effects(
                TelemetryRequest::Upload {
                    loop_flag,
                    intent: ExecutionIntent::Apply,
                },
                &ctx(InstallMode::System),
                &effects,
            )
            .expect("upload apply");
            let TelemetryApplicationOutcome::Applied { result, outcome } = outcome else {
                panic!("expected applied outcome");
            };
            assert_eq!(result, TelemetryApplied::Uploaded { loop_flag });
            assert_eq!(
                outcome.changes(),
                &[TelemetryChange::UploadCompleted { loop_flag }]
            );
            assert_eq!(
                effects.calls(),
                vec![if loop_flag {
                    "upload_loop"
                } else {
                    "upload_once"
                }]
            );
        }

        let effects = FakeEffects {
            link_id: Some("linked".to_string()),
            ..Default::default()
        };
        let outcome = run_with_effects(
            TelemetryRequest::Init {
                intent: ExecutionIntent::Apply,
            },
            &ctx(InstallMode::System),
            &effects,
        )
        .expect("init apply");
        let TelemetryApplicationOutcome::Applied { result, outcome } = outcome else {
            panic!("expected applied outcome");
        };
        assert_eq!(result, TelemetryApplied::Initialized);
        assert_eq!(outcome.changes(), &[TelemetryChange::OpsChannelInitialized]);
        assert_eq!(
            effects.calls(),
            vec!["require_root", "read_link_id", "init_ops_channel_linked"]
        );
    }

    #[test]
    fn executor_error_remains_the_existing_cli_error() {
        let effects = FakeEffects {
            root_error: Some("scripted failure"),
            ..Default::default()
        };
        let error = run_with_effects(
            TelemetryRequest::Init {
                intent: ExecutionIntent::Apply,
            },
            &ctx(InstallMode::System),
            &effects,
        )
        .expect_err("scripted apply failure");
        assert_eq!(error.code(), "EXECUTION_FAILED");
        assert!(error.to_string().contains("scripted failure"));
    }

    #[test]
    fn service_install_writes_unit_before_reload_and_enable() {
        let sandbox = TestSandbox::new();
        let ctx = sandbox.context(InstallMode::System);
        let unit_path = ctx.layout().systemd_unit_dir.join(UNIT_FILENAME);
        let executable = sandbox.root().join("bin with spaces/anolisa");
        let expected = include_str!("../../../../../systemd/anolisa-telemetry.service.in")
            .replace("@@ANOLISA_BIN@@", &executable.display().to_string());
        let mut runner =
            ScriptedRunner::new(vec![output(Some(0), "", ""), output(Some(0), "", "")]);
        runner.expected_unit = Some((unit_path.clone(), expected.clone()));
        let calls = runner.calls.clone();
        let systemd = Systemd::with_runner(runner);
        install_and_enable_service_with(&ctx, &systemd, || {
            assert!(!unit_path.parent().unwrap().exists());
            Ok(executable)
        })
        .unwrap();
        assert_eq!(fs::read_to_string(&unit_path).unwrap(), expected);
        assert_eq!(
            *calls.borrow(),
            vec![vec!["daemon-reload"], vec!["enable", "--now", SERVICE_NAME],]
        );
    }

    #[test]
    fn service_file_failures_never_call_systemd() {
        for stage in ["executable", "directory", "unit"] {
            let sandbox = TestSandbox::new();
            let ctx = sandbox.context(InstallMode::System);
            let unit_dir = &ctx.layout().systemd_unit_dir;
            let unit_path = unit_dir.join(UNIT_FILENAME);
            if stage == "directory" {
                let blocker = unit_dir.parent().unwrap();
                fs::create_dir_all(blocker.parent().unwrap()).unwrap();
                fs::write(blocker, "block directory creation").unwrap();
            } else if stage == "unit" {
                fs::create_dir_all(&unit_path).unwrap();
            }
            let runner = ScriptedRunner::new(vec![]);
            let calls = runner.calls.clone();
            let resolutions = std::cell::Cell::new(0);
            let error =
                install_and_enable_service_with(&ctx, &Systemd::with_runner(runner), || {
                    resolutions.set(resolutions.get() + 1);
                    if stage == "executable" {
                        Err(io::Error::new(
                            io::ErrorKind::NotFound,
                            "executable unavailable",
                        ))
                    } else {
                        Ok(ctx.layout().bin_dir.join("anolisa"))
                    }
                })
                .unwrap_err();
            assert_eq!(error.command(), "telemetry enable");
            assert_eq!(error.code(), "EXECUTION_FAILED");
            assert_eq!(error.exit_code(), 1);
            if stage == "executable" {
                assert_eq!(error.reason(), "executable unavailable");
                assert!(!unit_dir.exists());
            }
            assert_eq!(resolutions.get(), 1);
            assert!(calls.borrow().is_empty());
        }
    }

    #[test]
    fn service_reload_preserves_stderr_only_and_spawn_diagnostics() {
        for (result, reason) in [
            (
                Err(io::Error::new(io::ErrorKind::NotFound, "no systemctl")),
                "no systemctl",
            ),
            (
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")),
                "denied",
            ),
            (
                output(Some(1), "ignored stdout", " \treload denied\n"),
                "systemctl daemon-reload failed: reload denied",
            ),
            (
                output(Some(1), "stdout only", ""),
                "systemctl daemon-reload failed: ",
            ),
            (
                output(Some(1), "", " \t\n"),
                "systemctl daemon-reload failed: ",
            ),
            (
                output(None, "ignored stdout", " killed\n"),
                "systemctl daemon-reload failed: killed",
            ),
            (output(None, "", ""), "systemctl daemon-reload failed: "),
        ] {
            let sandbox = TestSandbox::new();
            let ctx = sandbox.context(InstallMode::System);
            let runner = ScriptedRunner::new(vec![result]);
            let calls = runner.calls.clone();
            let error =
                install_and_enable_service_with(&ctx, &Systemd::with_runner(runner), || {
                    Ok(ctx.layout().bin_dir.join("anolisa"))
                })
                .unwrap_err();
            assert_eq!(error.reason(), reason);
            assert_eq!(error.to_string(), format!("execution failed: {reason}"));
            assert_eq!(error.code(), "EXECUTION_FAILED");
            assert_eq!(error.exit_code(), 1);
            assert_eq!(*calls.borrow(), vec![vec!["daemon-reload"]]);
            assert!(ctx.layout().systemd_unit_dir.join(UNIT_FILENAME).is_file());
        }
    }

    #[test]
    fn real_service_failures_keep_application_fallback_and_warning_order() {
        for (enable_reached, result, reason) in [
            (
                false,
                Err(io::Error::new(io::ErrorKind::NotFound, "no systemctl")),
                "no systemctl",
            ),
            (
                false,
                output(Some(1), "ignored", " reload denied\n"),
                "systemctl daemon-reload failed: reload denied",
            ),
            (
                true,
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")),
                "systemctl command failed: failed to spawn systemctl: denied",
            ),
            (
                true,
                output(Some(1), "stdout", "stderr\n"),
                "systemctl command failed: stderr\nstdout",
            ),
            (
                true,
                output(None, "", ""),
                "systemctl command failed: systemctl exited with signal",
            ),
        ] {
            let sandbox = TestSandbox::new();
            let ctx = sandbox.context(InstallMode::System);
            let mut outputs = Vec::new();
            if enable_reached {
                outputs.push(output(Some(0), "", ""));
            }
            outputs.push(result);
            let (mut effects, calls) = service_effects(outputs);
            effects.spawn_error = Some("spawn failed");
            let result = run_with_effects(
                TelemetryRequest::Enable {
                    intent: ExecutionIntent::Apply,
                },
                &ctx,
                &effects,
            )
            .unwrap();
            let TelemetryApplicationOutcome::Applied { result, outcome } = result else {
                panic!("expected applied result");
            };
            let warnings = [
                format!(
                    "could not enable {UNIT_FILENAME} (execution failed: {reason}); falling back to lazy start"
                ),
                "telemetry enabled, but uploader failed to start: spawn failed".to_string(),
            ];
            assert_eq!(outcome.warnings(), &warnings);
            assert_eq!(outcome.status(), &CommandOutcomeStatus::Completed);
            assert_eq!(outcome.operation_id(), None);
            assert_eq!(outcome.changes(), &[TelemetryChange::CollectionEnabled]);
            assert_eq!(
                effects.calls(),
                vec![
                    "require_root",
                    "read_link_id",
                    "enable_collection_anonymous",
                    "systemd_available",
                    "install_and_enable_service",
                    "current_exe",
                    "spawn_uploader"
                ]
            );
            let mut expected_calls = vec![vec!["daemon-reload"]];
            if enable_reached {
                expected_calls.push(vec!["enable", "--now", SERVICE_NAME]);
            }
            assert_eq!(*calls.borrow(), expected_calls);
            assert_eq!(
                super::super::applied_output(&result, outcome.warnings()),
                vec![
                    super::super::TelemetryOutput::Stderr(format!("warn: {}", warnings[0])),
                    super::super::TelemetryOutput::Stderr(format!("warn: {}", warnings[1])),
                    super::super::TelemetryOutput::Stdout(
                        "Telemetry collection enabled.".to_string()
                    ),
                ]
            );
        }
    }

    #[test]
    fn real_service_disable_preserves_deferred_stop_and_missing_unit_behavior() {
        for (stop, disable, status, warning) in [
            (output(Some(0), "", ""), output(Some(0), "", ""), None, None),
            (
                Err(io::Error::new(io::ErrorKind::NotFound, "stop spawn failed")),
                output(Some(0), "", ""),
                None,
                None,
            ),
            (
                output(Some(1), "", "stop failed"),
                output(Some(0), "", ""),
                None,
                None,
            ),
            (output(None, "", ""), output(Some(0), "", ""), None, None),
            (
                output(Some(0), "", ""),
                output(Some(1), "", "missing unit"),
                Some(output(Some(0), "LoadState=not-found\n", "")),
                None,
            ),
            (
                output(Some(0), "", ""),
                output(Some(1), "", "disable denied"),
                Some(output(
                    Some(0),
                    "LoadState=loaded\nActiveState=active\n",
                    "",
                )),
                Some("systemctl command failed: disable denied"),
            ),
            (
                output(Some(0), "", ""),
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")),
                None,
                Some("systemctl command failed: failed to spawn systemctl: denied"),
            ),
            (
                output(Some(0), "", ""),
                output(Some(1), "", "disable denied"),
                Some(Err(io::Error::new(io::ErrorKind::NotFound, "show failed"))),
                Some("systemctl command failed: disable denied"),
            ),
            (
                output(Some(0), "", ""),
                output(None, "", ""),
                Some(output(Some(1), "", "bus unavailable")),
                Some("systemctl command failed: systemctl exited with signal"),
            ),
        ] {
            let sandbox = TestSandbox::new();
            let ctx = sandbox.context(InstallMode::System);
            let mut expected_calls = vec![
                vec!["stop", "--no-block", SERVICE_NAME],
                vec!["disable", SERVICE_NAME],
            ];
            let mut outputs = vec![stop, disable];
            if let Some(status) = status {
                outputs.push(status);
                expected_calls.push(vec![
                    "show",
                    SERVICE_NAME,
                    "--no-pager",
                    "--property=LoadState,ActiveState,UnitFileState,Description",
                ]);
            }
            let (effects, calls) = service_effects(outputs);
            let result = run_with_effects(
                TelemetryRequest::Disable {
                    intent: ExecutionIntent::Apply,
                },
                &ctx,
                &effects,
            )
            .unwrap();
            let TelemetryApplicationOutcome::Applied { result, outcome } = result else {
                panic!("expected applied result");
            };
            assert_eq!(result, TelemetryApplied::Disabled);
            assert_eq!(outcome.status(), &CommandOutcomeStatus::Completed);
            assert_eq!(outcome.operation_id(), None);
            assert_eq!(outcome.changes(), &[TelemetryChange::CollectionDisabled]);
            assert_eq!(
                outcome.warnings(),
                &warning
                    .map(|reason| format!("failed to disable {UNIT_FILENAME}: {reason}"))
                    .into_iter()
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                effects.calls(),
                vec![
                    "require_root",
                    "disable_collection",
                    "systemd_available",
                    "disable_service"
                ]
            );
            assert_eq!(*calls.borrow(), expected_calls);
            assert!(!ctx.layout().systemd_unit_dir.exists());
        }
    }

    #[test]
    fn service_guards_skip_executable_files_and_systemd() {
        for enable in [true, false] {
            for scenario in ["preview", "permission", "no-systemd"] {
                let sandbox = TestSandbox::new();
                let ctx = sandbox.context(InstallMode::System);
                let (mut effects, calls) = service_effects(vec![]);
                effects.root_error = (scenario == "permission").then_some("root required");
                effects.systemd = scenario != "no-systemd";
                let intent = if scenario == "preview" {
                    ExecutionIntent::Plan
                } else {
                    ExecutionIntent::Apply
                };
                let request = if enable {
                    TelemetryRequest::Enable { intent }
                } else {
                    TelemetryRequest::Disable { intent }
                };
                let result = run_with_effects(request, &ctx, &effects);
                match scenario {
                    "preview" => {
                        assert!(matches!(
                            result.unwrap(),
                            TelemetryApplicationOutcome::Preview(_)
                        ));
                        assert!(effects.calls().is_empty());
                    }
                    "permission" => {
                        let error = result.unwrap_err();
                        assert_eq!(error.reason(), "root required");
                        assert_eq!(error.code(), "EXECUTION_FAILED");
                        assert_eq!(error.exit_code(), 1);
                        assert_eq!(effects.calls(), vec!["require_root"]);
                    }
                    "no-systemd" => {
                        let TelemetryApplicationOutcome::Applied { outcome, .. } = result.unwrap()
                        else {
                            panic!("expected applied");
                        };
                        assert!(outcome.warnings().is_empty());
                        assert_eq!(
                            effects.calls(),
                            if enable {
                                vec![
                                    "require_root",
                                    "read_link_id",
                                    "enable_collection_anonymous",
                                    "systemd_available",
                                    "spawn_uploader",
                                ]
                            } else {
                                vec!["require_root", "disable_collection", "systemd_available"]
                            }
                        );
                    }
                    _ => unreachable!(),
                }
                assert!(!ctx.layout().systemd_unit_dir.exists());
                assert!(calls.borrow().is_empty());
            }
        }
    }

    #[test]
    fn service_results_preserve_human_json_and_quiet_output() {
        let (_, module) = module_path!().split_once("::").unwrap();
        for scenario in [
            "enable",
            "disable",
            "enable-failure",
            "disable-failure",
            "preview-enable",
            "preview-disable",
        ] {
            for mode in ["human", "json", "quiet"] {
                // Keep stdout capture out of the process running parallel tests.
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        format!("{module}::service_output_child"),
                        "--exact".to_string(),
                        "--nocapture".to_string(),
                    ])
                    .env("ANOLISA_TEST_TELEMETRY_SERVICE_SCENARIO", scenario)
                    .env("ANOLISA_TEST_TELEMETRY_SERVICE_OUTPUT", mode)
                    .output()
                    .unwrap();
                assert_eq!(
                    output.status.code(),
                    Some(0),
                    "{scenario}/{mode}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let stdout = String::from_utf8(output.stdout).unwrap();
                let (_, rendered) = stdout.split_once("TELEMETRY_OUTPUT_BEGIN\n").unwrap();
                let (rendered, _) = rendered.split_once("TELEMETRY_OUTPUT_END\n").unwrap();
                let stderr = String::from_utf8(output.stderr).unwrap();
                if scenario.starts_with("preview-") {
                    let enable = scenario.ends_with("enable");
                    let message = if enable {
                        "would enable telemetry collection and start the uploader"
                    } else {
                        "would disable telemetry collection and stop the uploader"
                    };
                    match mode {
                        "json" => {
                            let json: serde_json::Value = serde_json::from_str(rendered).unwrap();
                            assert_eq!(json["ok"], true);
                            assert_eq!(json["schema_version"], crate::response::SCHEMA_VERSION);
                            assert_eq!(
                                json["command"],
                                if enable {
                                    "telemetry enable"
                                } else {
                                    "telemetry disable"
                                }
                            );
                            assert_eq!(
                                json["data"],
                                serde_json::json!({"dry_run": true, "message": message})
                            );
                        }
                        "human" => assert_eq!(rendered, format!("[dry-run] {message}\n")),
                        "quiet" => assert!(rendered.is_empty()),
                        _ => unreachable!(),
                    }
                    assert!(stderr.is_empty());
                } else {
                    // Apply historically emits the same text for all output flags.
                    // This injection slice must not change that compatibility behavior.
                    assert_eq!(
                        rendered,
                        if scenario.starts_with("enable") {
                            "Telemetry collection enabled.\n"
                        } else {
                            "Telemetry collection disabled.\n  The uploader stops shortly; buffered logs are preserved.\n"
                        }
                    );
                    assert_eq!(
                        stderr,
                        match scenario {
                            "enable-failure" =>
                                "warn: could not enable anolisa-telemetry.service (execution failed: systemctl daemon-reload failed: reload denied); falling back to lazy start\nwarn: telemetry enabled, but uploader failed to start: spawn failed\n",
                            "disable-failure" =>
                                "warn: failed to disable anolisa-telemetry.service: systemctl command failed: disable denied\n",
                            _ => "",
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn service_output_environment_does_not_short_circuit_suite() {
        let (_, module) = module_path!().split_once("::").unwrap();
        for (scenario, mode) in [("enable", "human"), ("invalid", "invalid")] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "telemetry".to_string(),
                    "--test-threads=1".to_string(),
                    "--nocapture".to_string(),
                    // Avoid recursively spawning this regression test.
                    "--skip".to_string(),
                    format!("{module}::service_output_environment_does_not_short_circuit_suite"),
                ])
                .env("ANOLISA_TEST_TELEMETRY_SERVICE_SCENARIO", scenario)
                .env("ANOLISA_TEST_TELEMETRY_SERVICE_OUTPUT", mode)
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(0),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let stdout = String::from_utf8(output.stdout).unwrap();
            assert!(stdout.contains("test result: ok."), "{stdout}");
            assert!(
                stdout.contains(&format!(
                    "test {module}::service_results_preserve_human_json_and_quiet_output ... ok"
                )),
                "{stdout}"
            );
            assert!(!stdout.contains("TELEMETRY_OUTPUT_BEGIN"), "{stdout}");
        }
    }

    #[test]
    fn service_output_child() {
        let (_, module) = module_path!().split_once("::").unwrap();
        // Environment alone must not select this path in a broader test run.
        if std::env::args().skip(1).collect::<Vec<_>>()
            != [
                format!("{module}::service_output_child"),
                "--exact".to_string(),
                "--nocapture".to_string(),
            ]
        {
            return;
        }
        let Ok(scenario) = std::env::var("ANOLISA_TEST_TELEMETRY_SERVICE_SCENARIO") else {
            return;
        };
        let mode = std::env::var("ANOLISA_TEST_TELEMETRY_SERVICE_OUTPUT").unwrap();
        let sandbox = TestSandbox::new();
        let preview = scenario.starts_with("preview-");
        let ctx = sandbox.context_with(
            InstallMode::System,
            TestContextOptions {
                json: mode == "json",
                quiet: mode == "quiet",
                dry_run: preview,
                ..Default::default()
            },
        );
        let outputs = match scenario.as_str() {
            "enable" | "disable" => vec![output(Some(0), "", ""), output(Some(0), "", "")],
            "enable-failure" => vec![output(Some(1), "ignored stdout", " reload denied\n")],
            "disable-failure" => vec![
                output(Some(0), "", ""),
                output(Some(1), "", "disable denied"),
                output(Some(0), "LoadState=loaded\n", ""),
            ],
            "preview-enable" | "preview-disable" => vec![],
            _ => panic!("unknown service fixture"),
        };
        let (mut effects, calls) = service_effects(outputs);
        effects.spawn_error = Some("spawn failed");
        let intent = if preview {
            ExecutionIntent::Plan
        } else {
            ExecutionIntent::Apply
        };
        let request = if scenario.contains("enable") {
            TelemetryRequest::Enable { intent }
        } else {
            TelemetryRequest::Disable { intent }
        };
        let result = run_with_effects(request, &ctx, &effects).unwrap();
        if preview {
            assert!(effects.calls().is_empty());
            assert!(calls.borrow().is_empty());
            assert!(!ctx.layout().systemd_unit_dir.exists());
        } else if let TelemetryApplicationOutcome::Applied { outcome, .. } = &result {
            assert_eq!(outcome.status(), &CommandOutcomeStatus::Completed);
            assert_eq!(outcome.operation_id(), None);
            if scenario == "enable" {
                assert!(outcome.warnings().is_empty());
                assert_eq!(
                    effects.calls(),
                    vec![
                        "require_root",
                        "read_link_id",
                        "enable_collection_anonymous",
                        "systemd_available",
                        "install_and_enable_service",
                        "current_exe"
                    ]
                );
                assert_eq!(
                    *calls.borrow(),
                    vec![vec!["daemon-reload"], vec!["enable", "--now", SERVICE_NAME]]
                );
            }
        }
        println!("TELEMETRY_OUTPUT_BEGIN");
        super::super::render_mutation(result, &ctx).unwrap();
        println!("TELEMETRY_OUTPUT_END");
        drop(sandbox);
    }
}
