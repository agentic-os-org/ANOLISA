//! Pi Coding Agent public factory bridge in an owned foreground Node worker.

use crate::{
    adapter::{
        self, Adapter, HookEvent, HookOutput, LaunchContext, LaunchInput, LaunchPlan,
        NativeContext, PreparedLaunch, Readiness,
    },
    Exit, Result,
};
use aw_core::admission::{AdapterCapabilities as Capabilities, AdmittedStep};
use serde_json::{json, Value};
use std::{collections::BTreeMap, ffi::OsString, fs, path::PathBuf, time::Duration};

use super::sdk_tools::invocation_uuid;

pub(crate) struct Pi;
struct Prepared {
    input: LaunchInput,
    version: String,
    core_version: String,
    strict_output: bool,
    application: PathBuf,
}

impl Adapter for Pi {
    fn post_permission(&self, entrypoint: &str) -> bool {
        entrypoint == "sdk"
    }
    fn prepare(
        &self,
        mut input: LaunchInput,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<Box<dyn PreparedLaunch>> {
        if !input.options.is_empty() {
            return Err("Pi SDK workers do not accept native CLI overrides".into());
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
            include_str!("../../native/pi/entrypoint.mjs")
        );
        let result = adapter::Probe(cancelled).version_output(
            &input.command,
            &[
                "--input-type=module".into(),
                "--eval".into(),
                probe.into(),
                "--".into(),
                application.clone().into_os_string(),
            ],
        )?;
        let (sdk, core) = result
            .strip_prefix("AW Pi SDK ")
            .and_then(|value| value.split_once(':'))
            .ok_or("Pi SDK metadata probe did not identify both required packages")?;
        let version = crate::version::reported(sdk, "Pi SDK")?;
        let core_version = crate::version::reported(core, "Pi Core")?;
        let strict_output = strict_output_release(&version, &core_version);
        input
            .command
            .environment
            .remove(std::ffi::OsStr::new("AW_PI_PROBE"));
        Ok(Box::new(Prepared {
            input,
            application,
            version,
            core_version,
            strict_output,
        }))
    }
    fn normalize(&self, binding: &NativeContext, event: &str, native: &Value) -> Result<Value> {
        if native["event"] != event {
            return Err("Pi event does not match binding".into());
        }
        if crate::callback::lifecycle(event) {
            return super::lifecycle::normalize(binding, "pi", event, native);
        }
        if event == "input.submit" {
            return super::submission::normalize(binding, native, "pi", "text");
        }
        if !matches!(event, "tool.before" | "tool.after") {
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
            json!({"name":event,"agent":{"adapter":"pi","binding_id":binding.target,"instance_id":binding.instance_id},
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

fn strict_output_release(sdk: &str, core: &str) -> bool {
    if sdk != core {
        return false;
    }
    let Ok(version) = semver::Version::parse(sdk) else {
        return false;
    };
    if !version.pre.is_empty() || !version.build.is_empty() {
        return false;
    }
    // Bound admission to published releases covered by native acceptance. Managed
    // launch and the SDK worker read the same range; extending it is a data change.
    let Ok(range) =
        serde_json::from_str::<Value>(include_str!("../../native/pi/capabilities.json"))
    else {
        return false;
    };
    let bound = |name: &str| {
        range["strict_output_releases"][name]
            .as_str()
            .and_then(|value| semver::Version::parse(value).ok())
    };
    let (Some(minimum), Some(maximum)) = (bound("minimum"), bound("maximum")) else {
        return false;
    };
    version >= minimum && version <= maximum
}

impl PreparedLaunch for Prepared {
    fn capabilities(&self) -> Capabilities {
        let mut capabilities =
            super::super::adapter::catalog::PI.capabilities(&self.version, "sdk");
        if self.strict_output {
            capabilities
                .failure_actions
                .insert("tool.after".into(), vec!["withhold_result".into()]);
        }
        capabilities
    }
    fn validate_steps(&self, steps: &[AdmittedStep]) -> Result<()> {
        routes(&self.input.document, steps).map(|_| ())
    }
    fn configure(&mut self, context: &LaunchContext<'_>) -> Result<LaunchPlan> {
        let (events, hooks) = routes(&self.input.document, context.steps)?;
        let token = fs::read_to_string("/proc/sys/kernel/random/uuid")?
            .trim()
            .to_owned();
        let ready = context.files.directory().join("pi-ready.json");
        let observe = adapter::observation_enabled(&self.input.document, &self.input.target);
        let config=context.files.write("pi-hooks.json",&serde_json::to_vec(&json!({"executable":context.executable,"binding":context.binding_path,"ready_path":ready,"ready_token":token,"observation_enabled":observe,"resident_policy_reload":self.input.options.resident_policy_reload,"sdk_version":self.version,"core_version":self.core_version,"strict_output_supported":self.strict_output,"replacement_step":events.get("tool.after").and_then(|route|route.replacement_step.as_ref()),"hooks":hooks}))?)?;
        let entrypoint = context.files.write(
            "pi-entrypoint.mjs",
            include_bytes!("../../native/pi/entrypoint.mjs"),
        )?;
        context
            .files
            .write("bridge.mjs", include_bytes!("../../native/pi/bridge.mjs"))?;
        for (name, bytes) in [
            (
                "skills.mjs",
                include_bytes!("../../native/pi/skills.mjs").as_slice(),
            ),
            (
                "submission.mjs",
                include_bytes!("../../native/pi/submission.mjs").as_slice(),
            ),
            (
                "transport.mjs",
                include_bytes!("../../native/shared/transport.mjs").as_slice(),
            ),
            (
                "producer.mjs",
                include_bytes!("../../native/pi/producer.mjs").as_slice(),
            ),
            (
                "scope.mjs",
                include_bytes!("../../native/shared/scope.mjs").as_slice(),
            ),
            (
                "resident.mjs",
                include_bytes!("../../native/pi/resident.mjs").as_slice(),
            ),
        ] {
            context.files.write(name, bytes)?;
        }
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
pub(crate) const fn route_options() -> aw_core::routes::RouteOptions<'static> {
    aw_core::routes::RouteOptions {
        events: &[
            "session.start",
            "input.submit",
            "tool.before",
            "tool.after",
            "turn.stop",
            "session.end",
        ],
        observe_events: &[],
        event_filter: aw_core::routes::EventFilter::All,
        max_budget_ms: aw_core::routes::DEFAULT_MAX_BUDGET_MS,
        reverse_on_result_replacement: false,
        include_replace_result: false,
        include_replace_input: true,
        include_tool_selectors: true,
        skill_check: aw_core::routes::SkillCheck::InputSubmit,
    }
}

pub(crate) fn routes(
    document: &Value,
    steps: &[AdmittedStep],
) -> Result<(BTreeMap<String, HookEvent>, Vec<Value>)> {
    let prepared =
        aw_core::routes::compile(document, steps, &route_options()).map_err(|error| {
            super::sdk_tools::format_route_error(
                error,
                "invalid Pi event budget",
                "Pi event budget must be 1..55000 ms",
                "Pi event budget must be integer milliseconds in 1..55000 ms",
            )
        })?;
    Ok(super::sdk_tools::convert_routes(&prepared))
}

// Model-issued tool IDs may recur in later turns; only this per-preparation UUID
// binds AW before/after without changing the durable SDK session identity.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_observes_the_native_turn_reason_without_a_tool() {
        let binding = NativeContext {
            target: "pi".into(),
            instance_id: "instance".into(),
            cwd: "/work".into(),
        };
        for (event, reason) in [
            ("session.start", Value::Null),
            ("turn.stop", json!("aborted")),
            ("session.end", json!("disposed")),
        ] {
            let native =
                json!({"event":event,"session_id":"session","turn_id":"turn","reason":reason});
            let value = Pi.normalize(&binding, event, &native).unwrap();
            assert_eq!(value["native"]["reason"], reason);
            assert!(value.get("tool").is_none());
        }
    }

    #[test]
    fn strict_output_uses_the_shared_release_declaration() {
        for (sdk, core, admitted) in [
            ("0.84.2", "0.84.2", true),
            ("1.3.0", "1.3.0", true),
            ("1.5.2", "1.5.2", true),
            ("0.84.1", "0.84.1", false),
            ("1.5.3", "1.5.3", false),
            ("1.5.2", "1.5.1", false),
            ("1.5.2-rc.1", "1.5.2-rc.1", false),
        ] {
            assert_eq!(strict_output_release(sdk, core), admitted, "{sdk}:{core}");
        }
    }

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
