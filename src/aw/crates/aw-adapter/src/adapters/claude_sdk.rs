//! Claude SDK workers explicitly compose the bridge in their public client factory.

use crate::{
    adapter::{
        self, Adapter, HookOutput, LaunchContext, LaunchInput, LaunchPlan, NativeContext,
        PreparedLaunch, Readiness,
    },
    Exit, Result,
};
use aw_core::admission::{AdapterCapabilities as Capabilities, AdmittedStep};
use serde_json::{json, Value};
use std::{fs, time::Duration};

pub(crate) struct ClaudeSdk;
struct Prepared {
    input: LaunchInput,
    version: String,
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
        let output = adapter::Probe(cancelled)
            .version_output(&input.command, &["-c".into(), script.into()])?;
        let (sdk, cli) = output
            .split_once(':')
            .ok_or("Claude SDK probe must report SDK and bundled CLI versions")?;
        let version = crate::version::reported(sdk, "Claude SDK")?;
        crate::version::reported(
            cli.strip_suffix(" (Claude Code)")
                .ok_or("Claude SDK probe must identify its bundled Claude Code CLI")?,
            "Claude Code",
        )?;
        Ok(Box::new(Prepared { input, version }))
    }
    fn normalize(&self, binding: &NativeContext, event: &str, native: &Value) -> Result<Value> {
        if event == "input.submit" {
            if native["hook_event_name"] != "AWInputSubmit"
                || native["cwd"].as_str() != binding.cwd.to_str()
                || native.get("aw_source").is_some()
            {
                return Err("Claude submission requires its factory route and bound cwd".into());
            }
            return super::submission::normalize(binding, native, "claude-sdk", "input_text");
        }
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
        super::super::adapter::catalog::CLAUDE_SDK.capabilities(&self.version, "sdk-worker")
    }
    fn validate_steps(&self, steps: &[AdmittedStep]) -> Result<()> {
        super::sdk_tools::routes(&self.input.document, steps, "claude-sdk").map(|_| ())
    }
    fn configure(&mut self, context: &LaunchContext<'_>) -> Result<LaunchPlan> {
        let (events, hooks) =
            super::sdk_tools::routes(&self.input.document, context.steps, "claude-sdk")?;
        let token = fs::read_to_string("/proc/sys/kernel/random/uuid")?
            .trim()
            .to_owned();
        let ready = context.files.directory().join("claude-sdk-ready.json");
        context.files.write(
            "aw_claude.py",
            include_bytes!("../../native/claude-sdk/aw_claude.py"),
        )?;
        context.files.write(
            "aw_submission.py",
            include_bytes!("../../native/claude-sdk/aw_submission.py"),
        )?;
        context.files.write(
            "aw_foreground.py",
            include_bytes!("../../native/claude-sdk/aw_foreground.py"),
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
    fn submission_fault_recovery_retains_existing_terminal_classification() {
        for failure in [
            "exit",
            "incomplete_input",
            "provider",
            "protocol",
            "transport",
        ] {
            let fault = json!({"status":"error", "kind":"provider", "effects":[],
                "failure":failure, "failure_detail":"deadline_exceeded", "failure_action":"report"});
            let invoke = |value, action| {
                crate::callback::response(
                    &ClaudeSdk,
                    "input.submit",
                    action,
                    value,
                    "claude-sdk",
                    &json!({}),
                    &mut Vec::new(),
                )
            };
            assert!(matches!(
                invoke(fault.clone(), "report").unwrap().exit,
                Exit::Code(0)
            ));
            let mut blocked = fault.clone();
            blocked["failure_action"] = json!("block");
            assert!(matches!(
                invoke(blocked, "block").unwrap().exit,
                Exit::Code(2)
            ));
            for (key, value) in [
                ("status", json!("unknown")),
                ("kind", json!("native")),
                ("effects", json!([{"type":"block"}])),
                ("failure", json!("native_environment")),
                ("failure", json!("unknown")),
                ("failure_action", json!("block")),
            ] {
                let mut invalid = fault.clone();
                invalid[key] = value;
                assert!(invoke(invalid, "report").is_err());
            }
            if failure == "transport" {
                for detail in [
                    Value::Null,
                    json!("cancelled"),
                    json!("cleanup"),
                    json!("unknown"),
                ] {
                    let mut invalid = fault.clone();
                    invalid["failure_detail"] = detail;
                    assert!(invoke(invalid, "report").is_err());
                }
            }
        }
        assert!(crate::callback::supports_event_close("claude-sdk"));
        assert!(crate::callback::supports_event_close("agentscope"));
        assert!(!crate::callback::supports_event_close("qoder-sdk"));
    }
    #[test]
    fn submissions_preserve_text_and_reject_unbound_identity() {
        let native = json!({"hook_event_name":"AWInputSubmit", "cwd":"/work",
            "session_id":"session", "aw_turn_id":"turn", "input_text":" 中文 \n"});
        let event = ClaudeSdk
            .normalize(&binding(), "input.submit", &native)
            .unwrap();
        assert_eq!(event["input"]["text"], native["input_text"]);
        assert_eq!(event["session_id"], "session");
        assert_eq!(event["turn_id"], "turn");
        assert!(event.get("tool").is_none());
        for (key, value) in [
            ("session_id", json!("")),
            ("aw_turn_id", json!(" ")),
            ("session_id", json!("x".repeat(513))),
            ("input_text", json!({"text":"x"})),
            ("cwd", json!("/other")),
            ("hook_event_name", json!("UserPromptSubmit")),
            ("aw_source", json!("mcp")),
        ] {
            let mut invalid = native.clone();
            invalid[key] = value;
            assert!(ClaudeSdk
                .normalize(&binding(), "input.submit", &invalid)
                .is_err());
        }
        assert!(ClaudeSdk
            .normalize(&binding(), "tool.before", &native)
            .is_err());
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
