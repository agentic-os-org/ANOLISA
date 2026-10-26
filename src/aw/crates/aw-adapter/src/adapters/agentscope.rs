//! AgentScope 2.0.6 public factory bridge with explicit application opt-in.

use crate::{
    adapter::{
        self, Adapter, HookEvent, HookOutput, LaunchContext, LaunchInput, LaunchPlan,
        NativeContext, PreparedLaunch, Readiness,
    },
    Exit, Result,
};
use aw_provider::admission::AdapterCapabilities as Capabilities;
use aw_provider::admission::AdmittedStep;
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, time::Duration};

const VERSION: &str = "2.0.6";
pub(crate) struct AgentScope;
struct Prepared {
    input: LaunchInput,
}

impl Adapter for AgentScope {
    fn prepare(
        &self,
        input: LaunchInput,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<Box<dyn PreparedLaunch>> {
        if !input.options.is_empty() {
            return Err("AgentScope uses an explicit public factory Bridge; native override flags are unsupported".into());
        }
        let first = input
            .command
            .args
            .first()
            .and_then(|arg| arg.to_str())
            .ok_or("AgentScope requires a Python script or -m module entrypoint")?;
        if first == "-m" {
            if !input
                .command
                .args
                .get(1)
                .and_then(|arg| arg.to_str())
                .is_some_and(|arg| !arg.is_empty() && !arg.starts_with('-'))
            {
                return Err("AgentScope -m requires a module name".into());
            }
        } else if first.starts_with('-') || !first.ends_with(".py") {
            return Err("AgentScope requires a Python script or -m module entrypoint; interpreter override flags are unsupported".into());
        }
        let probe = adapter::Probe(cancelled).version_output(
            &input.command,
            &[
                "-c".into(),
                "from importlib.metadata import version; print(version('agentscope'))".into(),
            ],
        )?;
        if probe != VERSION {
            return Err(format!("AgentScope {VERSION} is required by this adapter").into());
        }
        Ok(Box::new(Prepared { input }))
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
        super::super::adapter::catalog::AGENTSCOPE.capabilities(VERSION, "factory")
    }
    fn validate_steps(&self, steps: &[AdmittedStep]) -> Result<()> {
        event_routes(&self.input.document, steps).map(|_| ())
    }
    fn configure(&mut self, context: &LaunchContext<'_>) -> Result<LaunchPlan> {
        let (events, hooks) = event_routes(&self.input.document, context.steps)?;
        let token = fs::read_to_string("/proc/sys/kernel/random/uuid")?
            .trim()
            .to_owned();
        let ready = context.files.directory().join("agentscope-ready.json");
        let native = context.files.write(
            "agentscope-hooks.json",
            &serde_json::to_vec(&json!({
                "executable": context.executable, "binding": context.binding_path,
                "ready_path": ready, "ready_token": token, "hooks": hooks,
            }))?,
        )?;
        context.files.write(
            "aw_agentscope.py",
            include_bytes!("../../native/agentscope/aw_agentscope.py"),
        )?;
        let mut command = self.input.command.clone();
        let inherited = command.environment.get(std::ffi::OsStr::new("PYTHONPATH"));
        let mut paths = vec![context.files.directory().to_path_buf()];
        if let Some(inherited) = inherited {
            paths.extend(std::env::split_paths(inherited));
        }
        command
            .environment
            .insert("PYTHONPATH".into(), std::env::join_paths(paths)?);
        command
            .environment
            .insert("PYTHONDONTWRITEBYTECODE".into(), "1".into());
        command
            .environment
            .insert("AW_NATIVE_CONFIG".into(), native.into_os_string());
        // Providers retain the original launch environment, without bridge import paths.
        let provider_environment = self.input.command.environment.clone();
        Ok(LaunchPlan {
            command,
            provider_environment,
            events,
            readiness: Some(Readiness {
                path: ready,
                token,
                adapter: "agentscope".into(),
                hooks: hooks.len(),
                timeout: Duration::from_secs(30),
            }),
        })
    }
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
            return Err("AgentScope native event budgets must be 1..55000 ms".into());
        }
        let mut registration = selected.clone();
        if selected.iter().any(|step| step.execution.replaces_result()) {
            // After handlers unwind the native middleware stack inside out.
            registration.reverse();
        }
        for step in registration {
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

fn identifier<'a>(value: &'a Value, label: &str) -> Result<&'a str> {
    value
        .as_str()
        .filter(|value| !value.trim().is_empty() && value.len() <= 512)
        .ok_or_else(|| format!("AgentScope requires nonempty {label} (up to 512 bytes)").into())
}

