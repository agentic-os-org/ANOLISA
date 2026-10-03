//! Local Hermes chat uses an explicitly installed native plugin in the selected profile.

mod install;
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
        if std::fs::symlink_metadata(profile.join(".container-mode")).is_ok() {
            return Err(
                "Hermes managed-container profiles are not supported by the local adapter".into(),
            );
        }
        check_args(&input.command.args)?;
        if input
            .command
            .environment
            .get(&OsString::from("HERMES_SAFE_MODE"))
            .is_some_and(|value| {
                matches!(
                    value.to_string_lossy().trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
        {
            return Err("HERMES_SAFE_MODE disables the AW native plugin".into());
        }
        let config = install::installed(&profile).map_err(|error| {
            format!(
                "{error}; run aw install --config FILE --agent {} --native-profile {}",
                input.target,
                profile.display()
            )
        })?;
        let timeout = config
            .get("plugins")
            .and_then(|plugins| plugins.get("hook_callback_timeout"))
            .map(|timeout| {
                timeout
                    .as_f64()
                    .ok_or("Hermes hook_callback_timeout must be numeric")
            })
            .transpose()?
            .unwrap_or(30.0);
        if !timeout.is_finite() || timeout < 0.0 {
            return Err("Hermes hook_callback_timeout must be nonnegative and finite".into());
        }
        let selector = checked_profile_command(&mut input.command, &profile)?;
        input.command.args.insert(1, "--cli".into());
        input
            .command
            .args
            .splice(0..0, ["--profile".into(), selector]);
        Ok(Box::new(Prepared {
            input,
            profile,
            callback_timeout: timeout.min(600.0),
        }))
    }

    fn normalize(&self, binding: &HookBinding, event: &str, native: &Value) -> Result<Value> {
        if native["hook_event_name"] != event_name(event)?
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

    fn install(&self, args: &Arguments) -> Result<Exit> {
        let profile = install::profile(args.required("--native-profile")?)?;
        let bytes = read_file(args.required("--config")?, 4 * 1024 * 1024)?;
        let config = aw_config::Validator::new()?.parse(&bytes)?;
        let program = config.as_value()["spec"]["agents"][args.required("--agent")?]["argv"][0]
            .as_str()
            .ok_or("Hermes executable missing from Agent argv")?;
        let mut command = aw_exec::CommandSpec {
            program: program.into(),
            args: Vec::new(),
            cwd: std::env::current_dir()?.canonicalize()?,
            environment: std::env::vars_os().collect(),
        };
        checked_profile_command(&mut command, &profile)?;
        let backup = install::install(&profile, |candidate, field, names| {
            let mut writer = command.clone();
            writer
                .environment
                .insert("HERMES_HOME".into(), candidate.as_os_str().into());
            writer.args = vec![
                "--profile".into(),
                "default".into(),
                "config".into(),
                "set".into(),
                field.into(),
                serde_json::to_string(names)?.into(),
            ];
            let output = aw_exec::run(
                &writer,
                &[],
                aw_exec::Limits {
                    input_bytes: 0,
                    stdout_bytes: 16384,
                    stderr_bytes: 16384,
                },
                std::time::Instant::now() + Duration::from_secs(30),
                &std::sync::atomic::AtomicBool::new(false),
            )?;
            if !output.status.success() {
                return Err(format!(
                    "Hermes native config writer failed for {field}; original profile is unchanged"
                )
                .into());
            }
            Ok(())
        })?;
        println!(
            "{}",
            json!({"adapter":"hermes", "profile":profile,
            "plugin":install::PLUGIN, "backup":backup, "status":"installed"})
        );
        Ok(Exit::Code(0))
    }
}

impl PreparedLaunch for Prepared {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            adapter: "hermes".into(),
            version: REVISION.into(),
            entrypoint: "chat".into(),
            events: BTreeMap::from([
                ("tool.before".into(), vec!["observe".into(), "block".into()]),
                ("tool.after".into(), vec!["observe".into()]),
            ]),
        }
    }

    fn validate_steps(&self, steps: &[AdmittedStep]) -> Result<()> {
        if steps.is_empty() {
            return Err("Hermes target has no enabled tool.before or tool.after steps".into());
        }
        for event in ["tool.before", "tool.after"] {
            if steps.iter().any(|step| step.event == event) {
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
            let selected: Vec<_> = context
                .steps
                .iter()
                .filter(|step| step.event == event)
                .collect();
            if selected.is_empty() {
                continue;
            }
            let budget = self.budget(event)?;
            for step in &selected {
                hooks.push(json!({"event":event, "step":step.step_id,
                    "on_error":step.on_error, "timeout":budget.div_ceil(1000) + 4}));
            }
            events.insert(
                event.into(),
                HookEvent {
                    budget_ms: budget,
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
        let launch = json!({"schema":"aw-hermes/v1alpha1", "binary":context.executable,
            "binding":context.binding_path, "profile":self.profile, "cwd":self.input.command.cwd,
            "ready_path":ready, "token":token, "hooks":hooks});
        let path = context
            .files
            .write("hermes-launch.json", &serde_json::to_vec(&launch)?)?;
        let mut command = self.input.command.clone();
        command
            .environment
            .insert("AW_HERMES_LAUNCH".into(), path.into_os_string());
        let provider_environment = adapter::environment(&command)?;
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
                | "--help"
                | "-h"
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

fn checked_profile_command(
    command: &mut aw_exec::CommandSpec,
    profile: &std::path::Path,
) -> Result<OsString> {
    command
        .environment
        .insert("HERMES_HOME".into(), profile.as_os_str().into());
    // Hermes follows active_profile even with a root HERMES_HOME. An explicit
    // native selector pins the same directory that AW inspected above.
    let selector = if profile
        .parent()
        .is_some_and(|parent| parent.file_name().is_some_and(|name| name == "profiles"))
    {
        profile
            .file_name()
            .ok_or("Hermes profile has no name")?
            .to_os_string()
    } else {
        OsString::from("default")
    };
    let version = adapter::version_output_with_timeout(
        command,
        &["--profile".into(), selector.clone(), "--version".into()],
        Duration::from_secs(60),
    )?;
    let source = version
        .lines()
        .find_map(|line| line.strip_prefix("Install directory: "))
        .ok_or("Hermes version output has no installation directory")?;
    let source = PathBuf::from(source).canonicalize()?;
    let mut identity = command.clone();
    identity.program = "git".into();
    identity.cwd = source.clone();
    for key in ["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR"] {
        identity.environment.remove(&OsString::from(key));
    }
    let revision = adapter::version_output(
        &identity,
        &[
            "-C".into(),
            source.into_os_string(),
            "rev-parse".into(),
            "HEAD".into(),
        ],
    )?;
    if revision != REVISION {
        return Err(
            format!("Hermes official checkout {REVISION} is required by this adapter").into(),
        );
    }
    Ok(selector)
}
