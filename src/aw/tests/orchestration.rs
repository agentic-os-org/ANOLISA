mod common;
#[path = "orchestration/scenario.rs"]
mod scenario;

use aw_contracts::{canonical, orchestration::InvocationEvidence, validation::AdoptionCheck};
use common::{fixtures, REGISTRY};
use scenario::Scenario;
use serde_json::{json, Value};

#[test]
fn ordered_plans_admit_only_complete_native_and_os_checks() {
    Scenario::post_tool().check().unwrap();
    let pre = Scenario::pre_tool();
    let wire_plan = canonical::parse(include_bytes!("fixtures/pre-tool-plan.json")).unwrap();
    assert_eq!(wire_plan, pre.plan);
    pre.check().unwrap();
    pre.dispatch().unwrap();
    assert!(pre
        .dispatch_current(&pre.intent, &pre.protection, "untrusted-provider", 1500)
        .is_err());
}

#[test]
fn skipped_reordered_missing_or_overlapping_steps_are_rejected() {
    let mut s = Scenario::pre_tool();
    s.execution["steps"].as_array_mut().unwrap().swap(0, 1);
    assert!(s.check().is_err());
    let mut s = Scenario::pre_tool();
    s.execution["steps"].as_array_mut().unwrap().pop();
    assert!(s.check().is_err());
    let mut s = Scenario::pre_tool();
    s.execution["steps"][1]["started_sequence"] = json!(2);
    assert!(s.check().is_err());
    let mut s = Scenario::pre_tool();
    s.execution["steps"][0] = json!({"step_id":"step-0", "outcome":"skipped", "reason":"plugin_short_circuit", "invocations":[]});
    s.records.remove(0);
    s.refresh();
    assert!(s.dispatch().is_err());
}

#[test]
fn mandatory_checks_cannot_be_optional_or_ignore_failure() {
    for (field, value) in [
        ("required", json!(false)),
        ("on_failure", json!("record_gap_and_continue")),
    ] {
        let mut s = Scenario::pre_tool();
        s.plan["steps"][0][field] = value;
        s.refresh();
        assert!(s.check().is_err());
    }
    let mut s = Scenario::pre_tool();
    s.boundary["composition"]["input_finality"] = json!("uncontrolled");
    assert!(s.dispatch().is_err());
    let mut s = Scenario::pre_tool();
    s.boundary["composition"]["gate"] = json!("none");
    assert!(s.dispatch().is_err());
}

fn deny(output: &mut Value) {
    output["decision"]["verdict"] = json!("deny");
    output["decision"]["findings"] = json!([{"rule_id":"fixture-rule", "category":"dangerous_pattern", "severity":"high", "confidence":"high", "count":1}]);
    output["decision"]["reasons"] = json!(["policy.deny"]);
}

#[test]
fn denial_is_terminal_and_later_allow_cannot_override_it() {
    let mut s = Scenario::pre_tool();
    deny(s.records[0].output.as_mut().unwrap());
    s.execution["decision"] = json!("deny");
    s.skip_after_first();
    s.check().unwrap();
    assert!(s.dispatch().is_err());
    s.execution["decision"] = json!("proceed");
    assert!(s.check().is_err());
    let mut s = Scenario::pre_tool();
    deny(s.records[0].output.as_mut().unwrap());
    s.refresh();
    assert!(s.check().is_err());
}

#[test]
fn all_selected_providers_are_accounted_for_and_any_denial_wins() {
    let mut s = Scenario::pre_tool();
    let mut target = s.plan["steps"][0]["providers"][0].clone();
    target["provider_id"] = json!("second-provider");
    s.plan["steps"][0]["selection"] = json!("all_distinct_providers");
    s.plan["steps"][0]["providers"]
        .as_array_mut()
        .unwrap()
        .push(target);
    s.refresh();
    assert!(s.check().is_err());
    let mut extra = s.records[0].clone();
    extra.invocation["provider_id"] = json!("second-provider");
    extra.invocation["invocation_id"] = json!("second-call");
    extra.invocation["idempotency_key"] = json!("second-request");
    s.records.push(extra);
    s.refresh();
    s.dispatch().unwrap();
    deny(s.records[2].output.as_mut().unwrap());
    s.execution["decision"] = json!("deny");
    s.skip_after_first();
    s.check().unwrap();
    assert!(s.dispatch().is_err());
}

