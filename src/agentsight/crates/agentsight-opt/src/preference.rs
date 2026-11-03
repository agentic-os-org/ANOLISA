//! LLM-based user preference analysis over a window of user turns.
//!
//! Complements the rule engine in the main crate: the caller collects the
//! user-authored turn texts from a query window, this module compacts them
//! into one prompt and asks the configured LLM for structured preference
//! findings. Failures are surfaced as [`PreferenceError`] so the API layer
//! can degrade to rule-only results.

use std::collections::HashSet;

use serde::Deserialize;

use crate::llm::{ChatMessage, LlmClient};

/// Per-turn character cap — one verbose turn must not crowd out the rest.
pub const MAX_TURN_CHARS: usize = 400;

/// Total input character budget (~8K tokens at a conservative ~3 chars per
/// token for mixed Chinese/English developer text).
pub const MAX_TOTAL_CHARS: usize = 24_000;

/// System prompt: extraction task, taxonomy, and the strict JSON contract.
const PREFERENCE_SYSTEM_PROMPT: &str = r#"You are an analyst extracting durable USER PREFERENCES from a list of user messages sent to a coding agent.

Only report preferences with recurring or explicit evidence — ignore one-off task details. Focus on:
- communication: language, tone, verbosity expectations
- workflow: planning before coding, review habits, correction style, commit/PR discipline
- technical: testing requirements, preferred tools/frameworks/languages, code style demands

Respond with ONLY a JSON object in this exact shape (no markdown, no commentary):
{"preferences":[{"category":"communication|workflow|technical","key":"short_snake_case_key","value":"short_snake_case_or_plain_value","rationale":"one short sentence citing the evidence"}]}

Rules:
- "category" must be exactly one of: communication, workflow, technical.
- Use concise stable keys (e.g. "language", "workflow_style", "testing", "preferred_tool").
- Return {"preferences":[]} when no durable preference is evident."#;

/// One preference finding produced by the LLM.
#[derive(Debug, Clone, Deserialize)]
pub struct LlmPreference {
    /// Expected to be one of `communication` / `workflow` / `technical`;
    /// the caller validates before merging.
    pub category: String,
    pub key: String,
    pub value: String,
    /// Short evidence sentence; optional in the model output.
    #[serde(default)]
    pub rationale: String,
}

/// Top-level response object (`response_format: json_object` requires the
/// model to emit an object, not a bare array).
#[derive(Debug, Deserialize)]
struct PreferenceReport {
    #[serde(default)]
    preferences: Vec<LlmPreference>,
}

/// Why LLM preference analysis could not produce findings.
#[derive(Debug)]
pub enum PreferenceError {
    /// No usable user turns after dedup/trimming — nothing to analyze.
    EmptyInput,
    /// The chat call or JSON parsing failed.
    Llm(anyhow::Error),
}

impl std::fmt::Display for PreferenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyInput => write!(f, "no user turns to analyze"),
            Self::Llm(e) => write!(f, "LLM preference analysis failed: {e}"),
        }
    }
}

impl std::error::Error for PreferenceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::EmptyInput => None,
            Self::Llm(e) => Some(e.as_ref()),
        }
    }
}

/// Compact user turns into the prompt payload: trim, deduplicate exact
/// repeats (agents re-send the same query across follow-up calls) at
/// their NEWEST occurrence, cap each turn at [`MAX_TURN_CHARS`] chars and
/// the total at [`MAX_TOTAL_CHARS`], keeping the NEWEST turns when the
/// budget cannot fit the window — the same newest slice the /turns
/// endpoint reports. Turns are expected in chronological order, and the
/// output keeps it: the prompt presents the list as "most recent last".
/// Returns an empty string when nothing usable remains.
pub fn build_analysis_input(turns: &[String]) -> String {
    // Walk newest-first so a turn repeated across the window dedupes at
    // its newest occurrence, and so the budget, when it cannot fit every
    // line, drops the OLDEST turns — recent behavior is the analysis
    // target, not the start of the window.
    let mut seen: HashSet<&str> = HashSet::new();
    let mut newest_first: Vec<&str> = Vec::new();
    for turn in turns.iter().rev() {
        let trimmed = turn.trim();
        if !trimmed.is_empty() && seen.insert(trimmed) {
            newest_first.push(trimmed);
        }
    }
    // Back to chronological order (oldest first, most recent last).
    newest_first.reverse();

    // Number and emit chronologically; when the budget is exceeded, drop
    // the oldest line and retry — the remaining newest lines renumber
    // from 1, so the emitted list always ends on the newest turn.
    let mut start = 0usize;
    loop {
        let mut out = String::new();
        let mut fits = true;
        for (index, trimmed) in newest_first[start..].iter().enumerate() {
            let capped: String = if trimmed.chars().count() > MAX_TURN_CHARS {
                let mut s: String = trimmed.chars().take(MAX_TURN_CHARS).collect();
                s.push('…');
                s
            } else {
                trimmed.to_string()
            };
            let line = format!("{}. {}\n", index + 1, capped);
            if out.chars().count() + line.chars().count() > MAX_TOTAL_CHARS {
                fits = false;
                break;
            }
            out.push_str(&line);
        }
        if fits || start + 1 >= newest_first.len() {
            return out;
        }
        start += 1;
    }
}

