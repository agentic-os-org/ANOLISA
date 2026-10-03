//! Qoder CLI 1.1.64 native command hooks; scheduling remains in the Agent.

use super::{
    adapter::{
        self, Adapter, HookBinding, HookEvent, HookOutput, LaunchContext, LaunchInput, LaunchPlan,
        PreparedLaunch,
    },
    read_file, Exit, Result,
};
use aw_provider::admission::AdmittedStep;
use aw_service::Capabilities;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

pub(super) const VERSION: &str = "1.1.64";

pub(super) struct Qoder;

struct Prepared {
    input: LaunchInput,
    settings: Value,
    native: Vec<String>,
}

impl Adapter for Qoder {
    fn prepare(&self, input: LaunchInput) -> Result<Box<dyn PreparedLaunch>> {
        for flag in input.flags.keys() {
            if !matches!(flag.as_str(), "--config" | "--agent" | "--native-settings") {
                return Err(format!("Qoder does not support {flag}").into());
            }
        }
        let native = input
            .command
            .args
            .iter()
            .map(|arg| {
                arg.to_str()
                    .map(str::to_owned)
                    .ok_or("non-UTF-8 native argument")
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        check_args(&native)?;
        let settings = settings(input.flags.get("--native-settings"))?;
        check_sources(&input.command.cwd, &settings)?;
        if adapter::version_output(&input.command, &["--version".into()])? != VERSION {
            return Err(format!("Qoder CLI {VERSION} is required by this adapter").into());
        }
        Ok(Box::new(Prepared {
            input,
            settings,
            native,
        }))
    }

    fn normalize(&self, binding: &HookBinding, event: &str, native: &Value) -> Result<Value> {
        normalize(binding, event, native)
    }

    fn reply(&self, blocked: bool) -> HookOutput {
        HookOutput {
            exit: Exit::Code(if blocked { 2 } else { 0 }),
            stdout: if blocked { vec![] } else { b"{}".to_vec() },
            stderr: if blocked {
                b"Tool blocked by AW policy\n".to_vec()
            } else {
                vec![]
            },
        }
    }
}

impl PreparedLaunch for Prepared {
    fn capabilities(&self) -> Capabilities {
        capabilities(&self.native)
    }

    fn validate_steps(&self, steps: &[AdmittedStep]) -> Result<()> {
        generate(
            self.settings.clone(),
            &self.input.document,
            &self.input.target,
            steps,
            Path::new("/aw"),
            Path::new("/binding.json"),
        )?;
        Ok(())
    }

    fn configure(&mut self, context: &LaunchContext<'_>) -> Result<LaunchPlan> {
        let (settings, events) = generate(
            self.settings.clone(),
            &self.input.document,
            &self.input.target,
            context.steps,
            context.executable,
            context.binding_path,
        )?;
        let path = context
            .files
            .write("settings.json", &serde_json::to_vec(&settings)?)?;
        let mut command = self.input.command.clone();
        command.args = vec![OsString::from("--settings"), path.into_os_string()];
        command.args.extend(self.native.iter().map(OsString::from));
        let mut environment = command.environment.clone();
        for name in ["QODER_PROJECT_DIR", "CLAUDE_PROJECT_DIR"] {
            environment.insert(
                name.into(),
                command
                    .cwd
                    .to_str()
                    .ok_or("non-UTF-8 working directory")?
                    .into(),
            );
        }
        let source = if environment
            .get(std::ffi::OsStr::new("QODER_WORK_INTEGRATION_MODE"))
            .is_some_and(|v| v == "1")
        {
            "qoderwork"
        } else {
            "cli"
        };
        let version = environment
            .get(std::ffi::OsStr::new("QODER_CLIENT_VERSION"))
            .filter(|v| !v.is_empty())
            .cloned()
            .unwrap_or_else(|| VERSION.into());
        environment.insert("QODER_HOOK_SOURCE".into(), source.into());
        environment.insert("QODER_HOOK_VERSION".into(), version);
        environment.insert("QODER_SITE".into(), "GLOBAL".into());
        Ok(LaunchPlan {
            command,
            provider_environment: environment,
            events,
            readiness: None,
        })
    }
}

pub(super) fn capabilities(args: &[String]) -> Capabilities {
    Capabilities {
        adapter: "qoder".into(),
        version: VERSION.into(),
        entrypoint: if args
            .iter()
            .take_while(|arg| arg.as_str() != "--")
            .any(|arg| matches!(arg.as_str(), "-p" | "--print"))
        {
            "print"
        } else {
            "tui"
        }
        .into(),
        events: BTreeMap::from([
            ("tool.before".into(), vec!["observe".into(), "block".into()]),
            ("tool.after".into(), vec!["observe".into()]),
        ]),
    }
}

pub(super) fn check_args(args: &[String]) -> Result<()> {
    for arg in args {
        if arg == "--" {
            break;
        }
        let name = arg.split('=').next().unwrap_or(arg);
        if matches!(
            name,
            "--settings"
                | "--setting-sources"
                | "--headless-fast-hooks"
                | "--bare"
                | "--cwd"
                | "-w"
                | "--worktree"
                | "--remote"
                | "--remote-session"
                | "--teleport"
                | "--remote-control"
                | "remote-control"
                | "--config-dir"
                | "--resume"
                | "-r"
                | "--continue"
                | "-c"
        ) || (arg.starts_with('-') && !arg.starts_with("--") && arg.len() > 2)
        {
            return Err(format!("Qoder option {name} is not supported by this launcher; use --native-settings for flag settings").into());
        }
    }
    if std::env::var("QODER_HEADLESS_FAST_HOOKS").is_ok_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    }) {
        return Err("QODER_HEADLESS_FAST_HOOKS disables native hooks".into());
    }
    Ok(())
}

