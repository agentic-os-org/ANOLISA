use agentsight::preferences::detector::{PreferenceEventRow, detect_signals};
use agentsight::preferences::signals::SignalKind;
use agentsight::preferences::trajectory_source::rows_from_atif_json;
use agentsight::preferences::{aggregator::confidence_for_evidence, analyze_rows};
use agentsight_trajectory_collector::atif::convert_qoder_events;
use serde_json::json;

#[test]
fn repeated_human_requests_are_distinct_evidence_not_tool_loop_replays() {
    let mut events = Vec::new();
    for turn in 0..3 {
        events.push(json!({"type":"user","timestamp":format!("2026-11-01T10:0{turn}:00Z"),"message":{"content":"please run tests before finishing"}}));
        for call in 0..2 {
            events.push(json!({"type":"assistant","message":{"content":[{
                "type":"tool_use","id":format!("{turn}-{call}"),"name":"Bash","input":{"command":"cargo test"}
            }]}}));
        }
    }
    let trajectory = convert_qoder_events(&events, "qoder").unwrap();
    let mut next_id = 1;
    let rows = rows_from_atif_json(
        "fixture",
        &trajectory.to_json_value().unwrap().to_string(),
        &mut next_id,
    );
    assert_eq!(rows.len(), 3);
    let signals = detect_signals(&rows);
    assert_eq!(
        signals
            .iter()
            .filter(|s| matches!(s.kind, SignalKind::TestRequirement))
            .count(),
        3
    );
    assert_eq!(
        signals
            .iter()
            .filter(|s| matches!(s.kind, SignalKind::ToolUsage { .. }))
            .count(),
        6
    );
    let preferences = analyze_rows(&rows);
    let testing = preferences.iter().find(|p| p.key == "testing").unwrap();
    assert_eq!(testing.evidence_count, 3);
    assert_eq!(testing.confidence, confidence_for_evidence(3));
    // Duplicate genai captures for one identified turn still count once.
    let row = PreferenceEventRow {
        id: 1,
        session_id: Some("genai".into()),
        conversation_id: Some("one-turn".into()),
        timestamp_ns: Some(1),
        user_text: Some("please run tests".into()),
        tool_names: vec![],
    };
    assert_eq!(
        detect_signals(&[row.clone(), row])
            .iter()
            .filter(|s| matches!(s.kind, SignalKind::TestRequirement))
            .count(),
        1
    );
}

#[test]
fn distinct_turn_ids_keep_cross_turn_correction_grouping() {
    let raw = json!({"agent":{"name":"fixture"},"steps":[
        {"step_id":1,"source":"user","timestamp":"2026-11-01T10:00:00Z","message":"inspect the implementation"},
        {"step_id":2,"source":"user","timestamp":"2026-11-01T10:00:10Z","message":"focus on the parser"}
    ]});
    let rows = rows_from_atif_json("session", &raw.to_string(), &mut 1);
    assert_eq!(
        detect_signals(&rows)
            .iter()
            .filter(|s| matches!(s.kind, SignalKind::Correction))
            .count(),
        1
    );
}
