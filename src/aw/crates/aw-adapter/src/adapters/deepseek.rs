//! DeepSeek Harness profile plugin with launch-scoped registration.

use crate::{
    adapter::{
        self, Adapter, HookEvent, HookOutput, LaunchContext, LaunchInput, LaunchPlan,
        NativeContext, PreparedLaunch, Readiness,
    },
    Exit, Result,
};
use aw_core::admission::{AdapterCapabilities as Capabilities, AdmittedStep};
use serde_json::{json, Value};
use std::{collections::BTreeMap, ffi::OsStr, fs, time::Duration};

pub(crate) struct DeepSeek;

const RESULT_GATE_RELEASE: &str = "0.2.1-rc.5";

pub(crate) fn replaceable(native: &Value) -> bool {
    let result = &native["tool_response"];
    let structured_bash = native["tool_call"]["name"] == "bash"
        && (result["value"].get("stdout").is_some() || result["value"].get("kind").is_some());
    (!structured_bash || mirrored_stdout(native))
        && native["nested"] != true
        && native["tool_response"]["isError"] == false
        && native["tool_response"]["content"]
            .as_array()
            .is_some_and(|blocks| {
                blocks.len() == 1 && blocks[0]["type"] == "text" && blocks[0]["text"].is_string()
            })
}

// Only the verified pre-publication gate can adopt both fields atomically.
pub(crate) fn mirrored_stdout(native: &Value) -> bool {
    let result = &native["tool_response"];
    native["aw_result_gate"] == true
        && native["nested"] == false
        && native["tool_call"]["name"] == "bash"
        && result["value"]["kind"] == "foreground"
        && result["value"]["stdout"]["text"].is_string()
        && result["value"]["stdout"]["text"] == result["content"][0]["text"]
}