/// Ask the LLM for preference findings over the given user turns.
///
/// # Errors
///
/// [`PreferenceError::EmptyInput`] when no usable text remains after
/// compaction; [`PreferenceError::Llm`] when the chat call or response
/// parsing fails. Callers are expected to degrade to rule-only results.
pub async fn analyze_user_turns(
    client: &LlmClient,
    turns: &[String],
) -> Result<Vec<LlmPreference>, PreferenceError> {
    let input = build_analysis_input(turns);
    if input.is_empty() {
        return Err(PreferenceError::EmptyInput);
    }
    let messages = vec![
        ChatMessage::system(PREFERENCE_SYSTEM_PROMPT),
        ChatMessage::user(format!("User messages (most recent last):\n{input}")),
    ];
    let report: PreferenceReport = client
        .chat_json_parsed_labeled(messages, Some("preference_analysis"))
        .await
        .map_err(PreferenceError::Llm)?;
    Ok(report.preferences)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_input_dedups_trims_and_numbers() {
        let turns = vec![
            "  fix the bug in parser  ".to_string(),
            "fix the bug in parser".to_string(), // exact dup after trim
            String::new(),
            "请先出方案再写代码".to_string(),
        ];
        let input = build_analysis_input(&turns);
        assert_eq!(input, "1. fix the bug in parser\n2. 请先出方案再写代码\n");
    }

    #[test]
    fn build_input_caps_turn_and_total_length() {
        let long_turn = "x".repeat(MAX_TURN_CHARS * 3);
        let input = build_analysis_input(std::slice::from_ref(&long_turn));
        // "1. " + capped turn + ellipsis + newline
        assert_eq!(input.chars().count(), 3 + MAX_TURN_CHARS + 1 + 1);
        assert!(input.trim_end().ends_with('…'));

        // Many distinct near-cap turns must stop at the total budget.
        let turns: Vec<String> = (0..200)
            .map(|i| format!("{i:04} {}", "y".repeat(MAX_TURN_CHARS - 10)))
            .collect();
        let input = build_analysis_input(&turns);
        assert!(input.chars().count() <= MAX_TOTAL_CHARS);
        assert!(!input.is_empty());
    }

    #[test]
    fn build_input_preserves_chronological_order() {
        // The prompt presents the list as "most recent last": a
        // chronological input must come back in the same order.
        let turns: Vec<String> = (1..=3).map(|i| format!("message {i}")).collect();
        assert_eq!(
            build_analysis_input(&turns),
            "1. message 1\n2. message 2\n3. message 3\n"
        );
    }

    #[test]
    fn build_input_keeps_the_newest_turns_under_the_budget() {
        // 100 distinct turns of ~300 chars each (~30K total) cannot all
        // fit MAX_TOTAL_CHARS: the OLDEST turns are the ones that must be
        // dropped — the same newest-slice rule the /turns endpoint applies
        // — so the model still sees recent behavior and the list still
        // ends with the newest turn.
        let turns: Vec<String> = (0..100)
            .map(|i| format!("turn {i:03} {}", "x".repeat(290)))
            .collect();
        let input = build_analysis_input(&turns);
        assert!(input.chars().count() <= MAX_TOTAL_CHARS);
        assert!(input.contains("turn 099"));
        assert!(input.contains("turn 098"));
        assert!(!input.contains("turn 000"));
        assert!(!input.contains("turn 010"));
        let first = input.lines().next().expect("non-empty");
        assert!(first.starts_with("1. "), "numbering starts at 1");
        let last = input.lines().next_back().expect("non-empty");
        assert!(last.contains("turn 099"), "the newest turn must be last");
    }

    #[test]
    fn build_input_dedupes_a_repeat_at_its_newest_position() {
        // "alpha" is re-sent AFTER "beta": its newest occurrence is the
        // later one, so the chronological list places it after beta — the
        // same newest-position dedupe the /turns endpoint applies.
        let turns = vec!["alpha".to_string(), "beta".to_string(), "alpha".to_string()];
        assert_eq!(build_analysis_input(&turns), "1. beta\n2. alpha\n");
    }

    #[test]
    fn empty_turns_produce_empty_input() {
        assert!(build_analysis_input(&[]).is_empty());
        assert!(build_analysis_input(&["   ".to_string()]).is_empty());
    }

    #[test]
    fn report_parses_with_and_without_optional_fields() {
        let json = r#"{"preferences":[{"category":"workflow","key":"workflow_style","value":"plan_first","rationale":"asked for plans twice"},{"category":"technical","key":"testing","value":"require_tests"}]}"#;
        let report: PreferenceReport = serde_json::from_str(json).expect("parse");
        assert_eq!(report.preferences.len(), 2);
        assert_eq!(report.preferences[0].value, "plan_first");
        assert_eq!(report.preferences[1].rationale, "");

        let report: PreferenceReport = serde_json::from_str("{}").expect("parse empty");
        assert!(report.preferences.is_empty());
    }
}
