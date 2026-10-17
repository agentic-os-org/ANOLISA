//! Local Hermes chat uses an explicitly installed native plugin in the selected profile.

mod install;
mod preflight;

#[cfg(test)]
use preflight::python_entrypoint;
use preflight::{checked_profile_command, python_args};
#[cfg(test)]
mod tests;

use super::super::{
    adapter::{
        self, Adapter, HookBinding, HookEvent, HookOutput, LaunchContext, LaunchInput, LaunchPlan,
        PreparedLaunch, Readiness,
    },
    read_file, Arguments, Exit, Result,
};
use aw_provider::admission::AdmittedStep;
use aw_service::Capabilities;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf, time::Duration};

const REVISION: &str = "952c941e741e922a9be8fc403c8944c6e96318bb";

pub(crate) struct Hermes;

struct Prepared {
    input: LaunchInput,
    profile: PathBuf,
    python: PathBuf,
    callback_timeout: f64,
}

impl Adapter for Hermes {
    fn prepare(&self, mut input: LaunchInput) -> Result<Box<dyn PreparedLaunch>> {
        if input.flags.contains_key("--native-settings")
            || input.flags.contains_key("--native-state-dir")
        {
            return Err(
                "Hermes uses --native-profile; it has no temporary settings overlay".into(),
            );
        }
        let profile = install::profile(input.flags.get("--native-profile").ok_or(
            "Hermes requires --native-profile pointing to an existing installed profile",
        )?)?;
        check_args(&input.command.args)?;
        install::installed(&profile).map_err(|error| {
            format!(
                "{error}; run aw install --config FILE --agent {} --native-profile {}",
                input.target,
                profile.display()
            )
        })?;
        let (selector, python, cache) = checked_profile_command(&mut input.command, &profile)?;
        let timeout = callback_timeout(&input.command, &python, &selector, cache.path())?;
        cache.close()?;
        input.command.args.insert(1, "--cli".into());
        input
            .command
            .args
            .splice(0..0, ["--profile".into(), selector]);
        Ok(Box::new(Prepared {
            input,
            profile,
            python,
            callback_timeout: timeout,
        }))
    }

    fn normalize(&self, binding: &HookBinding, event: &str, native: &Value) -> Result<Value> {
        if (native["hook_event_name"] != event_name(event)?
            && !(event == "tool.after" && native["hook_event_name"] == "transform_tool_result"))
            || native["cwd"].as_str() != binding.cwd.to_str()
        {
            return Err(
                "Hermes callback event or working directory differs from its binding".into(),
            );
        }
        let session = identifier(&native["session_id"], "session_id")?;
        let call = identifier(&native["extra"]["tool_call_id"], "extra.tool_call_id")?;
        let request = identifier(&native["extra"]["api_request_id"], "extra.api_request_id")?;
        // Hermes only deduplicates model call IDs within one API request.
        let call = format!(
            "hermes:{:x}",
            Sha256::digest(serde_json::to_vec(&(request, call))?)
        );
        let tool = identifier(&native["tool_name"], "tool_name")?;
        if !native["tool_input"].is_object() {
            return Err("Hermes callback requires a tool_input object".into());
        }
        Ok(json!({
            "name": event,
            "agent": {"adapter": "hermes", "binding_id": binding.binding.target,
                      "instance_id": binding.binding.instance_id},
            "session_id": session,
            "tool": {"name": tool, "native_name": tool, "call_id": call,
                     "input": native["tool_input"], "result": native["extra"]["result"]},
            "native": native,
        }))
    }

    fn reply(&self, blocked: bool) -> HookOutput {
        HookOutput {
            exit: Exit::Code(if blocked { 2 } else { 0 }),
            stdout: if blocked {
                br#"{"action":"block","message":"Tool blocked by AW policy"}"#.to_vec()
            } else {
                b"{}".to_vec()
            },
            stderr: Vec::new(),
        }
    }

