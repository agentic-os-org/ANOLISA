//! Pinned DeepSeek Harness headless plugin with launch-scoped registration.

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

const VERSION: &str = "0.2.1-rc.2";
pub(in crate::cli) struct DeepSeek;
struct Prepared {
    input: LaunchInput,
    app_offset: usize,
}

impl Adapter for DeepSeek {
    fn prepare(&self, input: LaunchInput) -> Result<Box<dyn PreparedLaunch>> {
        if [
            "--native-settings",
            "--native-profile",
            "--native-state-dir",
        ]
        .iter()
        .any(|flag| input.flags.contains_key(*flag))
        {
            return Err("DeepSeek Harness uses DSH_HOME and native --patch configuration".into());
        }
        let args = &input.command.args;
        if args.first().and_then(|v| v.to_str()) != Some("--profile")
            || args.get(1).and_then(|v| v.to_str()) != Some("headless")
        {
            return Err("DeepSeek argv must start with dsh --profile headless".into());
        }
        let mut offset = 2;
        while args.get(offset).and_then(|v| v.to_str()) == Some("--patch") {
            if args.get(offset + 1).is_none() {
                return Err("DeepSeek --patch requires a path".into());
            }
            offset += 2;
        }
        for arg in &args[offset..] {
            if arg.to_str().is_some_and(|arg| {
                matches!(
                    arg.split('=').next(),
                    Some(
                        "--profile"
                            | "--patch"
                            | "--from-default-profile"
                            | "--dump-config"
                            | "--dump-config-schema"
                            | "--dump-default-config"
                    )
                )
            }) {
                return Err(
                    "DeepSeek launcher overrides must not bypass the AW headless overlay".into(),
                );
            }
        }
        if adapter::version_output(&input.command, &["--version".into()])? != VERSION {
            return Err(format!("AW requires DeepSeek Harness {VERSION}").into());
        }
        Ok(Box::new(Prepared {
            input,
            app_offset: offset,
        }))
    }

    fn normalize(&self, binding: &HookBinding, event: &str, native: &Value) -> Result<Value> {
        if !matches!(event, "tool.before" | "tool.after") || native["event"] != event {
            return Err("DeepSeek event does not match binding".into());
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
            "agent":{"adapter":"deepseek-harness", "binding_id":binding.binding.target,
                "instance_id":binding.binding.instance_id},
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
        adapter::catalog::DEEPSEEK.capabilities(VERSION, "headless")
    }
    fn validate_steps(&self, steps: &[AdmittedStep]) -> Result<()> {
        routes(&self.input.document, steps).map(|_| ())
    }
    fn configure(&mut self, context: &LaunchContext<'_>) -> Result<LaunchPlan> {
        let (events, hooks) = routes(&self.input.document, context.steps)?;
        let token = fs::read_to_string("/proc/sys/kernel/random/uuid")?
            .trim()
            .to_owned();
        let ready = context.files.0.join("deepseek-ready.json");
        let config = context.files.write("deepseek-hooks.json", &serde_json::to_vec(&json!({
            "executable":context.executable, "binding":context.binding_path,
            "ready_path":ready, "ready_token":token, "hooks":hooks,
            "replacement_step":events.get("tool.after").and_then(|route| route.replacement_step.as_ref())
        }))?)?;
        let plugin = context.files.write(
            "deepseek-plugin.mjs",
            include_bytes!("../../../../../adapters/deepseek-harness/plugin.mjs"),
        )?;
        context.files.write(
            "callback.mjs",
            include_bytes!("../../../../../adapters/deepseek-harness/callback.mjs"),
        )?;
        // JSON is a YAML subset. Add the dependency without replacing runner config.
        let patch = context.files.write(
            "deepseek.patch.json",
            &serde_json::to_vec(&json!([
                {"insert":[{"id":"aw-deepseek", "name":plugin}]},
                {"id":"headless-runner", "inject":["headlessStartup", "awDeepseek"]}
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
    for event in ["tool.before", "tool.after"] {
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
