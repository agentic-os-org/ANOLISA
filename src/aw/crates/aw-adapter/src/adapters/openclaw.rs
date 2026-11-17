//! OpenClaw Gateway plugin registration with persistent native state.

use super::super::{
    adapter::{
        self, Adapter, HookEvent, HookOutput, LaunchContext, LaunchInput, LaunchPlan,
        NativeContext, PreparedLaunch, Readiness,
    },
    read_file, Exit, Result,
};
use super::openclaw_bundle::verified_bundle;
use aw_core::admission::{AdapterCapabilities as Capabilities, AdmittedStep};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    time::Duration,
};

const PLUGIN: &str = "aw-native-hooks";
const MAX_BUDGET_MS: u64 = 12000;

/// Match the pinned runtime's completion names without replacing native identity.
pub(crate) fn policy_name(name: &str) -> String {
    match name.trim().to_lowercase().as_str() {
        "bash" => "exec".into(),
        "apply-patch" => "apply_patch".into(),
        "cron" => "automations".into(),
        other => other.to_owned(),
    }
}

pub(crate) struct OpenClaw;

struct Prepared {
    input: LaunchInput,
    version: String,
    bundle_verified: bool,
    settings: Value,
    profile: PathBuf,
    _lock: File,
}

impl Adapter for OpenClaw {
    fn prepare(
        &self,
        input: LaunchInput,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<Box<dyn PreparedLaunch>> {
        if input.options.profile.is_some() {
            return Err("OpenClaw supports --native-settings and --native-state-dir; another adapter's profile options are not accepted".into());
        }
        let version_args = check_args(&input.command.args)?;
        if input
            .command
            .environment
            .get(&OsString::from("OPENCLAW_ALLOW_MULTI_GATEWAY"))
            .is_some_and(|value| value == "1")
        {
            return Err("OPENCLAW_ALLOW_MULTI_GATEWAY bypasses native Gateway ownership".into());
        }
        if input
            .command
            .environment
            .get(&OsString::from("NODE_ENV"))
            .is_some_and(|value| value == "test")
            || input
                .command
                .environment
                .get(&OsString::from("VITEST"))
                .is_some_and(|value| !value.is_empty())
        {
            return Err("OpenClaw test-mode environment bypasses native Gateway ownership".into());
        }
        let settings_path = input
            .options
            .settings
            .as_ref()
            .ok_or("OpenClaw requires --native-settings FILE and --native-state-dir ABS_DIR")?;
        let state_path = input
            .options
            .state_dir
            .as_ref()
            .ok_or("OpenClaw requires --native-settings FILE and --native-state-dir ABS_DIR")?;
        let settings: Value = serde_json::from_slice(&read_file(settings_path, 4 * 1024 * 1024)?)?;
        check_settings(&settings)?;
        let (profile, lock) = lock_profile(Path::new(state_path))?;
        let version = adapter::Probe(cancelled).version_output(&input.command, &version_args)?;
        let version = version
            .strip_prefix("OpenClaw ")
            .and_then(|value| value.split_whitespace().next())
            .ok_or("OpenClaw --version must identify OpenClaw and its version")?;
        let version = crate::version::reported(version, "OpenClaw")?;
        let bundle_verified = verified_bundle(&input.command, &version);
        Ok(Box::new(Prepared {
            input,
            version,
            bundle_verified,
            settings,
            profile,
            _lock: lock,
        }))
    }

    fn normalize(&self, binding: &NativeContext, name: &str, native: &Value) -> Result<Value> {
        let hook = match name {
            "tool.before" => "before_tool_call",
            "tool.after" => "after_tool_call",
            "security.violation" => "final_tool_result",
            _ => return Err("unsupported OpenClaw tool event".into()),
        };
        if native["hook"] != hook {
            return Err("OpenClaw hook does not match its binding event".into());
        }
        let event = native["event"]
            .as_object()
            .ok_or("missing OpenClaw event")?;
        let context = native["context"]
            .as_object()
            .ok_or("missing OpenClaw context")?;
        if name == "security.violation"
            && (!event.get("result").is_some_and(Value::is_object)
                || !event["result"]["content"].is_array()
                || !event["result"]["isError"].is_boolean())
        {
            return Err("OpenClaw final tool result must contain content and error status".into());
        }
        let native_tool = identifier(event.get("toolName"), "toolName")?;
        // Native completion uses policy names; middleware retains the tool's name.
        // Match the pinned runtime's three aliases without changing before input.
        let tool = if name != "tool.before" {
            policy_name(native_tool)
        } else {
            native_tool.to_owned()
        };
        let call = identifier(
            event.get("toolCallId").or(context.get("toolCallId")),
            "toolCallId",
        )?;
        let run = identifier(event.get("runId").or(context.get("runId")), "runId")?;
        let session = identifier(context.get("sessionId"), "sessionId")?;
        let params = event
            .get("params")
            .filter(|value| value.is_object())
            .ok_or("OpenClaw tool params must be an object")?;
        // Model-generated call IDs can be reused in later runs of one session.
        // Every step in this native event derives the same daemon correlation.
        let call_id = format!("{:x}", Sha256::digest(serde_json::to_vec(&[run, call])?));
        Ok(json!({
            "name": name,
            "agent": {"adapter":"openclaw", "binding_id":binding.target,
                "instance_id":binding.instance_id},
            "session_id": session,
            "tool": {"name":tool, "native_name":native_tool, "call_id":call_id,
                "input":params, "result":event.get("result").unwrap_or(&Value::Null)},
            "native": native,
        }))
    }

    fn reply(&self, blocked: bool) -> HookOutput {
        HookOutput {
            exit: Exit::Code(0),
            stdout: if blocked {
                br#"{"block":true,"blockReason":"Tool blocked by AW policy"}"#.to_vec()
            } else {
                b"{}".to_vec()
            },
            stderr: Vec::new(),
        }
    }
}

impl PreparedLaunch for Prepared {
    fn capabilities(&self) -> Capabilities {
        let mut capabilities =
            super::super::adapter::catalog::OPENCLAW.capabilities(&self.version, "gateway");
        if self.bundle_verified
            && !self.input.options.reloadable
            && verified_model_api(&self.settings)
        {
            capabilities.events.insert(
                "security.violation".into(),
                ["observe".into(), "block".into()].into(),
            );
        }
        capabilities
    }

