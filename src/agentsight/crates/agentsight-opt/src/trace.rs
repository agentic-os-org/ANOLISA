//! Shared trace parsing layer — `TraceInventory`.
//!
//! Extracts structured data from ATIF trajectories that all analysis
//! dimensions (accuracy, perf, cost) consume.

use crate::atif::{observation_result_is_error, AtifTrajectory};
use crate::types::ToolCallRecord;

// ── Public types ──

/// A user's natural-language turn in the conversation.
#[derive(Debug, Clone)]
pub struct UserTurn {
    pub turn: usize, // 1-based ordinal among genuine user text turns
    pub text: String,
}

/// Placeholder for the Skill contract (layer II oracle, not implemented yet).
#[derive(Debug, Clone)]
pub struct SkillContract {
    pub _placeholder: (),
}

/// Structured inventory of a trajectory — shared across all analysis dimensions.
#[derive(Debug, Clone)]
pub struct TraceInventory {
    pub tool_calls: Vec<ToolCallRecord>,
    pub user_turns: Vec<UserTurn>,
    pub final_answer: String,
    pub skill_contract: Option<SkillContract>,
}

/// Maximum characters for command summary truncation (UTF-8 safe).
const CMD_TRUNCATE_CHARS: usize = 50;

/// Maximum characters per user turn text (UTF-8 safe).
const USER_TURN_TEXT_CHARS: usize = 300;

/// System-injected XML tags to strip from user/assistant text.
///
/// These are injected by the IDEs and CLIs this pipeline reads (QoderWork/Qoder
/// reminders, Claude Code slash-command transcripts) and are not genuine user
/// content. The list must stay in step with
/// `agentsight_trajectory_collector::strip_system_context`, whose copy is the
/// documented contract for external callers: a tag only this list knows about is
/// stripped here and kept there, and the two views of one trajectory then
/// disagree about which turns the user actually wrote. The earlier two-pair list
/// made the optimization pipeline count a `/clear` transcript as a user turn
/// while the preferences page dropped it.
const SYSTEM_TAGS: &[(&str, &str)] = &[
    ("<system-reminder>", "</system-reminder>"),
    ("<current_notes_content>", "</current_notes_content>"),
    ("<command-message>", "</command-message>"),
    ("<command-name>", "</command-name>"),
    ("<command-args>", "</command-args>"),
    ("<local-command-caveat>", "</local-command-caveat>"),
    ("<local-command-stdout>", "</local-command-stdout>"),
    ("<local-command-stderr>", "</local-command-stderr>"),
];

/// Known plain-text patterns that indicate a QoderWork system-injected block.
/// When a text block contains one of these patterns near the start, the entire
/// block is system context (e.g., MEMORY.md dump, awareness-mode file injection)
/// and should be treated as non-user content.
const SYSTEM_TEXT_MARKERS: &[&str] = &["Target file this round:", "[SYSTEM NOTIFICATION"];

/// Maximum byte offset within which a system marker must appear to be
/// considered a system-only block (handles leading app name / path lines).
const SYSTEM_MARKER_SCAN_LIMIT: usize = 200;

/// System command suffixes injected by QoderWork at the end of text blocks.
const SYSTEM_TEXT_SUFFIXES: &[&str] = &["Please reflect and reorganize the target file."];

/// Check whether a text block is entirely system-injected context
/// (e.g., a QoderWork MEMORY.md dump with no real user message).
pub fn is_system_only_text(text: &str) -> bool {
    let trimmed = text.trim();
    // Check if any system marker appears within the first N bytes
    // Use char-boundary-safe slicing for multi-byte UTF-8 (Chinese, emoji, etc.)
    let scan_end = trimmed
        .char_indices()
        .take_while(|(i, _)| *i < SYSTEM_MARKER_SCAN_LIMIT)
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let scan_window = &trimmed[..scan_end];
    for marker in SYSTEM_TEXT_MARKERS {
        if scan_window.contains(marker) {
            return true;
        }
    }
    false
}

