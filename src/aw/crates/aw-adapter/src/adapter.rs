//! Native dialects own registration and response semantics; the launcher owns lifetimes.

pub(crate) mod catalog;

#[cfg(test)]
use crate::Exit;
use crate::Result;
use aw_exec::CommandSpec;
use aw_provider::admission::{AdapterCapabilities, AdmittedStep};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

/// Minimum admitted context required to normalize a native callback.
#[derive(Debug)]
pub struct NativeContext {
    /// Configuration target selected by the launcher.
    pub target: String,
    /// Service-issued instance identity.
    pub instance_id: String,
    /// Canonical working directory bound to the instance.
    pub cwd: PathBuf,
}

/// Event registration returned to the launcher for its private Hook binding.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookEvent {
    /// Admitted whole-event budget.
    pub budget_ms: u64,
    /// Final step allowed to replace a native text result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement_step: Option<String>,
    /// Step IDs and their configured error actions.
    pub steps: BTreeMap<String, String>,
}

/// Native overrides parsed by the CLI, independent of its option syntax.
#[derive(Default)]
pub struct NativeOptions {
    /// Preinstall a stable bridge for explicit idle-only policy updates.
    pub reloadable: bool,
    /// Existing native settings file.
    pub settings: Option<String>,
    /// Existing native profile directory.
    pub profile: Option<String>,
    /// Native persistent state directory.
    pub state_dir: Option<String>,
}

impl NativeOptions {
    pub(crate) fn is_empty(&self) -> bool {
        self.settings.is_none() && self.profile.is_none() && self.state_dir.is_none()
    }
}

/// Selection for persistent native registration.
pub struct InstallOptions {
    /// Configured Agent target.
    pub target: String,
    /// Native profile supplied by the user.
    pub profile: Option<String>,
}

/// Validated configuration and literal native command for one launch.
pub struct LaunchInput {
    /// Configuration snapshot already validated by aw-config.
    pub document: Value,
    /// Selected Agent target.
    pub target: String,
    /// Framework-specific overrides, without CLI or RPC data.
    pub options: NativeOptions,
    /// Command and environment whose lifetime the launcher will own.
    pub command: CommandSpec,
}

/// Write access to a private launch directory owned and cleaned by the caller.
pub trait LaunchFiles {
    /// Directory used for generated bridge configuration and assets.
    fn directory(&self) -> &Path;
    /// Create a private file without replacing an existing path.
    fn write(&self, name: &str, bytes: &[u8]) -> Result<PathBuf> {
        use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt};
        let path = self.directory().join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(path)
    }
}

/// Caller-owned resources supplied after capability admission.
pub struct LaunchContext<'a> {
    /// Generated-file access; ownership remains with the launcher.
    pub files: &'a dyn LaunchFiles,
    /// Private binding path; the Adapter does not read the service binding.
    pub binding_path: &'a Path,
    /// AW executable used by native callbacks.
    pub executable: &'a Path,
    /// Provider steps admitted for this instance.
    pub steps: &'a [AdmittedStep],
}

/// Native registration and command for the launcher to execute.
pub struct LaunchPlan {
    /// Configured native process.
    pub command: CommandSpec,
    /// Environment available to Providers bound to this instance.
    pub provider_environment: BTreeMap<OsString, OsString>,
    /// Callback routes to persist in the launcher's private binding.
    pub events: BTreeMap<String, HookEvent>,
    /// Optional native registration acknowledgement.
    pub readiness: Option<Readiness>,
}

/// Native acknowledgement expected before launch is considered ready.
pub struct Readiness {
    /// Private acknowledgement file.
    pub path: PathBuf,
    /// Per-launch token checked by the launcher.
    pub token: String,
    /// Expected integration name.
    pub adapter: String,
    /// Number of registered callbacks.
    pub hooks: usize,
    /// Maximum acknowledgement wait.
    pub timeout: Duration,
}

