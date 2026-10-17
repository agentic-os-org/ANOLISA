//! Session history tools — query past sessions and their tool call logs.
//!
//! Tools:
//! - `memory_sessions`: list historical session summaries from facts/summary/
//! - `memory_timeline`: show tool call log from a specific session

use serde::Serialize;

use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::service::MemoryService;

/// Summary of a historical session.
#[derive(Debug, Serialize)]
pub struct SessionSummary {
    /// Session ID.
    pub session_id: String,
    /// When the session summary was created.
    pub created_at: String,
    /// Number of tool calls in the session.
    pub tool_calls: usize,
    /// Tools used (unique names).
    pub tools_used: Vec<String>,
    /// Files modified during the session.
    pub files_modified: Vec<String>,
    /// Brief description (from summary fact content).
    pub description: String,
}

/// A single tool call entry in a session timeline.
#[derive(Debug, Clone, Serialize)]
pub struct TimelineEntry {
    pub timestamp: String,
    pub tool: String,
    pub path: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// List historical session summaries from `facts/summary/` directory.
pub fn memory_sessions(svc: &MemoryService, limit: usize) -> Result<String> {
    let summary_dir = svc.mount.root.join("facts").join("summary");
    let mut sessions: Vec<SessionSummary> = Vec::new();

    if !summary_dir.exists() {
        return Ok("(no historical sessions found)".to_string());
    }

    for entry in std::fs::read_dir(&summary_dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let fm = parse_frontmatter_flat(&content);
        let body = extract_body(&content);

        let session_id = fm.get("session_id").cloned().unwrap_or_else(|| {
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        });
        let created_at = fm.get("created_at").cloned().unwrap_or_default();

        // Parse tool_calls count from content
        let tool_calls = parse_count_from_body(&body);
        let tools_used = parse_list_from_body(&body, "tools:");
        let files_modified = parse_list_from_body(&body, "files:");

        sessions.push(SessionSummary {
            session_id,
            created_at,
            tool_calls,
            tools_used,
            files_modified,
            description: body.chars().take(200).collect(),
        });
    }

    // Sort by created_at descending
    sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    sessions.truncate(limit);

    if sessions.is_empty() {
        return Ok("(no historical sessions found)".to_string());
    }

    svc.audit_log(
        AuditEntry::new("memory_sessions")
            .path(format!("{} sessions", sessions.len()))
            .bytes(0),
    );

    serde_json::to_string_pretty(&sessions)
        .map_err(|e| MemoryError::Other(format!("serialize: {e}")))
}

/// Show the tool call timeline for a specific session.
/// Reads from `.anolisa/session-logs/<session_id>.jsonl`.
pub fn memory_timeline(svc: &MemoryService, session_id: &str, limit: usize) -> Result<String> {
    // Validate session_id to prevent path traversal (e.g. "../../etc/passwd").
    if session_id.is_empty()
        || session_id.contains('/')
        || session_id.contains('\\')
        || session_id.contains("..")
        || session_id.contains('\0')
    {
        return Err(MemoryError::InvalidArgument(format!(
            "invalid session_id: {session_id:?}"
        )));
    }

    let log_path = svc
        .mount
        .meta_dir
        .join("session-logs")
        .join(format!("{session_id}.jsonl"));

    if !log_path.exists() {
        return Err(MemoryError::NotFound(format!(
            "session log for '{session_id}' not found"
        )));
    }

    let content = std::fs::read_to_string(&log_path)?;
    let mut entries: Vec<TimelineEntry> = Vec::new();

    for line in content.lines() {
        if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) {
            entries.push(TimelineEntry {
                timestamp: entry["ts"].as_str().unwrap_or("").to_string(),
                tool: entry["tool"].as_str().unwrap_or("").to_string(),
                path: entry["path"].as_str().unwrap_or("").to_string(),
                ok: entry["ok"].as_bool().unwrap_or(false),
                bytes: entry["bytes"].as_u64(),
                error: entry["error"].as_str().map(|s| s.to_string()),
            });
        }
    }

    // Take the most recent entries
    let total = entries.len();
    if entries.len() > limit {
        entries = entries[entries.len() - limit..].to_vec();
    }

    svc.audit_log(
        AuditEntry::new("memory_timeline")
            .path(session_id.to_string())
            .bytes(entries.len() as u64),
    );

    let mut out = format!(
        "Timeline for session {session_id} ({total} total entries, showing {}):\n\n",
        entries.len()
    );
    for entry in &entries {
        let status = if entry.ok { "✓" } else { "✗" };
        out.push_str(&format!(
            "[{}] {} {} {}\n",
            entry.timestamp, status, entry.tool, entry.path
        ));
        if let Some(ref err) = entry.error {
            out.push_str(&format!("       error: {err}\n"));
        }
    }

    Ok(out)
}

// ── Helpers ─────────────────────────────────────────────────────

fn parse_frontmatter_flat(content: &str) -> std::collections::HashMap<String, String> {
    let mut fm = std::collections::HashMap::new();
    if let Some(rest) = content.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---") {
            for line in rest[..end].lines() {
                if let Some((key, value)) = line.split_once(": ") {
                    let key = key.trim();
                    let value = value.trim().trim_matches('"');
                    if !key.starts_with(' ') && !key.starts_with('-') {
                        fm.insert(key.to_string(), value.to_string());
                    }
                }
            }
        }
    }
    fm
}

fn extract_body(content: &str) -> String {
    if let Some(rest) = content.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---\n") {
            return rest[end + 5..].trim().to_string();
        }
    }
    content.trim().to_string()
}