/// Remove system-injected context blocks from message text.
///
/// QoderWork / Qoder IDE injects environment info, MEMORY.md content,
/// MCP server lists, etc. in two formats:
/// 1. XML-tagged: `<system-reminder>...</system-reminder>`
/// 2. Plain-text: "Target file this round: ..." (entire block is system context)
///
/// Both formats are stripped. For plain-text injections, any trailing
/// system commands (e.g., "Please reflect and reorganize...") are also removed.
pub fn strip_system_context(text: &str) -> String {
    // Phase 1: If the text is entirely a system-injected block, return empty
    if is_system_only_text(text) {
        return String::new();
    }

    let mut result = text.to_string();

    // Phase 2: Strip XML tags
    for &(open, close) in SYSTEM_TAGS {
        while let Some(start) = result.find(open) {
            if let Some(end) = result[start..].find(close) {
                let end = start + end + close.len();
                result.replace_range(start..end, "");
            } else {
                result.truncate(start);
                break;
            }
        }
    }

    // Phase 3: Strip trailing system command lines
    for suffix in SYSTEM_TEXT_SUFFIXES {
        if let Some(pos) = result.rfind(suffix) {
            result.truncate(pos);
        }
    }

    // Collapse multiple blank lines left by stripping
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }
    result.trim().to_string()
}

// ── Public API ──

/// Build a `TraceInventory` from an ATIF trajectory.
///
/// Tool calls with their observations live on the same agent step, so no
/// cross-event pairing is needed. Tool durations come from the step timing
/// model (see [`crate::atif`] module docs).
pub fn build_inventory(traj: &AtifTrajectory) -> TraceInventory {
    let tool_calls = collect_tool_calls(traj);
    let user_turns = extract_user_turns(traj);
    let final_answer = extract_final_answer(traj);

    TraceInventory {
        tool_calls,
        user_turns,
        final_answer,
        skill_contract: None, // Layer II — not implemented yet
    }
}

/// Collect tool calls across agent steps with timing and error detection.
///
/// Per-call duration = the step's tool window (next agent step start − this
/// step end) divided evenly among the step's parallel calls, so summing
/// durations stays consistent with wall-clock accounting.
pub fn collect_tool_calls(traj: &AtifTrajectory) -> Vec<ToolCallRecord> {
    collect_tool_calls_with(traj, CMD_TRUNCATE_CHARS)
}

/// Same as [`collect_tool_calls`] with a custom command summary length.
pub fn collect_tool_calls_with(traj: &AtifTrajectory, cmd_chars: usize) -> Vec<ToolCallRecord> {
    let steps = &traj.steps;
    let origin = match traj.origin_ts() {
        Some(t) => t,
        None => return vec![],
    };

    let mut out = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        if !step.is_agent() || step.calls().is_empty() {
            continue;
        }
        let window = tool_window_secs(traj, i);
        let n = step.calls().len();
        let per_call = if n > 0 { window / n as f64 } else { 0.0 };
        let start = step
            .end_ts()
            .map(|t| (t - origin).as_seconds_f64())
            .unwrap_or(0.0);

        // Match observations to calls by id; positional fallback only for
        // documents whose results carry no ids at all. Falling back whenever
        // the id lookup misses would hand a sibling's observation to a call
        // whose result never arrived (interrupted execution), reporting that
        // call as failed and feeding the misattribution into the accuracy
        // detectors and cost ledger.
        for (k, call) in step.calls().iter().enumerate() {
            let result = step
                .results()
                .iter()
                .find(|r| r.source_call_id.as_deref() == Some(call.tool_call_id.as_str()))
                .or_else(|| {
                    step.results()
                        .iter()
                        .all(|r| r.source_call_id.is_none())
                        .then(|| step.results().get(k))
                        .flatten()
                });
            // Structured flag first, text heuristic only for flag-less documents.
            let err = result.map(observation_result_is_error).unwrap_or(false);
            out.push(ToolCallRecord {
                name: call.display_name(),
                call_id: call.tool_call_id.clone(),
                start,
                dur: per_call.max(0.0),
                cmd: call.command_summary(cmd_chars),
                err,
                target: file_target(call),
                result_tokens: None,
            });
        }
    }
    out
}

/// The file/path argument a call acts on, when it takes one. The command
/// summary is a JSON blob truncated at ~50 chars - too short for deep paths -
/// so consumers that need the target (e.g. files-touched aggregation) must
/// read the argument, not the summary.
fn file_target(call: &crate::atif::AtifToolCall) -> Option<String> {
    ["file_path", "path", "notebook_path", "filePath"]
        .iter()
        .find_map(|k| call.arguments.get(k).and_then(|v| v.as_str()))
        .map(str::to_string)
}

