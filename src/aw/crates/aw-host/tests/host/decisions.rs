//! Exercise advisory and executor subprocesses with one admitted Host event.

use crate::fixture::{capabilities, event, Fixture, TIMEOUT};
use aw_host::{BindingContext, Failure, FailureAction, Host, Invocation, StepOutput};
use serde_json::{json, Value};
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

fn workflow(fixture: &Fixture, mode: &str) -> Value {
    let mut document = fixture.document();
    let mut arbiter = document["spec"]["providers"]["policy"].clone();
    arbiter["transport"]["argv"][4] = json!("arbiter");
    let mut executor = arbiter.clone();
    executor["transport"]["argv"][4] = json!("executor");
    document["spec"]["providers"] = json!({"semantic":arbiter,"snapshots":executor});
    document["spec"]["events"] = json!({
        "tool.before": {"enabled":true, "match":{"tools":["native:qoder:*"]}, "steps":[{
            "id":"semantic", "provider":"semantic", "operation":"evaluate", "mode":mode,
            "effects":["observe","block"], "on_error":"block", "decision":true,
            "obligations":{
                "snapshot":{"provider":"snapshots","operation":"checkpoint","order":10},
                "audit":{"provider":"snapshots","operation":"checkpoint","order":20}
            }
        }]}
    });
    document
}

fn callback(scenario: &str) -> Value {
    let mut value = event("tool.before", scenario);
    value["agent"]["instance_id"] = json!("instance-1");
    value["session_id"] = json!("raw-session");
    value["native"]["binding"] = json!({"workspace_id":"forged", "task_version":"forged"});
    value
}

fn context() -> BindingContext {
    BindingContext {
        session_id: "session-1".into(),
        instance_id: "instance-1".into(),
        workspace_id: "workspace-1".into(),
        task_version: "task-1".into(),
    }
}

fn invoke(host: &Host, scenario: &str, limit: Duration) -> Invocation {
    let cancelled = AtomicBool::new(false);
    let opened = host
        .event(callback(scenario), Instant::now() + limit, &cancelled)
        .unwrap()
        .with_binding(context(), &|| true)
        .unwrap()
        .with_obligation_audit(&|_| true);
    let result = opened.invoke("semantic").unwrap();
    assert!(opened.invoke("semantic").is_err());
    result
}

#[test]
fn off_omits_the_step_and_both_provider_processes() {
    let fixture = Fixture::new();
    let host = fixture.prepare(&workflow(&fixture, "off"));
    assert!(host.steps().is_empty());
    assert!(host.preparation().is_empty());
    assert!(fixture.calls("describe").is_empty());
}

