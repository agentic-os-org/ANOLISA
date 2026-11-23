// Exercise the desktop reader on Linux together with the shared converter.
// The unchanged desktop fallback predates Clippy's let-chain lint.
#[allow(clippy::collapsible_if)]
#[path = "../src/local/collector/converter.rs"]
mod converter;

use agentsight_trajectory_collector::gemini::convert_gemini_events;
use serde_json::{Value, json};

fn conversation(shared: bool, patched: bool) -> Vec<Value> {
    let outputs = [
        json!({"functionResponse":{"id":"read-a","name":"read_file","response":{"output":"alpha file contents"}}}),
        json!({"functionResponse":{"id":"read-b","name":"read_file","response":{"output":"beta file contents"}}}),
    ];
    let mut calls = vec![];
    let mut patches = vec![];
    for (index, id) in ["read-a", "read-b"].into_iter().enumerate() {
        let result = if shared {
            json!(outputs)
        } else {
            json!([outputs[index]])
        };
        let mut call = json!({"id":id,"name":"read_file","args":{"path":format!("{id}.rs")},"status":"success"});
        if !patched {
            call["result"] = result.clone();
        }
        calls.push(call);
        patches.push(json!({"id":id,"result":result}));
    }
    let mut events = vec![
        json!({"sessionId":"fixture","projectHash":"test-project"}),
        json!({"id":"u","type":"user","content":"Read both files"}),
        json!({"id":"g","type":"gemini","toolCalls":calls}),
    ];
    if patched {
        events.push(json!({"$patch":{"updates":[{"id":"g","toolCalls":patches}]}}));
    }
    events.push(json!({"id":"answer","type":"gemini","content":"Both files read"}));
    events
}

fn jsonl(events: &[Value]) -> String {
    events.iter().map(|event| format!("{event}\n")).collect()
}

#[test]
fn history_sync_results_stay_with_their_call_in_all_readers() {
    // Gemini updateMessagesFromHistory assigns the entire user turn's parts
    // to EACH function response's call, including parallel same-name calls.
    for patched in [false, true] {
        let events = conversation(true, patched);
        let expected = convert_gemini_events(&conversation(false, patched), "gemini-cli").unwrap();
        let shared = convert_gemini_events(&events, "gemini-cli").unwrap();
        let local = converter::convert_jsonl_content_to_atif(&jsonl(&events)).unwrap();
        let legacy = json!({"sessionId":"fixture","projectHash":"test-project",
            "messages":conversation(true, false).into_iter().skip(1).collect::<Vec<_>>()});
        let legacy_local = converter::convert_jsonl_content_to_atif(&legacy.to_string()).unwrap();
        for trajectory in [shared, local, legacy_local] {
            assert_eq!(
                serde_json::to_value(trajectory).unwrap(),
                serde_json::to_value(&expected).unwrap(),
                "a sibling's response must not be copied into this call (patched={patched})"
            );
        }
    }
}

fn replay_tokens(events: &[Value]) -> usize {
    let converted = convert_gemini_events(events, "gemini-cli").unwrap();
    let trajectory =
        agentsight_opt::AtifTrajectory::from_json(&serde_json::to_string(&converted).unwrap())
            .unwrap();
    agentsight_opt::cost::compute_cost(&trajectory)
        .unwrap()
        .calls[1]
        .tool_results
}

#[test]
fn next_model_turn_does_not_pay_for_duplicated_sibling_results() {
    let reference = replay_tokens(&conversation(false, false));
    assert!(reference > 0);
    assert_eq!(replay_tokens(&conversation(true, true)), reference);
}

#[test]
fn explicit_foreign_response_is_not_relabelled_as_the_current_call() {
    let mut events = conversation(false, false);
    events[2]["toolCalls"][0]["result"] = json!([
        {"functionResponse":{"id":"read-b","response":{"output":"only beta"}}}
    ]);
    let converted = convert_gemini_events(&events, "gemini-cli").unwrap();
    assert_eq!(
        converted.steps[1].observation.as_ref().unwrap().results[0].content,
        Some(json!(""))
    );
}

#[test]
fn idless_legacy_parts_and_plain_text_keep_their_existing_order() {
    let mut events = conversation(false, false);
    events[2]["toolCalls"][0]["result"] = json!([
        {"text":"before"},
        {"functionResponse":{"name":"read_file","response":"legacy"}},
        {"functionResponse":{"id":"read-a","response":"owned"}},
        {"inlineData":{"mimeType":"image/png","data":"ignored"}},
        {"text":"after"}
    ]);
    let converted = convert_gemini_events(&events, "gemini-cli").unwrap();
    let observation = &converted.steps[1].observation.as_ref().unwrap().results[0];
    assert_eq!(
        observation.content,
        Some(json!("before\nlegacy\nowned\nafter"))
    );
    assert_eq!(observation.extra.as_ref().unwrap()["is_error"], false);
}