    fn validate_steps(&self, steps: &[AdmittedStep]) -> Result<()> {
        for step in steps {
            budget(&self.input.document, &step.event)?;
            if step.event == "security.violation" && step.on_error != "block" {
                return Err("OpenClaw final result gate requires on_error: block".into());
            }
        }
        Ok(())
    }

    fn configure(&mut self, context: &LaunchContext<'_>) -> Result<LaunchPlan> {
        let plugin = context.files.directory().join("openclaw-plugin");
        fs::create_dir(&plugin)?;
        context.files.write(
            "openclaw-plugin/index.mjs",
            include_bytes!("../../native/openclaw/index.mjs"),
        )?;
        context.files.write(
            "openclaw-plugin/hooks.mjs",
            include_bytes!("../../native/openclaw/hooks.mjs"),
        )?;
        context.files.write(
            "openclaw-plugin/package.json",
            include_bytes!("../../native/openclaw/package.json"),
        )?;
        context.files.write(
            "openclaw-plugin/openclaw.plugin.json",
            include_bytes!("../../native/openclaw/openclaw.plugin.json"),
        )?;
        context.files.write(
            "openclaw-plugin/reload.mjs",
            include_bytes!("../../native/openclaw/reload.mjs"),
        )?;
        let ready = context.files.directory().join("ready.json");
        let token = fs::read_to_string("/proc/sys/kernel/random/uuid")?
            .trim()
            .to_owned();
        let mut hooks = json!({"before":[],"after":[]});
        let mut events = BTreeMap::new();
        for name in ["tool.before", "tool.after", "security.violation"] {
            let selected = adapter::native_steps(
                context.steps,
                name,
                name != "security.violation"
                    && adapter::observation_enabled(&self.input.document, &self.input.target),
            );
            if selected.is_empty() {
                continue;
            }
            let budget = budget(&self.input.document, name)?;
            let group = match name {
                "tool.before" => "before",
                "tool.after" => "after",
                _ => "final",
            };
            hooks[group] = json!(selected
                .iter()
                .map(|step| json!({"step":step.step_id,"onError":step.on_error,"budgetMs":budget,"replaceResult":step.replaces_result}))
                .collect::<Vec<_>>());
            events.insert(
                name.into(),
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
        let mut settings = self.settings.clone();
        let plugins = object(&mut settings, "plugins")?;
        let load = plugins
            .entry("load")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or("plugins.load must be an object")?;
        load.entry("paths")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("plugins.load.paths must be an array")?
            .push(json!(plugin));
        if let Some(allow) = plugins.get_mut("allow") {
            let allow = allow
                .as_array_mut()
                .ok_or("plugins.allow must be an array")?;
            // An empty native allow list means unrestricted plugin admission.
            if !allow.is_empty() && !allow.iter().any(|id| id == PLUGIN) {
                allow.push(json!(PLUGIN));
            }
        }
        let entries = plugins
            .entry("entries")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or("plugins.entries must be an object")?;
        entries.insert(
            PLUGIN.into(),
            json!({"enabled":true,"config":{
                "binary":context.executable,"binding":context.binding_path,
                "ready":{"path":ready,"token":token},"hooks":hooks,
                "reloadable":self.input.options.reloadable,
            }}),
        );
        if self.input.options.reloadable {
            // The native request gate is conversation-scoped; only the AW entry
            // receives permission in this private overlay.
            entries.get_mut(PLUGIN).ok_or("missing AW plugin entry")?["hooks"] =
                json!({"allowConversationAccess":true});
        }
        let settings_path = context
            .files
            .write("openclaw.json", &serde_json::to_vec(&settings)?)?;
        let mut command = self.input.command.clone();
        command
            .environment
            .insert("OPENCLAW_CONFIG_PATH".into(), settings_path.into());
        command
            .environment
            .insert("OPENCLAW_STATE_DIR".into(), self.profile.clone().into());
        // The generated overlay must not become a durable user configuration.
        command
            .environment
            .insert("OPENCLAW_CONFIG_READONLY".into(), "1".into());
        // The packaged launcher respawns when relocating an inherited Node
        // compile cache. Keep the Gateway as the owned readiness PID.
        command
            .environment
            .insert("NODE_DISABLE_COMPILE_CACHE".into(), "1".into());
        let provider_environment = command.environment.clone();
        if events.contains_key("security.violation") {
            fs::create_dir_all(context.files.directory().join("shared/js-runtime"))?;
            context.files.write(
                "shared/js-runtime/module-patch.mjs",
                include_bytes!("../../native/shared/js-runtime/module-patch.mjs"),
            )?;
            let preload = context.files.write(
                "openclaw-plugin/final-preload.mjs",
                include_bytes!("../../native/openclaw/final-preload.mjs"),
            )?;
            let url = reqwest::Url::from_file_path(preload)
                .map_err(|_| "OpenClaw final preload path is not a file URL")?;
            let mut options = command
                .environment
                .get(OsStr::new("NODE_OPTIONS"))
                .cloned()
                .or_else(|| std::env::var_os("NODE_OPTIONS"))
                .unwrap_or_default();
            if !options.is_empty() {
                options.push(" ");
            }
            options.push(format!("--import={url}"));
            command.environment.insert("NODE_OPTIONS".into(), options);
        }
        if self.input.options.reloadable {
            events = crate::reload::routes();
            for event in events.values_mut() {
                event.budget_ms = MAX_BUDGET_MS;
            }
        }
        let hook_count = events.values().map(|event| event.steps.len()).sum();
        Ok(LaunchPlan {
            command,
            provider_environment,
            events,
            readiness: Some(Readiness {
                path: ready,
                token,
                adapter: "openclaw".into(),
                hooks: hook_count,
                timeout: Duration::from_secs(30),
            }),
        })
    }
}

fn identifier<'a>(value: Option<&'a Value>, field: &str) -> Result<&'a str> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty() && value.len() <= 512)
        .ok_or_else(|| format!("OpenClaw callback requires {field} (1..512 bytes)").into())
}

fn budget(document: &Value, name: &str) -> Result<u64> {
    let spec = &document["spec"];
    let value = spec["events"][name]
        .get("budget_ms")
        .unwrap_or(&spec["execution"]["default_event_budget_ms"])
        .as_f64()
        .map(|value| value as u64)
        .ok_or("missing OpenClaw event budget")?;
    if !(1..=MAX_BUDGET_MS).contains(&value) {
        return Err("OpenClaw native event budgets must be 1..12000 ms".into());
    }
    Ok(value)
}

fn object<'a>(value: &'a mut Value, name: &str) -> Result<&'a mut Map<String, Value>> {
    value
        .as_object_mut()
        .ok_or("native config must be an object")?
        .entry(name)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| format!("{name} must be an object").into())
}

