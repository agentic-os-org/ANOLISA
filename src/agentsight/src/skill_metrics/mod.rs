//! Skill Metrics module
//!
//! Extracts and computes skill-related metrics from stored GenAI events.
//! Provides 9 metrics covering skill downloads, loads, usage patterns,
//! co-occurrence, and cross-agent overlap.
//!
//! # Architecture
//!
//! ```text
//! GenAISqliteStore (genai_events)
//!        |
//!        v
//!   query.rs  (time-range fetch)
//!        |
//!        v
//!   extractor.rs  (parse system_instructions / filesystem scan + output_messages)
//!        |
//!        v
//!   metrics.rs  (compute 9 metrics)
//!        |
//!        v
//!   types.rs  (SkillMetricsReport)
//! ```

pub mod extractor;
pub mod metrics;
pub mod query;
pub mod types;

pub use metrics::compute_skill_metrics;
pub use types::{HotnessGranularity, MetricOptions, SkillMetricsReport};