/// Tool execution window of agent step `idx`: next agent step's start (or
/// end, when start is unrecorded) minus this step's end. Zero when a user
/// step intervenes (turn ended — the gap is user idle) or no next step exists.
pub(crate) fn tool_window_secs(traj: &AtifTrajectory, idx: usize) -> f64 {
    let steps = &traj.steps;
    let Some(end) = steps[idx].end_ts() else {
        return 0.0;
    };
    for next in &steps[idx + 1..] {
        if next.is_user() {
            return 0.0;
        }
        if next.is_agent() {
            let next_start = next.start_ts().or_else(|| next.end_ts());
            return next_start
                .map(|t| (t - end).as_seconds_f64().max(0.0))
                .unwrap_or(0.0);
        }
    }
    0.0
}

// ── Internal implementation ──

/// Extract user natural-language turns from ATIF user steps.
fn extract_user_turns(traj: &AtifTrajectory) -> Vec<UserTurn> {
    let mut out = Vec::new();
    let mut turn = 0usize;

    for step in traj.steps.iter().filter(|s| s.is_user()) {
        let raw = step.message.as_deref().unwrap_or("");
        let text = strip_system_context(raw);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        turn += 1;
        out.push(UserTurn {
            turn,
            text: text.chars().take(USER_TURN_TEXT_CHARS).collect::<String>(),
        });
    }

    out
}

