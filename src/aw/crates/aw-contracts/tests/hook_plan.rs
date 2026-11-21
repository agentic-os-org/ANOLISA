//! Versioned resolved Hook plan contracts enforce single control/dispatch owners and coverage.

use aw_contracts::{
    canonical,
    hook_plan::{
        Coverage, CoverageGap, CoverageGapReason, CoverageGapStatus, ResolvedHookPlan, VERSION,
    },
    source::{FailureAction, HookEffect, SourceKind},
};
use serde_json::{json, Value};

fn plan_document() -> Value {
    let vectors: Value =
        canonical::parse(include_bytes!("fixtures/source-plan-vectors.json")).unwrap();
    vectors["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "combined-plan")
        .unwrap()["document"]
        .clone()
}

fn parse(value: &Value) -> Result<ResolvedHookPlan, aw_contracts::Error> {
    ResolvedHookPlan::parse(&serde_json::to_vec(value).unwrap())
}

#[test]
fn resolved_hook_plan_round_trip_preserves_control_owner_observers_and_coverage() {
    let value = plan_document();
    let plan = parse(&value).unwrap();
    assert_eq!(plan.version(), VERSION);
    assert_eq!(plan.target(), "qwenpaw");
    assert_eq!(plan.adapter(), "qwenpaw");
    assert_eq!(plan.hooks().len(), 2);

    let before = plan.hook("tool.before").unwrap();
    let owner = before.control_owner.as_ref().unwrap();
    assert_eq!(owner.source_id, "qwenpaw-native");
    assert_eq!(owner.kind, SourceKind::Native);
    assert!(owner.dispatches_steps);
    assert_eq!(before.observation_sources.len(), 2);
    assert!(before
        .observation_sources
        .iter()
        .all(|observer| !observer.dispatches_steps));
    assert_eq!(before.steps.len(), 2);
    assert!(before
        .steps
        .iter()
        .all(|step| step.dispatch_owner == "qwenpaw-native"));

    let model = plan.hook("model.before_request").unwrap();
    assert!(model.control_owner.is_none());
    assert_eq!(model.observation_sources.len(), 1);
    assert!(model.observation_sources[0].dispatches_steps);
    assert_eq!(
        model.dispatch_owner().unwrap().source_id,
        "python-monitoring"
    );

    assert!(!plan.coverage().is_complete());
    assert_eq!(plan.coverage().satisfied.len(), 2);
    assert_eq!(plan.coverage().gaps.len(), 2);
    assert_eq!(serde_json::to_value(&plan).unwrap(), value);
}

#[test]
fn resolved_hook_plan_rejects_duplicate_dispatch_owners_or_duplicate_steps() {
    let base = plan_document();

    // Auxiliary observer claiming step dispatch alongside a control owner is rejected.
    let mut duplicate_dispatcher = base.clone();
    duplicate_dispatcher["hooks"]["tool.before"]["observation_sources"][0]["dispatches_steps"] =
        json!(true);
    assert!(parse(&duplicate_dispatcher).is_err());

    // A step pointing to an auxiliary observation source instead of the single dispatch owner is rejected.
    let mut split_step_owner = base.clone();
    split_step_owner["hooks"]["tool.before"]["steps"][1]["dispatch_owner"] =
        json!("python-monitoring");
    assert!(parse(&split_step_owner).is_err());

    // Duplicate step_id within the same Hook point is rejected.
    let mut duplicate_step = base.clone();
    duplicate_step["hooks"]["tool.before"]["steps"][1]["step_id"] = json!("rewrite");
    assert!(parse(&duplicate_step).is_err());

    // Duplicate source_id between control_owner and observation_sources is rejected.
    let mut duplicate_source = base;
    duplicate_source["hooks"]["tool.before"]["observation_sources"][0]["source_id"] =
        json!("qwenpaw-native");
    assert!(parse(&duplicate_source).is_err());
}

#[test]
fn resolved_hook_plan_enforces_control_authority_and_step_ordering_rules() {
    let base = plan_document();

    // Observation-only point cannot execute a blocking step or blocking failure action.
    let mut observe_blocking = base.clone();
    observe_blocking["hooks"]["model.before_request"]["steps"][0]["on_error"] = json!("block");
    assert!(parse(&observe_blocking).is_err());

    // A Hook point without control_owner cannot hide a control-mode dispatcher in observation_sources.
    let mut hidden_control_observer = base.clone();
    hidden_control_observer["hooks"]["model.before_request"]["observation_sources"][0]["mode"] =
        json!("control");
    assert!(parse(&hidden_control_observer).is_err());

    // A non-dispatching auxiliary observer may still declare control capability when control_owner is None.
    let mut aux_control_observer = base.clone();
    aux_control_observer["hooks"]["model.before_request"]["observation_sources"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "source_id": "js-final-gate",
            "kind": "js",
            "contract": "js-final-gate/v1",
            "entrypoint": "sdk-worker",
            "mode": "control",
            "effects": ["observe", "block"],
            "failure_actions": [],
            "dispatches_steps": false
        }));
    aux_control_observer["coverage"]["satisfied"][0]["observation_sources"] =
        json!(["python-monitoring", "js-final-gate"]);
    assert!(parse(&aux_control_observer).is_ok());

    // Input replacement cannot appear after another step.
    let mut misordered_rewrite = base.clone();
    misordered_rewrite["hooks"]["tool.before"]["steps"]
        .as_array_mut()
        .unwrap()
        .swap(0, 1);
    assert!(parse(&misordered_rewrite).is_err());

    // Input replacement requires fail-closed before steps, with a read-only shadow exception.
    let mut non_blocking_before = base;
    non_blocking_before["hooks"]["tool.before"]["steps"][1]["on_error"] = json!("report");
    assert!(parse(&non_blocking_before).is_err());

    let mut shadow = non_blocking_before;
    let step = &mut shadow["hooks"]["tool.before"]["steps"][1];
    step["effects"] = json!(["observe"]);
    step["shadow"] = json!(true);
    shadow["coverage"]["satisfied"][1]["failure_actions"] = json!(["report", "block"]);
    let plan = parse(&shadow).unwrap();
    assert_eq!(serde_json::to_value(&plan).unwrap(), shadow);

    shadow["hooks"]["tool.before"]["steps"]
        .as_array_mut()
        .unwrap()
        .swap(0, 1);
    let plan = parse(&shadow).unwrap();
    assert_eq!(serde_json::to_value(&plan).unwrap(), shadow);

    shadow["hooks"]["tool.before"]["steps"][0]["effects"] = json!(["observe", "block"]);
    assert!(parse(&shadow).is_err());
}