#[test]
fn failures_and_cancellation_cannot_become_success() {
    let mut s = Scenario::pre_tool();
    s.records[0].receipt["disposition"] = json!("failed");
    s.records[0].receipt["error_code"] = json!("provider_timeout");
    s.records[0].output = None;
    s.execution["steps"][0]["outcome"] = json!("gap");
    s.execution["steps"][0]["reason"] = json!("provider_timeout");
    s.execution["decision"] = json!("deny");
    s.skip_after_first();
    s.check().unwrap();
    assert!(s.dispatch().is_err());
    let mut s = Scenario::pre_tool();
    s.execution["steps"][0]["outcome"] = json!("cancelled");
    s.execution["steps"][0]["reason"] = json!("caller_cancelled");
    s.execution["decision"] = json!("cancelled");
    s.skip_after_first();
    s.check().unwrap();
    assert!(s.dispatch().is_err());
    s.execution["decision"] = json!("proceed");
    assert!(s.check().is_err());
}

#[test]
fn optional_observation_gaps_continue_but_required_gaps_stop() {
    let mut s = Scenario::post_tool();
    s.plan["steps"][0]["required"] = json!(false);
    s.plan["steps"][0]["on_failure"] = json!("record_gap_and_continue");
    s.records[0].receipt["disposition"] = json!("failed");
    s.records[0].receipt["error_code"] = json!("provider_unavailable");
    s.records[0].output = None;
    s.execution["steps"][0]["outcome"] = json!("gap");
    s.execution["steps"][0]["reason"] = json!("provider_unavailable");
    s.refresh();
    s.check().unwrap();
    s.plan["steps"][0]["required"] = json!(true);
    s.plan["steps"][0]["on_failure"] = json!("reject_plan");
    s.refresh();
    assert!(s.check().is_err());
    s.execution["decision"] = json!("preserve");
    s.skip_after_first();
    s.check().unwrap();
}

#[test]
fn stale_plan_receipt_reuse_and_source_substitution_are_rejected() {
    let mut s = Scenario::pre_tool();
    s.records[0].invocation["plan_ref"]["digest"] = json!("0".repeat(64));
    assert!(s.check().is_err());
    let mut s = Scenario::pre_tool();
    s.plan["source_digest"] = json!("0".repeat(64));
    s.refresh();
    assert!(s.check().is_err());
    let mut s = Scenario::pre_tool();
    s.records[1].invocation["invocation_id"] = s.records[0].invocation["invocation_id"].clone();
    s.refresh();
    assert!(s.check().is_err());
    let mut s = Scenario::pre_tool();
    s.records[0].invocation["provider_version"] = json!("unselected-version");
    s.refresh();
    assert!(s.check().is_err());
}

#[test]
fn os_binding_must_be_fresh_enforced_and_cover_the_exact_policy_target() {
    let s = Scenario::pre_tool();
    for (field, value) in [
        ("state", json!("unavailable")),
        ("state", json!("failed")),
        ("expires_at_ms", json!(1500)),
        ("observed_at_ms", json!(1501)),
        ("policy_digest", json!("0".repeat(64))),
        ("target_generation", json!("new-generation")),
    ] {
        let mut bad = s.protection.clone();
        bad[field] = value;
        assert!(
            s.dispatch_current(&s.intent, &bad, "fixture-os-authority", 1500)
                .is_err(),
            "{field}"
        );
    }
    for value in ["declared", "unsupported"] {
        let mut bad = s.protection.clone();
        bad["controls"][0]["coverage"] = json!(value);
        assert!(s
            .dispatch_current(&s.intent, &bad, "fixture-os-authority", 1500)
            .is_err());
    }
    let mut bad = s.protection.clone();
    bad["scope"]["runtime_generation"] = json!(2);
    assert!(s
        .dispatch_current(&s.intent, &bad, "fixture-os-authority", 1500)
        .is_err());
    let mut bad = s.protection.clone();
    bad["controls"][0]["control_id"] = json!("unrelated-control/v1");
    assert!(s
        .dispatch_current(&s.intent, &bad, "fixture-os-authority", 1500)
        .is_err());
    let mut changed = s.intent.clone();
    changed["working_directory_ref"] = json!("different-cwd");
    assert!(s
        .dispatch_current(&changed, &s.protection, "fixture-os-authority", 1500)
        .is_err());
}