    fn install(&self, args: &Arguments, config: &aw_config::Configuration) -> Result<Exit> {
        let profile = install::profile(args.required("--native-profile")?)?;
        let argv = config.as_value()["spec"]["agents"][args.required("--agent")?]["argv"]
            .as_array()
            .ok_or("Hermes Agent argv missing")?;
        let program = argv[0]
            .as_str()
            .ok_or("Hermes executable missing from Agent argv")?;
        let native = argv[1..]
            .iter()
            .map(|arg| {
                arg.as_str()
                    .map(OsString::from)
                    .ok_or_else(|| "invalid Hermes Agent argument".into())
            })
            .collect::<Result<Vec<_>>>()?;
        // A bare executable lets aw run supply chat after --.
        if !native.is_empty() {
            check_args(&native)?;
        }
        let mut command = aw_exec::CommandSpec {
            program: program.into(),
            args: native,
            cwd: std::env::current_dir()?.canonicalize()?,
            environment: std::env::vars_os().collect(),
        };
        let (selector, python, cache) = checked_profile_command(&mut command, &profile)?;
        let cancelled = &super::super::run::CANCELLED;
        let backup = install::install(
            &profile,
            cancelled,
            |names| {
                let mut probe = command.clone();
                probe.program = python.as_os_str().into();
                let script = r#"import json, sys
selector, plugin, names = sys.argv[1], sys.argv[2], json.load(sys.stdin)
sys.argv = ['hermes', '--profile', selector, 'config', 'show']
from hermes_cli import main
from hermes_cli.config import _expand_env_vars
print(json.dumps([[index for index, name in enumerate(selection)
    if _expand_env_vars(name) == plugin] for selection in names]))
"#;
                probe.args = python_args(
                    cache.path(),
                    &[
                        "-P".into(),
                        "-c".into(),
                        script.into(),
                        selector.clone(),
                        install::PLUGIN.into(),
                    ],
                );
                let output = aw_exec::run(
                    &probe,
                    &serde_json::to_vec(names)?,
                    aw_exec::Limits {
                        input_bytes: 4 * 1024 * 1024,
                        stdout_bytes: 4 * 1024 * 1024,
                        stderr_bytes: 4096,
                    },
                    std::time::Instant::now() + Duration::from_secs(30),
                    cancelled,
                )?;
                if !output.status.success() {
                    return Err("Hermes native plugin selection expansion failed".into());
                }
                Ok(serde_json::from_slice(&output.stdout)?)
            },
            |candidate, field, names| {
                let mut writer = command.clone();
                writer.program = python.as_os_str().into();
                // Native config set mutates aliased lists in place. Copy the target
                // mapping and replace only the selected list through its YAML editor.
                let script = r#"import json, sys
from pathlib import Path
from utils import _roundtrip_load, _roundtrip_dump
path = Path(sys.argv[1]) / 'config.yaml'
selector, field, names = sys.argv[2], sys.argv[3], json.load(sys.stdin)
sys.argv = ['hermes', '--profile', selector, 'config', 'set', field, json.dumps(names)]
# Resolve management from the selected profile; only the YAML edit targets staging.
from hermes_cli import main
from hermes_cli.config import is_managed, managed_error, _exit_if_key_managed
if is_managed():
    managed_error('set configuration values')
    raise SystemExit(1)
_exit_if_key_managed(field, 'set')
yaml, config = _roundtrip_load(path)
plugins = config.get('plugins')
plugins = plugins.copy() if plugins is not None else yaml.load('{}')
plugins.yaml_set_anchor(None)
plugins[field.split('.')[1]] = names
config['plugins'] = plugins
_roundtrip_dump(path, yaml, config)
"#;
                writer.args = python_args(
                    cache.path(),
                    &[
                        "-P".into(),
                        "-c".into(),
                        script.into(),
                        candidate.as_os_str().into(),
                        selector.clone(),
                        field.into(),
                    ],
                );
                let output = aw_exec::run(
                    &writer,
                    &serde_json::to_vec(names)?,
                    aw_exec::Limits {
                        input_bytes: 4 * 1024 * 1024,
                        stdout_bytes: 16384,
                        stderr_bytes: 16384,
                    },
                    std::time::Instant::now() + Duration::from_secs(30),
                    cancelled,
                )?;
                if !output.status.success() {
                    return Err(format!(
                    "Hermes native config writer failed for {field}; original profile is unchanged"
                )
                    .into());
                }
                Ok(())
            },
        )?;
        installed_output(&profile, backup.as_deref(), &mut std::io::stdout().lock())?;
        cache.close().map_err(|error| {
            format!("Hermes plugin installed, but Python cache cleanup failed: {error}")
        })?;
        Ok(Exit::Code(0))
    }
}

