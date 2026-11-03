//! Captured fractional durations must survive the actual ATIF export pipeline.

use agentsight::atif::converter::convert_trace_to_atif;
use agentsight::storage::sqlite::genai::TraceEventDetail;
use agentsight_opt::{atif::AtifTrajectory, perf::compute_stats};

fn event(start: i64, end: i64) -> TraceEventDetail {
    TraceEventDetail {
        id: 1,
        call_id: Some("fixture-call".into()),
        start_timestamp_ns: start,
        end_timestamp_ns: Some(end),
        model: Some("fixture-model".into()),
        input_tokens: 10,
        output_tokens: 5,
        total_tokens: 15,
        input_messages: None,
        output_messages: None,
        system_instructions: None,
        agent_name: Some("fixture-agent".into()),
        process_name: None,
        pid: Some(1),
        user_query: Some("fixture question".into()),
        event_json: None,
        trace_id: Some("fixture-trace".into()),
        conversation_id: Some("fixture-conversation".into()),
        cache_read_tokens: None,
        status: Some("complete".into()),
        interruption_type: None,
    }
}

fn exported_stats(start: i64, end: i64) -> agentsight_opt::types::PerfStats {
    let exported = convert_trace_to_atif("fixture-trace", vec![event(start, end)]).unwrap();
    let json = serde_json::to_string(&exported).unwrap();
    let validated: agentsight_atif::AtifTrajectory = serde_json::from_str(&json).unwrap();
    validated.validate_step_ids().unwrap();
    let analyzed = AtifTrajectory::from_json(&json).unwrap();
    compute_stats(&analyzed).unwrap()
}

#[test]
fn fractional_call_preserves_its_measured_model_duration() {
    let start = 1_791_849_600_100_000_000;
    let stats = exported_stats(start, start + 300_000_000);
    assert!(
        (stats.model_secs - 0.3).abs() < 1e-9,
        "captured 0.3 s, reported {} s",
        stats.model_secs
    );
    assert!(
        (stats.wall_secs - 0.3).abs() < 1e-9,
        "captured 0.3 s, reported {} s",
        stats.wall_secs
    );
}

#[test]
fn fractional_call_crossing_a_second_does_not_become_one_second() {
    let start = 1_791_849_600_900_000_000;
    let stats = exported_stats(start, start + 300_000_000);
    assert!(
        (stats.model_secs - 0.3).abs() < 1e-9,
        "captured 0.3 s, reported {} s",
        stats.model_secs
    );
    assert!(
        (stats.wall_secs - 0.3).abs() < 1e-9,
        "captured 0.3 s, reported {} s",
        stats.wall_secs
    );
}

#[test]
fn exported_timestamps_round_trip_the_captured_nanoseconds() {
    let start = 1_791_849_600_100_000_123;
    let end = start + 300_000_456;
    let exported = convert_trace_to_atif("fixture-trace", vec![event(start, end)]).unwrap();
    let agent = exported
        .steps
        .iter()
        .find(|step| step.source == agentsight_atif::StepSource::Agent)
        .unwrap();
    let parsed_end = agent
        .timestamp
        .as_ref()
        .unwrap()
        .parse::<chrono::DateTime<chrono::Utc>>()
        .unwrap();
    let start_text = agent.extra.as_ref().unwrap()["start_timestamp"]
        .as_str()
        .unwrap();
    let parsed_start = start_text.parse::<chrono::DateTime<chrono::Utc>>().unwrap();
    assert_eq!(parsed_start.timestamp_nanos_opt(), Some(start));
    assert_eq!(parsed_end.timestamp_nanos_opt(), Some(end));
}

#[test]
fn whole_second_timestamps_keep_the_existing_export_shape() {
    let start = 1_791_849_600_000_000_000;
    let exported =
        convert_trace_to_atif("fixture-trace", vec![event(start, start + 1_000_000_000)]).unwrap();
    for step in &exported.steps {
        let timestamp = step.timestamp.as_ref().unwrap();
        assert!(timestamp.ends_with('Z'));
        assert!(!timestamp.contains('.'));
    }
    let stats = exported_stats(start, start + 1_000_000_000);
    assert_eq!(stats.model_secs, 1.0);
    assert_eq!(stats.wall_secs, 1.0);
}
