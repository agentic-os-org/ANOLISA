//! Claude SDK workers explicitly compose the bridge in their public client factory.

use crate::cli::{
    adapter::{
        self, Adapter, HookBinding, HookEvent, HookOutput, LaunchContext, LaunchInput, LaunchPlan,
        PreparedLaunch, Readiness,
    },
    Exit, Result,
};
use aw_provider::admission::AdmittedStep;
use aw_service::Capabilities;
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, time::Duration};

pub(in crate::cli) struct ClaudeSdk;
struct Prepared {
    input: LaunchInput,
}

impl Adapter for ClaudeSdk {
    fn prepare(&self, input: LaunchInput) -> Result<Box<dyn PreparedLaunch>> {
        if input.flags.keys().any(|key| key.starts_with("--native-")) {
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
        if adapter::version_output(&input.command, &["-c".into(), script.into()])?
            != "0.2.88:2.1.161 (Claude Code)"
        {
            return Err("Claude SDK 0.2.88 with bundled CLI 2.1.161 is required".into());
        }
        Ok(Box::new(Prepared { input }))
    }
    fn normalize(&self, binding: &HookBinding, event: &str, native: &Value) -> Result<Value> {
        let name = native["hook_event_name"].as_str();
        if !matches!(
            (event, name),
            ("tool.before", Some("PreToolUse"))
                | ("tool.after", Some("PostToolUse" | "PostToolUseFailure"))
        ) {
            return Err("Claude SDK event differs from its callback route".into());
        }
        let id = |value: &Value| -> Result<String> {
            value
                .as_str()
                .filter(|s| !s.trim().is_empty() && s.len() <= 512)
                .map(str::to_owned)
                .ok_or_else(|| "Claude SDK requires bounded native identities".into())
        };
        let session = id(&native["session_id"])?;
        let call = id(&native["tool_use_id"])?;
        let tool = id(&native["tool_name"])?;
        if native["cwd"].as_str() != binding.cwd.to_str() || !native["tool_input"].is_object() {
            return Err("Claude SDK callback requires bound cwd and object tool_input".into());
        }
        let result = match name {
            Some("PostToolUse") => native
                .get("tool_response")
                .ok_or("Claude success callback requires tool_response")?
                .clone(),
            Some("PostToolUseFailure") => {
                if !native["error"].is_string() {
                    return Err("Claude failure callback requires error".into());
                }
                json!({"is_error": true, "error": native["error"], "is_interrupt": native["is_interrupt"]})
            }
            _ => Value::Null,
        };
        let standard = match tool.as_str() {
            "Bash" => "bash",
            "Read" => "file_read",
            "Write" | "Edit" => "file_write",
            _ => &tool,
        };
        Ok(
            json!({"name": event, "agent": {"adapter": "claude-sdk", "binding_id": binding.binding.target, "instance_id": binding.binding.instance_id}, "session_id": session,
            "tool": {"name": standard, "native_name": tool, "call_id": call, "input": native["tool_input"], "result": result}, "native": native}),
        )
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
        Capabilities {
            adapter: "claude-sdk".into(),
            version: "0.2.88".into(),
            entrypoint: "sdk-worker".into(),
            events: BTreeMap::from([
                ("tool.before".into(), vec!["observe".into(), "block".into()]),
                (
                    "tool.after".into(),
                    vec!["observe".into(), "replace_result".into()],
                ),
            ]),
        }
    }
    fn validate_steps(&self, steps: &[AdmittedStep]) -> Result<()> {
        routes(&self.input.document, steps).map(|_| ())
    }
    fn configure(&mut self, context: &LaunchContext<'_>) -> Result<LaunchPlan> {
        let (events, hooks) = routes(&self.input.document, context.steps)?;
        let token = fs::read_to_string("/proc/sys/kernel/random/uuid")?
            .trim()
            .to_owned();
        let ready = context.files.0.join("claude-sdk-ready.json");
        context.files.write(
            "aw_claude.py",
            include_bytes!("../../../../../adapters/claude-sdk/aw_claude.py"),
        )?;
        let config = context.files.write("claude-sdk-hooks.json", &serde_json::to_vec(&json!({"executable": context.executable, "binding": context.binding_path, "ready_path": ready, "ready_token": token, "hooks": hooks}))?)?;
        let mut command = self.input.command.clone();
        let mut paths = vec![context.files.0.clone()];
        if let Some(existing) = command.environment.get(std::ffi::OsStr::new("PYTHONPATH")) {
            paths.extend(std::env::split_paths(existing));
        }
        command
            .environment
            .insert("PYTHONPATH".into(), std::env::join_paths(paths)?);
        command.environment.insert(
            "PYTHONPYCACHEPREFIX".into(),
            context.files.0.join("pycache").into_os_string(),
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
fn routes(
    document: &Value,
    steps: &[AdmittedStep],
) -> Result<(BTreeMap<String, HookEvent>, Vec<Value>)> {
    let mut events = BTreeMap::new();
    let mut hooks = Vec::new();
    for event in ["tool.before", "tool.after"] {
        let selected: Vec<_> = steps.iter().filter(|step| step.event == event).collect();
        if selected.is_empty() {
            continue;
        }
        let budget = document["spec"]["events"][event]
            .get("budget_ms")
            .unwrap_or(&document["spec"]["execution"]["default_event_budget_ms"])
            .as_f64()
            .ok_or("invalid Claude event budget")? as u64;
        if !(1..=55000).contains(&budget) {
            return Err("Claude SDK hook budgets must be 1..55000 ms".into());
        }
        for step in &selected {
            hooks.push(json!({"event": event, "step": step.step_id, "on_error": step.on_error, "budget_ms": budget, "replace_result": step.execution.replaces_result()}));
        }
        events.insert(
            event.into(),
            HookEvent {
                budget_ms: budget,
                replacement_step: selected
                    .iter()
                    .find(|step| step.execution.replaces_result())
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

#[cfg(test)]
mod tests {
    use super::*;
    fn binding() -> HookBinding {
        HookBinding {
            adapter: "claude-sdk".into(),
            binding: aw_service::Binding {
                identity: aw_service::Identity {
                    generation: "generation".into(),
                    config_revision: "revision".into(),
                },
                instance_id: "instance".into(),
                target: "target".into(),
                audit_key: "audit".into(),
            },
            socket: "/tmp/aw.sock".into(),
            cwd: "/work".into(),
            events: BTreeMap::new(),
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
