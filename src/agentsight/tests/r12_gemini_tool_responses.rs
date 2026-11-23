// Local collection is a desktop module; compile its real source on Linux too.
// Existing desktop-only legacy fallback predates the let-chain lint.
#[allow(clippy::collapsible_if)]
#[path = "../src/local/collector/converter.rs"]
mod converter;
use agentsight_trajectory_collector::gemini::convert_gemini_events;
use converter::convert_jsonl_content_to_atif;
use serde_json::{Value, json};

fn convert(result: Value) -> agentsight_atif::AtifTrajectory {
    let events = [
        json!({"sessionId":"fixture","projectHash":"p"}),
        json!({"id":"m","type":"gemini","toolCalls":[
            {"id":"call","name":"read_file","args":{"path":"file.rs"},"result":result}]}),
        json!({"id":"end","type":"gemini","content":"done"}),
    ];
    let trajectory = convert_gemini_events(&events, "gemini-cli").unwrap();
    let jsonl = events.iter().map(|e| format!("{e}\n")).collect::<String>();
    assert_eq!(
        serde_json::to_value(&trajectory).unwrap(),
        serde_json::to_value(convert_jsonl_content_to_atif(&jsonl).unwrap()).unwrap()
    );
    trajectory
}

#[test]
fn structured_tool_response_reaches_local_display_and_cost_breakdown() {
    let payload = json!({"output":"first line\nsecond line","exit_code":0});
    let trajectory = convert(json!([{"functionResponse":{"name":"read_file","response":payload}}]));
    let result = &trajectory.steps[0].observation.as_ref().unwrap().results[0];
    assert_eq!(result.source_call_id.as_deref(), Some("call"));
    let text = result.content.as_ref().unwrap().as_str().unwrap();
    assert_eq!(serde_json::from_str::<Value>(text).unwrap(), payload);
    let analyzed =
        agentsight_opt::AtifTrajectory::from_json(&serde_json::to_string(&trajectory).unwrap())
            .unwrap();
    let stats = agentsight_opt::cost::compute_cost(&analyzed).unwrap();
    assert!(
        stats.calls[1].tool_results > 0,
        "the next turn must replay the tool output"
    );
}

#[test]
fn mixed_text_and_response_parts_preserve_order_without_binary_data() {
    let trajectory = convert(json!([{"text":"before"},
        {"inlineData":{"mimeType":"image/png","data":"not-text"}},
        {"functionResponse":{"response":{"items":["one","two"]}}},
        {"text":"after"}]));
    let content = trajectory.steps[0].observation.as_ref().unwrap().results[0]
        .content
        .as_ref()
        .unwrap();
    assert_eq!(
        content.as_str().unwrap(),
        "before\n{\"items\":[\"one\",\"two\"]}\nafter"
    );
}

#[test]
fn scalar_text_object_and_empty_controls() {
    for (input, expected) in [
        (json!("plain"), "plain"),
        (json!({"text":"text-part"}), "text-part"),
        (
            json!({"functionResponse":{"response":"string output"}}),
            "string output",
        ),
        (json!({"functionResponse":{"response":{}}}), "{}"),
        (json!([]), ""),
    ] {
        let trajectory = convert(input);
        assert_eq!(
            trajectory.steps[0].observation.as_ref().unwrap().results[0].content,
            Some(json!(expected))
        );
    }
}

#[test]
fn patched_results_are_decoded_after_replay_and_keep_failure_status() {
    let events = [
        json!({"sessionId":"s","projectHash":"p"}),
        json!({"id":"m","type":"gemini","toolCalls":[{"id":"c","name":"read_file","args":{},"status":"error"}]}),
        json!({"$patch":{"id":"m","toolCalls":[{"id":"c","result":[{"functionResponse":{"response":{"error":"permission denied"}}}]}]}}),
    ];
    let trajectory = convert_gemini_events(&events, "gemini-cli").unwrap();
    let result = &trajectory.steps[0].observation.as_ref().unwrap().results[0];
    assert_eq!(result.extra.as_ref().unwrap()["is_error"], json!(true));
    assert_eq!(
        serde_json::from_str::<Value>(result.content.as_ref().unwrap().as_str().unwrap()).unwrap(),
        json!({"error":"permission denied"})
    );
}
