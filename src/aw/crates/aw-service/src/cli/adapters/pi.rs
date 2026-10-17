//! Pi Coding Agent 0.84.2 public factory bridge in an owned foreground Node worker.

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
use std::{collections::BTreeMap, ffi::OsString, fs, path::PathBuf, time::Duration};

const VERSION: &str = "0.84.2";
pub(in crate::cli) struct Pi;
struct Prepared {
    input: LaunchInput,
    application: PathBuf,
}

impl Adapter for Pi {
    fn prepare(&self, mut input: LaunchInput) -> Result<Box<dyn PreparedLaunch>> {
        for flag in [
            "--native-settings",
            "--native-profile",
            "--native-state-dir",
        ] {
            if input.flags.contains_key(flag) {
                return Err("Pi SDK workers do not accept native CLI overrides".into());
            }
        }
        let application = input
            .command
            .args
            .first()
            .map(PathBuf::from)
            .ok_or("Pi argv requires node and an absolute application module")?;
        if !application.is_absolute()
            || !application.is_file()
            || application.extension().and_then(|s| s.to_str()) != Some("mjs")
        {
            return Err("Pi application must be an existing absolute .mjs file".into());
        }
        let application = application.canonicalize()?;
        // --eval executes only AW metadata validation, never the business application factory.
        let probe = format!(
            "process.env.AW_PI_PROBE='1';\n{}",
            include_str!("../../../../../adapters/pi/entrypoint.mjs")
        );
        let result = adapter::version_output(
            &input.command,
            &[
                "--input-type=module".into(),
                "--eval".into(),
                probe.into(),
                "--".into(),
                application.clone().into_os_string(),
            ],
        )?;
        if result != format!("AW Pi SDK {VERSION}") {
            return Err("Pi SDK metadata probe did not validate the required packages".into());
        }
        input
            .command
            .environment
            .remove(std::ffi::OsStr::new("AW_PI_PROBE"));
        Ok(Box::new(Prepared { input, application }))
    }
    fn normalize(&self, binding: &HookBinding, event: &str, native: &Value) -> Result<Value> {
        if !matches!(event, "tool.before" | "tool.after") || native["event"] != event {
            return Err("Pi event does not match binding".into());
        }
        let identifier = |value: &Value| -> Result<String> {
            value
                .as_str()
                .filter(|v| !v.trim().is_empty() && v.len() <= 512)
                .map(str::to_owned)
                .ok_or_else(|| {
                    "Pi requires nonempty session/call/tool identity up to 512 bytes".into()
                })
        };
        let session = identifier(&native["session_id"])?;
        identifier(&native["tool_call"]["id"])?;
        let call = native["invocation_id"]
            .as_str()
            .ok_or("Pi requires an invocation UUID")?;
        if !invocation_uuid(call) {
            return Err("Pi requires a canonical invocation UUID v4".into());
        }
        let tool = identifier(&native["tool_call"]["name"])?;
        if !native["tool_call"]["input"].is_object() {
            return Err("Pi requires validated object tool arguments".into());
        }
        if event == "tool.after"
            && (!native["tool_response"].is_object()
                || !native["tool_response"]["isError"].is_boolean())
        {
            return Err("Pi tool.after requires a result and isError".into());
        }
        Ok(
            json!({"name":event,"agent":{"adapter":"pi","binding_id":binding.binding.target,"instance_id":binding.binding.instance_id},
            "session_id":session,"tool":{"name":tool,"native_name":tool,"call_id":call,"input":native["tool_call"]["input"],"result":native.get("tool_response").cloned().unwrap_or(Value::Null)},"native":native}),
        )
    }
    fn reply(&self, blocked: bool) -> HookOutput {
        HookOutput {
            exit: Exit::Code(0),
            stdout: if blocked {
                b"{\"block\":true,\"reason\":\"Blocked by AW policy\"}\n".to_vec()
            } else {
                b"{}\n".to_vec()
            },
            stderr: Vec::new(),
        }
    }
}
impl PreparedLaunch for Prepared {
    fn capabilities(&self) -> Capabilities {
        super::super::adapter::catalog::PI.capabilities(VERSION, "sdk")
    }
    fn validate_steps(&self, steps: &[AdmittedStep]) -> Result<()> {
        routes(&self.input.document, steps).map(|_| ())
    }
    fn configure(&mut self, context: &LaunchContext<'_>) -> Result<LaunchPlan> {
        let (events, hooks) = routes(&self.input.document, context.steps)?;
        let token = fs::read_to_string("/proc/sys/kernel/random/uuid")?
            .trim()
            .to_owned();
        let ready = context.files.0.join("pi-ready.json");
        let config=context.files.write("pi-hooks.json",&serde_json::to_vec(&json!({"executable":context.executable,"binding":context.binding_path,"ready_path":ready,"ready_token":token,"replacement_step":events.get("tool.after").and_then(|route|route.replacement_step.as_ref()),"hooks":hooks}))?)?;
        let entrypoint = context.files.write(
            "pi-entrypoint.mjs",
            include_bytes!("../../../../../adapters/pi/entrypoint.mjs"),
        )?;
        context.files.write(
            "bridge.mjs",
            include_bytes!("../../../../../adapters/pi/bridge.mjs"),
        )?;
        let mut command = self.input.command.clone();
        command.args = std::iter::once(entrypoint.into_os_string())
            .chain(std::iter::once(self.application.clone().into_os_string()))
            .chain(self.input.command.args.iter().skip(1).cloned())
            .collect::<Vec<OsString>>();
        command
            .environment
            .insert("AW_NATIVE_CONFIG".into(), config.into_os_string());
        let provider_environment = command.environment.clone();
        Ok(LaunchPlan {
            command,
            provider_environment,
            events,
            readiness: Some(Readiness {
                path: ready,
                token,
                adapter: "pi".into(),
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
            .ok_or("invalid Pi event budget")?;
        if !(1..=55000).contains(&budget) {
            return Err("Pi event budget must be 1..55000 ms".into());
        }
        for step in &selected {
            hooks.push(json!({"event":event,"step":step.step_id,"on_error":step.on_error,"budget_ms":budget}));
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
                    .map(|s| (s.step_id.clone(), s.on_error.clone()))
                    .collect(),
            },
        );
    }
    Ok((events, hooks))
}

// Model-issued tool IDs may recur in later turns; only this per-preparation UUID
// binds AW before/after without changing the durable SDK session identity.
fn invocation_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, byte)| match i {
            8 | 13 | 18 | 23 => byte == b'-',
            14 => byte == b'4',
            19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
            _ => byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correlation_requires_canonical_per_invocation_uuid() {
        assert!(invocation_uuid("1fc3478f-a4bd-4ab3-9a3e-738542873ada"));
        for invalid in [
            "",
            "raw-tool-id",
            "1fc3478f-a4bd-1ab3-9a3e-738542873ada",
            "1fc3478f-a4bd-4ab3-7a3e-738542873ada",
            "1FC3478F-a4bd-4ab3-9a3e-738542873ada",
        ] {
            assert!(!invocation_uuid(invalid), "{invalid}");
        }
    }
}
