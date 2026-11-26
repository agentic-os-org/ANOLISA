//! Pure route compilation, subscription projection and u64 budget boundary tests.

use aw_core::{
    admission::{AdapterCapabilities, AdmittedStep, StepExecution},
    routes::{
        compile, event_budget_ms, schedule_event, EventFilter, GroupOrder, RouteError,
        RouteOptions, SkillCheck, DEFAULT_MAX_BUDGET_MS, OBSERVED_TOOL_EVENTS, OBSERVE_STEP,
        STANDARD_EVENTS,
    },
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn step(event: &str, id: &str, operation: &str, effects: &[&str]) -> AdmittedStep {
    AdmittedStep {
        post_permission: false,
        event: event.into(),
        step_id: id.into(),
        provider: "policy".into(),
        event_tools: vec!["*".into()],
        step_tools: vec!["*".into()],
        on_error: if event == "tool.after" {
            "report"
        } else {
            "block"
        }
        .into(),
        execution: StepExecution::Provider {
            operation: operation.into(),
            effects: effects.iter().map(|e| (*e).into()).collect(),
        },
    }
}

#[test]
fn standard_options_retain_declared_events_and_provide_o1_step_lookup() {
    let capabilities = AdapterCapabilities {
        post_permission: false,
        adapter: "custom-source".into(),
        version: "1.0.0".into(),
        entrypoint: "application".into(),
        events: BTreeMap::from([("tool.before".into(), vec!["observe".into(), "block".into()])]),
        failure_actions: BTreeMap::new(),
    };
    let document = json!({"spec":{"events":{},"execution":{"default_event_budget_ms":2500}}});
    let routes = compile(&document, &[], &RouteOptions::standard(&capabilities)).unwrap();
    assert!(routes.event("tool.after").is_none());
    let before = routes.event("tool.before").unwrap();
    assert_eq!(before.budget_ms(), 2500);
    assert_eq!(before.replacement_step(), None);
    assert_eq!(before.step_on_error(OBSERVE_STEP), Some("report"));
    assert_eq!(before.step_on_error("missing"), None);
    assert_eq!(routes.hooks().len(), 1);
    assert_eq!(routes.hooks()[0].step(), OBSERVE_STEP);
}

#[test]
fn project_after_validation_checks_unsubscribed_observed_budgets() {
    let subscribed = BTreeMap::from([("input.submit".into(), vec!["observe".into()])]);
    let options = RouteOptions {
        events: &["input.submit", "tool.before", "tool.after"],
        observe_events: OBSERVED_TOOL_EVENTS,
        event_filter: EventFilter::ProjectAfterValidation(&subscribed),
        max_budget_ms: DEFAULT_MAX_BUDGET_MS,
        reverse_on_result_replacement: false,
        include_replace_result: true,
        include_replace_input: false,
        include_tool_selectors: false,
        skill_check: SkillCheck::None,
    };
    let mut doc = json!({
        "spec": {
            "events": {"tool.before": {"enabled": false, "budget_ms": 55_000, "steps": []}},
            "execution": {"default_event_budget_ms": 5_000}
        }
    });
    let routes = compile(&doc, &[], &options).unwrap();
    assert_eq!(routes.to_policy_value(), json!({"events": {}, "hooks": []}));
    doc["spec"]["events"]["tool.before"]["budget_ms"] = json!(55_001);
    assert_eq!(
        compile(&doc, &[], &options),
        Err(RouteError::BudgetOutOfRange {
            max_ms: DEFAULT_MAX_BUDGET_MS
        })
    );
}

#[test]
fn reverse_replacement_and_metadata_projections_stay_deterministic() {
    let mut skill = step("tool.before", "skill", "check_skill", &["observe", "block"]);
    skill.step_tools = vec!["native:custom:Skill".into()];
    let mut rewrite = step("tool.before", "rewrite", "rewrite", &["replace_input"]);
    rewrite.post_permission = true;
    let observe = step("tool.after", "observe", "record", &["observe"]);
    let compress = step("tool.after", "compress", "compress", &["replace_result"]);
    let document = json!({
        "spec": {
            "events": {"tool.after": {"budget_ms": 4000}},
            "execution": {"default_event_budget_ms": 1500}
        }
    });
    let routes = compile(
        &document,
        &[skill, rewrite, observe, compress],
        &RouteOptions {
            events: &["tool.after", "tool.before"],
            observe_events: &[],
            event_filter: EventFilter::All,
            max_budget_ms: DEFAULT_MAX_BUDGET_MS,
            reverse_on_result_replacement: true,
            include_replace_result: true,
            include_replace_input: true,
            include_tool_selectors: true,
            skill_check: SkillCheck::Tool {
                source_id: "custom",
                tool_name: "Skill",
            },
        },
    )
    .unwrap();
    let after = routes.event("tool.after").unwrap();
    assert_eq!(after.budget_ms(), 4000);
    assert_eq!(after.replacement_step(), Some("compress"));
    let policy = routes.to_policy_value();
    let hooks = policy["hooks"].as_array().unwrap();
    assert_eq!(hooks[0]["step"], "compress");
    assert_eq!(hooks[0]["replace_result"], true);
    assert_eq!(hooks[1]["step"], "observe");
    assert_eq!(hooks[1]["replace_result"], false);
    assert_eq!(hooks[2]["step"], "skill");
    assert_eq!(hooks[2]["skill_check"], true);
    assert_eq!(hooks[2]["step_tools"], json!(["native:custom:Skill"]));
    assert_eq!(hooks[3]["step"], "rewrite");
    assert_eq!(hooks[3]["replace_input"], true);
    assert_eq!(hooks[3]["post_permission"], true);
}

#[test]
fn budgets_u64_precision_parallel_replacements_and_skill_errors_stay_exact() {
    let base = RouteOptions {
        events: STANDARD_EVENTS,
        observe_events: OBSERVED_TOOL_EVENTS,
        event_filter: EventFilter::All,
        max_budget_ms: 12_000,
        reverse_on_result_replacement: false,
        include_replace_result: false,
        include_replace_input: false,
        include_tool_selectors: false,
        skill_check: SkillCheck::Unsupported,
    };
    let doc =
        |val: Value| json!({"spec":{"events":{},"execution":{"default_event_budget_ms":val}}});
    assert_eq!(
        compile(&doc(json!(0)), &[], &base),
        Err(RouteError::BudgetOutOfRange { max_ms: 12_000 })
    );
    assert_eq!(
        event_budget_ms(&doc(json!(-5)), "tool.before", 55_000),
        Err(RouteError::BudgetOutOfRange { max_ms: 55_000 })
    );
    assert_eq!(
        event_budget_ms(&doc(json!(1000.5)), "tool.before", 55_000),
        Err(RouteError::NonIntegralBudget)
    );
    let large: u64 = (1_u64 << 53) + 1;
    assert_eq!(
        event_budget_ms(&doc(json!(large)), "tool.before", large - 1),
        Err(RouteError::BudgetOutOfRange { max_ms: large - 1 })
    );
    assert_eq!(
        event_budget_ms(&doc(json!(large)), "tool.before", large),
        Ok(large)
    );
    assert_eq!(
        event_budget_ms(&doc(json!(u64::MAX)), "tool.before", u64::MAX),
        Ok(u64::MAX)
    );
    assert_eq!(
        event_budget_ms(
            &doc(json!(18_446_744_073_709_551_616.0_f64)),
            "tool.before",
            u64::MAX
        ),
        Err(RouteError::BudgetOutOfRange { max_ms: u64::MAX })
    );
    let valid = doc(json!(1000));
    let skill = step("tool.before", "skill", "check_skill", &["block"]);
    assert_eq!(
        compile(&valid, &[skill], &base),
        Err(RouteError::UnsupportedSkillCheck)
    );
    let replace = step("tool.after", "compress", "compress", &["replace_result"]);
    assert_eq!(
        schedule_event(
            &doc(Value::Null),
            std::slice::from_ref(&replace),
            "tool.after",
            12_000,
            GroupOrder::Parallel,
            SkillCheck::None
        ),
        Err(RouteError::InvalidBudget)
    );
    assert_eq!(
        schedule_event(
            &valid,
            &[replace],
            "tool.after",
            12_000,
            GroupOrder::Parallel,
            SkillCheck::None
        ),
        Err(RouteError::UnorderedReplacement)
    );
}
