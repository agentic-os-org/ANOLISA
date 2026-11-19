//! Shared tool callback payloads and bounded routes for explicit SDK factories.

use crate::{
    adapter::{HookEvent, NativeContext},
    Result,
};
use aw_core::{
    admission::{AdapterCapabilities, AdmittedStep},
    routes::{
        EventFilter, HookRoute, PreparedRoutes, RouteError, RouteOptions, SkillCheck,
        DEFAULT_MAX_BUDGET_MS, OBSERVED_TOOL_EVENTS, STANDARD_EVENTS,
    },
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

const fn eq_bytes(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut i = 0;
    while i < left.len() {
        if left[i] != right[i] {
            return false;
        }
        i += 1;
    }
    true
}

// Keep `aw_adapter::OBSERVE_STEP` and `aw_core::routes::OBSERVE_STEP` locked at compile time.
const _: () = assert!(eq_bytes(
    crate::OBSERVE_STEP.as_bytes(),
    aw_core::routes::OBSERVE_STEP.as_bytes()
));

const SDK_EVENTS: &[&str] = &["input.submit", "tool.before", "tool.after"];
const STANDARD_RESIDENT_ENTRYPOINTS: &[&str] = &["application", "sdk", "sdk-worker", "factory"];

struct ResidentDialect {
    adapter: &'static str,
    entrypoint: Option<&'static str>,
    options: Option<fn(&str) -> RouteOptions<'_>>,
    project_after_validation: bool,
    validate: Option<fn(&AdapterCapabilities) -> Result<()>>,
    budget_errors: (&'static str, &'static str, &'static str),
}

const SDK_BUDGET_ERRORS: (&str, &str, &str) = (
    "invalid SDK event budget",
    "SDK hook budgets must be 1..55000 ms",
    "SDK hook budgets must be integer milliseconds in 1..55000 ms",
);

fn sdk_worker_observed(source_id: &str) -> RouteOptions<'_> {
    RouteOptions {
        events: STANDARD_EVENTS,
        observe_events: OBSERVED_TOOL_EVENTS,
        event_filter: EventFilter::All,
        max_budget_ms: DEFAULT_MAX_BUDGET_MS,
        reverse_on_result_replacement: false,
        include_replace_result: true,
        include_replace_input: false,
        include_tool_selectors: false,
        skill_check: SkillCheck::Tool {
            source_id,
            tool_name: "Skill",
        },
    }
}

fn sdk_worker_explicit(source_id: &str) -> RouteOptions<'_> {
    RouteOptions {
        observe_events: &[],
        ..sdk_worker_observed(source_id)
    }
}

fn semantic_sdk(source_id: &str) -> RouteOptions<'_> {
    RouteOptions {
        events: SDK_EVENTS,
        ..sdk_worker_observed(source_id)
    }
}

fn qwenpaw_resident(_: &str) -> RouteOptions<'_> {
    super::qwenpaw::route_options(true)
}

fn agentscope_resident(_: &str) -> RouteOptions<'_> {
    super::agentscope::route_options()
}

fn pi_resident(_: &str) -> RouteOptions<'_> {
    super::pi::route_options()
}

