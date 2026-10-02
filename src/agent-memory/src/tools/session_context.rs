//! Session-Start context injection (A-7).
//!
//! Assembles historical context for a new session by combining:
//! 1. Recent session summaries (from `facts/summary/*.md`)
//! 2. High-confidence facts from other categories
//!
//! Designed to be called at session start (e.g. by copilot-shell's
//! `autoRecallHook`) so the agent begins with awareness of past work.

use std::collections::HashMap;
use std::os::fd::AsFd;
use std::path::Path;

use walkdir::WalkDir;

use crate::audit::AuditEntry;
use crate::error::Result;
use crate::safe_fs;
use crate::service::MemoryService;

const TOOL: &str = "memory_session_context";
const DEFAULT_LIMIT: usize = 5;
const HIGH_CONFIDENCE_THRESHOLD: f64 = 0.8;
const PREVIEW_BYTES: usize = 512;
const MAX_CONTEXT_BYTES: usize = 8192;

/// Build a session-start context string from recent session summaries and
/// high-confidence facts. Returns a markdown-formatted context suitable for
/// injection into the agent's system prompt.
///
/// - `limit`: max number of recent session summaries to include (default 5)
pub fn memory_session_context(svc: &MemoryService, limit: Option<usize>) -> Result<String> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT).max(1);

    // Collect all fact files with their frontmatter metadata.
    let mut summaries: Vec<FactEntry> = Vec::new();
    let mut high_confidence: Vec<FactEntry> = Vec::new();

    let facts_dir = svc.mount.root.join("facts");
    for entry in WalkDir::new(&facts_dir).follow_links(false).into_iter() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        // Compute mount-relative path
        let rel = match path.strip_prefix(&svc.mount.root) {
            Ok(r) => r.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        if !rel.starts_with("facts/") {
            continue;
        }

        let content = match safe_fs::read_to_string(svc.mount.root_fd.as_fd(), Path::new(&rel)) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let (fm, _body) = parse_frontmatter(&content);
        let category = fm.get("category").cloned().unwrap_or_default();
        let created_at = fm.get("created_at").cloned().unwrap_or_default();
        let title = fm
            .get("title")
            .cloned()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| rel.clone());
        let confidence: f64 = fm
            .get("confidence")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);
        let session_id = fm.get("session_id").cloned().unwrap_or_default();

        let fact = FactEntry {
            category,
            title,
            created_at,
            confidence,
            session_id,
            body: _body,
        };

        if fact.category == "summary" {
            summaries.push(fact);
        } else if fact.confidence >= HIGH_CONFIDENCE_THRESHOLD {
            high_confidence.push(fact);
        }
    }

    // Sort summaries by created_at descending (most recent first).
    summaries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    summaries.truncate(limit);

    // Sort high-confidence facts by confidence descending.
    high_confidence.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    high_confidence.truncate(limit);

    // Assemble context string.
    let mut out = String::new();
    let mut bytes_used = 0;

    if !summaries.is_empty() {
        out.push_str("# Recent Sessions\n\n");
        for s in &summaries {
            if bytes_used >= MAX_CONTEXT_BYTES {
                break;
            }
            let section = format!(
                "## {}\n_session: {} | created: {}_\n\n{}\n\n",
                s.title,
                s.session_id,
                s.created_at,
                take_preview(&s.body)
            );
            if bytes_used + section.len() > MAX_CONTEXT_BYTES {
                break;
            }
            out.push_str(&section);
            bytes_used += section.len();
        }
    }

    if !high_confidence.is_empty() {
        out.push_str("# Key Memories\n\n");
        for f in &high_confidence {
            if bytes_used >= MAX_CONTEXT_BYTES {
                break;
            }
            let section = format!(
                "## [{}] {}\n_confidence: {:.2} | created: {}_\n\n{}\n\n",
                f.category,
                f.title,
                f.confidence,
                f.created_at,
                take_preview(&f.body)
            );
            if bytes_used + section.len() > MAX_CONTEXT_BYTES {
                break;
            }
            out.push_str(&section);
            bytes_used += section.len();
        }
    }

    if out.is_empty() {
        out.push_str("(no historical context available yet)");
    }

    svc.audit_log(AuditEntry::new(TOOL).bytes(out.len() as u64));
    Ok(out)
}

struct FactEntry {
    category: String,
    title: String,
    created_at: String,
    confidence: f64,
    session_id: String,
    body: String,
}

/// Parse YAML-like frontmatter from a markdown string.
/// Returns (frontmatter_map, body).
fn parse_frontmatter(content: &str) -> (HashMap<String, String>, String) {
    let mut fm = HashMap::new();
    if let Some(rest) = content.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---\n") {
            let fm_str = &rest[..end];
            let body = rest[end + 5..].to_string();
            for line in fm_str.lines() {
                if let Some(item) = line.strip_prefix("  - ") {
                    // List item — skip for flat parsing
                    let _ = item;
                    continue;
                }
                if let Some((key, value)) = line.split_once(": ") {
                    let value = unquote_yaml(value.trim());
                    fm.insert(key.trim().to_string(), value);
                } else if let Some(key) = line.strip_suffix(':') {
                    fm.insert(key.trim().to_string(), String::new());
                }
            }
            return (fm, body);
        }
    }
    (fm, content.to_string())
}

/// Strip YAML double-quotes from a frontmatter value.
fn unquote_yaml(s: &str) -> String {
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        s[1..s.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    } else {
        s.to_string()
    }
}

/// Take a preview of the body, capped at PREVIEW_BYTES (char-boundary safe).
fn take_preview(s: &str) -> String {
    if s.len() <= PREVIEW_BYTES {
        return s.trim().to_string();
    }
    let mut idx = PREVIEW_BYTES;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    let mut out = s[..idx].trim().to_string();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_frontmatter_extracts_fields() {
        let content = "---\nid: 01J\nsession_id: ses-1\ncategory: summary\ntitle: \"Test session\"\ncreated_at: 2026-06-25T10:00:00Z\nconfidence: 0.9\n---\n\nSession body text here.";
        let (fm, body) = parse_frontmatter(content);
        assert_eq!(fm.get("category").unwrap(), "summary");
        assert_eq!(fm.get("title").unwrap(), "Test session");
        assert_eq!(fm.get("session_id").unwrap(), "ses-1");
        assert!(body.contains("Session body text"));
    }

    #[test]
    fn parse_frontmatter_no_frontmatter() {
        let content = "Just a body without frontmatter.";
        let (fm, body) = parse_frontmatter(content);
        assert!(fm.is_empty());
        assert_eq!(body, content);
    }

    #[test]
    fn take_preview_truncates_safely() {
        let long = "a".repeat(1000);
        let preview = take_preview(&long);
        assert!(preview.len() <= PREVIEW_BYTES + 10); // +10 for ellipsis
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn take_preview_short_unchanged() {
        let short = "short text";
        assert_eq!(take_preview(short), short);
    }
}
