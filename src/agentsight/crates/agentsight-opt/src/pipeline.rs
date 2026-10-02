//! Thin orchestration facade over the per-dimension analysis modules.

use std::path::Path;

use anyhow::Result;

use crate::atif::AtifTrajectory;
use crate::llm::LlmClient;
use crate::summary::TrajectorySummary;
use crate::types::{AccuracyResult, AnalysisReport, CostStats, PerfReport, PerfStats, WasteReport};
use crate::{accuracy, cost, perf};

/// Unified entry point for trajectory analysis across all three dimensions.
pub struct AnalyzePipeline<'a> {
    client: &'a LlmClient,
}

impl<'a> AnalyzePipeline<'a> {
    pub fn new(client: &'a LlmClient) -> Self {
        Self { client }
    }

    /// Full analysis: pure-compute perf/cost plus LLM-backed accuracy.
    pub async fn run(
        &self,
        trajectory: &AtifTrajectory,
        repo_root: Option<&Path>,
    ) -> Result<AnalysisReport> {
        let perf = Self::run_perf(trajectory).ok();
        let cost = Self::run_cost(trajectory).ok();
        let accuracy = self.run_accuracy(trajectory, repo_root).await?;
        Ok(AnalysisReport {
            extraction: accuracy.extraction,
            failures: accuracy.failures,
            perf,
            cost,
        })
    }

    /// Performance analysis: pure computation, millisecond-scale.
    pub fn run_perf(trajectory: &AtifTrajectory) -> Result<PerfStats> {
        perf::compute_stats(trajectory)
    }

    /// Cost analysis: pure computation, millisecond-scale.
    pub fn run_cost(trajectory: &AtifTrajectory) -> Result<CostStats> {
        cost::compute_cost(trajectory)
    }

    /// Performance bottleneck identification: Rust candidates + LLM judgment, 10-30s.
    pub async fn run_perf_issues(&self, trajectory: &AtifTrajectory) -> Result<PerfReport> {
        perf::llm::identify_issues(self.client, trajectory).await
    }

    /// Cost waste identification: Rust candidates + LLM judgment, 10-30s.
    pub async fn run_cost_waste(&self, trajectory: &AtifTrajectory) -> Result<WasteReport> {
        cost::llm::identify_waste(self.client, trajectory).await
    }

    /// Narrative summary (goal / process / outcome): one LLM call, a few seconds.
    pub async fn run_summary(&self, trajectory: &AtifTrajectory) -> Result<TrajectorySummary> {
        crate::summary::summarize(self.client, trajectory).await
    }

    /// Accuracy analysis (extraction + orthogonal attribution): LLM-backed, 30-60s.
    ///
    /// `repo_root` enables the fact-check detector (grep existence checks).
    pub async fn run_accuracy(
        &self,
        trajectory: &AtifTrajectory,
        repo_root: Option<&Path>,
    ) -> Result<AccuracyResult> {
        accuracy::analyze(self.client, trajectory, repo_root).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_trajectory() -> AtifTrajectory {
        AtifTrajectory::from_json(
            r#"{
              "schema_version": "ATIF-v1.6",
              "session_id": "empty",
              "agent": {"name": "test", "model_name": "test-model"},
              "steps": []
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn run_pure_dimensions_on_empty_trajectory() {
        let trajectory = empty_trajectory();

        let perf = AnalyzePipeline::run_perf(&trajectory).unwrap();
        assert_eq!(perf.tool_count, 0);
        assert_eq!(perf.wall_secs, 0.0);
        assert_eq!(perf.tool_secs, 0.0);

        let cost = AnalyzePipeline::run_cost(&trajectory).unwrap();
        assert_eq!(cost.total_events, 0);
        assert!(cost.breakdown.is_empty());
        assert!(cost.calls.is_empty());
    }

    #[test]
    fn constructs_pipeline_with_client() {
        let client = LlmClient::with_config("http://localhost/v1", "test-key", "test-model");
        let _pipeline = AnalyzePipeline::new(&client);
    }
}