const RESIDENT_DIALECTS: &[ResidentDialect] = &[
    ResidentDialect {
        adapter: "qoder-sdk",
        entrypoint: Some("sdk-worker"),
        options: Some(sdk_worker_observed),
        project_after_validation: false,
        validate: None,
        budget_errors: SDK_BUDGET_ERRORS,
    },
    ResidentDialect {
        adapter: "claude-sdk",
        entrypoint: Some("sdk-worker"),
        options: Some(sdk_worker_explicit),
        project_after_validation: false,
        validate: None,
        budget_errors: SDK_BUDGET_ERRORS,
    },
    ResidentDialect {
        adapter: "sdk",
        entrypoint: Some("application"),
        options: Some(semantic_sdk),
        project_after_validation: true,
        validate: Some(super::sdk::validate_capabilities),
        budget_errors: SDK_BUDGET_ERRORS,
    },
    ResidentDialect {
        adapter: "qwenpaw",
        entrypoint: Some("app"),
        options: Some(qwenpaw_resident),
        project_after_validation: false,
        validate: None,
        budget_errors: (
            "invalid native event budget",
            "QwenPaw native event budgets must be 1..55000 ms",
            "QwenPaw native event budgets must be integer milliseconds in 1..55000 ms",
        ),
    },
    ResidentDialect {
        adapter: "agentscope",
        entrypoint: Some("factory"),
        options: Some(agentscope_resident),
        project_after_validation: false,
        validate: None,
        budget_errors: (
            "invalid native event budget",
            "AgentScope native event budgets must be 1..55000 ms",
            "AgentScope native event budgets must be integer milliseconds in 1..55000 ms",
        ),
    },
    ResidentDialect {
        adapter: "pi",
        entrypoint: Some("sdk"),
        options: Some(pi_resident),
        project_after_validation: false,
        validate: None,
        budget_errors: (
            "invalid Pi event budget",
            "Pi event budget must be 1..55000 ms",
            "Pi event budget must be integer milliseconds in 1..55000 ms",
        ),
    },
    // Launch-bound CLI and Gateway adapters use dynamic native reload, not child task Scopes.
    ResidentDialect {
        adapter: "qoder",
        entrypoint: None,
        options: None,
        project_after_validation: false,
        validate: None,
        budget_errors: SDK_BUDGET_ERRORS,
    },
    ResidentDialect {
        adapter: "openclaw",
        entrypoint: None,
        options: None,
        project_after_validation: false,
        validate: None,
        budget_errors: SDK_BUDGET_ERRORS,
    },
    ResidentDialect {
        adapter: "hermes",
        entrypoint: None,
        options: None,
        project_after_validation: false,
        validate: None,
        budget_errors: SDK_BUDGET_ERRORS,
    },
    ResidentDialect {
        adapter: "codex",
        entrypoint: None,
        options: None,
        project_after_validation: false,
        validate: None,
        budget_errors: SDK_BUDGET_ERRORS,
    },
    ResidentDialect {
        adapter: "deepseek-harness",
        entrypoint: None,
        options: None,
        project_after_validation: false,
        validate: None,
        budget_errors: SDK_BUDGET_ERRORS,
    },
];

pub(crate) fn format_route_error(
    error: RouteError,
    invalid_budget: &'static str,
    out_of_range: &'static str,
    non_integral: &'static str,
) -> crate::Error {
    match error {
        RouteError::InvalidBudget => invalid_budget.into(),
        RouteError::NonIntegralBudget => non_integral.into(),
        RouteError::BudgetOutOfRange { .. } => out_of_range.into(),
        other => other.to_string().into(),
    }
}

pub(crate) fn resident_supported(capabilities: &AdapterCapabilities) -> Result<()> {
    if let Some(dialect) = RESIDENT_DIALECTS
        .iter()
        .find(|dialect| dialect.adapter == capabilities.adapter)
    {
        if dialect.entrypoint == Some(capabilities.entrypoint.as_str()) {
            return Ok(());
        }
        return Err("resident policy updates require a verified resident entrypoint".into());
    }
    if !capabilities.adapter.trim().is_empty()
        && !capabilities.version.trim().is_empty()
        && STANDARD_RESIDENT_ENTRYPOINTS.contains(&capabilities.entrypoint.as_str())
    {
        return Ok(());
    }
    Err("resident policy updates require a supported resident Source profile".into())
}

pub(crate) fn compile_resident_routes(
    document: &Value,
    steps: &[AdmittedStep],
    capabilities: &AdapterCapabilities,
) -> Result<PreparedRoutes> {
    resident_supported(capabilities)?;
    if let Some(dialect) = RESIDENT_DIALECTS
        .iter()
        .find(|dialect| dialect.adapter == capabilities.adapter)
    {
        if let Some(validate) = dialect.validate {
            validate(capabilities)?;
        }
        let build = dialect
            .options
            .ok_or("unsupported resident policy adapter")?;
        let mut options = build(&capabilities.adapter);
        if dialect.project_after_validation {
            options.event_filter = EventFilter::ProjectAfterValidation(&capabilities.events);
        }
        let (invalid, range, non_integral) = dialect.budget_errors;
        return aw_core::routes::compile(document, steps, &options)
            .map_err(|error| format_route_error(error, invalid, range, non_integral));
    }
    aw_core::routes::compile(document, steps, &RouteOptions::standard(capabilities)).map_err(
        |error| {
            format_route_error(
                error,
                SDK_BUDGET_ERRORS.0,
                SDK_BUDGET_ERRORS.1,
                SDK_BUDGET_ERRORS.2,
            )
        },
    )
}

