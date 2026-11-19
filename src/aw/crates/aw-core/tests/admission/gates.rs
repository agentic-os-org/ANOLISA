//! Capability admission across lifecycle, decision and mutation gates.

use super::*;

#[test]
fn lifecycle_and_turn_stop_gates_admit_structured_and_boolean_steps() {
    let mut doc = document();
    doc["spec"]["events"]["session.start"] = json!({
        "enabled": true,
        "required": false,
        "steps": [{
            "id": "on-start",
            "provider": "policy",
            "operation": "lifecycle",
            "effects": ["observe"],
            "on_error": "report"
        }]
    });
    doc["spec"]["events"]["session.end"] = json!({
        "enabled": true,
        "required": false,
        "steps": [{
            "id": "on-end",
            "provider": "policy",
            "operation": "lifecycle",
            "effects": ["observe"],
            "on_error": "report"
        }]
    });
    doc["spec"]["events"]["turn.stop"] = json!({
        "enabled": true,
        "required": true,
        "steps": [{
            "id": "on-stop",
            "provider": "policy",
            "operation": "stop",
            "effects": ["observe", "block"],
            "on_error": "block"
        }]
    });

    let mut ops = operations();
    ops.as_array_mut().unwrap().extend([
        json!({"name": "lifecycle", "events": ["session.start", "session.end"], "effects": ["observe"]}),
        json!({"name": "stop", "events": ["turn.stop"], "effects": ["observe", "block"]}),
    ]);
    let providers = evidence(&doc["spec"]["providers"]["policy"]["config"], ops);

    let mut cap = capabilities("qoder");
    cap.events
        .insert("session.start".into(), vec!["observe".into()]);
    cap.events
        .insert("session.end".into(), vec!["observe".into()]);
    cap.events
        .insert("turn.stop".into(), vec!["observe".into(), "block".into()]);

    let policy = admit_policy(&doc, "qoder", &cap, &providers).unwrap();
    assert_eq!(policy.steps().len(), 5);
    for (event, id, on_error, expected_effects) in [
        ("session.start", "on-start", "report", vec!["observe"]),
        ("session.end", "on-end", "report", vec!["observe"]),
        ("turn.stop", "on-stop", "block", vec!["observe", "block"]),
    ] {
        let step = policy
            .steps()
            .iter()
            .find(|step| step.event == event)
            .unwrap();
        assert_eq!(step.step_id, id);
        assert_eq!(step.on_error, on_error);
        assert_eq!(step.event_tools, ["*"]);
        assert_eq!(step.step_tools, ["*"]);
        let StepExecution::Provider { effects, .. } = &step.execution else {
            panic!("expected structured Provider step for {event}");
        };
        assert_eq!(effects, &expected_effects);
    }

    // Boolean hooks reuse the same gate capability rules without discovery evidence.
    let mut hook_doc = doc.clone();
    let provider = hook_doc["spec"]["providers"]["policy"]
        .as_object_mut()
        .unwrap();
    provider.remove("protocol");
    provider.remove("config");
    for event in [
        "tool.before",
        "tool.after",
        "session.start",
        "session.end",
        "turn.stop",
    ] {
        hook_doc["spec"]["events"][event]["steps"][0]
            .as_object_mut()
            .unwrap()
            .remove("operation");
    }
    hook_doc["spec"]["events"]["turn.stop"]["steps"][0]["reason_code"] = json!("premature_stop");
    let hook_policy = admit_policy(&hook_doc, "qoder", &cap, &BTreeMap::new()).unwrap();
    assert_eq!(hook_policy.steps().len(), 5);
}

#[test]
fn gate_boundaries_reject_escalated_adapter_or_step_capabilities() {
    let doc = document();
    let providers = providers(&doc);

    // Lifecycle events and decision gates cannot declare disallowed effects in Adapter evidence.
    for (event, effect) in [
        ("session.start", "block"),
        ("session.start", "replace_input"),
        ("session.end", "block"),
        ("session.end", "replace_result"),
        ("turn.stop", "replace_input"),
        ("turn.stop", "replace_result"),
        ("turn.stop", "ask"),
        ("model.before_request", "observe"),
    ] {
        let mut cap = capabilities("qoder");
        cap.events.insert(event.into(), vec![effect.into()]);
        assert_eq!(
            admit_policy(&doc, "qoder", &cap, &providers).unwrap_err(),
            "Adapter evidence exceeds the supported event or effect surface",
            "{event}/{effect}"
        );
    }

    // Failure withholding remains strictly confined to the post-tool mutation gate,
    // including when an empty failure-action list is supplied on another event.
    for (event, actions) in [
        ("tool.before", vec![]),
        ("session.start", vec![]),
        ("turn.stop", vec![]),
        ("session.start", vec!["withhold_result".into()]),
        ("session.end", vec!["withhold_result".into()]),
        ("turn.stop", vec!["withhold_result".into()]),
        ("input.submit", vec!["withhold_result".into()]),
    ] {
        let mut cap = capabilities("qoder");
        cap.events.insert(event.into(), vec!["observe".into()]);
        cap.failure_actions.insert(event.into(), actions);
        assert_eq!(
            admit_policy(&doc, "qoder", &cap, &providers).unwrap_err(),
            "Adapter evidence exceeds the supported failure action surface",
            "{event}"
        );
    }

    // turn.stop on_error: block requires Adapter block capability even for observe-only steps.
    let mut stop_doc = document();
    stop_doc["spec"]["events"]["turn.stop"] = json!({
        "enabled": true,
        "steps": [{
            "id": "stop-observe",
            "provider": "policy",
            "operation": "stop",
            "effects": ["observe"],
            "on_error": "block"
        }]
    });
    let mut ops = operations();
    ops.as_array_mut()
        .unwrap()
        .push(json!({"name": "stop", "events": ["turn.stop"], "effects": ["observe"]}));
    let stop_providers = evidence(&stop_doc["spec"]["providers"]["policy"]["config"], ops);
    let mut observe_only = capabilities("qoder");
    observe_only
        .events
        .insert("turn.stop".into(), vec!["observe".into()]);
    assert_eq!(
        admit_policy(&stop_doc, "qoder", &observe_only, &stop_providers).unwrap_err(),
        "failure action is not supported by the implementation and Adapter"
    );
    observe_only
        .events
        .insert("turn.stop".into(), vec!["observe".into(), "block".into()]);
    assert!(admit_policy(&stop_doc, "qoder", &observe_only, &stop_providers).is_ok());

    // Unimplemented events remain rejected even when empty and optional.
    for unimplemented in [
        "model.before_request",
        "compact.before",
        "subagent.start",
        "runtime.observed",
        "coverage.changed",
    ] {
        let mut rejected = document();
        rejected["spec"]["events"][unimplemented] =
            json!({"enabled": true, "required": false, "steps": []});
        assert_eq!(
            admit_policy(&rejected, "qoder", &capabilities("qoder"), &providers).unwrap_err(),
            "enabled event is not supported",
            "{unimplemented}"
        );
    }
}