fn check_settings(value: &Value) -> Result<()> {
    fn includes(value: &Value) -> bool {
        match value {
            Value::Object(object) => {
                object.contains_key("$include") || object.values().any(includes)
            }
            Value::Array(array) => array.iter().any(includes),
            _ => false,
        }
    }
    if !value.is_object() || includes(value) {
        return Err("OpenClaw requires an expanded native JSON config without $include".into());
    }
    if value["gateway"]["mode"] != "local" {
        return Err("OpenClaw native config must select gateway.mode=local".into());
    }
    if value["plugins"]["enabled"] == false
        || value["plugins"]["deny"].as_array().is_some_and(|deny| {
            deny.iter()
                .any(|id| id.as_str().is_some_and(|id| id.trim() == PLUGIN))
        })
    {
        return Err("OpenClaw native configuration disables the AW plugin".into());
    }
    if value["plugins"]["entries"].get(PLUGIN).is_some() {
        return Err("OpenClaw native configuration already owns aw-native-hooks".into());
    }
    Ok(())
}

fn verified_model_api(settings: &Value) -> bool {
    let primary = settings["agents"]["defaults"]["model"]["primary"].as_str();
    let provider = primary.and_then(|value| value.split_once('/').map(|(provider, _)| provider));
    let fallbacks = &settings["agents"]["defaults"]["model"]["fallbacks"];
    provider
        .is_some_and(|name| settings["models"]["providers"][name]["api"] == "openai-completions")
        && fallbacks.as_array().is_none_or(Vec::is_empty)
}

