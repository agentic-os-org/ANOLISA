use agentsight::reuse::label::TrajectoryLabel;
use agentsight::reuse::triage::{GroundingSummary, TriageConfig, triage};
use agentsight_trajectory_collector::atif::convert_qoder_events;
use serde_json::json;

#[test]
fn a_collected_single_tool_turn_is_not_empty() {
    for failed in [false, true] {
        let doc = convert_qoder_events(&[
            json!({"type":"assistant","message":{"content":[
                {"type":"tool_use","id":"read-1","name":"Read","input":{"path":"fixture.txt"}}
            ]}}),
            json!({"type":"user","message":{"content":[
                {"type":"tool_result","tool_use_id":"read-1","content":"fixture result","is_error":failed}
            ]}}),
        ], "qoder").unwrap();
        assert_eq!(doc.steps.len(), 1);
        assert_eq!(doc.steps[0].observation.as_ref().unwrap().results.len(), 1);
        let grounding = GroundingSummary {
            all_calls_ok: !failed,
            has_persisted_failure: failed,
            ..GroundingSummary::default()
        };
        let outcome = triage(&doc, &grounding, &TriageConfig::default());
        assert_eq!(outcome.metrics.n_tool_calls, 1);
        assert_eq!(
            outcome.label,
            if failed {
                TrajectoryLabel::Unknown
            } else {
                TrajectoryLabel::Good
            }
        );
    }
}

#[test]
fn a_single_substantive_answer_keeps_its_content_threshold() {
    for (length, expected) in [
        (0, TrajectoryLabel::Useless),
        (1999, TrajectoryLabel::Useless),
        (2000, TrajectoryLabel::Good),
    ] {
        let doc = convert_qoder_events(&[
            json!({"type":"assistant","message":{"content":[{"type":"text","text":"x".repeat(length)}]}}),
        ], "qoder").unwrap();
        let outcome = triage(
            &doc,
            &GroundingSummary {
                all_calls_ok: true,
                ..GroundingSummary::default()
            },
            &TriageConfig::default(),
        );
        assert_eq!(outcome.label, expected, "answer length {length}");
    }
}
