//! Local Codex CLI hooks preserve native permissions and tool results.

mod installation;
mod preflight;

#[cfg(test)]
mod tests;

use crate::{
    adapter::{
        self, Adapter, HookEvent, HookOutput, LaunchContext, LaunchInput, LaunchPlan,
        NativeContext, PreparedLaunch,
    },
    Exit, InstallOptions, Result,
};
use aw_core::admission::{AdapterCapabilities as Capabilities, AdmittedStep, StepExecution};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path};

pub(crate) struct Codex;
struct Prepared {
    input: LaunchInput,
    invocation: preflight::Invocation,
    version: String,
}

impl Adapter for Codex {
    fn prepare(
        &self,
        input: LaunchInput,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<Box<dyn PreparedLaunch>> {
        if !input.options.is_empty()
            || input.options.reloadable
            || input.options.resident_policy_reload
        {
            return Err(
                "Codex uses its existing native configuration, not launcher overrides".into(),
            );
        }
        let invocation = preflight::arguments(&input.command.args)?;
        let version =
            adapter::Probe(cancelled).version_output(&input.command, &["--version".into()])?;
        let version = version
            .strip_prefix("codex-cli ")
            .ok_or_else(|| format!("unexpected Codex version output: {version:?}"))?;
        let version = crate::version::reported(version, "Codex CLI")?;
        let events: Vec<_> = ["input.submit", "tool.before", "tool.after"]
            .into_iter()
            .filter(|event| {
                adapter::observation_enabled(&input.document, &input.target)
                    || (input.document["spec"]["events"][*event]["enabled"] == true
                        && input.document["spec"]["events"][*event]["steps"]
                            .as_array()
                            .is_some_and(|steps| !steps.is_empty()))
            })
            .collect();
        preflight::hooks(
            &input.command,
            &events,
            invocation.bypass_hook_trust,
            input.document["spec"]["events"]["tool.before"]["required"] == true,
            cancelled,
        )?;
        Ok(Box::new(Prepared {
            input,
            invocation,
            version,
        }))
    }

    fn normalize(&self, context: &NativeContext, event: &str, native: &Value) -> Result<Value> {
        let expected = native_event(event)?;
        if native["hook_event_name"] != expected || native["cwd"].as_str() != context.cwd.to_str() {
            return Err("Codex callback event or cwd does not match this launch".into());
        }
        let identity = |key: &str| -> Result<&str> {
            native[key]
                .as_str()
                .filter(|s| !s.trim().is_empty() && s.len() <= 512)
                .ok_or_else(|| format!("Codex requires nonempty {key} up to 512 bytes").into())
        };
        if event == "input.submit" {
            return super::submission::normalize_turn(
                context, native, "codex", "prompt", "turn_id",
            );
        }
        let session = identity("session_id")?;
        let call = identity("tool_use_id")?;
        let tool = identity("tool_name")?;
        if !native["tool_input"].is_object() {
            return Err("Codex tool_input must be an object".into());
        }
        if event == "tool.after" && native.get("tool_response").is_none() {
            return Err("Codex tool.after requires tool_response".into());
        }
        Ok(json!({"name":event,
            "agent":{"adapter":"codex","binding_id":context.target,"instance_id":context.instance_id},
            "session_id":session,
            "tool":{"name":tool,"native_name":tool,"call_id":call,"input":native["tool_input"],
                "result":native.get("tool_response").cloned().unwrap_or(Value::Null)},
            "native":native}))
    }

    fn reply(&self, blocked: bool) -> HookOutput {
        HookOutput {
            exit: Exit::Code(if blocked { 2 } else { 0 }),
            stdout: if blocked { vec![] } else { b"{}\n".to_vec() },
            stderr: if blocked {
                b"Blocked by AW policy\n".to_vec()
            } else {
                vec![]
            },
        }
    }

    fn install(
        &self,
        options: &InstallOptions,
        _config: &aw_config::Configuration,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<()> {
        if options.profile.is_some() {
            return Err("Codex installation uses CODEX_HOME, not a native profile".into());
        }
        installation::install(cancelled)
    }
}

impl PreparedLaunch for Prepared {
    fn capabilities(&self) -> Capabilities {
        adapter::catalog::CODEX.capabilities(
            &self.version,
            if self.invocation.exec { "exec" } else { "tui" },
        )
    }

    fn validate_steps(&self, steps: &[AdmittedStep]) -> Result<()> {
        if steps
            .iter()
            .any(|step| matches!(step.execution, StepExecution::Native))
        {
            return Err("Codex AW chains accept boolean Hooks or Providers; keep native Hooks in Codex configuration".into());
        }
        routes(&self.input, steps)?;
        Ok(())
    }

    fn configure(&mut self, context: &LaunchContext<'_>) -> Result<LaunchPlan> {
        let (events, chains) = routes(&self.input, context.steps)?;
        let config = context.files.write(
            "codex-chain.json",
            &serde_json::to_vec(&json!({
                "binding":context.binding_path,"chains":chains
            }))?,
        )?;
        let mut command = self.input.command.clone();
        if !self.invocation.exec {
            command.args.insert(0, "--no-daemon".into());
        }
        command
            .environment
            .insert("AW_CODEX_CONFIG".into(), config.into_os_string());
        let provider_environment = command.environment.clone();
        Ok(LaunchPlan {
            command,
            provider_environment,
            events,
            readiness: None,
        })
    }
}

pub(super) fn native_event(event: &str) -> Result<&'static str> {
    match event {
        "input.submit" => Ok("UserPromptSubmit"),
        "tool.before" => Ok("PreToolUse"),
        "tool.after" => Ok("PostToolUse"),
        _ => Err("unsupported Codex Hook event".into()),
    }
}

pub(super) fn hook_command(executable: &Path, event: &str) -> Result<String> {
    native_event(event)?;
    let executable = executable.to_str().ok_or("non-UTF-8 AW executable")?;
    Ok(format!(
        "'{}' codex-hook --event {event}",
        executable.replace('\'', "'\"'\"'")
    ))
}

fn routes(
    input: &LaunchInput,
    steps: &[AdmittedStep],
) -> Result<(BTreeMap<String, HookEvent>, Value)> {
    let mut events = BTreeMap::new();
    let mut chains = json!({});
    for event in ["input.submit", "tool.before", "tool.after"] {
        let selected = adapter::native_steps(
            steps,
            event,
            adapter::observation_enabled(&input.document, &input.target),
        );
        if selected.is_empty() {
            continue;
        }
        let spec = &input.document["spec"];
        let budget = spec["events"][event]
            .get("budget_ms")
            .unwrap_or(&spec["execution"]["default_event_budget_ms"])
            .as_f64()
            .filter(|value| {
                value.is_finite() && value.fract() == 0.0 && (1.0..=55000.0).contains(value)
            })
            .ok_or("Codex event budget must be an integer in 1..55000 ms")?
            as u64;
        chains[event] = json!({"budget_ms":budget,"steps":selected.iter()
            .map(|s| json!({"id":s.step_id,"on_error":s.on_error})).collect::<Vec<_>>()});
        events.insert(
            event.into(),
            HookEvent {
                budget_ms: budget,
                replacement_step: None,
                steps: selected
                    .into_iter()
                    .map(|s| (s.step_id, s.on_error))
                    .collect(),
            },
        );
    }
    Ok((events, chains))
}