fn check_args(args: &[OsString]) -> Result<Vec<OsString>> {
    let offset = usize::from(args.first().is_some_and(|arg| {
        Path::new(arg)
            .file_name()
            .is_some_and(|name| name == "openclaw.mjs")
    }));
    if args.get(offset).is_none_or(|arg| arg != "gateway")
        || args.get(offset + 1).is_none_or(|arg| arg != "run")
    {
        return Err("OpenClaw adapter requires the gateway run entrypoint; existing Gateways are not attached".into());
    }
    for arg in &args[offset + 2..] {
        let value = arg.to_str().ok_or("OpenClaw arguments must be UTF-8")?;
        if matches!(
            value.split('=').next(),
            Some("--force" | "--dev" | "--reset" | "--profile")
        ) {
            return Err(
                "OpenClaw force, reset and profile switching are not supported by this launcher"
                    .into(),
            );
        }
    }
    let mut version = args[..offset].to_vec();
    version.push("--version".into());
    Ok(version)
}

fn lock_profile(path: &Path) -> Result<(PathBuf, File)> {
    if !path.is_absolute() {
        return Err("--native-state-dir must be an absolute existing OpenClaw profile".into());
    }
    let path = path.canonicalize()?;
    let metadata = fs::metadata(&path)?;
    // SAFETY: geteuid has no arguments and only reads the caller's identity.
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err("OpenClaw profile must be a directory owned by the current user".into());
    }
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path.join(".aw-launch.lock"))?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o7777 != 0o600
    {
        return Err(
            "OpenClaw AW profile lock must be a private regular file owned by the current user"
                .into(),
        );
    }
    // SAFETY: the owned descriptor remains open throughout the foreground launch.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("OpenClaw profile is already used by an AW launch".into());
    }
    Ok((path, file))
}

#[cfg(test)]
mod tests;
