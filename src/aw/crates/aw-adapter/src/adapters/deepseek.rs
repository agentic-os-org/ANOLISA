//! DeepSeek Harness profile plugin with launch-scoped registration.

use crate::{
    adapter::{
        self, Adapter, HookEvent, HookOutput, LaunchContext, LaunchInput, LaunchPlan,
        NativeContext, PreparedLaunch, Readiness,
    },
    Exit, Result,
};
use aw_core::admission::{AdapterCapabilities as Capabilities, AdmittedStep, StepExecution};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, time::Duration};

pub(crate) struct DeepSeek;

pub(crate) fn replaceable(native: &Value) -> bool {
    native["nested"] != true
        && native["tool_response"]["isError"] == false
        && native["tool_response"]["content"]
            .as_array()
            .is_some_and(|blocks| {
                blocks.len() == 1 && blocks[0]["type"] == "text" && blocks[0]["text"].is_string()
            })
}

struct Prepared {
    input: LaunchInput,
    app_offset: usize,
    profile: String,
    version: String,
}

impl Adapter for DeepSeek {
    fn prepare(
        &self,
        input: LaunchInput,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<Box<dyn PreparedLaunch>> {
        if !input.options.is_empty()
            || input.options.reloadable
            || input.options.resident_policy_reload
        {
            return Err("DeepSeek Harness uses DSH_HOME and native --patch configuration".into());
        }
        let args = &input.command.args;
        let mut profile = args
            .first()
            .and_then(|arg| arg.to_str())
            .filter(|arg| !arg.starts_with('-'))
            .map(str::to_owned);
        if profile.as_deref() == Some("plugin") {
            return Err("DeepSeek plugin management does not launch an Agent".into());
        }
        let mut offset = usize::from(profile.is_some());
        while let Some(arg) = args.get(offset).and_then(|arg| arg.to_str()) {
            match arg {
                "--profile" => {
                    if profile.is_some() {
                        return Err("DeepSeek profile may be selected only once".into());
                    }
                    profile = Some(
                        args.get(offset + 1)
                            .and_then(|arg| arg.to_str())
                            .filter(|arg| !arg.is_empty())
                            .ok_or("DeepSeek --profile requires a name")?
                            .to_owned(),
                    );
                    offset += 2;
                }
                "--patch" => {
                    if args.get(offset + 1).is_none() {
                        return Err("DeepSeek --patch requires a path".into());
                    }
                    offset += 2;
                }
                "--from-default-profile" => {
                    if args.get(offset + 1).is_none() {
                        return Err("DeepSeek --from-default-profile requires a name".into());
                    }
                    offset += 2;
                }
                "--dump-config" | "--dump-config-schema" | "--dump-default-config" => {
                    return Err("DeepSeek configuration commands cannot launch an Agent".into());
                }
                _ if arg.starts_with("--from-default-profile=") => {
                    if arg.len() == "--from-default-profile=".len() {
                        return Err("DeepSeek --from-default-profile requires a name".into());
                    }
                    offset += 1;
                }
                _ if arg.starts_with("--profile=") => {
                    if profile.is_some() || arg.len() == "--profile=".len() {
                        return Err("DeepSeek --profile requires one name".into());
                    }
                    profile = Some(arg["--profile=".len()..].to_owned());
                    offset += 1;
                }
                _ if arg.starts_with("--patch=") => {
                    if arg.len() == "--patch=".len() {
                        return Err("DeepSeek --patch requires a path".into());
                    }
                    offset += 1;
                }
                _ => break,
            }
        }
        let profile = profile.ok_or("DeepSeek Agent launch requires a profile")?;
        if profile.trim().is_empty() || profile.len() > 128 || profile.chars().any(char::is_control)
        {
            return Err("DeepSeek profile must be 1..128 bytes without control characters".into());
        }
        let version = crate::version::reported(
            &adapter::Probe(cancelled).version_output(&input.command, &["--version".into()])?,
            "DeepSeek Harness",
        )?;
        Ok(Box::new(Prepared {
            input,
            app_offset: offset,
            profile,
            version,
        }))
    }

    fn normalize(&self, binding: &NativeContext, event: &str, native: &Value) -> Result<Value> {
        if !matches!(event, "input.submit" | "tool.before" | "tool.after")
            || native["event"] != event
        {
            return Err("DeepSeek event does not match binding".into());
        }
        if event == "input.submit" {
            return super::submission::normalize(binding, native, "deepseek-harness", "input_text");
        }
        for value in [
            &native["session_id"],
            &native["invocation_id"],
            &native["tool_call"]["id"],
            &native["tool_call"]["name"],
        ] {
            if !value
                .as_str()
                .is_some_and(|v| !v.trim().is_empty() && v.len() <= 512)
            {
                return Err("DeepSeek requires bounded session and invocation identity".into());
            }
        }
        if !native["tool_call"]["input"].is_object()
            || (event == "tool.after"
                && (!native["tool_response"].is_object()
                    || !native["tool_response"]["isError"].is_boolean()))
        {
            return Err("DeepSeek requires object arguments and a typed result".into());
        }
        Ok(json!({"name":event,
            "agent":{"adapter":"deepseek-harness", "binding_id":binding.target,
                "instance_id":binding.instance_id},
            "session_id":native["session_id"],
            "tool":{"name":native["tool_call"]["name"], "native_name":native["tool_call"]["name"],
                "call_id":native["invocation_id"], "input":native["tool_call"]["input"],
                "result":native.get("tool_response").cloned().unwrap_or(Value::Null)},
            "native":native}))
    }

    fn reply(&self, blocked: bool) -> HookOutput {
        HookOutput {
            exit: Exit::Code(0),
            stderr: Vec::new(),
            stdout: if blocked {
                b"{\"block\":true,\"reason\":\"Blocked by AW policy\"}\n".to_vec()
            } else {
                b"{}\n".to_vec()
            },
        }
    }
}

impl PreparedLaunch for Prepared {
    fn capabilities(&self) -> Capabilities {
        adapter::catalog::DEEPSEEK.capabilities(&self.version, &self.profile)
    }
    fn validate_steps(&self, steps: &[AdmittedStep]) -> Result<()> {
        routes(&self.input.document, steps).map(|_| ())
    }
    fn configure(&mut self, context: &LaunchContext<'_>) -> Result<LaunchPlan> {
        let (events, hooks) = routes(&self.input.document, context.steps)?;
        let token = fs::read_to_string("/proc/sys/kernel/random/uuid")?
            .trim()
            .to_owned();
        let ready = context.files.directory().join("deepseek-ready.json");
        let config = context.files.write("deepseek-hooks.json", &serde_json::to_vec(&json!({
            "executable":context.executable, "binding":context.binding_path,
            "ready_path":ready, "ready_token":token, "hooks":hooks,
            "replacement_step":events.get("tool.after").and_then(|route| route.replacement_step.as_ref())
        }))?)?;
        let plugin = context.files.write(
            "deepseek-plugin.mjs",
            include_bytes!("../../native/deepseek-harness/plugin.mjs"),
        )?;
        context.files.write(
            "callback.mjs",
            include_bytes!("../../native/deepseek-harness/callback.mjs"),
        )?;
        // JSON is a YAML subset. Agent work waits for Hook registration.
        let patch = context.files.write(
            "deepseek.patch.json",
            &serde_json::to_vec(&json!([
                {"insert":[{"id":"aw-deepseek", "name":plugin}]},
                {"id":"agent-loop", "inject":["awDeepseek"]}
            ]))?,
        )?;
        let mut command = self.input.command.clone();
        command.args.splice(
            self.app_offset..self.app_offset,
            ["--patch".into(), patch.into_os_string()],
        );
        command
            .environment
            .insert("AW_NATIVE_CONFIG".into(), config.into_os_string());
        Ok(LaunchPlan {
            provider_environment: command.environment.clone(),
            command,
            events,
            readiness: Some(Readiness {
                path: ready,
                token,
                adapter: "deepseek-harness".into(),
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
    for event in ["input.submit", "tool.before", "tool.after"] {
        let selected: Vec<_> = steps.iter().filter(|step| step.event == event).collect();
        if selected.is_empty() {
            continue;
        }
        let budget = document["spec"]["events"][event]
            .get("budget_ms")
            .unwrap_or(&document["spec"]["execution"]["default_event_budget_ms"])
            .as_f64()
            .map(|v| v as u64)
            .ok_or("invalid DeepSeek event budget")?;
        if !(1..=55000).contains(&budget) {
            return Err("DeepSeek event budget must be 1..55000 ms".into());
        }
        for step in &selected {
            if matches!(&step.execution, StepExecution::Provider { operation, .. } if operation == "check_skill")
            {
                return Err("DeepSeek cannot check a resolved Skill object".into());
            }
            hooks.push(json!({"event":event, "step":step.step_id,
                "on_error":step.on_error, "budget_ms":budget}));
        }
        events.insert(
            event.into(),
            HookEvent {
                budget_ms: budget,
                replacement_step: selected
                    .iter()
                    .find(|s| s.execution.replaces_result())
                    .map(|s| s.step_id.clone()),
                steps: selected
                    .into_iter()
                    .map(|s| (s.step_id.clone(), s.on_error.clone()))
                    .collect(),
            },
        );
    }
    Ok((events, hooks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn projection_matches_the_native_replacement_boundary() {
        let base = json!({"tool_call":{"name":"read_file","input":{}},
            "tool_response":{"isError":false,"content":[{"type":"text","text":"original"}]}});
        assert!(crate::projection::project("deepseek-harness", &base).is_some());
        assert!(crate::projection::reply("deepseek-harness", &base, "replaced").is_ok());
        for response in [
            json!({"isError":true,"content":[{"type":"text","text":"error"}]}),
            json!({"isError":false,"stdout":"stdout"}),
            json!({"isError":false,"llmContent":"llm"}),
            json!({"isError":false,"output":"output"}),
        ] {
            let mut native = base.clone();
            native["tool_response"] = response;
            assert!(crate::projection::project("deepseek-harness", &native).is_none());
            assert!(crate::projection::reply("deepseek-harness", &native, "replaced").is_err());
        }
        let mut nested = base.clone();
        nested["nested"] = json!(true);
        assert!(crate::projection::reply("deepseek-harness", &nested, "replaced").is_err());
    }

    #[test]
    fn submission_uses_the_shared_input_contract() {
        let capabilities = adapter::catalog::DEEPSEEK.capabilities("0.2.1-rc.5", "headless");
        assert_eq!(
            capabilities.events["input.submit"],
            vec!["observe", "block"]
        );
        let binding = NativeContext {
            target: "dsh".into(),
            instance_id: "instance".into(),
            cwd: PathBuf::from("/tmp"),
        };
        let native = json!({"event":"input.submit", "session_id":"session",
            "aw_turn_id":"1:message", "input_text":"inspect this repository"});
        let event = DeepSeek
            .normalize(&binding, "input.submit", &native)
            .unwrap();
        assert_eq!(event["name"], "input.submit");
        assert_eq!(event["turn_id"], "1:message");
        assert_eq!(event["input"]["text"], "inspect this repository");
        assert!(DeepSeek
            .normalize(&binding, "tool.before", &native)
            .is_err());
    }

    #[test]
    fn resolved_skill_check_is_rejected_for_both_failure_actions() {
        let document = json!({"spec":{"execution":{"default_event_budget_ms":1000},
            "events":{"tool.before":{"enabled":true}}}});
        for on_error in ["block", "report"] {
            let step = AdmittedStep {
                event: "tool.before".into(),
                step_id: "skill".into(),
                provider: "security".into(),
                execution: StepExecution::Provider {
                    operation: "check_skill".into(),
                    effects: vec!["observe".into(), "block".into()],
                },
                on_error: on_error.into(),
                event_tools: vec!["*".into()],
                step_tools: vec!["*".into()],
            };
            assert!(routes(&document, &[step]).is_err(), "{on_error}");
        }
    }
}
