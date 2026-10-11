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
        let (selector, python) = checked_profile_command(&mut input.command, &profile)?;
        let timeout = callback_timeout(&input.command, &python, &selector)?;
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
        checked_profile_command(&mut command, &profile)?;
        let cancelled = &super::super::run::CANCELLED;
        let backup = install::install(&profile, cancelled, |candidate, field, names| {
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
                cancelled,
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
            "plugin":install::PLUGIN, "displaced":backup.as_deref().map(install::displaced),
            "backup":backup, "status":"installed"})
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

fn checked_profile_command(
    command: &mut aw_exec::CommandSpec,
    profile: &std::path::Path,
) -> Result<(OsString, PathBuf)> {
    let python = python_entrypoint(command)?;
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
    let changes = adapter::version_output(
        &identity,
        &[
            "status".into(),
            "--porcelain".into(),
            "--untracked-files=no".into(),
        ],
    )?;
    if !changes.is_empty() {
        return Err(
            "Hermes official checkout must have no staged or unstaged tracked changes".into(),
        );
    }
    Ok((selector, python))
}

fn python_entrypoint(command: &aw_exec::CommandSpec) -> Result<PathBuf> {
    use std::{
        io::Read,
        os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    };
    let can_execute = |path: &std::path::Path| {
        let Ok(name) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: name is live; check execute access as the native child identity.
        path.is_file()
            && unsafe {
                libc::faccessat(libc::AT_FDCWD, name.as_ptr(), libc::X_OK, libc::AT_EACCESS) == 0
            }
    };
    let program = std::path::Path::new(&command.program);
    let executable = if program.components().count() > 1 {
        command.cwd.join(program)
    } else {
        std::env::split_paths(
            command
                .environment
                .get(&OsString::from("PATH"))
                .ok_or("Hermes executable requires PATH")?,
        )
        .map(|path| command.cwd.join(path).join(program))
        .find(|path| can_execute(path))
        .ok_or("Hermes executable was not found in PATH")?
    };
    if !can_execute(&executable) {
        return Err(format!(
            "Hermes entrypoint must be an executable regular file: {}",
            executable.display()
        )
        .into());
    }
    // A path can be replaced after lookup; never block opening a substituted FIFO.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&executable)?;
    if !file.metadata()?.is_file() {
        return Err("Hermes entrypoint must be a regular file".into());
    }
    let mut script = Vec::new();
    file.take(65537).read_to_end(&mut script)?;
    if script.len() > 65536 {
        return Err("Hermes entrypoint exceeds 65536 bytes".into());
    }
    let script = std::str::from_utf8(&script)?;
    let python = script
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("#!"))
        .map(std::path::Path::new)
        .filter(|path| {
            path.is_absolute()
                && path.is_file()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("python"))
        })
        .ok_or("Hermes requires its installed Python console-script entrypoint")?;
    if !script.contains("from hermes_cli.main import main") {
        return Err("Hermes requires its installed Python console-script entrypoint".into());
    }
    Ok(python.to_path_buf())
}

fn callback_timeout(
    command: &aw_exec::CommandSpec,
    python: &std::path::Path,
    selector: &std::ffi::OsStr,
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
        &[
            "-c".into(),
            script.into(),
            command.program.as_os_str().into(),
            selector.into(),
        ],
        Duration::from_secs(30),
    )
    .map_err(|error| format!("Hermes callback timeout probe failed: {error}"))?;
    let timeout: f64 = serde_json::from_str(&value)?;
    if !timeout.is_finite() || timeout < 0.0 {
        return Err("Hermes hook_callback_timeout must be nonnegative and finite".into());
    }
    Ok(timeout)
}