fn installed_output(
    profile: &std::path::Path,
    backup: Option<&std::path::Path>,
    writer: &mut impl std::io::Write,
) -> Result<()> {
    let state = json!({"adapter":"hermes", "profile":profile,
        "plugin":install::PLUGIN, "displaced":backup.map(install::displaced),
        "backup":backup, "status":"installed"});
    writeln!(writer, "{state}").map_err(|error| {
        format!("Hermes plugin installed, but installed-state output failed: {error}; recovery: {state}").into()
    })
}

impl PreparedLaunch for Prepared {
    fn capabilities(&self) -> Capabilities {
        super::super::adapter::catalog::HERMES.capabilities(REVISION, "chat")
    }

    fn validate_steps(&self, steps: &[AdmittedStep]) -> Result<()> {
        for event in ["tool.before", "tool.after"] {
            if !self.native_steps(steps, event).is_empty() {
                let budget = self.budget(event)?;
                if self.callback_timeout > 0.0
                    && (budget.div_ceil(1000) + 4) as f64 > self.callback_timeout
                {
                    return Err("Hermes event budget plus cleanup exceeds native plugins.hook_callback_timeout".into());
                }
            }
        }
        Ok(())
    }

    fn configure(&mut self, context: &LaunchContext<'_>) -> Result<LaunchPlan> {
        let mut events = BTreeMap::new();
        let mut hooks = Vec::new();
        for event in ["tool.before", "tool.after"] {
            let selected = self.native_steps(context.steps, event);
            if selected.is_empty() {
                continue;
            }
            let budget = self.budget(event)?;
            for step in &selected {
                hooks.push(json!({"event":event, "step":step.step_id,
                    "on_error":step.on_error, "timeout":budget.div_ceil(1000) + 4, "replace_result":step.replaces_result}));
            }
            events.insert(
                event.into(),
                HookEvent {
                    budget_ms: budget,
                    replacement_step: selected
                        .iter()
                        .find(|step| step.replaces_result)
                        .map(|step| step.step_id.clone()),
                    steps: selected
                        .into_iter()
                        .map(|step| (step.step_id.clone(), step.on_error.clone()))
                        .collect(),
                },
            );
        }
        let ready = context.files.0.join("hermes-ready.json");
        let token = String::from_utf8(read_file("/proc/sys/kernel/random/uuid", 128)?)?
            .trim()
            .to_owned();
        let launch = json!({"schema":"aw-hermes/v1alpha1", "binary":context.executable.to_str().ok_or("Hermes AW executable path must be UTF-8")?,
            "binding":context.binding_path.to_str().ok_or("Hermes binding path must be UTF-8")?,
            "profile":self.profile.to_str().ok_or("Hermes profile path must be UTF-8")?,
            "cwd":self.input.command.cwd.to_str().ok_or("Hermes working directory must be UTF-8")?,
            "ready_path":ready.to_str().ok_or("Hermes launch directory must be UTF-8")?, "token":token, "hooks":hooks});
        let path = context
            .files
            .write("hermes-launch.json", &serde_json::to_vec(&launch)?)?;
        let mut command = self.input.command.clone();
        let entrypoint = context.files.write(
            "hermes-entrypoint.py",
            include_bytes!("../../../../../adapters/hermes/launch.py"),
        )?;
        command.args.splice(
            0..0,
            [
                entrypoint.into_os_string(),
                command.program.clone().into_os_string(),
            ],
        );
        command.args = python_args(&context.files.0.join("pycache"), &command.args);
        command.program = self.python.as_os_str().into();
        command
            .environment
            .insert("AW_HERMES_LAUNCH".into(), path.into_os_string());
        let provider_environment = command.environment.clone();
        Ok(LaunchPlan {
            command,
            provider_environment,
            events,
            readiness: Some(Readiness {
                path: ready,
                token,
                adapter: "hermes".into(),
                hooks: hooks.len(),
                timeout: Duration::from_secs(30),
            }),
        })
    }
}