#[test]
fn result_replacement_can_have_shadow_steps_on_either_side() {
    let base = plan_document();
    let mut hook = base["hooks"]["tool.before"].clone();
    hook["event"] = json!("tool.after");
    hook["observation_sources"] = json!([]);
    hook["control_owner"]["effects"] = json!(["observe", "replace_result"]);
    hook["steps"][0]["effects"] = json!(["replace_result"]);
    hook["steps"][0]["on_error"] = json!("report");
    hook["steps"][1]["effects"] = json!(["observe"]);
    hook["steps"][1]["on_error"] = json!("report");
    hook["steps"][1]["shadow"] = json!(true);
    let mut document = json!({
        "version": VERSION, "target": "qwenpaw", "adapter": "qwenpaw",
        "hooks": {"tool.after": hook},
        "coverage": {"satisfied": [{
            "event": "tool.after", "required": true, "control_owner": "qwenpaw-native",
            "observation_sources": [], "effects": ["observe", "replace_result"],
            "failure_actions": ["report"]
        }], "gaps": []}
    });
    let plan = parse(&document).unwrap();
    assert_eq!(serde_json::to_value(&plan).unwrap(), document);

    // Making the trailing observe step enforcing restores the last-step constraint.
    document["hooks"]["tool.after"]["steps"][1]["shadow"] = json!(false);
    assert!(parse(&document).is_err());

    let steps = document["hooks"]["tool.after"]["steps"]
        .as_array_mut()
        .unwrap();
    steps[1]["shadow"] = json!(true);
    steps.swap(0, 1);
    let plan = parse(&document).unwrap();
    assert_eq!(serde_json::to_value(&plan).unwrap(), document);
}

