//! Claude SDK workers explicitly compose the bridge in their public client factory.

use crate::{
    adapter::{
        self, Adapter, HookOutput, LaunchContext, LaunchInput, LaunchPlan, NativeContext,
        PreparedLaunch, Readiness,
    },
    Exit, Result,
};
use aw_provider::admission::AdapterCapabilities as Capabilities;
use aw_provider::admission::AdmittedStep;
use serde_json::{json, Value};
use std::{fs, time::Duration};

pub(crate) struct ClaudeSdk;
struct Prepared {
    input: LaunchInput,
}

impl Adapter for ClaudeSdk {
    fn prepare(
        &self,
        input: LaunchInput,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<Box<dyn PreparedLaunch>> {
        if !input.options.is_empty() {
            return Err(
                "Claude SDK workers use their public options factory, not native profiles".into(),
            );
        }
        let args = &input.command.args;
        let first = args
            .first()
            .and_then(|arg| arg.to_str())
            .ok_or("Claude SDK requires a Python worker script or -m module")?;
        if (first == "-m" && args.len() < 2) || (first != "-m" && !first.ends_with(".py")) {
            return Err("Claude SDK requires a Python worker script or -m module".into());
        }
        let script = "from importlib.metadata import version; from pathlib import Path; import claude_agent_sdk, subprocess; bundled = Path(claude_agent_sdk.__file__).parent / '_bundled' / 'claude'; print(version('claude-agent-sdk') + ':' + subprocess.check_output([str(bundled), '--version'], timeout=4, text=True).strip())";
        if adapter::Probe(cancelled)
            .version_output(&input.command, &["-c".into(), script.into()])?
            != "0.2.88:2.1.161 (Claude Code)"
        {
            return Err("Claude SDK 0.2.88 with bundled CLI 2.1.161 is required".into());
        }
        Ok(Box::new(Prepared { input }))
    }
    fn normalize(&self, binding: &NativeContext, event: &str, native: &Value) -> Result<Value> {
        if native.get("aw_source").is_some() {
            return super::claude_mcp::normalize(binding, event, native);
        }
        super::sdk_tools::normalize_tools(binding, event, native, "claude-sdk")
    }

    fn reply(&self, blocked: bool) -> HookOutput {
        HookOutput {
            exit: Exit::Code(if blocked { 2 } else { 0 }),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }
}
impl PreparedLaunch for Prepared {
    fn capabilities(&self) -> Capabilities {
        super::super::adapter::catalog::CLAUDE_SDK.capabilities("0.2.88", "sdk-worker")
    }
    fn validate_steps(&self, steps: &[AdmittedStep]) -> Result<()> {
        super::sdk_tools::routes(&self.input.document, steps).map(|_| ())
    }
    fn configure(&mut self, context: &LaunchContext<'_>) -> Result<LaunchPlan> {
        let (events, hooks) = super::sdk_tools::routes(&self.input.document, context.steps)?;
        let token = fs::read_to_string("/proc/sys/kernel/random/uuid")?
            .trim()
            .to_owned();
        let ready = context.files.directory().join("claude-sdk-ready.json");
        context.files.write(
            "aw_claude.py",
            include_bytes!("../../native/claude-sdk/aw_claude.py"),
        )?;
        context.files.write(
            "aw_scope.py",
            include_bytes!("../../native/claude-sdk/aw_scope.py"),
        )?;
        context.files.write(
            "aw_resident.py",
            include_bytes!("../../native/claude-sdk/aw_resident.py"),
        )?;
        context.files.write(
            "aw_mcp.py",
            include_bytes!("../../native/claude-sdk/aw_mcp.py"),
        )?;
        context.files.write(
            "aw_invocations.py",
            include_bytes!("../../native/claude-sdk/aw_invocations.py"),
        )?;
        context.files.write(
            "aw_model.py",
            include_bytes!("../../native/claude-sdk/aw_model.py"),
        )?;
        context.files.write(
            "aw_model_stream.py",
            include_bytes!("../../native/claude-sdk/aw_model_stream.py"),
        )?;
        context.files.write(
            "aw_transport.py",
            include_bytes!("../../native/claude-sdk/aw_transport.py"),
        )?;
        let observe = adapter::observation_enabled(&self.input.document, &self.input.target);
        let config = context.files.write("claude-sdk-hooks.json", &serde_json::to_vec(&json!({"executable": context.executable, "binding": context.binding_path, "ready_path": ready, "ready_token": token, "observation_enabled": observe, "hooks": hooks}))?)?;
        let mut command = self.input.command.clone();
        let mut paths = vec![context.files.directory().to_path_buf()];
        if let Some(existing) = command.environment.get(std::ffi::OsStr::new("PYTHONPATH")) {
            paths.extend(std::env::split_paths(existing));
        }
        command
            .environment
            .insert("PYTHONPATH".into(), std::env::join_paths(paths)?);
        command.environment.insert(
            "PYTHONPYCACHEPREFIX".into(),
            context.files.directory().join("pycache").into_os_string(),
        );
        command
            .environment
            .insert("AW_CLAUDE_CONFIG".into(), config.into_os_string());
        let provider_environment = command.environment.clone();
        Ok(LaunchPlan {
            command,
            provider_environment,
            events,
            readiness: Some(Readiness {
                path: ready,
                token,
                adapter: "claude-sdk".into(),
                hooks: hooks.len(),
                timeout: Duration::from_secs(30),
            }),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn binding() -> NativeContext {
        NativeContext {
            instance_id: "instance".into(),
            target: "target".into(),
            cwd: "/work".into(),
        }
    }
    fn native() -> Value {
        json!({"session_id":"native-session", "cwd":"/work", "tool_use_id":"native-call", "tool_name":"Bash", "tool_input":{"command":"exit 7"}, "hook_event_name":"PreToolUse"})
    }
    #[test]
    fn maps_success_failure_and_native_identity_without_adopting_results() {
        let mut payload = native();
        let before = ClaudeSdk
            .normalize(&binding(), "tool.before", &payload)
            .unwrap();
        assert_eq!(before["tool"]["name"], "bash");
        assert_eq!(before["tool"]["native_name"], "Bash");
        payload["hook_event_name"] = json!("PostToolUseFailure");
        payload["error"] = json!("exit 7");
        let after = ClaudeSdk
            .normalize(&binding(), "tool.after", &payload)
            .unwrap();
        assert_eq!(before["session_id"], after["session_id"]);
        assert_eq!(before["tool"]["call_id"], after["tool"]["call_id"]);
        assert_eq!(after["tool"]["result"]["is_error"], true);
        payload["hook_event_name"] = json!("PostToolUse");
        payload["tool_response"] = json!([{"type":"text","text":"ok"}]);
        let success = ClaudeSdk
            .normalize(&binding(), "tool.after", &payload)
            .unwrap();
        assert_eq!(success["tool"]["result"], payload["tool_response"]);
    }
    #[test]
    fn rejects_wrong_routes_cwd_missing_failure_and_call_identity() {
        let original = native();
        assert!(ClaudeSdk
            .normalize(&binding(), "tool.after", &original)
            .is_err());
        for (key, value) in [
            ("cwd", json!("/other")),
            ("tool_use_id", json!("")),
            ("tool_input", Value::Null),
            ("session_id", json!("")),
        ] {
            let mut bad = original.clone();
            bad[key] = value;
            assert!(ClaudeSdk
                .normalize(&binding(), "tool.before", &bad)
                .is_err());
        }
        let mut failure = original;
        failure["hook_event_name"] = json!("PostToolUseFailure");
        assert!(ClaudeSdk
            .normalize(&binding(), "tool.after", &failure)
            .is_err());
    }
}
