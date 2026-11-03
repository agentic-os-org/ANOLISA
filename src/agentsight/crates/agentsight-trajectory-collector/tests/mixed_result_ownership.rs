//! Mixed user replies retain prior results on the issuing agent step.

use agentsight_atif::{AtifTrajectory, StepSource};
use agentsight_trajectory_collector::{atif::convert_qoder_events, qoder::load_jsonl_events};
use serde_json::{json, Value};

fn calls() -> Value {
    json!({"type":"assistant","timestamp":"2026-10-01T00:00:01Z","message":{
        "role":"assistant","content":[
            {"type":"tool_use","id":"call-a","name":"Read","input":{"file_path":"/fixture/a"}},
            {"type":"tool_use","id":"call-b","name":"Read","input":{"file_path":"/fixture/b"}}
        ]
    }})
}

fn reply(id: &str, text: Option<&str>) -> Value {
    let mut content = vec![json!({"type":"tool_result","tool_use_id":id,
        "content":format!("recorded result {id}")})];
    if let Some(text) = text {
        content.push(json!({"type":"text","text":text}));
    }
    json!({"type":"user","timestamp":"2026-10-01T00:00:02Z","message":{
        "role":"user","content":content
    }})
}

fn convert(events: &[Value]) -> AtifTrajectory {
    let jsonl = events
        .iter()
        .map(|event| format!("{event}\n"))
        .collect::<String>();
    let converted = convert_qoder_events(&load_jsonl_events(&jsonl), "qoder").unwrap();
    converted.validate_step_ids().unwrap();
    serde_json::from_str(&serde_json::to_string(&converted).unwrap()).unwrap()
}

fn result_ids(trajectory: &AtifTrajectory, source: StepSource) -> Vec<&str> {
    trajectory
        .steps
        .iter()
        .filter(|step| step.source == source)
        .flat_map(|step| step.observation.iter().flat_map(|obs| &obs.results))
        .filter_map(|result| result.source_call_id.as_deref())
        .collect()
}

#[test]
fn mixed_reply_preserves_the_preceding_result_of_another_call() {
    let trajectory = convert(&[
        calls(),
        reply("call-a", None),
        reply("call-b", Some("also review the docs")),
    ]);
    assert_eq!(
        result_ids(&trajectory, StepSource::Agent),
        vec!["call-a", "call-b"]
    );
    let user = trajectory
        .steps
        .iter()
        .find(|step| step.source == StepSource::User)
        .unwrap();
    assert_eq!(user.message, "also review the docs");
    assert!(user.observation.is_none());
}

#[test]
fn mixed_reply_after_user_text_attaches_to_the_issuing_agent() {
    let trajectory = convert(&[
        calls(),
        json!({"type":"user","message":{"role":"user","content":"new instruction"}}),
        reply("call-a", Some("keep the recorded result")),
    ]);
    assert_eq!(result_ids(&trajectory, StepSource::Agent), vec!["call-a"]);
    assert!(result_ids(&trajectory, StepSource::User).is_empty());
}

#[test]
fn result_only_replies_still_preserve_both_calls() {
    let trajectory = convert(&[calls(), reply("call-a", None), reply("call-b", None)]);
    assert_eq!(
        result_ids(&trajectory, StepSource::Agent),
        vec!["call-a", "call-b"]
    );
}

#[test]
fn mixed_replayed_context_without_an_owner_retains_its_observation() {
    let trajectory = convert(&[reply("historical-call", Some("continue"))]);
    assert_eq!(
        result_ids(&trajectory, StepSource::User),
        vec!["historical-call"]
    );
    assert_eq!(trajectory.steps[0].message, "continue");
}

#[test]
fn mixed_reply_keeps_owned_and_replayed_results_without_losing_either() {
    let mut mixed = reply("call-b", Some("continue with the recorded context"));
    mixed["message"]["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "type":"tool_result","tool_use_id":"historical-call","content":"replayed result"
        }));
    let trajectory = convert(&[calls(), reply("call-a", None), mixed]);
    assert_eq!(
        result_ids(&trajectory, StepSource::Agent),
        vec!["call-a", "call-b"]
    );
    assert_eq!(
        result_ids(&trajectory, StepSource::User),
        vec!["historical-call"]
    );
}

#[test]
fn mixed_reply_retains_call_identity_error_flag_and_result_timestamp() {
    let mut mixed = reply("callb", Some("record the failure"));
    mixed["message"]["content"][0]["is_error"] = json!(true);
    let trajectory = convert(&[calls(), reply("call-a", None), mixed]);
    let agent = &trajectory.steps[0];
    let results = &agent.observation.as_ref().unwrap().results;
    assert_eq!(results.len(), 2);
    assert_eq!(results[1].source_call_id.as_deref(), Some("callb"));
    assert_eq!(results[1].is_error(), Some(true));
    let call = agent
        .tool_calls
        .as_ref()
        .unwrap()
        .iter()
        .find(|call| call.tool_call_id == "call-b")
        .unwrap();
    assert_eq!(
        call.extra.as_ref().unwrap()["result_timestamp"],
        "2026-10-01T00:00:02Z"
    );
}
