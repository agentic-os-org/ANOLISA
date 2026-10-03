//! QwenPaw App 2.2.2b4 middleware; the native runtime owns tool scheduling.

use crate::cli::{
    adapter::{
        self, Adapter, HookBinding, HookEvent, HookOutput, LaunchContext, LaunchInput, LaunchPlan,
        PreparedLaunch, Readiness,
    },
    read_file, Exit, Result,
};
use aw_provider::admission::AdmittedStep;
use aw_service::Capabilities;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{self, DirBuilder, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::PathBuf,
    time::Duration,
};

const VERSION: &str = "2.2.2b4";
pub(in crate::cli) struct QwenPaw;

struct Prepared {
    input: LaunchInput,
    working: PathBuf,
    plugin: Option<OwnedPlugin>,
}

struct OwnedPlugin {
    path: PathBuf,
    identity: (u64, u64),
    created_parent: bool,
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
        };
        for (name, bytes) in [
            (
                "plugin.json",
                include_bytes!("../../../../../adapters/qwenpaw/plugin.json").as_slice(),
            ),
            (
                "plugin.py",
                include_bytes!("../../../../../adapters/qwenpaw/plugin.py").as_slice(),
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
        let _ = self.cleanup();
    }
}

impl Adapter for QwenPaw {
    fn prepare(&self, input: LaunchInput) -> Result<Box<dyn PreparedLaunch>> {
        for flag in [
            "--native-settings",
            "--native-profile",
            "--native-config",
            "--native-state-dir",
        ] {
            if input.flags.contains_key(flag) {
                return Err(
                    "QwenPaw uses QWENPAW_WORKING_DIR; native override flags are unsupported"
                        .into(),
                );
            }
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
        if adapter::version_output(&input.command, &["--version".into()])?
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

    fn normalize(&self, binding: &HookBinding, event: &str, native: &Value) -> Result<Value> {
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
        Capabilities {
            adapter: "qwenpaw".into(),
            version: VERSION.into(),
            entrypoint: "app".into(),
            events: BTreeMap::from([
                ("tool.before".into(), vec!["observe".into(), "block".into()]),
                ("tool.after".into(), vec!["observe".into()]),
            ]),
        }
    }

    fn validate_steps(&self, steps: &[AdmittedStep]) -> Result<()> {
        event_routes(&self.input.document, steps).map(|_| ())
    }

    fn configure(&mut self, context: &LaunchContext<'_>) -> Result<LaunchPlan> {
        let (events, hooks) = event_routes(&self.input.document, context.steps)?;
        let token = fs::read_to_string("/proc/sys/kernel/random/uuid")?
            .trim()
            .to_owned();
        let ready = context.files.0.join("qwenpaw-ready.json");
        let native = context.files.write(
            "qwenpaw-hooks.json",
            &serde_json::to_vec(&json!({
                "executable": context.executable, "binding": context.binding_path,
                "ready_path": ready, "ready_token": token, "hooks": hooks,
            }))?,
        )?;
        self.plugin = Some(OwnedPlugin::create(&self.working)?);
        let mut command = self.input.command.clone();
        command.environment.insert(
            "QWENPAW_WORKING_DIR".into(),
            self.working.clone().into_os_string(),
        );
        command
            .environment
            .insert("AW_NATIVE_CONFIG".into(), native.into_os_string());
        let provider_environment = adapter::environment(&command)?;
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

    fn finish(&mut self) -> Result<()> {
        if let Some(plugin) = self.plugin.as_ref() {
            plugin.cleanup()?;
        }
        self.plugin = None;
        Ok(())
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

fn event_routes(
    document: &Value,
    steps: &[AdmittedStep],
) -> Result<(BTreeMap<String, HookEvent>, Vec<Value>)> {
    let mut events = BTreeMap::new();
    let mut hooks = Vec::new();
    // After wrappers are outermost, so they also observe an inner before denial.
    for event in ["tool.after", "tool.before"] {
        let selected: Vec<_> = steps.iter().filter(|step| step.event == event).collect();
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
        for step in &selected {
            hooks.push(json!({"event": event, "step": step.step_id, "on_error": step.on_error, "budget_ms": budget}));
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
    if hooks.is_empty() {
        return Err("QwenPaw requires at least one admitted tool hook".into());
    }
    Ok((events, hooks))
}

fn identifier<'a>(value: &'a Value, label: &str) -> Result<&'a str> {
    value
        .as_str()
        .filter(|value| !value.trim().is_empty() && value.len() <= 512)
        .ok_or_else(|| format!("QwenPaw requires nonempty {label} (up to 512 bytes)").into())
}

fn normalize(binding: &HookBinding, event: &str, native: &Value) -> Result<Value> {
    if !matches!(event, "tool.before" | "tool.after") || native["event"] != event {
        return Err("QwenPaw callback event does not match the binding".into());
    }
    let session = identifier(&native["session_id"], "session_id")?;
    let call = identifier(&native["tool_call"]["id"], "tool_call.id")?;
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
        "agent": {"adapter": "qwenpaw", "binding_id": binding.binding.target, "instance_id": binding.binding.instance_id},
        "session_id": session,
        "tool": {"name": tool, "native_name": tool, "call_id": call, "input": input,
            "result": native.get("tool_response").cloned().unwrap_or(Value::Null)},
        "native": native,
    }))
}