pub(crate) fn convert_routes(
    prepared: &PreparedRoutes,
) -> (BTreeMap<String, HookEvent>, Vec<Value>) {
    let events = prepared
        .events()
        .map(|(name, route)| {
            (
                name.to_owned(),
                HookEvent {
                    budget_ms: route.budget_ms(),
                    replacement_step: route.replacement_step().map(str::to_owned),
                    steps: route.steps_btree(),
                },
            )
        })
        .collect();
    let hooks = prepared.hooks().iter().map(HookRoute::to_value).collect();
    (events, hooks)
}

pub(crate) fn routes(
    document: &Value,
    steps: &[AdmittedStep],
    adapter: &str,
) -> Result<(BTreeMap<String, HookEvent>, Vec<Value>)> {
    let options = RESIDENT_DIALECTS
        .iter()
        .find(|dialect| dialect.adapter == adapter)
        .and_then(|dialect| dialect.options)
        .map(|build| build(adapter))
        .unwrap_or_else(|| sdk_worker_explicit(adapter));
    let prepared = aw_core::routes::compile(document, steps, &options).map_err(|error| {
        format_route_error(
            error,
            SDK_BUDGET_ERRORS.0,
            SDK_BUDGET_ERRORS.1,
            SDK_BUDGET_ERRORS.2,
        )
    })?;
    Ok(convert_routes(&prepared))
}

// SDK dialects share tool payload fields and preserve native session/call identities.
pub(super) fn normalize_tools(
    binding: &NativeContext,
    event: &str,
    native: &Value,
    adapter: &str,
) -> Result<Value> {
    let name = native["hook_event_name"].as_str();
    if !matches!(
        (event, name),
        ("tool.before", Some("PreToolUse"))
            | ("tool.after", Some("PostToolUse" | "PostToolUseFailure"))
    ) {
        return Err("SDK event differs from its callback route".into());
    }
    let id = |value: &Value| -> Result<String> {
        value
            .as_str()
            .filter(|s| !s.trim().is_empty() && s.len() <= 512)
            .map(str::to_owned)
            .ok_or_else(|| "SDK requires bounded native identities".into())
    };
    let session = id(&native["session_id"])?;
    let call = id(&native["tool_use_id"])?;
    let tool = id(&native["tool_name"])?;
    if native["cwd"].as_str() != binding.cwd.to_str() || !native["tool_input"].is_object() {
        return Err("SDK callback requires bound cwd and object tool_input".into());
    }
    let result = match name {
        Some("PostToolUse") => native
            .get("tool_response")
            .ok_or("SDK success callback requires tool_response")?
            .clone(),
        Some("PostToolUseFailure") => {
            if !native["error"].is_string() {
                return Err("SDK failure callback requires error".into());
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
        json!({"name": event, "agent": {"adapter": adapter, "binding_id": binding.target, "instance_id": binding.instance_id}, "session_id": session,
            "tool": {"name": standard, "native_name": tool, "call_id": call, "input": native["tool_input"], "result": result}, "native": native}),
    )
}

pub(super) fn invocation_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, byte)| match i {
            8 | 13 | 18 | 23 => byte == b'-',
            14 => byte == b'4',
            19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
            _ => byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'),
        })
}

/// Select Skill resolution only for an admitted check of that native tool.
pub(crate) fn checks_skill(step: &AdmittedStep, adapter: &str) -> bool {
    checks_skill_tool(step, adapter, "Skill")
}

