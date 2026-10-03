//! OpenClaw 2026.9.6 Gateway plugin registration with persistent native state.

use super::super::{
    adapter::{
        self, Adapter, HookBinding, HookEvent, HookOutput, LaunchContext, LaunchInput, LaunchPlan,
        PreparedLaunch, Readiness,
    },
    read_file, Exit, Result,
};
use aw_provider::admission::AdmittedStep;
use aw_service::Capabilities;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    time::Duration,
};

const VERSION: &str = "2026.9.6";
const PLUGIN: &str = "aw-native-hooks";
const MAX_BUDGET_MS: u64 = 12000;

pub(in crate::cli) struct OpenClaw;

struct Prepared {
    input: LaunchInput,
    settings: Value,
    profile: PathBuf,
    _lock: File,
}

impl Adapter for OpenClaw {
    fn prepare(&self, input: LaunchInput) -> Result<Box<dyn PreparedLaunch>> {
        if input.flags.keys().any(|key| {
            ![
                "--config",
                "--agent",
                "--native-settings",
                "--native-state-dir",
            ]
            .contains(&key.as_str())
        }) {
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
            .flags
            .get("--native-settings")
            .ok_or("OpenClaw requires --native-settings FILE and --native-state-dir ABS_DIR")?;
        let state_path = input
            .flags
            .get("--native-state-dir")
            .ok_or("OpenClaw requires --native-settings FILE and --native-state-dir ABS_DIR")?;
        let settings: Value = serde_json::from_slice(&read_file(settings_path, 4 * 1024 * 1024)?)?;
        check_settings(&settings)?;
        let (profile, lock) = lock_profile(Path::new(state_path))?;
        let version = adapter::version_output(&input.command, &version_args)?;
        if version.split_whitespace().take(2).collect::<Vec<_>>() != ["OpenClaw", VERSION] {
            return Err(format!("OpenClaw {VERSION} is required by this adapter").into());
        }
        Ok(Box::new(Prepared {
            input,
            settings,
            profile,
            _lock: lock,
        }))
    }

    fn normalize(&self, binding: &HookBinding, name: &str, native: &Value) -> Result<Value> {
        let hook = match name {
            "tool.before" => "before_tool_call",
            "tool.after" => "after_tool_call",
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
        let tool = identifier(event.get("toolName"), "toolName")?;
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
            "agent": {"adapter":"openclaw", "binding_id":binding.binding.target,
                "instance_id":binding.binding.instance_id},
            "session_id": session,
            "tool": {"name":tool, "native_name":tool, "call_id":call_id,
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
        Capabilities {
            adapter: "openclaw".into(),
            version: VERSION.into(),
            entrypoint: "gateway".into(),
            events: BTreeMap::from([
                ("tool.before".into(), vec!["observe".into(), "block".into()]),
                ("tool.after".into(), vec!["observe".into()]),
            ]),
        }
    }

    fn validate_steps(&self, steps: &[AdmittedStep]) -> Result<()> {
        for step in steps {
            budget(&self.input.document, &step.event)?;
        }
        Ok(())
    }

    fn configure(&mut self, context: &LaunchContext<'_>) -> Result<LaunchPlan> {
        let plugin = context.files.0.join("openclaw-plugin");
        fs::create_dir(&plugin)?;
        context.files.write(
            "openclaw-plugin/index.mjs",
            include_bytes!("../../../../../adapters/openclaw/index.mjs"),
        )?;
        context.files.write(
            "openclaw-plugin/package.json",
            include_bytes!("../../../../../adapters/openclaw/package.json"),
        )?;
        context.files.write(
            "openclaw-plugin/openclaw.plugin.json",
            include_bytes!("../../../../../adapters/openclaw/openclaw.plugin.json"),
        )?;
        let ready = context.files.0.join("ready.json");
        let token = fs::read_to_string("/proc/sys/kernel/random/uuid")?
            .trim()
            .to_owned();
        let mut hooks = json!({"before":[],"after":[]});
        let mut events = BTreeMap::new();
        for name in ["tool.before", "tool.after"] {
            let selected: Vec<_> = context
                .steps
                .iter()
                .filter(|step| step.event == name)
                .collect();
            if selected.is_empty() {
                continue;
            }
            let budget = budget(&self.input.document, name)?;
            hooks[if name == "tool.before" {
                "before"
            } else {
                "after"
            }] = json!(selected
                .iter()
                .map(|step| json!({"step":step.step_id,"onError":step.on_error,"budgetMs":budget}))
                .collect::<Vec<_>>());
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
            }}),
        );
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
        let provider_environment = adapter::environment(&command)?;
        Ok(LaunchPlan {
            command,
            provider_environment,
            events,
            readiness: Some(Readiness {
                path: ready,
                token,
                adapter: "openclaw".into(),
                hooks: context.steps.len(),
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
mod tests {
    use super::*;

    #[test]
    fn gateway_arguments_preserve_literal_values_and_version_prefix() {
        let args = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            check_args(&args(&["gateway", "run", "--port", "12345"])).unwrap(),
            args(&["--version"])
        );
        assert_eq!(
            check_args(&args(&["/native/openclaw.mjs", "gateway", "run"])).unwrap(),
            args(&["/native/openclaw.mjs", "--version"])
        );
        for input in [
            vec!["agent", "exec"],
            vec!["gateway", "stop"],
            vec!["gateway", "run", "--force"],
            vec!["gateway", "run", "--profile=another"],
            vec!["--dev", "gateway", "run"],
        ] {
            assert!(check_args(&args(&input)).is_err(), "{input:?}");
        }
    }

    #[test]
    fn native_settings_require_local_gateway_and_enabled_unowned_plugin() {
        let config = json!({"gateway":{"mode":"local"},"plugins":{"allow":["existing"],
            "entries":{"existing":{"enabled":true}}}});
        check_settings(&config).unwrap();
        for path in ["$include", "disabled", "owned", "remote", "denied"] {
            let mut modified = config.clone();
            match path {
                "$include" => modified["models"] = json!({"$include":"models.json"}),
                "disabled" => modified["plugins"]["enabled"] = json!(false),
                "owned" => modified["plugins"]["entries"][PLUGIN] = json!({"enabled":false}),
                "remote" => modified["gateway"]["mode"] = json!("remote"),
                _ => modified["plugins"]["deny"] = json!([PLUGIN]),
            }
            assert!(check_settings(&modified).is_err(), "{path}");
        }
    }

    #[test]
    fn budget_stays_below_native_before_timeout_and_accepts_integer_floats() {
        let mut config =
            json!({"spec":{"execution":{"default_event_budget_ms":5000.0},"events":{}}});
        assert_eq!(budget(&config, "tool.before").unwrap(), 5000);
        config["spec"]["events"]["tool.before"] = json!({"budget_ms":12001});
        assert!(budget(&config, "tool.before").is_err());
    }

    #[test]
    fn response_returns_native_block_without_granting_permissions() {
        let output = OpenClaw.reply(true);
        assert!(matches!(output.exit, Exit::Code(0)));
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap()["block"],
            true
        );
        assert_eq!(OpenClaw.reply(false).stdout, b"{}");
    }

    #[test]
    fn normalized_events_obey_provider_contract_and_scope_call_ids_to_runs() {
        let binding: HookBinding = serde_json::from_value(json!({"adapter":"openclaw",
            "binding":{"identity":{"generation":"fixture","config_revision":"a".repeat(64)},
                "instance_id":"instance","target":"target","audit_key":"key"},
            "socket":"/tmp/fixture.sock","cwd":"/tmp","events":{}}))
        .unwrap();
        let mut native = json!({"hook":"before_tool_call","event":{"toolName":"exec",
            "toolCallId":"call","runId":"first","params":{"command":"literal $HOME"}},
            "context":{"sessionId":"session"}});
        let first = OpenClaw
            .normalize(&binding, "tool.before", &native)
            .unwrap();
        let protocol = aw_provider::Protocol::new().unwrap();
        protocol
            .bind_invocation(
                json!({"api_version":"aw-provider/v1alpha1","method":"invoke",
            "request_id":"request","operation":"check","config_revision":"a".repeat(64),
            "budget_ms":1000,"allowed_effects":["observe","block"],"config":{},"event":first}),
            )
            .unwrap();
        assert_eq!(
            OpenClaw
                .normalize(&binding, "tool.before", &native)
                .unwrap()["tool"]["call_id"],
            first["tool"]["call_id"]
        );
        native["event"]["runId"] = json!("second");
        assert_ne!(
            OpenClaw
                .normalize(&binding, "tool.before", &native)
                .unwrap()["tool"]["call_id"],
            first["tool"]["call_id"]
        );
        native["context"] = json!({});
        assert!(OpenClaw
            .normalize(&binding, "tool.before", &native)
            .is_err());
    }
}