#[test]
fn resolved_hook_plan_rejects_required_gaps_and_inconsistent_coverage() {
    let base = plan_document();

    // A required Hook requirement can never appear in coverage.gaps.
    let mut required_gap = base.clone();
    required_gap["coverage"]["gaps"][0]["required"] = json!(true);
    assert!(parse(&required_gap).is_err());

    // An event cannot be both in resolved hooks and in coverage.gaps.
    let mut overlapping_gap = base.clone();
    overlapping_gap["coverage"]["gaps"][0]["event"] = json!("tool.before");
    assert!(parse(&overlapping_gap).is_err());

    // Uncovered gap cannot list fallback observation sources.
    let mut bad_uncovered = base.clone();
    bad_uncovered["coverage"]["gaps"][0]["fallback_sources"] = json!(["sight-observer"]);
    assert!(parse(&bad_uncovered).is_err());

    // Degraded gap requires fallback observation sources.
    let mut bad_degraded = base.clone();
    bad_degraded["coverage"]["gaps"][1]["fallback_sources"] = json!([]);
    assert!(parse(&bad_degraded).is_err());

    // Coverage satisfied list must match resolved hooks.
    let mut missing_satisfied = base;
    missing_satisfied["coverage"]["satisfied"]
        .as_array_mut()
        .unwrap()
        .pop();
    assert!(parse(&missing_satisfied).is_err());

    let empty = ResolvedHookPlan::new(
        "qwenpaw",
        "qwenpaw",
        Default::default(),
        Coverage {
            satisfied: Vec::new(),
            gaps: vec![CoverageGap {
                event: "tool.after".into(),
                required: false,
                status: CoverageGapStatus::Degraded,
                reason: CoverageGapReason::UnsupportedFailureAction,
                missing_effects: Vec::new(),
                missing_failure_actions: vec![FailureAction::WithholdResult],
                fallback_sources: vec!["sight-observer".into()],
            }],
        },
    )
    .unwrap();
    assert!(!empty.coverage().is_complete());
    let mut bad_gap = serde_json::to_value(&empty).unwrap();
    bad_gap["coverage"]["gaps"][0]["reason"] = json!("unsupported_effect");
    assert!(parse(&bad_gap).is_err());
    bad_gap["coverage"]["gaps"][0]["missing_effects"] = json!([HookEffect::ReplaceResult]);
    assert!(parse(&bad_gap).is_ok());

    // A gap on session.end cannot claim missing replace_input or withhold_result.
    let mut illegal_gap_effect = serde_json::to_value(&empty).unwrap();
    illegal_gap_effect["coverage"]["gaps"][0] = json!({
        "event": "session.end",
        "required": false,
        "status": "uncovered",
        "reason": "unsupported_event",
        "missing_effects": ["observe", "replace_input"],
        "missing_failure_actions": [],
        "fallback_sources": []
    });
    assert!(parse(&illegal_gap_effect).is_err());

    let mut illegal_gap_action = serde_json::to_value(&empty).unwrap();
    illegal_gap_action["coverage"]["gaps"][0] = json!({
        "event": "session.end",
        "required": false,
        "status": "uncovered",
        "reason": "unsupported_event",
        "missing_effects": ["observe"],
        "missing_failure_actions": ["withhold_result"],
        "fallback_sources": []
    });
    assert!(parse(&illegal_gap_action).is_err());
}

#[test]
fn native_command_steps_only_parse_on_tool_before_and_after() {
    let fixture = plan_document();
    let mut point = fixture["hooks"]["tool.before"].clone();
    point["control_owner"]["effects"] = json!(["observe"]);
    point["observation_sources"] = json!([]);
    point["steps"] = json!([{
        "step_id": "command", "provider": "native", "effects": [], "on_error": "report",
        "step_tools": ["*"], "native": true, "dispatch_owner": "qwenpaw-native"
    }]);
    for event in aw_contracts::source::HOOK_EVENTS {
        point["event"] = json!(event);
        let document = json!({
            "version": VERSION, "target": "qwenpaw", "adapter": "qwenpaw",
            "hooks": {event: point},
            "coverage": {"satisfied": [{
                "event": event, "required": true, "control_owner": "qwenpaw-native",
                "observation_sources": [], "effects": ["observe"], "failure_actions": ["report"]
            }], "gaps": []}
        });
        assert_eq!(
            parse(&document).is_ok(),
            matches!(event, "tool.before" | "tool.after"),
            "{event}"
        );
    }
}

#[test]
fn native_command_steps_reject_withhold_result() {
    let fixture = plan_document();
    let mut point = fixture["hooks"]["tool.before"].clone();
    point["event"] = json!("tool.after");
    point["control_owner"]["effects"] = json!(["observe"]);
    point["control_owner"]["failure_actions"] = json!(["withhold_result"]);
    point["observation_sources"] = json!([]);
    point["steps"] = json!([{
        "step_id": "command", "provider": "native", "effects": [],
        "on_error": "withhold_result", "step_tools": ["*"], "native": true,
        "dispatch_owner": "qwenpaw-native"
    }]);
    let mut document = json!({
        "version": VERSION, "target": "qwenpaw", "adapter": "qwenpaw",
        "hooks": {"tool.after": point},
        "coverage": {"satisfied": [{
            "event": "tool.after", "required": true, "control_owner": "qwenpaw-native",
            "observation_sources": [], "effects": ["observe"],
            "failure_actions": ["withhold_result"]
        }], "gaps": []}
    });

    assert!(matches!(
        parse(&document),
        Err(aw_contracts::Error::Invariant(
            "native Hook step supports report or block failure actions"
        ))
    ));

    let step = &mut document["hooks"]["tool.after"]["steps"][0];
    step["native"] = json!(false);
    step["provider"] = json!("policy");
    step["effects"] = json!(["observe"]);
    parse(&document).unwrap();
}