fn normalize(binding: &NativeContext, event: &str, native: &Value) -> Result<Value> {
    if !matches!(event, "tool.before" | "tool.after") || native["event"] != event {
        return Err("AgentScope callback event does not match the binding".into());
    }
    let session = identifier(&native["session_id"], "session_id")?;
    identifier(&native["tool_call"]["id"], "tool_call.id")?;
    let call = identifier(&native["invocation_id"], "invocation_id")?;
    let tool = identifier(&native["tool_call"]["name"], "tool_call.name")?;
    native["tool_call"]["input"]
        .as_str()
        .ok_or("AgentScope tool_call.input must be a string")?;
    let input = native
        .get("effective_input")
        .filter(|input| input.is_object())
        .ok_or("AgentScope requires SDK-effective input arguments")?;
    if event == "tool.after" && !native["tool_response"].is_object() {
        return Err("AgentScope tool.after requires a native tool_response object".into());
    }
    Ok(json!({
        "name": event,
        "agent": {"adapter": "agentscope", "binding_id": binding.target, "instance_id": binding.instance_id},
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
    use std::path::PathBuf;

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
        )
        .unwrap();
        assert_eq!(
            hooks
                .iter()
                .map(|hook| hook["step"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["compress", "user", "observe"]
        );
        assert_eq!(
            events["tool.after"].replacement_step.as_deref(),
            Some("compress")
        );
        assert_eq!(hooks[0]["replace_result"], true);
        assert_eq!(hooks[1]["replace_result"], false);
        assert_eq!(hooks[2]["replace_result"], false);
    }

    fn binding() -> NativeContext {
        NativeContext {
            instance_id: "instance".into(),
            target: "sdk".into(),
            cwd: PathBuf::from("/cwd"),
        }
    }

    #[test]
    fn normalization_keeps_native_terminal_and_call_identity() {
        let native = json!({"event": "tool.after", "session_id": "session-one", "invocation_id": "invocation-one", "effective_input": {"nested":[true,null]}, "tool_call": {
            "id": "same-call", "name": "any-tool", "input": "{\"nested\":[true,null]"},
            "tool_response": {"state": "interrupted", "content": []}});
        let value = normalize(&binding(), "tool.after", &native).unwrap();
        assert_eq!(value["agent"]["adapter"], "agentscope");
        assert_eq!(value["tool"]["input"], json!({"nested": [true, null]}));
        assert_eq!(value["tool"]["call_id"], "invocation-one");
        assert_eq!(value["tool"]["result"], native["tool_response"]);
        assert_eq!(value["native"], native);
    }

    #[test]
    fn incomplete_or_mismatched_native_callbacks_are_rejected() {
        let mut native = json!({"event": "tool.before", "session_id": "session", "invocation_id": "invocation", "effective_input": {}, "tool_call": {
            "id": "call", "name": "tool", "input": "{}"}});
        assert!(normalize(&binding(), "tool.after", &native).is_err());
        native["event"] = json!("tool.after");
        assert!(normalize(&binding(), "tool.after", &native).is_err());
        native["tool_response"] = json!({});
        native["session_id"] = json!("");
        assert!(normalize(&binding(), "tool.after", &native).is_err());
        native["session_id"] = json!("session");
        native["tool_call"]["input"] = Value::Null;
        assert!(normalize(&binding(), "tool.after", &native).is_err());
        native["tool_call"]["input"] = json!("{}");
        assert!(normalize(&binding(), "tool.after", &native).is_ok());
        native["effective_input"] = Value::Null;
        assert!(normalize(&binding(), "tool.after", &native).is_err());
        native["effective_input"] = json!([]);
        assert!(normalize(&binding(), "tool.after", &native).is_err());
        native["effective_input"] = json!({});
        native["invocation_id"] = Value::Null;
        assert!(normalize(&binding(), "tool.after", &native).is_err());
    }
}
