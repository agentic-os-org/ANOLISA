//! Native dialects own registration and response semantics; the launcher owns lifetimes.

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
    sync::atomic::AtomicBool,
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
    pub provider_environment: BTreeMap<String, String>,
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

pub(super) trait Adapter: Sync {
    fn prepare(&self, input: LaunchInput) -> Result<Box<dyn PreparedLaunch>>;
    fn normalize(&self, binding: &HookBinding, event: &str, native: &Value) -> Result<Value>;
    fn reply(&self, blocked: bool) -> HookOutput;
    fn install(&self, _args: &Arguments) -> Result<Exit> {
        Err("this adapter has no persistent installation command".into())
    }
}

pub(super) fn select(name: &str) -> Result<&'static dyn Adapter> {
    match name {
        "qoder" => Ok(&super::qoder::Qoder),
        "qwenpaw" => Ok(&super::adapters::qwenpaw::QwenPaw),
        _ => Err(format!("unsupported launcher adapter: {name}").into()),
    }
}

pub(super) fn install(args: &Arguments) -> Result<Exit> {
    args.check(&["--config", "--agent", "--native-profile"], false)?;
    let bytes = read_file(args.required("--config")?, 4 * 1024 * 1024)?;
    let config = aw_config::Validator::new()?.parse(&bytes)?;
    let target = args.required("--agent")?;
    let name = config.as_value()["spec"]["agents"][target]["adapter"]
        .as_str()
        .ok_or("configured Agent target not found")?;
    select(name)?.install(args)
}

pub(super) fn environment(command: &CommandSpec) -> Result<BTreeMap<String, String>> {
    command
        .environment
        .iter()
        .map(|(key, value)| {
            Ok((
                key.to_str().ok_or("non-UTF-8 environment key")?.into(),
                value.to_str().ok_or("non-UTF-8 environment value")?.into(),
            ))
        })
        .collect()
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
        &AtomicBool::new(false),
    )?;
    if !output.status.success() {
        return Err("native version probe failed".into());
    }
    Ok(std::str::from_utf8(&output.stdout)?.trim().into())
}
