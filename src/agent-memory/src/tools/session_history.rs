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
        let tool_calls = parse_count_from_body(&body, "tool calls");
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

fn parse_count_from_body(body: &str, keyword: &str) -> usize {
    // Look for patterns like "42 tool calls" or "42 次工具调用"
    for line in body.lines() {
        if line.contains(keyword) || (keyword == "tool calls" && line.contains("次")) {
            // Extract first number
            let num: String = line.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = num.parse::<usize>() {
                return n;
            }
            // Try to find number in the line
            for word in line.split_whitespace() {
                if let Ok(n) = word.parse::<usize>() {
                    return n;
                }
            }
        }
    }
    0
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
        assert_eq!(parse_count_from_body(body, "tool calls"), 42);
    }

    #[test]
    fn parse_count_chinese() {
        let body = "共 15 次工具调用";
        assert_eq!(parse_count_from_body(body, "次"), 15);
    }

    #[test]
    fn parse_count_returns_zero_when_not_found() {
        let body = "No tool calls mentioned here.";
        assert_eq!(parse_count_from_body(body, "tool calls"), 0);
    }
}
