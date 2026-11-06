use agentsight_trajectory_collector::cline::convert_cline_events;
use serde_json::json;

fn request(index: i64, timestamp: i64, usage: serde_json::Value) -> serde_json::Value {
    json!({"type":"say","say":"api_req_started","conversationHistoryIndex":index,"ts":timestamp,"text":usage.to_string()})
}

#[test]
fn a_failed_request_does_not_steal_the_next_responses_usage() {
    let base = 1763071200000i64;
    let events = vec![
        json!({"role":"user","content":"first request"}),
        json!({"role":"assistant","content":"[Response interrupted by API Error]"}),
        json!({"role":"user","content":"retry the request"}),
        json!({"role":"assistant","content":"completed response"}),
        request(
            -1,
            base,
            json!({"request":"first request","cancelReason":"streaming_failed"}),
        ),
        request(
            1,
            base + 10000,
            json!({"tokensIn":120,"tokensOut":30,"cacheReads":8,"cost":0.01}),
        ),
    ];
    let doc = convert_cline_events(&events, "cline").unwrap();
    assert_eq!(doc.steps.len(), 4);
    assert!(doc.steps[1].metrics.is_none());
    let metrics = doc.steps[3].metrics.as_ref().unwrap();
    assert_eq!(metrics.prompt_tokens, Some(128));
    assert_eq!(metrics.completion_tokens, Some(30));
    assert_eq!(
        doc.final_metrics.as_ref().unwrap().total_prompt_tokens,
        Some(128)
    );
    assert_eq!(
        doc.steps[0].timestamp.as_deref(),
        Some("2025-11-13T22:00:00.000Z")
    );
    assert_eq!(
        doc.steps[2].timestamp.as_deref(),
        Some("2025-11-13T22:00:10.000Z")
    );
}

#[test]
fn indexed_requests_do_not_shift_after_an_extra_assistant_annotation() {
    let events = vec![
        json!({"role":"user","content":"first"}),
        json!({"role":"assistant","content":"partial answer"}),
        json!({"role":"assistant","content":"[Response interrupted by user]"}),
        json!({"role":"user","content":"second"}),
        json!({"role":"assistant","content":"final answer"}),
        request(-1, 1763071200000, json!({"tokensIn":10,"tokensOut":2})),
        request(2, 1763071210000, json!({"tokensIn":20,"tokensOut":3})),
    ];
    let doc = convert_cline_events(&events, "cline").unwrap();
    let final_step = doc.steps.last().unwrap();
    assert_eq!(final_step.metrics.as_ref().unwrap().prompt_tokens, Some(20));
    assert_eq!(
        doc.final_metrics.as_ref().unwrap().total_prompt_tokens,
        Some(30)
    );
}

#[test]
fn indexless_failed_slots_and_explicit_zero_usage_are_preserved() {
    let mut events = vec![
        json!({"role":"user","content":"first"}),
        json!({"role":"assistant","content":"interrupted"}),
        json!({"role":"user","content":"second"}),
        json!({"role":"assistant","content":"completed"}),
    ];
    let mut failed = request(
        -1,
        1763071200000,
        json!({"cancelReason":"streaming_failed"}),
    );
    failed
        .as_object_mut()
        .unwrap()
        .remove("conversationHistoryIndex");
    let mut successful = request(
        1,
        1763071210000,
        json!({"tokensIn":0,"tokensOut":0,"cacheReads":50}),
    );
    successful
        .as_object_mut()
        .unwrap()
        .remove("conversationHistoryIndex");
    events.extend([failed, successful]);
    let doc = convert_cline_events(&events, "cline").unwrap();
    assert!(doc.steps[1].metrics.is_none());
    let metrics = doc.steps[3].metrics.as_ref().unwrap();
    assert_eq!(metrics.prompt_tokens, Some(50));
    assert_eq!(metrics.completion_tokens, Some(0));
    assert_eq!(metrics.cached_tokens, Some(50));
    assert_eq!(
        doc.final_metrics.as_ref().unwrap().total_prompt_tokens,
        Some(50)
    );
}
