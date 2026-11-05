// Local collection is a desktop module; compile its real source on Linux too.
// Existing desktop-only legacy fallback predates the let-chain lint.
#[allow(clippy::collapsible_if)]
#[path = "../src/local/collector/converter.rs"]
mod converter;
use converter::convert_jsonl_content_to_atif;
use serde_json::{Value, json};

fn convert(events: &[Value]) -> agentsight_atif::AtifTrajectory {
    let content = events
        .iter()
        .map(|event| format!("{event}\n"))
        .collect::<String>();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    std::fs::write(&path, &content).unwrap();
    let result = converter::convert_jsonl_to_atif(&path).unwrap();
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        serde_json::to_value(convert_jsonl_content_to_atif(&content).unwrap()).unwrap()
    );
    result.validate_step_ids().unwrap();
    result
}

fn call(id: &str) -> Value {
    json!({"type":"assistant","message":{"content":[
        {"type":"tool_use","id":id,"name":"Read","input":{"path":id}}]}})
}
fn reply(ids: &[&str], mixed: bool) -> Value {
    let mut blocks: Vec<Value> = ids
        .iter()
        .map(|id| json!({"type":"tool_result","tool_use_id":id,"content":format!("result {id}")}))
        .collect();
    if mixed {
        blocks.push(json!({"type":"text","text":"keep my instruction"}));
    }
    json!({"type":"user","message":{"content":blocks}})
}

#[test]
fn delayed_results_return_to_their_calling_steps() {
    let result = convert(&[
        call("a"),
        json!({"type":"user","message":{"content":"continue"}}),
        call("b"),
        reply(&["a", "b"], true),
        reply(&["a"], false),
    ]);
    assert_eq!(
        result.steps[0].observation.as_ref().unwrap().results.len(),
        2
    );
    assert_eq!(
        result.steps[2].observation.as_ref().unwrap().results.len(),
        1
    );
    assert_eq!(result.steps[3].message, "keep my instruction");
    assert!(result.steps[1].observation.is_none());
    let analyzed =
        agentsight_opt::AtifTrajectory::from_json(&serde_json::to_string(&result).unwrap())
            .unwrap();
    let inventory = agentsight_opt::trace::build_inventory(&analyzed);
    assert_eq!(inventory.tool_calls.len(), 2);
    assert_eq!(inventory.user_turns.len(), 2);
}

#[test]
fn exact_id_outranks_a_newer_alias() {
    let result = convert(&[call("call_a"), call("calla"), reply(&["call_a"], false)]);
    assert!(result.steps[0].observation.is_some());
    assert!(result.steps[1].observation.is_none());
}

#[test]
fn unique_echo_is_accepted_but_ambiguous_echo_is_not_assigned() {
    let result = convert(&[call("call_a"), reply(&["CALLA"], false)]);
    assert!(result.steps[0].observation.is_some());
    let result = convert(&[call("call_a"), call("call-a"), reply(&["CALLA"], true)]);
    assert!(
        result.steps[..2]
            .iter()
            .all(|step| step.observation.is_none())
    );
    assert_eq!(result.steps[2].message, "keep my instruction");
}

#[test]
fn foreign_results_do_not_invent_ownership() {
    let result = convert(&[call("a"), reply(&["missing", ""], false)]);
    assert!(result.steps[0].observation.is_none());
}
