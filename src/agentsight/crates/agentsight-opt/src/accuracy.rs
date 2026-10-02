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

    // One shared LLM extraction feeding all strategies (degrades to empty on failure).
    tracing::info!("[accuracy] Running shared extraction...");
    let shared = extract::shared_extract(client, &inv).await;

    // Run strategy orchestration.
    tracing::info!("[accuracy] Running strategy orchestration...");
    let issues = orchestrator::run_strategies(client, &inv, &shared, repo_root).await;
    tracing::info!("[accuracy] Found {} issues", issues.len());

    // `failures` preserved for backward compat with old rendering / stored sessions.
    Ok(AccuracyResult {
        extraction: ExtractionResult {
            final_answer: inv.final_answer,
        },
        failures: vec![],
        issues,
    })
}