pub(crate) const BASH_TEXT_PATHS: [&str; 2] = ["/content/0/text", "/value/stdout/text"];

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
        if matches!(event, "session.start" | "turn.stop") && native["event"] == event {
            let seam = if event == "session.start" {
                "agent/created"
            } else {
                "turn/end"
            };
            if native["seam"] != seam {
                return Err("DeepSeek lifecycle event comes from a different native seam".into());
            }
            return super::lifecycle::normalize(binding, "deepseek-harness", event, native);
        }
        if !matches!(
            event,
            "input.submit" | "tool.before" | "tool.after" | "security.violation"
        ) || native["event"] != event
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
            || (matches!(event, "tool.after" | "security.violation")
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
        let mut caps = adapter::catalog::DEEPSEEK.capabilities(&self.version, &self.profile);
        if self.version == RESULT_GATE_RELEASE {
            caps.failure_actions
                .insert("tool.after".into(), vec!["withhold_result".into()]);
        }
        caps
    }
    fn validate_steps(&self, steps: &[AdmittedStep]) -> Result<()> {
        routes(&self.input.document, &self.input.target, steps).map(|_| ())
    }
    fn configure(&mut self, context: &LaunchContext<'_>) -> Result<LaunchPlan> {
        let (events, hooks) = routes(&self.input.document, &self.input.target, context.steps)?;
        let token = fs::read_to_string("/proc/sys/kernel/random/uuid")?
            .trim()
            .to_owned();
        let ready = context.files.directory().join("deepseek-ready.json");
        let result_gate =
            self.version == RESULT_GATE_RELEASE && steps_need_result_gate(context.steps);
        let config = context.files.write("deepseek-hooks.json", &serde_json::to_vec(&json!({
            "executable":context.executable, "binding":context.binding_path,
            "result_gate":result_gate, "ready_path":ready, "ready_token":token, "hooks":hooks,
            "replacement_step":events.get("tool.after").and_then(|route| route.replacement_step.as_ref())
        }))?)?;
        let plugin = context.files.write(
            "deepseek-plugin.mjs",
            include_bytes!("../../native/deepseek-harness/plugin.mjs"),
        )?;
        context.files.write(
            "hooks.mjs",
            include_bytes!("../../native/deepseek-harness/hooks.mjs"),
        )?;
        context.files.write(
            "callback.mjs",
            include_bytes!("../../native/deepseek-harness/callback.mjs"),
        )?;
        let preload = if result_gate || events.contains_key("security.violation") {
            fs::create_dir(context.files.directory().join("deepseek-harness"))?;
            fs::create_dir_all(context.files.directory().join("shared/js-runtime"))?;
            context.files.write(
                "shared/js-runtime/module-patch.mjs",
                include_bytes!("../../native/shared/js-runtime/module-patch.mjs"),
            )?;
            if result_gate {
                context.files.write(
                    "deepseek-harness/result-preload.mjs",
                    include_bytes!("../../native/deepseek-harness/result-preload.mjs"),
                )?;
            }
            if events.contains_key("security.violation") {
                context.files.write(
                    "deepseek-harness/final-preload.mjs",
                    include_bytes!("../../native/deepseek-harness/final-preload.mjs"),
                )?;
            }
            Some(context.files.write(
                "deepseek-harness/bootstrap.mjs",
                include_bytes!("../../native/deepseek-harness/bootstrap.mjs"),
            )?)
        } else {
            None
        };
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
        let provider_environment = command.environment.clone();
        if let Some(path) = preload {
            let url = reqwest::Url::from_file_path(path)
                .map_err(|_| "invalid DeepSeek bootstrap path")?;
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
        Ok(LaunchPlan {
            provider_environment,
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

pub(crate) fn route_options(_: &str) -> aw_core::routes::RouteOptions<'static> {
    aw_core::routes::RouteOptions {
        events: &[
            "session.start",
            "input.submit",
            "tool.before",
            "tool.after",
            "security.violation",
            "turn.stop",
        ],
        observe_events: &[],
        event_filter: aw_core::routes::EventFilter::All,
        max_budget_ms: aw_core::routes::DEFAULT_MAX_BUDGET_MS,
        reverse_on_result_replacement: false,
        include_replace_result: false,
        include_replace_input: false,
        include_tool_selectors: false,
        skill_check: aw_core::routes::SkillCheck::Unsupported,
    }
}

fn steps_need_result_gate(steps: &[AdmittedStep]) -> bool {
    steps.iter().any(|step| {
        step.event == "tool.after"
            && (step.execution.replaces_result() || step.on_error == "withhold_result")
    })
}

fn routes(
    document: &Value,
    target: &str,
    steps: &[AdmittedStep],
) -> Result<(BTreeMap<String, HookEvent>, Vec<Value>)> {
    if !steps.iter().any(|step| step.event == "security.violation")
        && aw_config::requirements::project_document(document, target)
            .map_err(|_| "could not project native Hook requirements")?
            .iter()
            .any(|requirement| requirement.event == "security.violation")
    {
        return Err("DeepSeek final security gate requires a Provider step".into());
    }
    let options = route_options("");
    let prepared =
        aw_core::routes::compile(document, steps, &options).map_err(|error| match error {
            aw_core::routes::RouteError::UnsupportedSkillCheck => {
                "DeepSeek cannot check a resolved Skill object".into()
            }
            other => super::sdk_tools::format_route_error(
                other,
                "invalid DeepSeek event budget",
                "DeepSeek event budget must be 1..55000 ms",
                "DeepSeek event budget must be integer milliseconds in 1..55000 ms",
            ),
        })?;
    Ok(super::sdk_tools::convert_routes(&prepared))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aw_core::admission::StepExecution;
    use std::path::PathBuf;

    #[test]
    fn lifecycle_preserves_the_committed_turn_reason() {
        let binding = NativeContext {
            target: "dsh".into(),
            instance_id: "instance".into(),
            cwd: "/work".into(),
        };
        for (event, seam, reason) in [
            ("session.start", "agent/created", Value::Null),
            ("turn.stop", "turn/end", json!("aborted")),
        ] {
            let native = json!({"event":event,"seam":seam,"session_id":"session","turn_id":"2","reason":reason});
            let normalized = DeepSeek.normalize(&binding, event, &native).unwrap();
            assert_eq!(normalized["session_id"], "session");
            assert_eq!(normalized["native"]["reason"], reason);
            let mut wrong = native;
            wrong["seam"] = json!("process.exit");
            assert!(DeepSeek.normalize(&binding, event, &wrong).is_err());
        }
    }

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
    fn final_security_check_is_an_independent_route() {
        let step = crate::test_support::final_gate_step("enforce", "block");
        let mut document =
            crate::test_support::final_gate_document("deepseek-harness", json!([step]));
        let agent = document["spec"]["agents"]["sdk"].clone();
        document["spec"]["agents"] = json!({"dsh-final":agent, "dsh-plain":agent});
        let capabilities = adapter::catalog::DEEPSEEK.capabilities("0.2.1-rc.5", "headless");
        let validator = aw_config::Validator::new().unwrap();
        for scoped_path in [
            "/spec/events/security.violation",
            "/spec/events/security.violation/steps/0",
        ] {
            let mut scoped = document.clone();
            scoped.pointer_mut(scoped_path).unwrap()["match"] = json!({"agents":["dsh-final"]});
            let config = validator
                .parse(&serde_json::to_vec(&scoped).unwrap())
                .unwrap();
            for target in ["dsh-final", "dsh-plain"] {
                let steps = aw_core::admission::preflight(&config, target, &capabilities).unwrap();
                let (events, hooks) = routes(config.as_value(), target, &steps).unwrap();
                if target == "dsh-final" {
                    assert_eq!(events.len(), 1);
                    assert_eq!(hooks.len(), 1);
                    assert_eq!(hooks[0]["event"], "security.violation");
                    assert_eq!(hooks[0]["step"], "check");
                } else {
                    assert!(events.is_empty() && hooks.is_empty());
                }
            }
        }
        let gate = &mut document["spec"]["events"]["security.violation"];
        gate["steps"] = json!([]);
        gate["match"] = json!({"agents":["dsh-final"]});
        let error = routes(&document, "dsh-final", &[]).err().unwrap();
        assert_eq!(
            error.to_string(),
            "DeepSeek final security gate requires a Provider step"
        );
        let binding = NativeContext {
            target: "dsh".into(),
            instance_id: "instance".into(),
            cwd: PathBuf::from("/tmp"),
        };
        let mut native = json!({"event":"security.violation","session_id":"session",
            "invocation_id":"invocation","tool_call":{"id":"call","name":"read_file",
                "input":{}},"tool_response":{"isError":false,"content":[]}});
        assert!(DeepSeek
            .normalize(&binding, "security.violation", &native)
            .is_ok());
        native["tool_response"] = Value::Null;
        assert!(DeepSeek
            .normalize(&binding, "security.violation", &native)
            .is_err());
    }

    #[test]
    fn resolved_skill_check_is_rejected_for_both_failure_actions() {
        let document = json!({"spec":{"execution":{"default_event_budget_ms":1000},
            "agents":{"dsh":{"adapter":"deepseek-harness"}},
            "events":{"tool.before":{"enabled":true,"steps":[]}}}});
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
            assert_eq!(
                routes(&document, "dsh", &[step]).err().unwrap().to_string(),
                "DeepSeek cannot check a resolved Skill object"
            );
        }
    }
}