pub(crate) fn checks_skill_tool(step: &AdmittedStep, adapter: &str, tool: &str) -> bool {
    aw_core::routes::matches_skill_tool(step, adapter, tool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aw_core::admission::StepExecution;

    #[test]
    fn skill_resolution_is_selected_only_for_its_operation_and_native_tool() {
        let mut step = AdmittedStep {
            event: "tool.before".into(),
            step_id: "skill".into(),
            provider: "security".into(),
            event_tools: vec!["*".into()],
            step_tools: vec!["native:qoder-sdk:Skill".into()],
            on_error: "block".into(),
            execution: StepExecution::Provider {
                operation: "check_skill".into(),
                effects: vec!["observe".into(), "block".into()],
            },
        };
        assert!(checks_skill(&step, "qoder-sdk"));
        assert!(!checks_skill(&step, "qwenpaw"));
        let document = json!({"spec":{"execution":{"default_event_budget_ms":1000}}});
        let (_, hooks) = routes(&document, &[step.clone()], "qoder-sdk").unwrap();
        assert_eq!(hooks[0]["skill_check"], true);
        step.step_tools = vec!["native:qoder-sdk:Bash".into()];
        let (_, hooks) = routes(&document, &[step.clone()], "qoder-sdk").unwrap();
        assert!(hooks[0].get("skill_check").is_none());
        step.step_tools = vec!["native:qwenpaw:Skill".into()];
        let (_, hooks) =
            super::super::qwenpaw::event_routes(&document, &[step.clone()], false).unwrap();
        assert_eq!(hooks[0]["skill_check"], true);
        step.step_tools = vec!["native:hermes:skill_view".into()];
        assert!(checks_skill_tool(&step, "hermes", "skill_view"));
        assert!(!checks_skill(&step, "hermes"));
        step.execution = StepExecution::Provider {
            operation: "scan_code".into(),
            effects: vec!["observe".into()],
        };
        assert!(!checks_skill(&step, "qwenpaw"));
    }

    #[test]
    fn reloadable_skill_checks_fail_admission_before_native_launch() {
        let mut document: Value = serde_yaml_ng::from_str(include_str!(
            "../../../aw-service/examples/aw.sdk-security.yaml"
        ))
        .unwrap();
        document["spec"]["events"]
            .as_object_mut()
            .unwrap()
            .remove("tool.after");
        document["spec"]["events"]
            .as_object_mut()
            .unwrap()
            .remove("input.submit");
        for (adapter, version, entrypoint) in [
            ("qoder-sdk", "1.0.56", "sdk-worker"),
            ("qwenpaw", "2.2.2.post1", "app"),
        ] {
            let capabilities = crate::adapter::catalog::PROFILES
                .iter()
                .find(|profile| profile.name == adapter)
                .unwrap()
                .capabilities(version, entrypoint);
            let error = crate::reload::validate(&document, adapter, &capabilities).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("Skill checking requires ordinary resident mode"),
                "{error}"
            );
        }
    }

    #[test]
    fn scope_routes_match_every_resident_dialect_and_admit_standard_profiles() {
        let steps = vec![
            AdmittedStep {
                event: "input.submit".into(),
                step_id: "submit".into(),
                provider: "security".into(),
                event_tools: vec!["*".into()],
                step_tools: vec!["*".into()],
                on_error: "block".into(),
                execution: StepExecution::Provider {
                    operation: "check_skill".into(),
                    effects: vec!["observe".into(), "block".into()],
                },
            },
            AdmittedStep {
                event: "tool.before".into(),
                step_id: "rewrite".into(),
                provider: "tokenless".into(),
                event_tools: vec!["*".into()],
                step_tools: vec!["*".into()],
                on_error: "block".into(),
                execution: StepExecution::Provider {
                    operation: "rewrite".into(),
                    effects: vec!["replace_input".into()],
                },
            },
            AdmittedStep {
                event: "tool.after".into(),
                step_id: "observe".into(),
                provider: "audit".into(),
                event_tools: vec!["*".into()],
                step_tools: vec!["*".into()],
                on_error: "report".into(),
                execution: StepExecution::Provider {
                    operation: "record".into(),
                    effects: vec!["observe".into()],
                },
            },
            AdmittedStep {
                event: "tool.after".into(),
                step_id: "compress".into(),
                provider: "tokenless".into(),
                event_tools: vec!["*".into()],
                step_tools: vec!["*".into()],
                on_error: "report".into(),
                execution: StepExecution::Provider {
                    operation: "compress".into(),
                    effects: vec!["replace_result".into()],
                },
            },
        ];
        for (adapter, entrypoint, expected_steps, empty_steps) in [
            (
                "qoder-sdk",
                "sdk-worker",
                ["submit", "rewrite", "observe", "compress"],
                vec!["tool.before", "tool.after"],
            ),
            (
                "claude-sdk",
                "sdk-worker",
                ["submit", "rewrite", "observe", "compress"],
                vec![],
            ),
            (
                "qwenpaw",
                "app",
                ["compress", "observe", "rewrite", "submit"],
                vec!["tool.after", "tool.before"],
            ),
            (
                "agentscope",
                "factory",
                ["submit", "compress", "observe", "rewrite"],
                vec![],
            ),
            (
                "pi",
                "sdk",
                ["submit", "rewrite", "observe", "compress"],
                vec![],
            ),
        ] {
            let doc = json!({"spec": {"agents": {"target": {"adapter": adapter}}, "events": {}, "execution": {"default_event_budget_ms": 2000}}});
            let caps = AdapterCapabilities {
                adapter: adapter.into(),
                version: "1.0.0".into(),
                entrypoint: entrypoint.into(),
                events: BTreeMap::new(),
                failure_actions: BTreeMap::new(),
            };
            let policy = crate::reload::scope_policy(&doc, "target", &steps, &caps).unwrap();
            let actual_steps: Vec<&str> = policy["hooks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|h| h["step"].as_str().unwrap())
                .collect();
            assert_eq!(actual_steps, expected_steps, "{adapter}");
            assert_eq!(
                policy["hooks"][0].get("replace_result").is_some(),
                adapter != "pi",
                "{adapter}"
            );
            assert_eq!(
                policy["hooks"][0].get("event_tools").is_some(),
                adapter == "qwenpaw",
                "{adapter}"
            );
            assert_eq!(
                policy["hooks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|h| h["replace_input"] == true),
                adapter == "pi",
                "{adapter}"
            );
            assert_eq!(
                policy["hooks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|h| h["event"] == "input.submit" && h["skill_check"] == true),
                adapter == "pi",
                "{adapter}"
            );
            let empty = crate::reload::scope_policy(&doc, "target", &[], &caps).unwrap();
            let observed: Vec<&str> = empty["hooks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|h| h["event"].as_str().unwrap())
                .collect();
            assert_eq!(observed, empty_steps, "{adapter} empty");
            let mut over_budget = doc.clone();
            over_budget["spec"]["execution"]["default_event_budget_ms"] = json!(55001);
            let err = crate::reload::scope_policy(&over_budget, "target", &steps, &caps)
                .unwrap_err()
                .to_string();
            assert!(err.contains("1..55000 ms"), "{adapter}: {err}");
        }
        // Semantic SDK validates tool observation budgets before projecting subscribed events.
        let sdk_caps = AdapterCapabilities {
            adapter: "sdk".into(),
            version: "aw-hook-source/v1".into(),
            entrypoint: "application".into(),
            events: BTreeMap::from([("input.submit".into(), vec!["observe".into()])]),
            failure_actions: BTreeMap::new(),
        };
        let mut sdk_doc = json!({"spec": {"agents": {"target": {"adapter": "sdk"}}, "events": {"tool.before": {"enabled": false, "budget_ms": 55000, "steps": []}}, "execution": {"default_event_budget_ms": 5000}}});
        let sdk_policy = crate::reload::scope_policy(&sdk_doc, "target", &[], &sdk_caps).unwrap();
        assert_eq!(sdk_policy, json!({"events": {}, "hooks": []}));
        sdk_doc["spec"]["events"]["tool.before"]["budget_ms"] = json!(55001);
        assert_eq!(
            crate::reload::scope_policy(&sdk_doc, "target", &[], &sdk_caps)
                .unwrap_err()
                .to_string(),
            "SDK hook budgets must be 1..55000 ms"
        );
        // Standard Source profiles compile without adding branches in reload.rs.
        for entrypoint in ["application", "sdk", "sdk-worker", "factory"] {
            let doc = json!({
                "spec": {
                    "agents": {"target": {"adapter": "future-source"}},
                    "events": {},
                    "execution": {"default_event_budget_ms": 3000}
                }
            });
            let caps = AdapterCapabilities {
                adapter: "future-source".into(),
                version: "1.0.0".into(),
                entrypoint: entrypoint.into(),
                events: BTreeMap::from([("tool.before".into(), vec!["observe".into()])]),
                failure_actions: BTreeMap::new(),
            };
            let prepared = crate::reload::scope_routes(&doc, "target", &[], &caps).unwrap();
            assert_eq!(
                prepared
                    .event("tool.before")
                    .unwrap()
                    .step_on_error("$aw.observe"),
                Some("report")
            );
            assert!(prepared.event("tool.after").is_none());
        }
        for (adapter, entrypoint) in [
            ("qoder", "sdk-worker"),
            ("openclaw", "gateway"),
            ("hermes", "chat"),
            ("codex", "tui"),
            ("deepseek-harness", "default"),
            ("qoder-sdk", "foreground-final-worker"),
            ("claude-sdk", "typescript-sdk-worker"),
            ("future-source", "tui"),
        ] {
            let caps = AdapterCapabilities {
                adapter: adapter.into(),
                version: "1.0.0".into(),
                entrypoint: entrypoint.into(),
                events: BTreeMap::new(),
                failure_actions: BTreeMap::new(),
            };
            assert!(
                crate::reload::resident_supported(&caps).is_err(),
                "{adapter}:{entrypoint}"
            );
        }
    }
}