#[test]
fn partial_os_coverage_and_duplicate_controls_do_not_satisfy_requirements() {
    let mut s = Scenario::pre_tool();
    s.plan["os_requirement"]["required_controls"]
        .as_array_mut()
        .unwrap()
        .push(json!("network.egress/v1"));
    s.refresh();
    assert!(s.dispatch().is_err());
    let mut control = s.protection["controls"][0].clone();
    control["control_id"] = json!("network.egress/v1");
    s.protection["controls"]
        .as_array_mut()
        .unwrap()
        .push(control.clone());
    s.dispatch().unwrap();
    s.protection["controls"]
        .as_array_mut()
        .unwrap()
        .push(control);
    assert!(s.dispatch().is_err());
}

#[test]
fn plan_adoption_binds_candidate_to_the_executed_plan() {
    let f = fixtures();
    let evidence = [InvocationEvidence {
        invocation: &f["capability-invocation-v1"],
        receipt: &f["provider-receipt-v1"],
        output: Some(&f["context-projection-prepare-output-v2"]),
    }];
    let check = || AdoptionCheck {
        invocation: &f["capability-invocation-v1"],
        receipt: &f["provider-receipt-v1"],
        output: Some(&f["context-projection-prepare-output-v2"]),
        observation: &f["context-adoption-v1"],
        boundary: &f["boundary-descriptor-v1"],
        effective_text: Some("alpha*3"),
        recovered_text: Some("alpha alpha alpha\n"),
        now_ms: 1500,
    };
    assert_eq!(
        REGISTRY
            .validate_plan_adoption(
                &f["capability-plan-v1"],
                &f["plan-execution-v1"],
                &evidence,
                check()
            )
            .unwrap(),
        Some(11)
    );
    let mut cancelled = f["plan-execution-v1"].clone();
    cancelled["decision"] = json!("cancelled");
    cancelled["steps"][0]["outcome"] = json!("cancelled");
    cancelled["steps"][0]["reason"] = json!("caller_cancelled");
    assert!(REGISTRY
        .validate_plan_adoption(&f["capability-plan-v1"], &cancelled, &evidence, check())
        .is_err());
}

#[test]
fn projection_cannot_precede_source_inspection_or_repeat() {
    let mut s = Scenario::post_tool();
    s.plan["steps"].as_array_mut().unwrap().swap(0, 1);
    s.refresh();
    assert!(REGISTRY.validate_plan(&s.plan, &s.boundary).is_err());
    let mut s = Scenario::post_tool();
    let extra = s.plan["steps"][1].clone();
    s.plan["steps"].as_array_mut().unwrap().push(extra);
    s.refresh();
    assert!(REGISTRY.validate_plan(&s.plan, &s.boundary).is_err());
}

#[test]
fn absent_optional_route_is_a_gap_without_fabricated_receipt() {
    let mut s = Scenario::post_tool();
    s.plan["steps"][0]["providers"] = json!([]);
    s.plan["steps"][0]["required"] = json!(false);
    s.plan["steps"][0]["on_failure"] = json!("record_gap_and_continue");
    s.records.remove(0);
    s.execution["steps"][0]["outcome"] = json!("gap");
    s.execution["steps"][0]["reason"] = json!("route_unavailable");
    s.refresh();
    s.check().unwrap();
    assert!(s.execution["steps"][0]["invocations"]
        .as_array()
        .unwrap()
        .is_empty());
    s.execution["steps"][0]["outcome"] = json!("completed");
    s.execution["steps"][0]
        .as_object_mut()
        .unwrap()
        .remove("reason");
    assert!(s.check().is_err());
}

#[test]
fn different_call_ids_cannot_reuse_a_provider_idempotency_key() {
    let mut s = Scenario::pre_tool();
    s.records[1].invocation["idempotency_key"] = s.records[0].invocation["idempotency_key"].clone();
    s.refresh();
    assert!(s.dispatch().is_err());
    let s = Scenario::pre_tool();
    assert!(s
        .dispatch_current(&s.intent, &Value::Null, "fixture-os-authority", 1500)
        .is_err());
}
