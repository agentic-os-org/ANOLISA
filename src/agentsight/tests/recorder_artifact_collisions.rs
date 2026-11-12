//! Independent analysis runs must never replace one another's audit artifacts.
use agentsight_opt::llm::{ChatMessage, RecordParams, TrajectoryRecorder};
use std::{
    collections::HashSet,
    sync::{Arc, Barrier},
};

fn recorder(label: &str, response: &str) -> TrajectoryRecorder {
    let recorder = TrajectoryRecorder::new("fixture", label);
    recorder.record(RecordParams {
        messages: &[ChatMessage::user("Inspect fixture")],
        response,
        reasoning: None,
        model: "fixture",
        input_tokens: 1,
        output_tokens: 1,
        cached_tokens: 0,
        reasoning_tokens: 0,
        start_ts: "2026-11-08T00:00:00Z",
        end_ts: "2026-11-08T00:00:01Z",
        label: None,
    });
    recorder
}

#[test]
fn simultaneous_runs_retain_every_complete_artifact() {
    let directory = tempfile::tempdir().unwrap();
    let start = Arc::new(Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|index| {
            let directory = directory.path().to_path_buf();
            let start = start.clone();
            std::thread::spawn(move || {
                let answer = format!("run {index}");
                let recorder = recorder("same/session", &answer);
                start.wait();
                (recorder.save_to_dir(&directory).unwrap(), answer)
            })
        })
        .collect();
    let artifacts: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    let paths: HashSet<_> = artifacts.iter().map(|(path, _)| path).collect();
    assert_eq!(
        paths.len(),
        8,
        "same-label runs must receive distinct paths"
    );
    for (path, answer) in artifacts {
        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(document["steps"][1]["message"], answer);
        assert!(
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .ends_with(".atif.json")
        );
    }
}

#[test]
fn labels_that_sanitize_to_the_same_name_stay_separate() {
    let directory = tempfile::tempdir().unwrap();
    let now = chrono::Utc::now();
    for offset in [0, 1] {
        let ts = (now + chrono::Duration::seconds(offset)).format("%Y%m%d_%H%M%S");
        for seq in 0..16 {
            let existing = directory.path().join(format!(
                "case_a_{ts}_{}_{seq}.atif.json",
                std::process::id()
            ));
            std::fs::write(existing, "preexisting").unwrap();
        }
    }
    let first = recorder("case/a", "first")
        .save_to_dir(directory.path())
        .unwrap();
    let second = recorder("case:a", "second")
        .save_to_dir(directory.path())
        .unwrap();
    assert_ne!(first, second);
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(first).unwrap()).unwrap();
    assert_eq!(document["steps"][1]["message"], "first");
}

#[test]
fn invalid_destination_returns_an_error_without_changing_existing_data() {
    let directory = tempfile::tempdir().unwrap();
    let occupied = directory.path().join("occupied");
    std::fs::write(&occupied, "existing payload").unwrap();
    assert!(
        recorder("fixture", "new payload")
            .save_to_dir(&occupied)
            .is_err()
    );
    assert!(
        recorder(&"a".repeat(300), "new payload")
            .save_to_dir(directory.path())
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(&occupied).unwrap(),
        "existing payload"
    );
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn nested_destination_is_created_and_saved_trajectory_round_trips() {
    let directory = tempfile::tempdir().unwrap();
    let nested = directory.path().join("analyses/performance");
    let path = recorder("project/验证", "complete answer")
        .save_to_dir(&nested)
        .unwrap();
    assert_eq!(path.parent(), Some(nested.as_path()));
    let document =
        agentsight_opt::AtifTrajectory::from_json(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(document.session_id, "project/验证");
    assert_eq!(
        document.steps.last().unwrap().message.as_deref(),
        Some("complete answer")
    );
}
