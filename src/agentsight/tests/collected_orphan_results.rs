//! Partial transcripts retain result evidence without inventing a call owner.
#[allow(clippy::collapsible_if)] // Compile the existing desktop-only fallback on Linux.
#[path = "../src/local/collector/converter.rs"]
mod converter;

use agentsight_opt::{AnalyzePipeline, AtifTrajectory};
use serde_json::{Value, json};

fn event(kind: &str, content: Value) -> Value {
    json!({"type":kind,"timestamp":"2026-11-08T00:00:01Z",
        "sessionId":"partial","message":{"content":content}})
}

fn result(id: &str) -> Value {
    json!({"type":"tool_result","tool_use_id":id,"content":"retained evidence","is_error":true})
}

fn documents(events: &[Value]) -> [Value; 2] {
    let raw = events
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    [
        serde_json::to_value(
            agentsight_trajectory_collector::atif::convert_qoder_events(events, "claude-code")
                .unwrap(),
        )
        .unwrap(),
        serde_json::to_value(converter::convert_jsonl_content_to_atif(&raw).unwrap()).unwrap(),
    ]
}

fn assert_retained(document: &Value, text: &str, id: &str) {
    let steps = document["steps"].as_array().unwrap();
    let replay = steps.last().unwrap();
    assert_eq!(replay["source"], "user");
    assert_eq!(replay["message"], text);
    assert_eq!(replay["observation"]["results"][0]["source_call_id"], id);
    assert_eq!(
        replay["observation"]["results"][0]["content"],
        "retained evidence"
    );
    assert_eq!(
        replay["observation"]["results"][0]["extra"]["is_error"],
        true
    );
    let parsed = AtifTrajectory::from_json(&document.to_string()).unwrap();
    let cost = AnalyzePipeline::run_cost(&parsed).unwrap();
    assert!(
        cost.breakdown
            .iter()
            .any(|segment| segment.label == "工具返回" && segment.chars == 17)
    );
}

#[test]
fn result_only_partial_transcript_survives_both_readers() {
    for document in documents(&[event("user", json!([result("missing")]))]) {
        assert_orphan_metadata(&document, "missing");
        assert!(document["steps"].as_array().unwrap().is_empty());
        let parsed = AtifTrajectory::from_json(&document.to_string()).unwrap();
        assert!(
            agentsight_opt::trace::build_inventory(&parsed)
                .tool_calls
                .is_empty()
        );
    }
}

#[test]
fn mixed_text_and_unowned_result_keep_both_payloads() {
    for document in documents(&[event(
        "user",
        json!([
            result("missing"), {"type":"text","text":"continue"}
        ]),
    )]) {
        assert_retained(&document, "continue", "missing");
    }
}

#[test]
fn unmatched_tail_does_not_attach_to_an_unrelated_call() {
    for document in documents(&[
        event(
            "assistant",
            json!([{"type":"tool_use","id":"other","name":"Bash","input":{}}]),
        ),
        event("user", json!([result("missing")])),
    ]) {
        assert_orphan_metadata(&document, "missing");
        assert_eq!(document["steps"].as_array().unwrap().len(), 1);
        assert!(document["steps"][0]["observation"].is_null());
    }
}

fn assert_orphan_metadata(document: &Value, id: &str) {
    let group = &document["extra"]["unmatched_tool_results"][0];
    assert_eq!(group["timestamp"], "2026-11-08T00:00:01Z");
    assert_eq!(group["results"][0]["source_call_id"], id);
    assert_eq!(group["results"][0]["content"], "retained evidence");
    assert_eq!(group["results"][0]["extra"]["is_error"], true);
}

#[test]
fn owned_and_unowned_results_remain_separate_without_a_fake_turn() {
    for document in documents(&[
        event(
            "assistant",
            json!([{"type":"tool_use","id":"owned","name":"Bash","input":{}}]),
        ),
        event("user", json!([result("owned"), result("missing")])),
    ]) {
        assert_eq!(document["steps"].as_array().unwrap().len(), 1);
        assert_eq!(
            document["steps"][0]["observation"]["results"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            document["steps"][0]["observation"]["results"][0]["source_call_id"],
            "owned"
        );
        assert_orphan_metadata(&document, "missing");
    }
}

#[test]
fn orphan_result_does_not_erase_the_latest_final_answer() {
    for document in documents(&[
        event("user", json!("Review the project")),
        event(
            "assistant",
            json!([{"type":"text","text":"Review complete"}]),
        ),
        event("user", json!([result("missing")])),
    ]) {
        let trajectory = AtifTrajectory::from_json(&document.to_string()).unwrap();
        assert_eq!(
            agentsight_opt::trace::build_inventory(&trajectory).final_answer,
            "Review complete"
        );
        assert_eq!(document["steps"].as_array().unwrap().len(), 2);
        assert_orphan_metadata(&document, "missing");
    }
}

#[test]
fn collection_persists_a_result_only_excerpt_without_fabricated_steps() {
    use agentsight_trajectory_collector::{
        CollectorConfig, TrajectoryMaintenancePolicy, TrajectoryStore, scan_once,
    };
    let directory = tempfile::tempdir().unwrap();
    let projects = directory.path().join(".claude/projects");
    let project = projects.join("fixture");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("11111111-2222-3333-4444-555555555555.jsonl"),
        event("user", json!([result("missing")])).to_string(),
    )
    .unwrap();
    let store = TrajectoryStore::new_with_path(&directory.path().join("collection.db")).unwrap();
    let config = CollectorConfig {
        scan_interval_secs: 1,
        scan_dirs: Some(vec![projects]),
        maintenance: TrajectoryMaintenancePolicy::default(),
    };
    scan_once(&store, &config);
    let record = store
        .get("11111111-2222-3333-4444-555555555555")
        .unwrap()
        .expect("retain the excerpt's evidence");
    let document: Value = serde_json::from_str(&record.atif_json).unwrap();
    assert_orphan_metadata(&document, "missing");
    assert_eq!(record.num_steps, 0);
    assert!(record.first_user_message.is_none());
    scan_once(&store, &config);
    assert_eq!(store.count().unwrap(), 1);
}