/// Prepared native integration, completed only after Provider admission.
pub trait PreparedLaunch {
    /// Runtime capabilities supported by this exact native launch.
    fn capabilities(&self) -> AdapterCapabilities;
    /// Check native limits against the admitted steps.
    fn validate_steps(&self, steps: &[AdmittedStep]) -> Result<()>;
    /// Materialize bridge registration in the caller-owned directory.
    fn configure(&mut self, context: &LaunchContext<'_>) -> Result<LaunchPlan>;
    /// Release integration-owned files outside the common launch directory.
    fn finish(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Encoded native response; the CLI owns writing and process exit.
pub struct HookOutput {
    /// Native callback exit code.
    pub exit: crate::NativeExit,
    /// Native response on stdout.
    pub stdout: Vec<u8>,
    /// Native diagnostic or denial text on stderr.
    pub stderr: Vec<u8>,
}

// Preserve each dialect's existing blocking exit semantics while carrying a
// Provider suggestion to the model. Never emit an approval or updated arguments.
/// Add a bounded retry suggestion while preserving native denial semantics.
pub fn block_reason(adapter: &str, reply: &mut HookOutput, text: &str) -> Result<()> {
    if text.is_empty() || text.len() > aw_provider::MAX_MESSAGE_BYTES {
        return Err("block reason must be nonempty and bounded".into());
    }
    match adapter {
        "qoder" | "qwenpaw" | "agentscope" => reply.stderr = format!("{text}\n").into_bytes(),
        "claude-sdk" => reply.stdout = serde_json::to_vec(text)?,
        "openclaw" | "hermes" | "pi" => {
            let mut value: Value = serde_json::from_slice(&reply.stdout)?;
            value[if adapter == "openclaw" {
                "blockReason"
            } else if adapter == "pi" {
                "reason"
            } else {
                "message"
            }] = serde_json::json!(text);
            reply.stdout = serde_json::to_vec(&value)?;
        }
        _ => return Err("unsupported block reason adapter".into()),
    }
    Ok(())
}

/// Native dialect implementation; no service state or RPC ownership crosses this port.
pub trait Adapter: Sync {
    /// Validate native configuration and probe the selected runtime with cancellation.
    fn prepare(
        &self,
        input: LaunchInput,
        cancelled: &AtomicBool,
    ) -> Result<Box<dyn PreparedLaunch>>;
    /// Normalize a callback using only its admitted target, instance and directory.
    fn normalize(&self, context: &NativeContext, event: &str, native: &Value) -> Result<Value>;
    /// Encode the native allow/deny response without granting native permission.
    fn reply(&self, blocked: bool) -> HookOutput;
    /// Install persistent native registration for integrations that require it.
    fn install(
        &self,
        _options: &InstallOptions,
        _config: &aw_config::Configuration,
        _cancelled: &AtomicBool,
    ) -> Result<()> {
        Err("this adapter has no persistent installation command".into())
    }
}

/// Select a compiled native integration by its configured name.
pub fn select(name: &str) -> Result<&'static dyn Adapter> {
    catalog::PROFILES
        .iter()
        .find(|profile| profile.name == name)
        .map(|profile| profile.adapter)
        .ok_or_else(|| format!("unsupported launcher adapter: {name}").into())
}

pub(crate) struct Probe<'a>(pub(crate) &'a AtomicBool);

impl Probe<'_> {
    pub(crate) fn version_output(
        &self,
        command: &CommandSpec,
        args: &[OsString],
    ) -> Result<String> {
        self.version_output_with_timeout(command, args, Duration::from_secs(5))
    }

    pub(crate) fn version_output_with_timeout(
        &self,
        command: &CommandSpec,
        args: &[OsString],
        timeout: Duration,
    ) -> Result<String> {
        if timeout.is_zero() || timeout > Duration::from_secs(60) {
            return Err("native version probe deadline must be within 1..60000 ms".into());
        }
        let mut probe = command.clone();
        probe.args = args.to_vec();
        let output = aw_exec::run(
            &probe,
            &[],
            aw_exec::Limits {
                input_bytes: 0,
                stdout_bytes: 4096,
                stderr_bytes: 4096,
            },
            Instant::now() + timeout,
            self.0,
        )?;
        if !output.status.success() {
            return Err("native version probe failed".into());
        }
        Ok(std::str::from_utf8(&output.stdout)?.trim().into())
    }
}

// This registration ID cannot name a configured step (configuration IDs are names).
/// Synthetic observation route; cannot collide with configured step names.
pub const OBSERVE_STEP: &str = "$aw.observe";

pub(super) fn observation_enabled(document: &Value, target: &str) -> bool {
    let evidence = &document["spec"]["agents"][target]["evidence"];
    evidence.is_object() && evidence["mode"] != "off"
}

#[derive(Clone)]
pub(super) struct NativeStep {
    pub step_id: String,
    pub on_error: String,
    pub replaces_result: bool,
}

pub(super) fn native_steps(steps: &[AdmittedStep], event: &str, observe: bool) -> Vec<NativeStep> {
    let mut selected: Vec<_> = steps
        .iter()
        .filter(|step| step.event == event)
        .map(|step| NativeStep {
            step_id: step.step_id.clone(),
            on_error: step.on_error.clone(),
            replaces_result: step.execution.replaces_result(),
        })
        .collect();
    if selected.is_empty() && observe {
        selected.push(NativeStep {
            step_id: OBSERVE_STEP.into(),
            on_error: "report".into(),
            replaces_result: false,
        });
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_reasons_preserve_every_dialects_denial_and_never_emit_updates() {
        let message = "Retry: literal ; $(touch NEVER) ' spaces\nnot executable";
        for name in [
            "qoder",
            "qwenpaw",
            "hermes",
            "openclaw",
            "pi",
            "claude-sdk",
            "agentscope",
        ] {
            let mut reply = select(name).unwrap().reply(true);
            let was_success = matches!(reply.exit, Exit::Code(0));
            block_reason(name, &mut reply, message).unwrap();
            assert_eq!(matches!(reply.exit, Exit::Code(0)), was_success);
            if matches!(name, "qoder" | "qwenpaw" | "agentscope") {
                assert!(matches!(reply.exit, Exit::Code(2)));
                assert!(reply.stdout.is_empty());
                assert_eq!(reply.stderr, format!("{message}\n").as_bytes());
            } else if name == "claude-sdk" {
                assert!(matches!(reply.exit, Exit::Code(2)));
                assert!(reply.stderr.is_empty());
                assert_eq!(
                    serde_json::from_slice::<String>(&reply.stdout).unwrap(),
                    message
                );
            } else {
                let value: Value = serde_json::from_slice(&reply.stdout).unwrap();
                if name == "openclaw" {
                    assert_eq!(
                        value,
                        serde_json::json!({"block":true,"blockReason":message})
                    );
                } else if name == "pi" {
                    assert_eq!(value, serde_json::json!({"block":true,"reason":message}));
                } else {
                    assert_eq!(
                        value,
                        serde_json::json!({"action":"block","message":message})
                    );
                }
            }
        }
    }
}