pub(super) fn enabled(settings: &Value) -> Result<()> {
    if !settings.is_object() {
        return Err("Qoder settings must be a JSON object".into());
    }
    if settings["disableAllHooks"] == true
        || settings["hooksConfig"]["enabled"] == false
        || settings["hooksConfig"]["headlessFastMode"] == true
    {
        return Err("Qoder settings disable native hooks".into());
    }
    Ok(())
}

pub(super) fn settings(path: Option<&String>) -> Result<Value> {
    let value = match path {
        Some(path) => serde_json::from_slice(&read_file(path, 1024 * 1024)?)?,
        None => json!({}),
    };
    enabled(&value)?;
    Ok(value)
}

pub(super) fn check_sources(cwd: &Path, flags: &Value) -> Result<()> {
    for name in ["QODER_CONFIG_DIR_NAME", "QODERCN_CONFIG_DIR_NAME"] {
        if std::env::var_os(name).is_some() {
            return Err(format!("custom {name} is not supported by this adapter").into());
        }
    }
    if std::env::var("QODERCLI_SITE").is_ok_and(|v| v == "cn") {
        return Err("QODERCLI_SITE=cn requires a separately verified adapter".into());
    }
    let home =
        std::env::var_os("HOME").ok_or("HOME is required to inspect native Qoder settings")?;
    for path in [
        PathBuf::from(home).join(".qoder/settings.json"),
        cwd.join(".qoder/settings.json"),
        cwd.join(".qoder/settings.local.json"),
    ] {
        match std::fs::symlink_metadata(&path) {
            Ok(_) => enabled(&serde_json::from_slice::<Value>(&read_file(
                path,
                1024 * 1024,
            )?)?)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    enabled(flags)
}

pub(super) fn event_name(event: &str) -> Result<&'static str> {
    match event {
        "tool.before" => Ok("PreToolUse"),
        "tool.after" => Ok("PostToolUse"),
        _ => Err("unsupported native event".into()),
    }
}

pub(super) fn generate(
    mut settings: Value,
    document: &Value,
    target: &str,
    steps: &[AdmittedStep],
    executable: &Path,
    binding_path: &Path,
) -> Result<(Value, BTreeMap<String, HookEvent>)> {
    let mut events = BTreeMap::new();
    let spec = &document["spec"];
    let hooks = settings
        .as_object_mut()
        .ok_or("invalid settings")?
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("Qoder hooks must be an object")?;
    for name in ["tool.before", "tool.after"] {
        let selected: Vec<_> = steps.iter().filter(|step| step.event == name).collect();
        if selected.is_empty() {
            continue;
        }
        let budget = spec["events"][name]
            .get("budget_ms")
            .unwrap_or(&spec["execution"]["default_event_budget_ms"])
            .as_f64()
            .map(|value| value as u64)
            .ok_or("invalid event budget")?;
        if !(1..=55000).contains(&budget) {
            return Err("native launcher event budgets must be 1..55000 ms".into());
        }
        let commands: Vec<_> = selected
            .iter()
            .map(|step| {
                json!({
                    "type": "command", "command": executable,
                    "args": ["hook", "--adapter", "qoder", "--binding", binding_path, "--event", name,
                             "--step", step.step_id, "--on-error", step.on_error],
                    "timeout": budget.div_ceil(1000) + 4,
                })
            })
            .collect();
        let mut group = json!({"matcher": "*", "hooks": commands});
        if let Some(sequential) = spec["agents"][target]["qoder"].get("sequential") {
            group["sequential"] = sequential.clone();
        }
        hooks
            .entry(event_name(name)?)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("Qoder event hooks must be arrays")?
            .push(group);
        events.insert(
            name.into(),
            HookEvent {
                budget_ms: budget,
                steps: selected
                    .into_iter()
                    .map(|step| (step.step_id.clone(), step.on_error.clone()))
                    .collect(),
            },
        );
    }
    Ok((settings, events))
}

pub(super) fn normalize(binding: &HookBinding, name: &str, native: &Value) -> Result<Value> {
    if native["hook_event_name"] != event_name(name)?
        || native["cwd"].as_str() != binding.cwd.to_str()
    {
        return Err("Qoder callback event or working directory does not match the binding".into());
    }
    let identifier = |key: &str| -> Result<&str> {
        native[key]
            .as_str()
            .filter(|v| !v.trim().is_empty() && v.len() <= 512)
            .ok_or_else(|| {
                format!("Qoder callback requires nonempty {key} (up to 512 bytes)").into()
            })
    };
    let session = identifier("session_id")?;
    let call = identifier("tool_use_id")?;
    let tool = identifier("tool_name")?;
    if native.get("tool_input").is_none() {
        return Err("Qoder callback has no tool_input".into());
    }
    Ok(json!({
        "name": name,
        "agent": {"adapter": "qoder", "binding_id": binding.binding.target, "instance_id": binding.binding.instance_id},
        "session_id": session,
        "tool": {"name": tool, "native_name": tool, "call_id": call,
                 "input": native["tool_input"], "result": native.get("tool_response").cloned().unwrap_or(Value::Null)},
        "native": native,
    }))
}
