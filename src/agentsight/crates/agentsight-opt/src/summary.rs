//! Narrative trajectory summary — one LLM call that turns a session into a
//! human-readable "goal / process / outcome" digest for the Dashboard header.
//!
//! Per-item compaction is delegated to [`crate::trace::build_inventory`], which
//! already strips IDE-injected system blocks and truncates user turns and tool
//! commands — so summaries see the same normalized text the accuracy detectors
//! do. On top of that this module only bounds the *number* of rendered items
//! ([`MAX_TOOL_LINES`] head/tail sampling, [`FINAL_ANSWER_CHARS`]), because
//! `build_inventory` puts no cap on tool-call count and sessions of several
//! hundred steps would otherwise overflow the model's context window.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::atif::AtifTrajectory;
use crate::llm::{ChatMessage, LlmClient};
use crate::trace::build_inventory;

const SUMMARY_PROMPT: &str = include_str!("../prompts/trajectory_summary.md");

/// Max tool-call lines rendered into the prompt. Long sessions (hundreds of
/// steps) are head/tail sampled so a summary never blows the context window.
const MAX_TOOL_LINES: usize = 120;

/// Max characters kept from the final answer.
const FINAL_ANSWER_CHARS: usize = 1_200;

/// Narrative digest of one trajectory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrajectorySummary {
    /// One sentence: what the user wanted.
    #[serde(default)]
    pub goal: String,
    /// 3–6 ordered bullets: what the agent actually did.
    #[serde(default)]
    pub process: Vec<String>,
    /// One sentence: how it ended.
    #[serde(default)]
    pub outcome: String,
}

impl TrajectorySummary {
    /// True when the model returned nothing usable — callers treat this as
    /// "no summary" rather than rendering an empty card.
    pub fn is_empty(&self) -> bool {
        self.goal.trim().is_empty() && self.process.is_empty() && self.outcome.trim().is_empty()
    }
}

/// Summarize a trajectory in one LLM call.
///
/// # Errors
/// Propagates LLM transport / JSON parse failures from the client.
pub async fn summarize(
    client: &LlmClient,
    trajectory: &AtifTrajectory,
) -> Result<TrajectorySummary> {
    let inv = build_inventory(trajectory);

    let user_turns = if inv.user_turns.is_empty() {
        "（无用户诉求记录）".to_string()
    } else {
        inv.user_turns
            .iter()
            .map(|t| format!("[轮 {}] {}", t.turn, t.text))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let tool_calls = render_tool_calls(&inv.tool_calls);

    let final_answer = if inv.final_answer.trim().is_empty() {
        "（无最终答复）".to_string()
    } else {
        truncate_chars(inv.final_answer.trim(), FINAL_ANSWER_CHARS)
    };

    let payload = format!(
        "## 用户诉求\n\n{user_turns}\n\n## 工具调用序列\n\n{tool_calls}\n\n## 最终答复\n\n{final_answer}\n"
    );

    let messages = vec![
        ChatMessage::system(SUMMARY_PROMPT),
        ChatMessage::user(payload),
    ];
    let summary: TrajectorySummary = client
        .chat_json_parsed_labeled(messages, Some("summary"))
        .await?;
    Ok(summary)
}

/// Render tool calls one per line, head/tail sampling past [`MAX_TOOL_LINES`].
fn render_tool_calls(calls: &[crate::types::ToolCallRecord]) -> String {
    if calls.is_empty() {
        return "（无工具调用）".to_string();
    }
    let line = |(i, c): (usize, &crate::types::ToolCallRecord)| {
        let status = if c.err { "✗" } else { "✓" };
        format!("[{}] {} {} {}", i + 1, c.name, status, c.cmd)
    };

    if calls.len() <= MAX_TOOL_LINES {
        return calls
            .iter()
            .enumerate()
            .map(line)
            .collect::<Vec<_>>()
            .join("\n");
    }

    let head = MAX_TOOL_LINES / 2;
    let tail = MAX_TOOL_LINES - head;
    let mut lines: Vec<String> = calls.iter().enumerate().take(head).map(line).collect();
    lines.push(format!(
        "…（省略中间 {} 次调用）…",
        calls.len() - head - tail
    ));
    lines.extend(calls.iter().enumerate().skip(calls.len() - tail).map(line));
    lines.join("\n")
}

/// UTF-8 safe truncation by character count.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolCallRecord;

    fn call(name: &str, cmd: &str, err: bool) -> ToolCallRecord {
        ToolCallRecord {
            name: name.to_string(),
            call_id: String::new(),
            start: 0.0,
            dur: 0.0,
            cmd: cmd.to_string(),
            err,
            result_tokens: None,
        }
    }

    #[test]
    fn deserializes_full_payload() {
        let s: TrajectorySummary =
            serde_json::from_str(r#"{"goal":"修 bug","process":["定位","修改"],"outcome":"通过"}"#)
                .expect("parse");
        assert_eq!(s.goal, "修 bug");
        assert_eq!(s.process.len(), 2);
        assert_eq!(s.outcome, "通过");
        assert!(!s.is_empty());
    }

    #[test]
    fn missing_fields_default_instead_of_failing() {
        // The model occasionally omits `process`; that must not fail the request.
        let s: TrajectorySummary = serde_json::from_str(r#"{"goal":"只有目标"}"#).expect("parse");
        assert_eq!(s.goal, "只有目标");
        assert!(s.process.is_empty());
        assert!(s.outcome.is_empty());
        assert!(!s.is_empty());
    }

    #[test]
    fn empty_object_is_reported_empty() {
        let s: TrajectorySummary = serde_json::from_str("{}").expect("parse");
        assert!(s.is_empty());
    }

    #[test]
    fn renders_empty_and_small_tool_lists() {
        assert_eq!(render_tool_calls(&[]), "（无工具调用）");
        let rendered =
            render_tool_calls(&[call("Bash", "ls -l", false), call("Read", "a.rs", true)]);
        assert_eq!(rendered, "[1] Bash ✓ ls -l\n[2] Read ✗ a.rs");
    }

    #[test]
    fn samples_head_and_tail_for_long_tool_lists() {
        let calls: Vec<ToolCallRecord> = (0..MAX_TOOL_LINES + 50)
            .map(|i| call("Bash", &format!("cmd{i}"), false))
            .collect();
        let rendered = render_tool_calls(&calls);
        let lines: Vec<&str> = rendered.lines().collect();
        // head + ellipsis + tail
        assert_eq!(lines.len(), MAX_TOOL_LINES + 1);
        assert!(lines[0].contains("cmd0"));
        assert!(lines[MAX_TOOL_LINES / 2].contains("省略中间 50 次调用"));
        assert!(lines[lines.len() - 1].contains(&format!("cmd{}", MAX_TOOL_LINES + 49)));
    }

    #[test]
    fn truncate_chars_is_utf8_safe() {
        assert_eq!(truncate_chars("中文测试", 10), "中文测试");
        assert_eq!(truncate_chars("中文测试", 2), "中文…");
    }
}
