//! Verification decisions must use full execution arguments, not UI previews.
use crate::accuracy::{
    detector::{AnalysisCtx, Detector, JudgmentLog},
    extract::{CompletionClaim, SharedExtraction},
    strategies::verify_before_done::VerifyBeforeDoneStrategy,
};
use crate::llm::LlmClient;
use crate::{trace::build_inventory, AtifTrajectory};
use serde_json::{json, Value};

async fn lacks_verification(name: &str, arguments: Value) -> bool {
    let document = json!({"schema_version":"ATIF-v1.7", "session_id":"fixture",
        "steps":[{"step_id":1,"source":"agent","message":"All done.",
            "tool_calls":[{"tool_call_id":"call","function_name":name,"arguments":arguments}]}]});
    let trajectory = AtifTrajectory::from_json(&document.to_string()).unwrap();
    let inv = build_inventory(&trajectory);
    assert!(inv.tool_calls[0].cmd.chars().count() <= 51);
    let extraction = SharedExtraction {
        claims: vec![CompletionClaim {
            claim: "All done.".into(),
            kind: "done".into(),
        }],
        ..Default::default()
    };
    let client = LlmClient::with_config("http://127.0.0.1:1", "unused", "fixture");
    let judgments = JudgmentLog::default();
    let ctx = AnalysisCtx {
        inv: &inv,
        client: &client,
        repo_root: None,
        extraction: &extraction,
        judgments: &judgments,
    };
    !VerifyBeforeDoneStrategy::new()
        .detect(&ctx)
        .await
        .is_empty()
}

#[tokio::test]
async fn a_long_working_directory_does_not_hide_the_test_command() {
    let command = format!(
        "cd /workspace/{}/project && cargo test",
        "nested/".repeat(20)
    );
    assert!(!lacks_verification("Bash", json!({"command":command})).await);
}

#[tokio::test]
async fn a_truncated_todo_is_not_an_executed_test() {
    assert!(
        lacks_verification(
            "TodoWrite",
            json!({"todos":[{"content":format!("run tests {}", "later ".repeat(20))}]})
        )
        .await
    );
}

#[tokio::test]
async fn argv_commands_remain_verification_evidence() {
    assert!(!lacks_verification("shell", json!({"command":["cargo","test"]})).await);
}

#[tokio::test]
async fn a_short_real_command_and_a_read_remain_distinct() {
    assert!(!lacks_verification("Bash", json!({"command":"cargo test"})).await);
    assert!(lacks_verification("Read", json!({"file_path":"src/tests.rs"})).await);
}
