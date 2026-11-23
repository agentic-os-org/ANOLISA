// Local collection is a desktop module; compile its real source on Linux too.
// Existing desktop-only legacy fallback predates the let-chain lint.
#[allow(clippy::collapsible_if)]
#[path = "../src/local/collector/converter.rs"]
mod converter;
use converter::convert_jsonl_content_to_atif;
use serde_json::{Value, json};

fn convert(events: &[Value]) -> agentsight_atif::AtifTrajectory {
    let config = json!({"type":"runtime-config","entrypoint":"claude-code"});
    let content = std::iter::once(&config)
        .chain(events.iter())
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

fn record(id: Option<&str>, usage: Value) -> Value {
    json!({"type":"assistant","message":{"id":id,"usage":usage,
        "content":[{"type":"text","text":"measured answer"}]}})
}

#[test]
fn cached_input_reaches_local_cost_analysis() {
    let result = convert(&[record(
        Some("msg-1"),
        json!({"input_tokens":1000,
        "cache_read_input_tokens":200,"cache_creation_input_tokens":300,"output_tokens":50}),
    )]);
    let total = result.final_metrics.as_ref().unwrap();
    assert_eq!(total.total_prompt_tokens, Some(1500));
    assert_eq!(total.total_completion_tokens, Some(50));
    assert_eq!(total.total_cached_tokens, Some(200));
    let analyzed =
        agentsight_opt::AtifTrajectory::from_json(&serde_json::to_string(&result).unwrap())
            .unwrap();
    let cost = agentsight_opt::cost::compute_cost(&analyzed).unwrap();
    assert_eq!(cost.total_real_input_tok, 1500);
    assert_eq!(cost.total_real_output_tok, 50);
    assert_eq!(cost.usage_steps, 1);
}

#[test]
fn split_message_counts_only_latest_valid_snapshot() {
    for tail in [Value::Null, json!({}), json!({"service_tier":"standard"})] {
        let result = convert(&[
            record(Some("msg-1"), json!({"input_tokens":100,"output_tokens":1})),
            record(Some("msg-1"), json!({"input_tokens":100,"output_tokens":9})),
            record(Some("msg-1"), tail),
        ]);
        assert!(result.steps[0].metrics.is_none());
        assert_eq!(
            result.steps[1].metrics.as_ref().unwrap().completion_tokens,
            Some(9)
        );
        assert_eq!(result.final_metrics.unwrap().total_prompt_tokens, Some(100));
    }
}

#[test]
fn anonymous_and_distinct_messages_keep_independent_usage() {
    let result = convert(&[
        record(None, json!({"input_tokens":100})),
        record(None, json!({"input_tokens":20})),
        record(Some("a"), json!({"input_tokens":10})),
        record(Some("b"), json!({"input_tokens":5})),
    ]);
    assert_eq!(result.final_metrics.unwrap().total_prompt_tokens, Some(135));
}

#[test]
fn zeros_are_measured_and_invalid_values_are_unknown() {
    let result = convert(&[record(
        None,
        json!({"input_tokens":0,"output_tokens":0,"cache_read_input_tokens":0}),
    )]);
    let total = result.final_metrics.unwrap();
    assert_eq!(total.total_prompt_tokens, Some(0));
    assert_eq!(total.total_completion_tokens, Some(0));
    assert_eq!(total.total_cached_tokens, Some(0));
    let result = convert(&[record(
        None,
        json!({"input_tokens":true,"output_tokens":-1}),
    )]);
    assert!(result.steps[0].metrics.is_none());
    assert_eq!(result.final_metrics.unwrap().total_prompt_tokens, None);
}

#[test]
fn file_source_deduplicates_claude_but_keeps_qoder_partials() {
    let directory = tempfile::tempdir().unwrap();
    let events = [
        record(Some("same"), json!({"input_tokens":100})),
        record(Some("same"), json!({"input_tokens":40})),
    ];
    let content = events
        .iter()
        .map(|event| format!("{event}\n"))
        .collect::<String>();
    for (source, expected) in [(".claude", 40), (".qoder", 140)] {
        let path = directory.path().join(source).join("session.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &content).unwrap();
        let trajectory = converter::convert_jsonl_to_atif(&path).unwrap();
        assert_eq!(
            trajectory.final_metrics.unwrap().total_prompt_tokens,
            Some(expected)
        );
    }
    let trajectory = convert_jsonl_content_to_atif(&content).unwrap();
    assert_eq!(
        trajectory.final_metrics.unwrap().total_prompt_tokens,
        Some(140)
    );
}

#[test]
fn header_source_and_path_fallback_preserve_usage_and_metadata() {
    let directory = tempfile::tempdir().unwrap();
    for (source, entrypoint, expected) in [
        (".claude", None, 40),
        (".qoder", None, 140),
        (".claude", Some(json!(null)), 40),
        (".claude", Some(json!(false)), 40),
        (".claude", Some(json!("qoder")), 140),
        (".qoder", Some(json!("claude-code")), 40),
    ] {
        let mut header = json!({"type":"runtime-config","sessionId":"header-session",
            "model":"header-model","version":"fixture-version"});
        if let Some(entrypoint) = entrypoint {
            header["entrypoint"] = entrypoint;
        }
        // The ordinary parser already tolerates BOMs, blanks and broken lines.
        let content = format!(
            "\u{feff}\n  \nnot-json\n{header}\n{}\n{}\n",
            record(Some("same"), json!({"input_tokens":100})),
            record(Some("same"), json!({"input_tokens":40}))
        );
        let path = directory.path().join(source).join("session.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        let trajectory = converter::convert_jsonl_to_atif(&path).unwrap();
        assert_eq!(
            trajectory.final_metrics.unwrap().total_prompt_tokens,
            Some(expected)
        );
        assert_eq!(trajectory.session_id.as_deref(), Some("header-session"));
        assert_eq!(trajectory.agent.model_name.as_deref(), Some("header-model"));
        assert_eq!(trajectory.agent.version, "fixture-version");
        assert_eq!(trajectory.steps.len(), 2);
        assert_eq!(trajectory.steps[0].message, "measured answer");
    }
}
