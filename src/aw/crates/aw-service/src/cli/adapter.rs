//! Native dialects own registration and response semantics; the launcher owns lifetimes.

pub(super) mod catalog;

use super::{read_file, run::Artifacts, Arguments, Exit, Result};
use aw_exec::CommandSpec;
use aw_provider::admission::AdmittedStep;
use aw_service::{Binding, Capabilities};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HookBinding {
    pub adapter: String,
    pub binding: Binding,
    pub socket: PathBuf,
    pub cwd: PathBuf,
    pub events: BTreeMap<String, HookEvent>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HookEvent {
    pub budget_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement_step: Option<String>,
    pub steps: BTreeMap<String, String>,
}

pub(super) struct LaunchInput {
    pub document: Value,
    pub target: String,
    pub flags: BTreeMap<String, String>,
    pub command: CommandSpec,
}

pub(super) struct LaunchContext<'a> {
    pub files: &'a Artifacts,
    pub binding_path: &'a Path,
    pub executable: &'a Path,
    pub steps: &'a [AdmittedStep],
}

pub(super) struct LaunchPlan {
    pub command: CommandSpec,
    pub provider_environment: BTreeMap<OsString, OsString>,
    pub events: BTreeMap<String, HookEvent>,
    pub readiness: Option<Readiness>,
}

pub(super) struct Readiness {
    pub path: PathBuf,
    pub token: String,
    pub adapter: String,
    pub hooks: usize,
    pub timeout: Duration,
}

pub(super) trait PreparedLaunch {
    fn capabilities(&self) -> Capabilities;
    fn validate_steps(&self, steps: &[AdmittedStep]) -> Result<()>;
    fn configure(&mut self, context: &LaunchContext<'_>) -> Result<LaunchPlan>;
    // Adapters with owned files outside the launch directory report removal errors.
    fn finish(&mut self) -> Result<()> {
        Ok(())
    }
}

pub(super) struct HookOutput {
    pub exit: Exit,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

// Preserve each dialect's existing blocking exit semantics while carrying a
// Provider suggestion to the model. Never emit an approval or updated arguments.
pub(super) fn block_reason(adapter: &str, reply: &mut HookOutput, text: &str) -> Result<()> {
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

pub(super) trait Adapter: Sync {
    fn prepare(&self, input: LaunchInput) -> Result<Box<dyn PreparedLaunch>>;
    fn normalize(&self, binding: &HookBinding, event: &str, native: &Value) -> Result<Value>;
    fn reply(&self, blocked: bool) -> HookOutput;
    fn install(&self, _args: &Arguments, _config: &aw_config::Configuration) -> Result<Exit> {
        Err("this adapter has no persistent installation command".into())
    }
}

pub(super) fn select(name: &str) -> Result<&'static dyn Adapter> {
    catalog::PROFILES
        .iter()
        .find(|profile| profile.name == name)
        .map(|profile| profile.adapter)
        .ok_or_else(|| format!("unsupported launcher adapter: {name}").into())
}

pub(super) fn install(args: &Arguments) -> Result<Exit> {
    args.check(&["--config", "--agent", "--native-profile"], false)?;
    let bytes = read_file(args.required("--config")?, 4 * 1024 * 1024)?;
    let config = aw_config::Validator::new()?.parse(&bytes)?;
    let target = args.required("--agent")?;
    let name = config.as_value()["spec"]["agents"][target]["adapter"]
        .as_str()
        .ok_or("configured Agent target not found")?;
    let adapter = select(name)?;
    super::run::install_signals()?;
    match adapter.install(args, &config) {
        Ok(exit) => Ok(super::run::interrupted().unwrap_or(exit)),
        Err(error) => super::run::interrupted_error(error),
    }
}

pub(super) fn version_output(command: &CommandSpec, args: &[OsString]) -> Result<String> {
    version_output_with_timeout(command, args, Duration::from_secs(5))
}

pub(super) fn version_output_with_timeout(
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
        &super::run::CANCELLED,
    )?;
    if !output.status.success() {
        return Err("native version probe failed".into());
    }
    Ok(std::str::from_utf8(&output.stdout)?.trim().into())
}

// This registration ID cannot name a configured step (configuration IDs are names).
pub(super) const OBSERVE_STEP: &str = "$aw.observe";

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
