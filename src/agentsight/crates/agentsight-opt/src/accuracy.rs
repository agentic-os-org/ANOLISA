//! Accuracy analysis — per-strategy engine aligned with perf/cost.
//!
//! Pipeline:
//! 1. **Inventory** — `TraceInventory` parses tool calls, user turns, and a
//!    heuristic final answer (no LLM).
//! 2. **Shared extraction** — one LLM call extracts claims / assertions /
//!    checklist / ambiguity from final answer + user turns.
//! 3. **Strategies** — 5 strategies run in parallel (verify_before_done,
//!    requirement_check, confirm_before_act, fact_check, experience_library),
//!    each producing `RawIssue`s with explicit `EvidenceTier`.
//! 4. **Orchestration** — Merge, deduplicate, sort, apply rule-derived gates
//!    → `Vec<AccIssue>`.

mod detector;
mod extract;
mod orchestrator;
mod strategies;

use std::path::Path;

use anyhow::Result;

use crate::atif::AtifTrajectory;
use crate::llm::LlmClient;
use crate::trace;
use crate::types::{AccuracyResult, ExtractionResult};

/// Run full accuracy analysis: inventory + shared extraction + strategy orchestration.
///
/// `repo_root` enables the fact-check strategy (grep existence checks).
/// Pass `None` to skip fact-checking (e.g. when no repo context is available).
pub async fn analyze(
    client: &LlmClient,
    trajectory: &AtifTrajectory,
    repo_root: Option<&Path>,
) -> Result<AccuracyResult> {
    // Build shared trace inventory (heuristic final_answer, zero LLM).
    let inv = trace::build_inventory(trajectory);
    tracing::info!(
        "[accuracy] Inventory: {} tool calls, {} user turns, final answer {} chars",
        inv.tool_calls.len(),
        inv.user_turns.len(),
        inv.final_answer.len()
    );

    // One shared LLM extraction feeding all strategies (degrades to empty on
    // failure). Every LLM-backed judgment records its outcome in `judgments`.
    let judgments = detector::JudgmentLog::default();
    tracing::info!("[accuracy] Running shared extraction...");
    let shared = extract::shared_extract(client, &inv, &judgments).await;

    // Run strategy orchestration.
    tracing::info!("[accuracy] Running strategy orchestration...");
    let issues = orchestrator::run_strategies(client, &inv, &shared, repo_root, &judgments).await;
    tracing::info!("[accuracy] Found {} issues", issues.len());

    let tally = judgments.snapshot();
    // A run whose judgments all failed found nothing because nothing ran:
    // returning a clean report here would be persisted as "no accuracy
    // issues" (perf and cost error in the same case).
    if let Some(last_error) = tally.all_failed() {
        anyhow::bail!(
            "all {} accuracy judgments failed: {last_error}",
            tally.attempted
        );
    }

    // `failures` preserved for backward compat with old rendering / stored sessions.
    Ok(AccuracyResult {
        extraction: ExtractionResult {
            final_answer: inv.final_answer,
        },
        failures: vec![],
        failed: tally.failed,
        issues,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// When every accuracy judgment fails (endpoint down, bad key), the run
    /// must surface an error instead of returning a clean report: the optimize
    /// endpoint persists an Ok result as the accuracy dimension and renders it
    /// as "no issues", a false verdict that outlives the outage as stored data
    /// (the trap 3ef09abb4 closed for perf and 8c141e618 for cost).
    #[tokio::test]
    async fn all_failed_judgments_error_instead_of_a_clean_report() {
        let trajectory = AtifTrajectory::from_json(
            r#"{"schema_version":"ATIF-v1.6","session_id":"s1",
                "agent":{"name":"a","version":"1","model_name":"m"},
                "steps":[
                  {"step_id":1,"source":"user","timestamp":"2026-07-02T06:30:00.000Z","message":"fix the bug"},
                  {"step_id":2,"source":"agent","timestamp":"2026-07-02T06:30:20.000Z","message":"done"}
                ]}"#,
        )
        .unwrap();
        let client = LlmClient::with_config("http://127.0.0.1:1/v1", "key", "test-model");
        let result = analyze(&client, &trajectory, None).await;
        let err = result.expect_err(
            "the only judgment failed against a dead endpoint; expected Err, got a clean report",
        );
        assert!(
            err.to_string().contains("accuracy judgments failed"),
            "unexpected error: {err:#}"
        );
    }
}