#[test]
fn shadow_runs_on_observe_only_adapter_and_suppresses_decisions_and_actions() {
    let fixture = Fixture::new();
    let document = workflow(&fixture, "shadow");
    let mut capabilities = capabilities();
    capabilities
        .events
        .insert("tool.before".into(), vec!["observe".into()]);
    let host = Host::prepare(
        &document.to_string().into_bytes(),
        "target",
        capabilities,
        fixture.context(),
        Instant::now() + TIMEOUT,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(host.preparation().len(), 2);
    assert_eq!(host.steps()[0].on_error, "report");
    for (scenario, decision) in [
        ("block", "block"),
        ("ask", "ask"),
        ("abstain", "abstain"),
        ("snapshot-completed", "allow"),
    ] {
        let result = invoke(&host, scenario, TIMEOUT);
        let trace = result.trace.unwrap();
        assert!(trace.shadow);
        assert_eq!(trace.decision, Some(decision));
        assert!(trace.actions.is_empty());
        let Ok(StepOutput::Provider(outcome)) = result.result else {
            panic!("missing outcome")
        };
        assert!(!outcome.requests_block());
    }
    let timeout = invoke(&host, "arbiter-timeout", Duration::from_millis(600));
    assert!(matches!(
        timeout.result,
        Err(Failure::Transport(
            aw_exec::Error::DeadlineExceeded | aw_exec::Error::Cancelled
        ))
    ));
    assert_eq!(timeout.failure_action, Some(FailureAction::Report));
    assert!(fixture.calls("execute_obligation").is_empty());
    assert!(!fixture.0.join("confirmed-snapshot").exists());
}

#[test]
fn enforce_requires_correlated_snapshot_before_returning_allow() {
    let fixture = Fixture::new();
    let host = fixture.prepare(&workflow(&fixture, "enforce"));
    let invocation = invoke(&host, "snapshot-completed", TIMEOUT);
    let Ok(StepOutput::Provider(outcome)) = invocation.result else {
        panic!("expected successful decision")
    };
    assert!(!outcome.requests_block());
    let trace = invocation.trace.unwrap();
    assert_eq!(trace.decision, Some("allow"));
    assert_eq!(trace.obligations, ["snapshot", "audit"]);
    assert_eq!(trace.actions.len(), 2);
    let decision = fixture.call(&invocation.record.request_id)["request"].clone();
    assert_eq!(decision["binding"]["workspace_id"], "workspace-1");
    assert_eq!(decision["binding"]["session_id"], "session-1");
    assert_eq!(decision["binding"]["instance_id"], "instance-1");
    assert_eq!(decision["binding"]["task_version"], "task-1");
    assert_eq!(decision["binding"]["config_revision"], host.revision());
    assert_eq!(
        decision["binding"]["candidate_id"].as_str().unwrap().len(),
        64
    );
    for (action, kind) in trace.actions.iter().zip(["snapshot", "audit"]) {
        assert_eq!(action.kind, kind);
        assert_eq!(action.status, "completed");
        assert_eq!(action.record.event_id, invocation.record.event_id);
        let received = &fixture.call(&action.record.request_id)["request"];
        assert_eq!(received["binding"], decision["binding"]);
        assert_eq!(received["obligation"]["kind"], kind);
        assert!(received.get("event").is_none());
        assert!(received["budget_ms"].as_u64().unwrap() <= decision["budget_ms"].as_u64().unwrap());
        assert!(fixture.0.join(format!("confirmed-{kind}")).exists());
    }
}

#[test]
fn enforce_maps_block_and_ask_and_fails_closed_on_uncertain_protection() {
    let fixture = Fixture::new();
    let host = fixture.prepare(&workflow(&fixture, "enforce"));
    for (scenario, reason) in [
        ("block", "provider_decision_block"),
        ("ask", "provider_decision_approval_required"),
    ] {
        let result = invoke(&host, scenario, TIMEOUT);
        let Ok(StepOutput::Provider(outcome)) = result.result else {
            panic!("missing outcome")
        };
        assert!(outcome.requests_block());
        assert_eq!(outcome.as_value()["effects"][0]["reason_code"], reason);
    }
    for (scenario, expected) in [
        ("abstain", "decision_abstained"),
        ("route-unknown", "obligation_route_unavailable"),
        ("snapshot-skipped", "obligation_not_completed"),
    ] {
        let result = invoke(&host, scenario, TIMEOUT);
        assert!(matches!(result.result, Err(Failure::Decision(code)) if code == expected));
        assert_eq!(result.failure_action, Some(FailureAction::Block));
        let trace = result.trace.unwrap();
        if scenario == "route-unknown" {
            assert_eq!(trace.obligations, ["snapshot", "unmapped"]);
            assert!(trace.actions.is_empty());
        }
    }
    for scenario in [
        "direct-block",
        "snapshot-wrong-digest",
        "snapshot-provider-error",
    ] {
        let result = invoke(&host, scenario, TIMEOUT);
        assert_eq!(result.failure_action, Some(FailureAction::Block));
        assert!(matches!(
            result.result,
            Err(Failure::Protocol(_) | Failure::Provider { .. })
        ));
    }
    for scenario in ["arbiter-timeout", "snapshot-timeout"] {
        let result = invoke(&host, scenario, Duration::from_millis(700));
        assert_eq!(result.failure_action, Some(FailureAction::Block));
        assert!(matches!(
            result.result,
            Err(Failure::Transport(
                aw_exec::Error::DeadlineExceeded | aw_exec::Error::Cancelled
            ))
        ));
        let trace = result.trace.unwrap();
        if scenario == "snapshot-timeout" {
            assert_eq!(trace.actions[0].status, "deadline_exceeded");
        } else {
            assert!(trace.actions.is_empty());
        }
    }
    fixture.assert_reaped();
}

#[test]
fn unbound_enforcement_and_undeclared_executor_are_rejected() {
    let fixture = Fixture::new();
    let mut document = workflow(&fixture, "enforce");
    let host = fixture.prepare(&document);
    let cancelled = AtomicBool::new(false);
    let result = host
        .event(callback("allow"), Instant::now() + TIMEOUT, &cancelled)
        .unwrap()
        .invoke("semantic")
        .unwrap();
    assert!(matches!(
        result.result,
        Err(Failure::Decision("authoritative_binding_unavailable"))
    ));
    assert_eq!(result.failure_action, Some(FailureAction::Block));
    assert!(fixture.calls("invoke").is_empty());
    document["spec"]["events"]["tool.before"]["steps"][0]["obligations"]["snapshot"]["operation"] =
        json!("undeclared");
    assert!(Host::prepare(
        document.to_string().as_bytes(),
        "target",
        capabilities(),
        fixture.context(),
        Instant::now() + TIMEOUT,
        &cancelled
    )
    .is_err());
    assert!(fixture.calls("execute_obligation").is_empty());
}

#[test]
fn lost_authority_after_evaluation_prevents_an_action() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let fixture = Fixture::new();
    let host = fixture.prepare(&workflow(&fixture, "enforce"));
    let cancelled = AtomicBool::new(false);
    let checks = AtomicUsize::new(0);
    let active = || checks.fetch_add(1, Ordering::Relaxed) == 0;
    let result = host
        .event(
            callback("snapshot-completed"),
            Instant::now() + TIMEOUT,
            &cancelled,
        )
        .unwrap()
        .with_binding(context(), &active)
        .unwrap()
        .invoke("semantic")
        .unwrap();
    assert!(matches!(
        result.result,
        Err(Failure::Decision("authoritative_binding_expired"))
    ));
    assert_eq!(result.failure_action, Some(FailureAction::Block));
    assert_eq!(fixture.calls("invoke").len(), 1);
    assert!(fixture.calls("execute_obligation").is_empty());
}

