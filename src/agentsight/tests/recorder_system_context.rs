//! Request snapshots must preserve all system instructions, including removal.
use agentsight_opt::{
    AtifTrajectory,
    cost::compute_cost,
    llm::{ChatMessage, RecordParams, TrajectoryRecorder},
};

fn record(recorder: &TrajectoryRecorder, systems: &[usize], label: Option<&str>) {
    let mut messages: Vec<_> = systems
        .iter()
        .map(|size| ChatMessage::system("s".repeat(*size)))
        .collect();
    messages.push(ChatMessage::user(""));
    recorder.record(RecordParams {
        messages: &messages,
        response: "done",
        reasoning: None,
        model: "fixture",
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: 0,
        reasoning_tokens: 0,
        start_ts: "2026-11-13T00:00:00Z",
        end_ts: "2026-11-13T00:00:01Z",
        label,
    });
}

fn analyzed(recorder: &TrajectoryRecorder) -> Vec<usize> {
    let doc = recorder.to_atif();
    let mut parsed = AtifTrajectory::from_json(&serde_json::to_string(&doc).unwrap()).unwrap();
    // Inspect the recorded context independently of measured-usage calibration.
    for step in &mut parsed.steps {
        if let Some(metrics) = &mut step.metrics {
            metrics.prompt_tokens = None;
        }
    }
    compute_cost(&parsed)
        .unwrap()
        .calls
        .iter()
        .map(|call| call.system_prompt)
        .collect()
}

#[test]
fn every_system_block_reaches_cost_analysis() {
    let recorder = TrajectoryRecorder::new("fixture", "snapshot");
    record(&recorder, &[400, 800], None);
    assert_eq!(analyzed(&recorder), vec![300]);
}

#[test]
fn removing_the_prompt_clears_the_previous_snapshot() {
    let recorder = TrajectoryRecorder::new("fixture", "snapshot");
    record(&recorder, &[400], None);
    record(&recorder, &[], None);
    record(&recorder, &[800], None);
    assert_eq!(analyzed(&recorder), vec![100, 0, 200]);
}

#[test]
fn repeated_multiblock_requests_do_not_accumulate_context() {
    let recorder = TrajectoryRecorder::new("fixture", "snapshot");
    record(&recorder, &[400, 800], None);
    record(&recorder, &[400, 800], None);
    assert_eq!(analyzed(&recorder), vec![300, 300]);
}

#[test]
fn single_and_initially_absent_system_messages_keep_their_meaning() {
    let recorder = TrajectoryRecorder::new("fixture", "snapshot");
    record(&recorder, &[], None);
    record(&recorder, &[400], None);
    assert_eq!(analyzed(&recorder), vec![0, 100]);
}

#[test]
fn labeled_exports_keep_ordered_blocks_and_empty_replacements() {
    let recorder = TrajectoryRecorder::new("fixture", "snapshot");
    record(&recorder, &[400, 800], Some("cost:review"));
    record(&recorder, &[], Some("cost:review"));
    record(&recorder, &[200], Some("perf:review"));
    let doc = recorder.to_atif();
    let children = doc.subagent_trajectories.unwrap();
    let cost = children
        .iter()
        .find(|doc| doc.trajectory_id.as_deref() == Some("cost:review"))
        .unwrap();
    let systems: Vec<_> = cost
        .steps
        .iter()
        .filter(|step| step.source == agentsight_atif::StepSource::System)
        .collect();
    assert_eq!(
        systems
            .iter()
            .map(|step| step.message.len())
            .collect::<Vec<_>>(),
        [400, 800, 0]
    );
    assert_eq!(
        systems[0].extra.as_ref().unwrap()["system_prompt_snapshot"],
        true
    );
    assert!(systems[1].extra.is_none());
    assert_eq!(
        systems[2].extra.as_ref().unwrap()["system_prompt_snapshot"],
        true
    );
    assert!(
        cost.steps
            .windows(2)
            .all(|pair| pair[1].step_id == pair[0].step_id + 1)
    );
    assert_eq!(
        cost.final_metrics.as_ref().unwrap().total_steps,
        Some(cost.steps.len())
    );
}
