//! Skill Metrics data types
//!
//! Defines all input, intermediate, and output structures for Skill metrics computation.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── Input / Intermediate Types ──────────────────────────────────────────────

/// A single observation of a skill being present in `<available_skills>` XML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDownloadRecord {
    pub skill_name: String,
    pub session_id: String,
    pub timestamp_ns: i64,
}

/// A single observation of a SKILL.md file being read via tool_call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillLoadRecord {
    pub skill_name: String,
    pub session_id: String,
    pub call_id: String,
    pub timestamp_ns: i64,
    pub agent_name: Option<String>,
    pub function_name: String,
}

// ─── Metric 1: Skill Download Count ─────────────────────────────────────────

/// First-seen record for a downloaded skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillFirstSeen {
    pub first_seen_session_id: String,
    pub first_seen_timestamp_ns: i64,
    /// Number of distinct sessions where this skill appeared in available_skills.
    pub total_sessions: u64,
}

/// Metric 1 output: per-skill download tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDownloadMetrics {
    pub downloads: HashMap<String, SkillFirstSeen>,
}

// ─── Metric 2: Skill Load Count ─────────────────────────────────────────────

/// Metric 2 output: per-skill load count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillLoadMetrics {
    pub loads: HashMap<String, u64>,
    pub total_loads: u64,
}

// ─── Metric 3: Skill Usage Ratio ────────────────────────────────────────────

/// Metric 3 output: ratio of tasks with at least one skill load.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillUsageRatio {
    pub ratio: f64,
    pub with_skill_count: u64,
    pub without_skill_count: u64,
    pub total_sessions: u64,
}

// ─── Metric 4: Per-task Skill Count Distribution ────────────────────────────

/// Metric 4 output: distribution of distinct skill count per task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillCountDistribution {
    pub min: u32,
    pub max: u32,
    pub mean: f64,
    pub median: f64,
    pub p90: f64,
    /// Histogram buckets: [0, 1, 2, 3, 4, 5+]
    pub histogram: [u64; 6],
}

// ─── Metric 5: Skill Hotness Ranking ────────────────────────────────────────

/// Weekly rank entry for a skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeeklyRank {
    pub iso_week: String,
    pub load_count: u64,
    pub rank: u32,
}

/// Per-skill ranking entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillRankEntry {
    pub skill_name: String,
    pub total_loads: u64,
    pub total_rank: u32,
    pub weekly_ranks: Vec<WeeklyRank>,
    pub rank_delta: Option<i32>,
}

/// Metric 5 output: hotness ranking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillHotnessRanking {
    pub rankings: Vec<SkillRankEntry>,
}

// ─── Top-level Report ────────────────────────────────────────────────────────

/// Full skill metrics report combining all computed metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMetricsReport {
    pub downloads: Option<SkillDownloadMetrics>,
    pub loads: Option<SkillLoadMetrics>,
    pub usage_ratio: Option<SkillUsageRatio>,
    pub distribution: Option<SkillCountDistribution>,
    pub hotness: Option<SkillHotnessRanking>,
    pub computed_at: String,
    pub time_range_ns: (i64, i64),
    pub event_count: u64,
}

/// Granularity for hotness trend calculation.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum HotnessGranularity {
    Day,
    #[default]
    Week,
}

/// Options controlling which metrics to compute.
#[derive(Debug, Clone, Default)]
pub struct MetricOptions {
    pub downloads: bool,
    pub loads: bool,
    pub usage_ratio: bool,
    pub distribution: bool,
    pub hotness: bool,
    /// Granularity for hotness trend: day or week.
    pub hotness_granularity: HotnessGranularity,
}

impl MetricOptions {
    /// All metrics enabled.
    pub fn all() -> Self {
        Self {
            downloads: true,
            loads: true,
            usage_ratio: true,
            distribution: true,
            hotness: true,
            hotness_granularity: HotnessGranularity::Week,
        }
    }
}