/// Heuristic: extract the last agent text message as the "final answer".
/// This will be overridden by the LLM extractor in the accuracy pipeline.
/// Also strips system-injected context blocks.
fn extract_final_answer(traj: &AtifTrajectory) -> String {
    for step in traj.steps.iter().rev() {
        if !step.is_agent() {
            continue;
        }
        if let Some(msg) = step.message.as_deref() {
            let cleaned = strip_system_context(msg);
            if !cleaned.is_empty() {
                return cleaned;
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atif::AtifTrajectory;

    fn traj(steps_json: &str) -> AtifTrajectory {
        AtifTrajectory::from_json(&format!(
            r#"{{"schema_version":"ATIF-v1.6","session_id":"s1",
                "agent":{{"name":"a","version":"1"}},"steps":{steps_json}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn parses_tool_calls_from_atif() {
        let inv = build_inventory(&traj(
            r#"[
            {"step_id":1,"source":"agent","timestamp":"2025-01-01T00:00:01Z",
             "extra":{"start_timestamp":"2025-01-01T00:00:00Z"},
             "tool_calls":[{"tool_call_id":"c1","function_name":"Bash","arguments":{"command":"ls -la"}}],
             "observation":{"results":[{"source_call_id":"c1","content":"file1\nfile2"}]}},
            {"step_id":2,"source":"agent","timestamp":"2025-01-01T00:00:07Z",
             "extra":{"start_timestamp":"2025-01-01T00:00:06Z"},
             "message":"Done!"}
        ]"#,
        ));

        assert_eq!(inv.tool_calls.len(), 1);
        assert_eq!(inv.tool_calls[0].name, "Bash");
        assert_eq!(inv.tool_calls[0].call_id, "c1");
        assert!(!inv.tool_calls[0].err);
        // window = step2.start (t=6) − step1.end (t=1) = 5s
        assert!((inv.tool_calls[0].dur - 5.0).abs() < 0.01);
        assert!(inv.final_answer.contains("Done"));
    }

    #[test]
    fn extracts_user_turns() {
        let inv = build_inventory(&traj(
            r#"[
            {"step_id":1,"source":"user","timestamp":"2025-01-01T00:00:00Z","message":"请帮我修复这个 bug"},
            {"step_id":2,"source":"agent","timestamp":"2025-01-01T00:00:01Z","message":"好的"},
            {"step_id":3,"source":"user","timestamp":"2025-01-01T00:00:03Z","message":"为什么不用 gh cli 呢"}
        ]"#,
        ));

        assert_eq!(inv.user_turns.len(), 2);
        assert_eq!(inv.user_turns[0].turn, 1);
        assert!(inv.user_turns[0].text.contains("修复"));
        assert!(inv.user_turns[1].text.contains("gh cli"));
    }

    #[test]
    fn detects_errored_observation() {
        let inv = build_inventory(&traj(
            r#"[
            {"step_id":1,"source":"agent","timestamp":"2025-01-01T00:00:01Z",
             "tool_calls":[{"tool_call_id":"c1","function_name":"Bash","arguments":{"command":"ls /nope"}}],
             "observation":{"results":[{"source_call_id":"c1","content":"ls: cannot access '/nope': No such file or directory"}]}}
        ]"#,
        ));
        assert_eq!(inv.tool_calls.len(), 1);
        assert!(inv.tool_calls[0].err);
    }

    #[test]
    fn handles_empty_trajectory() {
        let inv = build_inventory(&traj("[]"));
        assert!(inv.tool_calls.is_empty());
        assert!(inv.user_turns.is_empty());
        assert!(inv.final_answer.is_empty());
        assert!(inv.skill_contract.is_none());
    }

    #[test]
    fn strip_system_reminder_blocks() {
        let input = "用户真实问题\n\n<system-reminder>MEMORY content here</system-reminder>\n<system-reminder>MCP servers</system-reminder>";
        let result = strip_system_context(input);
        assert_eq!(result, "用户真实问题");
    }

    /// The tag list is shared with the trajectory collector, whose copy is the
    /// documented contract for external callers and covers the slash-command and
    /// local-command transcripts the IDEs inject as well. This copy had only the
    /// first two pairs, so the optimization pipeline fed the summary stage a
    /// `/clear` transcript as if the user had typed it — while the preferences
    /// page stripped the same turn from the same trajectory.
    #[test]
    fn slash_command_transcripts_are_not_user_turns() {
        for transcript in [
            "<command-message>clear</command-message>\n<command-name>/clear</command-name>",
            "<command-args></command-args>",
            "<local-command-caveat>Caveat: the messages below were generated by the user</local-command-caveat>",
            "<local-command-stdout>done</local-command-stdout>",
            "<local-command-stderr>failed</local-command-stderr>",
        ] {
            assert_eq!(
                strip_system_context(transcript),
                "",
                "{transcript} is system-injected, not user text"
            );
        }

        let inv = build_inventory(&traj(
            r#"[
            {"step_id":1,"source":"user","timestamp":"2025-01-01T00:00:00Z",
             "message":"<command-message>clear</command-message>\n<command-name>/clear</command-name>"},
            {"step_id":2,"source":"user","timestamp":"2025-01-01T00:00:01Z","message":"真正的问题"},
            {"step_id":3,"source":"agent","timestamp":"2025-01-01T00:00:02Z","message":"回答"}
        ]"#,
        ));

        assert_eq!(
            inv.user_turns.len(),
            1,
            "only the genuine question is a turn"
        );
        assert_eq!(inv.user_turns[0].text, "真正的问题");
    }

    #[test]
    fn strip_preserves_user_selected_text() {
        let input = "请分析这段代码\n<user-selected-text>fn main() {}</user-selected-text>";
        let result = strip_system_context(input);
        assert!(result.contains("user-selected-text"));
    }

    #[test]
    fn strip_unclosed_system_tag() {
        let input = "用户问题\n<system-reminder>unclosed content without end tag";
        let result = strip_system_context(input);
        assert_eq!(result, "用户问题");
    }

    #[test]
    fn user_turns_strip_system_context() {
        let inv = build_inventory(&traj(
            r#"[
            {"step_id":1,"source":"user","timestamp":"2025-01-01T00:00:00Z",
             "message":"如何判断相关性\n\n<system-reminder>MEMORY.md content\nMCP servers list</system-reminder>"},
            {"step_id":2,"source":"agent","timestamp":"2025-01-01T00:00:01Z","message":"用embedding"}
        ]"#,
        ));

        assert_eq!(inv.user_turns.len(), 1);
        assert_eq!(inv.user_turns[0].text, "如何判断相关性");
    }

    #[test]
    fn is_system_only_detects_target_file() {
        assert!(is_system_only_text(
            "Target file this round: MEMORY.md\nCurrent usage: 5949 bytes\n..."
        ));
        // Qoder format: app name + path + Target file
        assert!(is_system_only_text(
            "QoderWork\n/Users/cheng\nTarget file this round: MEMORY.md..."
        ));
        assert!(is_system_only_text(
            "[SYSTEM NOTIFICATION - NOT USER INPUT]\nThis is automated..."
        ));
        assert!(!is_system_only_text("请帮我分析这个代码"));
        // Multi-byte UTF-8 near the scan boundary must not panic
        let chinese = "功能输出核心功能输出核心功能输出核心功能输出核心功能输出核心功能输出核心功能输出核心功能输出核心功能输出核心功能输出核心";
        assert!(!is_system_only_text(chinese));
    }

    #[test]
    fn strip_target_file_returns_empty() {
        let input = "Target file this round: MEMORY.md\nCurrent usage: 5949 / 10240 bytes (58%).\n\nFull MEMORY.md entries...\nPlease reflect and reorganize the target file.";
        let result = strip_system_context(input);
        assert!(result.is_empty());
    }

    #[test]
    fn tool_calls_carry_file_target() {
        let inv = build_inventory(&traj(
            r#"[
            {"step_id":1,"source":"agent","timestamp":"2025-01-01T00:00:01Z",
             "tool_calls":[{"tool_call_id":"c1","function_name":"Edit",
               "arguments":{"file_path":"src/agentsight/deep/path/mod.rs","old_string":"a","new_string":"b"}}],
             "observation":{"results":[{"source_call_id":"c1","content":"ok"}]}},
            {"step_id":2,"source":"agent","timestamp":"2025-01-01T00:00:02Z",
             "tool_calls":[{"tool_call_id":"c2","function_name":"Bash","arguments":{"command":"cargo test"}}]}
        ]"#,
        ));
        assert_eq!(
            inv.tool_calls[0].target.as_deref(),
            Some("src/agentsight/deep/path/mod.rs")
        );
        assert_eq!(inv.tool_calls[1].target, None);
    }

    #[test]
    fn user_turns_skip_target_file_session() {
        let inv = build_inventory(&traj(
            r#"[
            {"step_id":1,"source":"user","timestamp":"2025-01-01T00:00:00Z",
             "message":"Target file this round: MEMORY.md\nFull MEMORY.md entries...\nPlease reflect and reorganize the target file."}
        ]"#,
        ));
        assert!(inv.user_turns.is_empty());
    }

    #[test]
    fn a_call_without_an_observation_does_not_inherit_a_siblings_error() {
        // c1's tool result never arrived (interrupted execution); c2's did and
        // failed. The positional fallback must not hand c2's observation to c1:
        // err then claims c1 failed, which feeds the accuracy detectors and the
        // cost ledger's churn accounting as a misattributed failure.
        let inv = build_inventory(&traj(
            r#"[
            {"step_id":1,"source":"agent","timestamp":"2025-01-01T00:00:01Z",
             "tool_calls":[
                {"tool_call_id":"c1","function_name":"Bash","arguments":{"command":"ls"}},
                {"tool_call_id":"c2","function_name":"Bash","arguments":{"command":"ls /nope"}}
             ],
             "observation":{"results":[
                {"source_call_id":"c2","content":"ls: cannot access '/nope': No such file or directory"}]}}
        ]"#,
        ));
        assert_eq!(inv.tool_calls.len(), 2);
        assert!(
            !inv.tool_calls[0].err,
            "a call with no observation must not inherit its sibling's failure"
        );
        assert!(inv.tool_calls[1].err, "c2's own observation is an error");
    }

    #[test]
    fn id_less_observation_sets_still_pair_positionally() {
        let inv = build_inventory(&traj(
            r#"[
            {"step_id":1,"source":"agent","timestamp":"2025-01-01T00:00:01Z",
             "tool_calls":[
                {"tool_call_id":"c1","function_name":"Bash","arguments":{"command":"ls"}},
                {"tool_call_id":"c2","function_name":"Bash","arguments":{"command":"ls /nope"}}
             ],
             "observation":{"results":[
                {"content":"file1"},
                {"content":"ls: cannot access '/nope': No such file or directory"}]}}
        ]"#,
        ));
        assert!(!inv.tool_calls[0].err);
        assert!(inv.tool_calls[1].err);
    }
}