#[test]
fn ordinary_shadow_records_a_replacement_without_adopting_it() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    document["spec"]["events"]["tool.after"]["steps"] = json!([{
        "id":"ordinary-shadow", "provider":"policy", "operation":"replace",
        "mode":"shadow", "effects":["replace_result"], "on_error":"withhold_result",
    }]);
    let host = fixture.prepare(&document);
    let cancelled = AtomicBool::new(false);
    let mut value = event("tool.after", "replace");
    value["tool"]["result_projection"] =
        json!({"text":"original","status":"success","content_origin":"file_content"});
    let invocation = host
        .event(value, Instant::now() + TIMEOUT, &cancelled)
        .unwrap()
        .invoke("ordinary-shadow")
        .unwrap();
    assert_eq!(invocation.trace.unwrap().effect_types, ["replace_result"]);
    let Ok(StepOutput::Provider(outcome)) = invocation.result else {
        panic!("expected a shadow outcome")
    };
    assert_eq!(outcome.as_value()["effects"], json!([]));
}

#[test]
fn configured_obligation_order_overrides_the_response_order() {
    let fixture = Fixture::new();
    let host = fixture.prepare(&workflow(&fixture, "enforce"));
    let result = invoke(&host, "snapshot-completed-reversed", TIMEOUT);
    let trace = result.trace.unwrap();
    assert!(result.result.is_ok());
    assert_eq!(trace.obligations, ["audit", "snapshot"]);
    let dispatched = trace
        .actions
        .iter()
        .map(|action| action.kind.as_str())
        .collect::<Vec<_>>();
    assert_eq!(dispatched, ["snapshot", "audit"]);

    let result = invoke(&host, "snapshot-skipped-reversed", TIMEOUT);
    assert_eq!(result.failure_action, Some(FailureAction::Block));
    let trace = result.trace.unwrap();
    assert_eq!(trace.actions.len(), 1);
    assert_eq!(trace.actions[0].kind, "snapshot");
}

#[test]
fn failed_obligation_registration_prevents_spawn_and_failed_receipt_write_blocks() {
    for allow_pending in [false, true] {
        let fixture = Fixture::new();
        let host = fixture.prepare(&workflow(&fixture, "enforce"));
        let cancelled = AtomicBool::new(false);
        let journal =
            |update: aw_host::ObligationUpdate<'_>| allow_pending && update.status == "pending";
        let result = host
            .event(
                callback("snapshot-completed"),
                Instant::now() + TIMEOUT,
                &cancelled,
            )
            .unwrap()
            .with_binding(context(), &|| true)
            .unwrap()
            .with_obligation_audit(&journal)
            .invoke("semantic")
            .unwrap();
        assert!(matches!(
            result.result,
            Err(Failure::Decision("obligation_audit_unavailable"))
        ));
        assert_eq!(result.failure_action, Some(FailureAction::Block));
        assert_eq!(
            fixture.calls("execute_obligation").len(),
            usize::from(allow_pending)
        );
    }
}
