//! Preference taxonomy types and the centralized rule/threshold tables.
//!
//! Every matching rule (keyword lists, numeric thresholds, exclusivity)
//! lives here so tuning a rule never requires touching detection or
//! aggregation logic.

use serde::Serialize;

// ─── Rule tables and thresholds ─────────────────────────────────────────────

/// Minimum alphabetic/CJK character count in a user query before language
/// classification is attempted — shorter texts are statistically meaningless.
pub const MIN_QUERY_CHARS_FOR_LANGUAGE: usize = 8;

/// Fraction of CJK characters (over CJK + ASCII letters) above which a query
/// is classified as Chinese. Well below 0.5 because Chinese developer queries
/// are typically interleaved with English identifiers and paths.
pub const CJK_RATIO_THRESHOLD: f64 = 0.3;

/// Two user queries in the same conversation closer than this are treated as
/// a rapid follow-up, i.e. the user overriding/correcting the previous turn.
pub const CORRECTION_WINDOW_NS: i64 = 60 * 1_000_000_000;

/// Max characters kept in an evidence excerpt — enough context to recognize
/// the quote without shipping multi-KB prompts through the API.
pub const EXCERPT_MAX_CHARS: usize = 120;

/// "Plan before coding" phrasings (matched case-insensitively as substrings).
pub const PLAN_FIRST_PATTERNS: &[&str] = &[
    "先计划",
    "先规划",
    "先给出方案",
    "先出方案",
    "先设计",
    "别急着写代码",
    "先不要写代码",
    "不要直接写代码",
    "plan first",
    "make a plan",
    "plan before",
    "before writing code",
    "don't code yet",
    "do not code yet",
    "don't write code yet",
    "do not write code yet",
];

/// "Run/require tests" phrasings (matched case-insensitively as substrings).
pub const TEST_REQUIREMENT_PATTERNS: &[&str] = &[
    "跑测试",
    "跑一下测试",
    "运行测试",
    "执行测试",
    "写测试",
    "加测试",
    "补测试",
    "加单测",
    "单元测试",
    "run the tests",
    "run tests",
    "run unit tests",
    "add tests",
    "write tests",
    "add a test",
    "unit test",
    "make test",
    "cargo test",
    "npm test",
    "pytest",
];

/// Phrasings that signal the user is correcting/overriding the assistant
/// (matched case-insensitively as substrings).
pub const CORRECTION_PATTERNS: &[&str] = &[
    "不对",
    "不是这样",
    "不是这个意思",
    "重新来",
    "重新做",
    "改回去",
    "撤销",
    "别这样",
    "that's wrong",
    "that is wrong",
    "not what i meant",
    "not what i asked",
    "undo that",
    "revert that",
    "start over",
    "redo this",
];

/// Preference keys whose values are mutually exclusive: two strongly
/// evidenced values for the same key mean a genuine conflict rather than
/// coexisting habits (e.g. a user speaks either Chinese or English per key,
/// but may legitimately prefer several tools).
pub const EXCLUSIVE_KEYS: &[&str] = &["language"];

/// True when two different values under `key` cannot both hold.
pub fn is_exclusive_key(key: &str) -> bool {
    EXCLUSIVE_KEYS.contains(&key)
}

/// Trim and cap a piece of evidence text at [`EXCERPT_MAX_CHARS`] characters
/// (char-boundary safe; an ellipsis marks the cut).
pub fn truncate_excerpt(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= EXCERPT_MAX_CHARS {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(EXCERPT_MAX_CHARS).collect();
    out.push('…');
    out
}

// ─── Taxonomy types ──────────────────────────────────────────────────────────

/// Coarse preference grouping used by the API and the Markdown export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PreferenceCategory {
    Communication,
    Workflow,
    Technical,
}

impl PreferenceCategory {
    /// Stable lowercase name (the API/export contract).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Communication => "communication",
            Self::Workflow => "workflow",
            Self::Technical => "technical",
        }
    }

    /// Parse the lowercase name back; used to validate LLM-produced rows.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "communication" => Some(Self::Communication),
            "workflow" => Some(Self::Workflow),
            "technical" => Some(Self::Technical),
            _ => None,
        }
    }
}

/// Which analysis layer produced a preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PreferenceSource {
    Rule,
    Llm,
}

/// Lifecycle of a preference within one analysis window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PreferenceStatus {
    Active,
    Conflicting,
}

