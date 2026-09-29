//! Trajectory-driven agent optimization analysis (accuracy / performance / cost).
//!
//! Migrated from the standalone `agentopt` project. Each dimension has a
//! pure-compute layer (millisecond-scale) and an LLM judgment layer:
//! - `perf`     → [`perf::compute_stats`] + [`perf::llm::identify_issues`]
//! - `cost`     → [`cost::compute_cost`] + [`cost::llm::identify_waste`]
//! - `accuracy` → [`accuracy::analyze`]
//!
//! [`summary::summarize`] is a separate, single-call narrative digest (no
//! pure-compute layer) used by the Dashboard header.
//!
//! [`pipeline::AnalyzePipeline`] is the unified facade for callers.

pub mod accuracy;
pub mod atif;
pub mod cost;
pub mod llm;
pub mod perf;
pub mod pipeline;
pub mod preference;
pub mod summary;
pub mod trace;
pub mod types;

pub use atif::AtifTrajectory;
pub use llm::{LlmClient, TrajectoryRecorder};
pub use pipeline::AnalyzePipeline;
pub use summary::TrajectorySummary;