/// Ordered most-specific first; the first needle present in a line wins.
const COUNT_NEEDLES: &[&str] = &["次工具调用", "tool calls", "工具调用", "tool call", "调用"];

fn parse_count_from_body(body: &str) -> usize {
    // The count is anchored to a call-count keyword, not "first number on
    // the line": the summary generator writes the duration before the
    // count ("Session 持续约 15 分钟，共 42 次工具调用（…）"), so a
    // first-number scan reports the minutes as the count. The needles are
    // ordered most-specific first, so the loose 调用 tail only fires when
    // the tighter wordings are absent; the number before the keyword wins
    // over one after it, and both word orders ("42 次工具调用" and
    // "工具调用 42 次") report the count.
    for line in body.lines() {
        for needle in COUNT_NEEDLES {
            let Some(pos) = line.find(needle) else {
                continue;
            };
            if let Some(count) = last_number(&line[..pos]) {
                return count;
            }
            if let Some(count) = first_number(&line[pos + needle.len()..]) {
                return count;
            }
        }
    }
    0
}

/// ASCII digit runs in `text`, parsed, in order. Runs that overflow
/// `usize` (hash- or ID-like tokens) are skipped rather than aborting the
/// scan: an unparseable run must not discard an already-parsed neighbour.
fn digit_runs(text: &str) -> impl Iterator<Item = usize> + '_ {
    text.split(|c: char| !c.is_ascii_digit())
        .filter(|run| !run.is_empty())
        .filter_map(|run| run.parse().ok())
}

/// Last parsed digit run, as `usize`.
fn last_number(text: &str) -> Option<usize> {
    digit_runs(text).last()
}

/// First parsed digit run, as `usize`.
fn first_number(text: &str) -> Option<usize> {
    digit_runs(text).next()
}

fn parse_list_from_body(body: &str, prefix: &str) -> Vec<String> {
    for line in body.lines() {
        let lower = line.to_lowercase();
        if lower.starts_with(prefix) || lower.contains(prefix.trim_end_matches(':')) {
            // Extract comma-separated or backtick-quoted items
            let rest = line.split_once(':').map(|(_, v)| v).unwrap_or(line);
            return rest
                .split(',')
                .map(|s| s.trim().trim_matches('`').trim_matches('\'').to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_frontmatter_basic() {
        let content = "---\nsession_id: ses_abc\ncreated_at: 2026-06-11T10:00:00Z\n---\nBody";
        let fm = parse_frontmatter_flat(content);
        assert_eq!(fm.get("session_id").unwrap(), "ses_abc");
    }

    #[test]
    fn extract_body_works() {
        let content = "---\nid: x\n---\n\nSession had 42 tool calls.";
        let body = extract_body(content);
        assert!(body.contains("42 tool calls"));
    }

    #[test]
    fn parse_count_from_body_finds_number() {
        let body = "Session had 42 tool calls across 5 tools.";
        assert_eq!(parse_count_from_body(body), 42);
    }

    #[test]
    fn parse_count_chinese() {
        let body = "共 15 次工具调用";
        assert_eq!(parse_count_from_body(body), 15);
    }

    #[test]
    fn parse_count_returns_zero_when_not_found() {
        let body = "No tool calls mentioned here.";
        assert_eq!(parse_count_from_body(body), 0);
    }

    #[test]
    fn parse_count_generator_format_reports_calls_not_minutes() {
        // Regression: the summary generator writes the duration before the
        // call count, so the old first-number scan reported the minutes.
        let body = "Session 持续约 15 分钟，共 42 次工具调用（40 成功，2 失败），使用工具类型: `mem_write`，总数据量 1234 bytes。";
        assert_eq!(parse_count_from_body(body), 42);
    }

    #[test]
    fn parse_count_accepts_count_after_keyword_word_order() {
        // Free-form and agent-authored summaries also put the count after
        // the keyword; the exact-substring fallback used to return 0 for
        // these. The needle table keeps the precision (a bare duration
        // line still reports 0) while restoring the recall.
        assert_eq!(parse_count_from_body("工具调用 42 次"), 42);
        assert_eq!(parse_count_from_body("本次会话共 15 次调用工具"), 15);
    }

    #[test]
    fn parse_count_skips_unparseable_runs_without_losing_the_count() {
        // A hash- or ID-like token between the count and the keyword must
        // not discard the count: last_number used to clobber a good value
        // with None when a later, unparseable run appeared.
        assert_eq!(
            parse_count_from_body("42 99999999999999999999999999 次工具调用"),
            42
        );
    }

    #[test]
    fn parse_count_duration_without_call_keyword_is_zero() {
        // The old "any line containing 次" fallback reported the minutes of
        // a plain duration line as the call count.
        assert_eq!(parse_count_from_body("第一次尝试，耗时 15 分钟"), 0);
    }

    #[test]
    fn parse_count_fact_title_and_other_english_forms() {
        // Fact titles and non-canonical English phrasings carried the same
        // duration-first trap as the generator line; the keyword anchoring
        // fixes them all (pinned so the behaviour cannot silently regress).
        assert_eq!(parse_count_from_body("活跃会话 15 分钟，42 次工具调用"), 42);
        assert_eq!(
            parse_count_from_body("Session 持续约 <1 分钟，共 7 次工具调用（…）"),
            7
        );
        assert_eq!(
            parse_count_from_body("Session lasted 15 minutes with 42 tool calls."),
            42
        );
    }
}