/// What kind of observation a signal is.
///
/// Each variant maps to a stable (category, key, value) tuple via the
/// accessor methods below; those strings are the API contract.
#[derive(Debug, Clone, PartialEq)]
pub enum SignalKind {
    /// Dominant natural language of the user query ("chinese" / "english").
    Language { language: &'static str },
    /// The user asked for a plan/design before any code is written.
    PlanFirst,
    /// The user asked to run or add tests.
    TestRequirement,
    /// The user corrected/overrode the assistant (keyword or rapid re-send).
    Correction,
    /// A tool invoked by the model; frequency accumulates into a preference.
    ToolUsage { tool_name: String },
}

impl SignalKind {
    /// Category the signal aggregates under.
    pub fn category(&self) -> PreferenceCategory {
        match self {
            Self::Language { .. } => PreferenceCategory::Communication,
            Self::PlanFirst | Self::Correction => PreferenceCategory::Workflow,
            Self::TestRequirement | Self::ToolUsage { .. } => PreferenceCategory::Technical,
        }
    }

    /// Stable preference key within the category.
    pub fn key(&self) -> &'static str {
        match self {
            Self::Language { .. } => "language",
            Self::PlanFirst => "workflow_style",
            Self::TestRequirement => "testing",
            Self::Correction => "interaction_style",
            Self::ToolUsage { .. } => "preferred_tool",
        }
    }

    /// Observed value for the key.
    pub fn value(&self) -> String {
        match self {
            Self::Language { language } => (*language).to_string(),
            Self::PlanFirst => "plan_first".to_string(),
            Self::TestRequirement => "require_tests".to_string(),
            Self::Correction => "frequent_corrections".to_string(),
            Self::ToolUsage { tool_name } => tool_name.clone(),
        }
    }
}

/// One preference observation extracted from a single analysis turn,
/// carrying the provenance and evidence excerpt shown to the user.
#[derive(Debug, Clone)]
pub struct PreferenceSignal {
    pub kind: SignalKind,
    /// Id of the unified row the signal came from (see
    /// `detector::PreferenceEventRow::id`).
    pub event_id: i64,
    pub session_id: Option<String>,
    /// Turn timestamp in epoch nanoseconds; `None` when the source row had
    /// no reliable value.
    pub timestamp_ns: Option<i64>,
    /// Short quote of the triggering text (capped at [`EXCERPT_MAX_CHARS`]).
    pub excerpt: String,
}

/// One aggregated preference entry as returned by the API.
#[derive(Debug, Clone, Serialize)]
pub struct Preference {
    pub category: PreferenceCategory,
    pub key: String,
    pub value: String,
    /// Explainable score in [0, 0.99]; see `aggregator::confidence_for_evidence`.
    pub confidence: f64,
    pub evidence_count: usize,
    pub source: PreferenceSource,
    pub status: PreferenceStatus,
    /// Up to a few deduplicated evidence excerpts backing the entry.
    pub sample_evidence: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_tuple_mapping_is_stable() {
        let s = SignalKind::Language {
            language: "chinese",
        };
        assert_eq!(
            (s.category(), s.key(), s.value().as_str()),
            (PreferenceCategory::Communication, "language", "chinese")
        );

        let s = SignalKind::PlanFirst;
        assert_eq!(
            (s.category(), s.key(), s.value().as_str()),
            (PreferenceCategory::Workflow, "workflow_style", "plan_first")
        );

        let s = SignalKind::TestRequirement;
        assert_eq!(
            (s.category(), s.key(), s.value().as_str()),
            (PreferenceCategory::Technical, "testing", "require_tests")
        );

        let s = SignalKind::Correction;
        assert_eq!(
            (s.category(), s.key(), s.value().as_str()),
            (
                PreferenceCategory::Workflow,
                "interaction_style",
                "frequent_corrections"
            )
        );

        let s = SignalKind::ToolUsage {
            tool_name: "bash".to_string(),
        };
        assert_eq!(
            (s.category(), s.key(), s.value().as_str()),
            (PreferenceCategory::Technical, "preferred_tool", "bash")
        );
    }

    #[test]
    fn category_round_trips_and_rejects_unknown() {
        for c in [
            PreferenceCategory::Communication,
            PreferenceCategory::Workflow,
            PreferenceCategory::Technical,
        ] {
            assert_eq!(PreferenceCategory::parse(c.as_str()), Some(c));
        }
        assert_eq!(PreferenceCategory::parse("style"), None);
        assert_eq!(PreferenceCategory::parse(""), None);
    }

    #[test]
    fn only_language_is_exclusive() {
        assert!(is_exclusive_key("language"));
        assert!(!is_exclusive_key("preferred_tool"));
        assert!(!is_exclusive_key("workflow_style"));
    }

    #[test]
    fn excerpts_are_trimmed_and_capped_on_char_boundaries() {
        assert_eq!(truncate_excerpt("  hello  "), "hello");
        assert_eq!(truncate_excerpt(""), "");

        // Multi-byte text longer than the cap must not split a char.
        let long: String = "偏好".repeat(200);
        let cut = truncate_excerpt(&long);
        assert_eq!(cut.chars().count(), EXCERPT_MAX_CHARS + 1);
        assert!(cut.ends_with('…'));
    }
}
