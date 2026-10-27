//! QwenPaw App 2.2.2.post1 middleware; the native runtime owns tool scheduling.

use crate::{
    adapter::{
        self, Adapter, HookEvent, HookOutput, LaunchContext, LaunchInput, LaunchPlan,
        NativeContext, PreparedLaunch, Readiness,
    },
    read_file, Exit, Result,
};
use aw_provider::admission::AdapterCapabilities as Capabilities;
use aw_provider::admission::AdmittedStep;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{self, DirBuilder, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::PathBuf,
    time::Duration,
};

const VERSION: &str = "2.2.2.post1";
pub(crate) struct QwenPaw;

struct Prepared {
    input: LaunchInput,
    working: PathBuf,
    plugin: Option<OwnedPlugin>,
}

struct OwnedPlugin {
    path: PathBuf,
    identity: (u64, u64),
    created_parent: bool,
    cleanup_on_drop: bool,
}

impl OwnedPlugin {
    fn create(working: &std::path::Path) -> Result<Self> {
        let parent = working.join("plugins");
        let mut created_parent = false;
        match fs::symlink_metadata(&parent) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err("QwenPaw plugins path must be a directory, not a symlink".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                DirBuilder::new().mode(0o700).create(&parent)?;
                created_parent = true;
            }
            Err(error) => return Err(error.into()),
        }
        let path = parent.join("aw-native");
        DirBuilder::new().mode(0o700).create(&path).map_err(|error| {
            format!("cannot install owned QwenPaw plugins/aw-native (existing entries are preserved): {error}")
        })?;
        let metadata = fs::symlink_metadata(&path)?;
        let owned = Self {
            path,
            identity: (metadata.dev(), metadata.ino()),
            created_parent,
            cleanup_on_drop: true,
        };
        for (name, bytes) in [
            (
                "plugin.json",
                include_bytes!("../../native/qwenpaw/plugin.json").as_slice(),
            ),
            (
                "plugin.py",
                include_bytes!("../../native/qwenpaw/plugin.py").as_slice(),
            ),
            (
                "aw_private.py",
                include_bytes!("../../native/qwenpaw/aw_private.py").as_slice(),
            ),
            (
                "aw_resident.py",
                include_bytes!("../../native/qwenpaw/aw_resident.py").as_slice(),
            ),
            (
                "aw_mcp.py",
                include_bytes!("../../native/qwenpaw/aw_mcp.py").as_slice(),
            ),
            (
                "aw_native.py",
                include_bytes!("../../native/qwenpaw/aw_native.py").as_slice(),
            ),
            (
                "aw_scope.py",
                include_bytes!("../../native/claude-sdk/aw_scope.py").as_slice(),
            ),
        ] {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(owned.path.join(name))?;
            file.write_all(bytes)?;
        }
        Ok(owned)
    }

    fn cleanup(&self) -> Result<()> {
        match fs::symlink_metadata(&self.path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
            Ok(metadata)
                if metadata.is_dir() && (metadata.dev(), metadata.ino()) == self.identity => {}
            Ok(_) => {
                return Err("owned QwenPaw plugin directory was replaced; refusing cleanup".into())
            }
        }
        fs::remove_dir_all(&self.path)?;
        if self.created_parent {
            match fs::remove_dir(self.path.parent().ok_or("plugin has no parent")?) {
                Ok(()) => {}
                // Native or user plugins created meanwhile belong to their owners.
                Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

impl Drop for OwnedPlugin {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            let _ = self.cleanup();
        }
    }
}

impl Adapter for QwenPaw {
    fn prepare(
        &self,
        input: LaunchInput,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<Box<dyn PreparedLaunch>> {
        if !input.options.is_empty() {
            return Err(
                "QwenPaw uses QWENPAW_WORKING_DIR; native override flags are unsupported".into(),
            );
        }
        check_args(&input.command.args)?;
        let working = input
            .command
            .environment
            .get(std::ffi::OsStr::new("QWENPAW_WORKING_DIR"))
            .map(PathBuf::from)
            .ok_or("set QWENPAW_WORKING_DIR to an initialized native directory")?;
        if !working.is_absolute() || !working.is_dir() {
            return Err("QWENPAW_WORKING_DIR must be an existing absolute directory".into());
        }
        let working = working.canonicalize()?;
        let config: Value =
            serde_json::from_slice(&read_file(working.join("config.json"), 1024 * 1024)?)?;
        if !config.is_object() || config["plugins"]["aw-native"]["enabled"] == false {
            return Err("QwenPaw configuration must be an object and permit the AW plugin".into());
        }
        if fs::symlink_metadata(working.join("plugins/aw-native")).is_ok() {
            return Err("QwenPaw working directory already owns plugins/aw-native".into());
        }
        for name in ["QWENPAW_RUNTIME_ID", "QWENPAW_RUNTIME_PROVISIONER"] {
            if input
                .command
                .environment
                .get(std::ffi::OsStr::new(name))
                .is_some_and(|value| !value.is_empty())
            {
                return Err(
                    format!("{name} uses a separately managed runtime and is unsupported").into(),
                );
            }
        }
        if adapter::Probe(cancelled).version_output(&input.command, &["--version".into()])?
            != format!("QwenPaw, version {VERSION}")
        {
            return Err(format!("QwenPaw {VERSION} is required by this adapter").into());
        }
        Ok(Box::new(Prepared {
            input,
            working,
            plugin: None,
        }))
    }

    fn normalize(&self, binding: &NativeContext, event: &str, native: &Value) -> Result<Value> {
        normalize(binding, event, native)
    }

    fn reply(&self, blocked: bool) -> HookOutput {
        HookOutput {
            exit: Exit::Code(if blocked { 2 } else { 0 }),
            stdout: Vec::new(),
            stderr: if blocked {
                b"Tool blocked by AW policy\n".to_vec()
            } else {
                Vec::new()
            },
        }
    }
}

impl PreparedLaunch for Prepared {
    fn capabilities(&self) -> Capabilities {
        let mut capabilities = super::super::adapter::catalog::QWENPAW.capabilities(VERSION, "app");
        if !self.input.options.reloadable {
            capabilities
                .failure_actions
                .insert("tool.after".into(), vec!["withhold_result".into()]);
        }
        capabilities
    }

    fn validate_steps(&self, steps: &[AdmittedStep]) -> Result<()> {
        if !self.input.options.reloadable
            && adapter::observation_enabled(&self.input.document, &self.input.target)
        {
            return Err(
                "QwenPaw resident task scopes do not support native evidence capture".into(),
            );
        }
        if self.input.options.reloadable {
            crate::reload::validate(
                &self.input.document,
                &self.input.target,
                &self.capabilities(),
            )?;
        }
        event_routes(
            &self.input.document,
            steps,
            adapter::observation_enabled(&self.input.document, &self.input.target),
        )
        .map(|_| ())
    }

    fn configure(&mut self, context: &LaunchContext<'_>) -> Result<LaunchPlan> {
        self.configure_in(context, &self.working.clone())
    }

    fn attachment(
        &mut self,
        context: &LaunchContext<'_>,
    ) -> Result<crate::attachment::Registration> {
        let plan = self.configure_in(context, context.files.directory())?;
        let descriptor: Value = serde_json::from_slice(&read_file(
            context.files.directory().join("qwenpaw-hooks.json"),
            1024 * 1024,
        )?)?;
        let source = context.files.directory().join("plugins/aw-native");
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(source.join("aw-attachment.json"))?;
        output.write_all(&serde_json::to_vec(&descriptor)?)?;
        output.sync_all()?;
        let args = &plan.command.args;
        let mut port = 8088;
        for (index, arg) in args.iter().enumerate() {
            if arg == "--port" {
                port = args
                    .get(index + 1)
                    .and_then(|arg| arg.to_str())
                    .ok_or("missing App port")?
                    .parse::<u16>()
                    .map_err(|_| "invalid App port")?;
            }
            if let Some(value) = arg.to_str().and_then(|arg| arg.strip_prefix("--port=")) {
                port = value.parse::<u16>().map_err(|_| "invalid App port")?;
            }
        }
        // The attachment directory now owns staging, including failed-cleanup
        // retries. Ordinary aw run retains Prepared's drop cleanup.
        self.plugin
            .as_mut()
            .ok_or("missing staged plugin")?
            .cleanup_on_drop = false;
        Ok(crate::attachment::Registration::Qwenpaw {
            profile: self.working.clone(),
            port,
            source,
            descriptor,
        })
    }

    fn finish(&mut self) -> Result<()> {
        if let Some(plugin) = self.plugin.as_ref() {
            plugin.cleanup()?;
        }
        self.plugin = None;
        Ok(())
    }
}

impl Prepared {
    fn configure_in(
        &mut self,
        context: &LaunchContext<'_>,
        plugin_root: &std::path::Path,
    ) -> Result<LaunchPlan> {
        let (events, hooks) = if self.input.options.reloadable {
            (crate::reload::routes(), Vec::new())
        } else {
            event_routes(
                &self.input.document,
                context.steps,
                adapter::observation_enabled(&self.input.document, &self.input.target),
            )?
        };
        let token = fs::read_to_string("/proc/sys/kernel/random/uuid")?
            .trim()
            .to_owned();
        let ready = context.files.directory().join("qwenpaw-ready.json");
        let native = context.files.write(
            "qwenpaw-hooks.json",
            &serde_json::to_vec(&json!({
                "executable": context.executable, "binding": context.binding_path,
                "ready_path": ready, "ready_token": token, "hooks": hooks,
                "reloadable":self.input.options.reloadable,
                "resident": !self.input.options.reloadable,
                "resident_policy_reload": !self.input.options.reloadable,
            }))?,
        )?;
        self.plugin = Some(OwnedPlugin::create(plugin_root)?);
        let mut command = self.input.command.clone();
        command.environment.insert(
            "QWENPAW_WORKING_DIR".into(),
            self.working.clone().into_os_string(),
        );
        command
            .environment
            .insert("AW_NATIVE_CONFIG".into(), native.into_os_string());
        let provider_environment = command.environment.clone();
        Ok(LaunchPlan {
            command,
            provider_environment,
            events,
            readiness: Some(Readiness {
                path: ready,
                token,
                adapter: "qwenpaw".into(),
                hooks: hooks.len(),
                timeout: Duration::from_secs(30),
            }),
        })
    }
}

fn check_args(args: &[std::ffi::OsString]) -> Result<()> {
    if args.first().and_then(|value| value.to_str()) != Some("app") {
        return Err(
            "QwenPaw supports the app/API entrypoint only; ACP and TUI omit external plugins"
                .into(),
        );
    }
    let mut args = args[1..].iter();
    while let Some(argument) = args.next() {
        let argument = argument.to_str().ok_or("non-UTF-8 QwenPaw argument")?;
        let (name, attached) = argument
            .split_once('=')
            .map_or((argument, None), |(name, value)| (name, Some(value)));
        if !matches!(
            name,
            "--host" | "--port" | "--log-level" | "--hide-access-paths"
        ) {
            return Err(format!("unsupported QwenPaw app option: {name}").into());
        }
        if attached.is_none() {
            args.next().ok_or("QwenPaw app option requires a value")?;
        }
    }
    Ok(())
}

pub(crate) fn event_routes(
    document: &Value,
    steps: &[AdmittedStep],
    observe: bool,
) -> Result<(BTreeMap<String, HookEvent>, Vec<Value>)> {
    let mut events = BTreeMap::new();
    let mut hooks = Vec::new();
    // After wrappers are outermost, so they also observe an inner before denial.
    for event in ["tool.after", "tool.before"] {
        let selected = adapter::native_steps(steps, event, observe);
        if selected.is_empty() {
            continue;
        }
        let budget = document["spec"]["events"][event]
            .get("budget_ms")
            .unwrap_or(&document["spec"]["execution"]["default_event_budget_ms"])
            .as_f64()
            .map(|value| value as u64)
            .ok_or("invalid native event budget")?;
        if !(1..=55000).contains(&budget) {
            return Err("QwenPaw native event budgets must be 1..55000 ms".into());
        }
        let mut registration = selected.clone();
        if selected.iter().any(|step| step.replaces_result) {
            // After handlers unwind the native middleware stack inside out.
            registration.reverse();
        }
        for step in registration {
            let admitted = steps
                .iter()
                .find(|candidate| candidate.event == event && candidate.step_id == step.step_id);
            let all_tools = vec!["*".to_owned()];
            hooks.push(json!({"event": event, "step": step.step_id,
                "on_error": step.on_error, "budget_ms": budget,
                "event_tools": admitted.map(|step| &step.event_tools).unwrap_or(&all_tools),
                "step_tools": admitted.map(|step| &step.step_tools).unwrap_or(&all_tools)}));
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
    Ok((events, hooks))
}

fn identifier<'a>(value: &'a Value, label: &str) -> Result<&'a str> {
    value
        .as_str()
        .filter(|value| !value.trim().is_empty() && value.len() <= 512)
        .ok_or_else(|| format!("QwenPaw requires nonempty {label} (up to 512 bytes)").into())
}

fn normalize(binding: &NativeContext, event: &str, native: &Value) -> Result<Value> {
    if !matches!(event, "tool.before" | "tool.after") || native["event"] != event {
        return Err("QwenPaw callback event does not match the binding".into());
    }
    let session = identifier(&native["session_id"], "session_id")?;
    let native_call = identifier(&native["tool_call"]["id"], "tool_call.id")?;
    let call = if let Some(value) = native.get("aw_invocation_id") {
        let value = value
            .as_str()
            .ok_or("QwenPaw requires an invocation UUID")?;
        if !super::sdk_tools::invocation_uuid(value) {
            return Err("QwenPaw requires a canonical invocation UUID v4".into());
        }
        value
    } else {
        native_call
    };
    let tool = identifier(&native["tool_call"]["name"], "tool_call.name")?;
    let input: Value = serde_json::from_str(
        native["tool_call"]["input"]
            .as_str()
            .ok_or("QwenPaw tool_call.input must be a JSON string")?,
    )?;
    if event == "tool.after" && !native["tool_response"].is_object() {
        return Err("QwenPaw tool.after requires a native tool_response object".into());
    }
    Ok(json!({
        "name": event,
        "agent": {"adapter": "qwenpaw", "binding_id": binding.target, "instance_id": binding.instance_id},
        "session_id": session,
        "tool": {"name": tool, "native_name": tool, "call_id": call, "input": input,
            "result": native.get("tool_response").cloned().unwrap_or(Value::Null)},
        "native": native,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aw_provider::admission::StepExecution;

    #[test]
    fn withholding_is_limited_to_the_ordinary_resident_launch() {
        for reloadable in [false, true] {
            let prepared = Prepared {
                input: LaunchInput {
                    document: json!({}),
                    target: "target".into(),
                    options: adapter::NativeOptions {
                        reloadable,
                        ..Default::default()
                    },
                    command: aw_exec::CommandSpec {
                        program: "/not-launched".into(),
                        args: vec![],
                        cwd: "/".into(),
                        environment: BTreeMap::new(),
                    },
                },
                working: "/".into(),
                plugin: None,
            };
            assert_eq!(
                prepared
                    .capabilities()
                    .failure_actions
                    .contains_key("tool.after"),
                !reloadable
            );
        }
        for profile in super::super::super::adapter::catalog::PROFILES {
            assert!(profile
                .capabilities("fixture", "fixture")
                .failure_actions
                .is_empty());
        }
    }

    #[test]
    fn replacement_registers_outermost_so_observers_unwind_first() {
        let steps: Vec<_> = ["observe", "user", "compress"]
            .into_iter()
            .map(|id| AdmittedStep {
                event: "tool.after".into(),
                step_id: id.into(),
                provider: id.into(),
                event_tools: vec!["*".into()],
                step_tools: vec!["*".into()],
                on_error: "report".into(),
                execution: if id == "user" {
                    StepExecution::Native
                } else {
                    StepExecution::Provider {
                        operation: "record".into(),
                        effects: vec![if id == "compress" {
                            "replace_result"
                        } else {
                            "observe"
                        }
                        .into()],
                    }
                },
            })
            .collect();
        let (events, hooks) = event_routes(
            &json!({"spec":{"events":{},"execution":{"default_event_budget_ms":5000}}}),
            &steps,
            false,
        )
        .unwrap();
        assert_eq!(
            hooks
                .iter()
                .map(|hook| hook["step"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["compress", "user", "observe"]
        );
        assert!(hooks
            .iter()
            .all(|hook| hook["event_tools"] == json!(["*"]) && hook["step_tools"] == json!(["*"])));
        assert_eq!(
            events["tool.after"].replacement_step.as_deref(),
            Some("compress")
        );
    }
}