impl Prepared {
    fn native_steps(&self, steps: &[AdmittedStep], event: &str) -> Vec<adapter::NativeStep> {
        adapter::native_steps(
            steps,
            event,
            adapter::observation_enabled(&self.input.document, &self.input.target),
        )
    }

    fn budget(&self, event: &str) -> Result<u64> {
        let spec = &self.input.document["spec"];
        let budget = spec["events"][event]
            .get("budget_ms")
            .unwrap_or(&spec["execution"]["default_event_budget_ms"])
            .as_f64()
            .map(|value| value as u64)
            .ok_or("invalid Hermes event budget")?;
        if !(1..=55000).contains(&budget) {
            return Err("Hermes event budgets must be 1..55000 ms".into());
        }
        Ok(budget)
    }
}

fn identifier<'a>(value: &'a Value, label: &str) -> Result<&'a str> {
    value
        .as_str()
        .filter(|value| !value.trim().is_empty() && value.len() <= 512)
        .ok_or_else(|| {
            format!("Hermes callback requires nonempty {label} (up to 512 bytes)").into()
        })
}

fn event_name(event: &str) -> Result<&'static str> {
    match event {
        "tool.before" => Ok("pre_tool_call"),
        "tool.after" => Ok("post_tool_call"),
        _ => Err("unsupported Hermes hook event".into()),
    }
}

fn check_args(args: &[OsString]) -> Result<()> {
    if args.first().is_none_or(|arg| arg != "chat") {
        return Err("Hermes AW currently supports the local chat command only".into());
    }
    for arg in args.iter().skip(1) {
        if arg == "--" {
            break;
        }
        let arg = arg.to_str().ok_or("Hermes arguments must be UTF-8")?;
        let name = arg.split('=').next().unwrap_or(arg);
        if matches!(name, "--help" | "-h") {
            return Err(
                "Hermes chat help exits before hooks are ready; run hermes chat --help directly"
                    .into(),
            );
        }
        // Require complete supported options: argparse accepts abbreviations
        // such as --safe, which would otherwise disable installed plugins.
        if !matches!(
            name,
            "--query"
                | "-q"
                | "--query-file"
                | "--oneshot"
                | "--image"
                | "--model"
                | "-m"
                | "--toolsets"
                | "-t"
                | "--reasoning"
                | "--skills"
                | "-s"
                | "--provider"
                | "--verbose"
                | "-v"
                | "--quiet"
                | "-Q"
                | "--format"
                | "--accept-hooks"
                | "--checkpoints"
                | "--max-turns"
                | "--run-budget"
                | "--yolo"
                | "--pass-session-id"
                | "--ignore-rules"
                | "--source"
                | "--cli"
        ) && arg.starts_with('-')
        {
            return Err(format!(
                "Hermes option {name} is unsupported; use complete local chat options"
            )
            .into());
        }
    }
    Ok(())
}

fn callback_timeout(
    command: &aw_exec::CommandSpec,
    python: &std::path::Path,
    selector: &std::ffi::OsStr,
    cache: &std::path::Path,
) -> Result<f64> {
    let mut probe = command.clone();
    probe.program = python.into();
    // Importing the native CLI selects the profile and loads its dotenv before
    // the native resolver applies YAML 1.1, environment expansion and limits.
    let script = r#"import json, sys
sys.argv = [sys.argv[1], '--profile', sys.argv[2], 'config', 'get', 'plugins.hook_callback_timeout']
from hermes_cli import main
from hermes_cli.plugins import _resolve_hook_callback_timeout
print(json.dumps(_resolve_hook_callback_timeout(), allow_nan=False))
"#;
    let value = adapter::version_output_with_timeout(
        &probe,
        &python_args(
            cache,
            &[
                "-P".into(),
                "-c".into(),
                script.into(),
                command.program.as_os_str().into(),
                selector.into(),
            ],
        ),
        Duration::from_secs(30),
    )
    .map_err(|error| format!("Hermes callback timeout probe failed: {error}"))?;
    let timeout: f64 = serde_json::from_str(&value)?;
    if !timeout.is_finite() || timeout < 0.0 {
        return Err("Hermes hook_callback_timeout must be nonnegative and finite".into());
    }
    Ok(timeout)
}
