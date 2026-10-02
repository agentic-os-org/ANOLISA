//! Memory export — serialize the memory store to Anolisa Memory Archive (AMA) JSON.
//!
//! AMA format:
//! ```json
//! {
//!   "version": "1.0",
//!   "format": "anolisa-memory-archive",
//!   "exported_at": "<RFC3339>",
//!   "agent_id": "<MCP_CLIENT_NAME or unknown>",
//!   "user_id": "<user_id>",
//!   "total_memories": N,
//!   "memories": [{ "path", "frontmatter", "content" }],
//!   "tasks": [{ "path", "frontmatter", "content" }],
//!   "stats": { "by_category", "by_source", "total_bytes" }
//! }
//! ```

use std::collections::HashMap;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::audit::AuditEntry;
use crate::error::Result;
use crate::service::MemoryService;

/// One exported memory entry.
#[derive(Debug, Serialize, Deserialize)]
pub struct ExportedMemory {
    /// Mount-relative path (e.g. "facts/lesson/01J5.md").
    pub path: String,
    /// Parsed frontmatter key-value pairs.
    pub frontmatter: HashMap<String, String>,
    /// Markdown body (after frontmatter).
    pub content: String,
}

/// Export statistics.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ExportStats {
    pub by_category: HashMap<String, usize>,
    pub by_source: HashMap<String, usize>,
    pub total_bytes: u64,
}

/// The complete AMA export document.
#[derive(Debug, Serialize, Deserialize)]
pub struct AmaArchive {
    pub version: String,
    pub format: String,
    pub exported_at: String,
    pub agent_id: String,
    pub user_id: String,
    pub total_memories: usize,
    pub memories: Vec<ExportedMemory>,
    pub tasks: Vec<ExportedMemory>,
    pub stats: ExportStats,
}

/// Export filter options.
pub struct ExportFilter {
    /// Filter by category (e.g. "lesson", "interest"). None = all.
    pub category: Option<String>,
    /// Filter by source ("auto-consolidation", "manual-observe"). None = all.
    pub source: Option<String>,
    /// Include tasks in export. Default: true.
    pub include_tasks: bool,
}

impl Default for ExportFilter {
    fn default() -> Self {
        Self {
            category: None,
            source: None,
            include_tasks: true,
        }
    }
}

/// Parse YAML-like frontmatter from a markdown string.
/// Returns (frontmatter_map, body).
fn parse_frontmatter(content: &str) -> (HashMap<String, String>, String) {
    let mut fm = HashMap::new();
    let body;

    if let Some(rest) = content.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---\n") {
            let fm_str = &rest[..end];
            body = rest[end + 5..].to_string();
            let mut current_list_key: Option<String> = None;
            let mut current_list_items: Vec<String> = Vec::new();
            for line in fm_str.lines() {
                if let Some(item) = line.strip_prefix("  - ") {
                    // List item under the current list header
                    if current_list_key.is_some() {
                        current_list_items.push(item.trim().to_string());
                    }
                    continue;
                }
                // Flush any accumulated list items
                if let Some(key) = current_list_key.take() {
                    let json = serde_json::to_string(&current_list_items)
                        .unwrap_or_else(|_| "[]".to_string());
                    fm.insert(key, json);
                    current_list_items.clear();
                }
                if let Some((key, value)) = line.split_once(": ") {
                    fm.insert(key.trim().to_string(), value.trim().to_string());
                } else if let Some(key) = line.strip_suffix(':') {
                    current_list_key = Some(key.trim().to_string());
                }
            }
            // Flush trailing list
            if let Some(key) = current_list_key.take() {
                let json =
                    serde_json::to_string(&current_list_items).unwrap_or_else(|_| "[]".to_string());
                fm.insert(key, json);
            }
            return (fm, body);
        }
    }

    // No frontmatter found
    body = content.to_string();
    (fm, body)
}

/// Export the memory store to AMA JSON format.
pub fn memory_export(svc: &MemoryService, filter: &ExportFilter) -> Result<String> {
    let mut memories = Vec::new();
    let mut tasks = Vec::new();
    let mut stats = ExportStats::default();

    let meta_dir = svc.mount.meta_dir.clone();

    for entry in WalkDir::new(&svc.mount.root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !e.path().starts_with(&meta_dir))
    {
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

        let rel_path = path
            .strip_prefix(&svc.mount.root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let (frontmatter, body) = parse_frontmatter(&content);

        // Apply category filter
        if let Some(ref cat) = filter.category {
            let entry_cat = frontmatter.get("category").cloned().unwrap_or_default();
            // Also check path-based category (facts/<category>/...)
            let path_cat = rel_path.split('/').nth(1).unwrap_or("").to_string();
            if &entry_cat != cat && &path_cat != cat {
                continue;
            }
        }

        // Apply source filter
        if let Some(ref src) = filter.source {
            let entry_source = frontmatter.get("source").cloned().unwrap_or_default();
            if &entry_source != src {
                continue;
            }
        }

        let exported = ExportedMemory {
            path: rel_path.clone(),
            frontmatter: frontmatter.clone(),
            content: body,
        };

        // Separate tasks from regular memories
        if rel_path.starts_with("tasks/") {
            if filter.include_tasks {
                tasks.push(exported);
            }
        } else {
            // Update stats
            if let Some(cat) = frontmatter.get("category") {
                *stats.by_category.entry(cat.clone()).or_insert(0) += 1;
            }
            if let Some(src) = frontmatter.get("source") {
                *stats.by_source.entry(src.clone()).or_insert(0) += 1;
            }
            stats.total_bytes += content.len() as u64;
            memories.push(exported);
        }
    }

    let agent_id = std::env::var("MCP_CLIENT_NAME").unwrap_or_else(|_| "unknown".into());

    let archive = AmaArchive {
        version: "1.0".into(),
        format: "anolisa-memory-archive".into(),
        exported_at: Utc::now().to_rfc3339(),
        agent_id,
        user_id: svc.config.global.user_id.clone(),
        total_memories: memories.len(),
        memories,
        tasks,
        stats,
    };

    let json = serde_json::to_string_pretty(&archive)
        .map_err(|e| crate::error::MemoryError::Other(format!("export serialize: {e}")))?;

    svc.audit_log(
        AuditEntry::new("mem_export")
            .path(format!("{} memories", archive.total_memories))
            .bytes(json.len() as u64),
    );

    Ok(json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_frontmatter_with_yaml() {
        let content = "---\nid: abc123\ncategory: lesson\nsource: auto-consolidation\n---\n\nThis is the body.";
        let (fm, body) = parse_frontmatter(content);
        assert_eq!(fm.get("id").unwrap(), "abc123");
        assert_eq!(fm.get("category").unwrap(), "lesson");
        assert_eq!(fm.get("source").unwrap(), "auto-consolidation");
        assert!(body.contains("This is the body"));
    }

    #[test]
    fn parse_frontmatter_without() {
        let content = "Just plain markdown content.";
        let (fm, body) = parse_frontmatter(content);
        assert!(fm.is_empty());
        assert_eq!(body, content);
    }

    #[test]
    fn parse_frontmatter_with_list() {
        let content = "---\ntitle: Test\nnext_steps:\n  - Step 1\n  - Step 2\n---\nBody";
        let (fm, body) = parse_frontmatter(content);
        assert_eq!(fm.get("title").unwrap(), "Test");
        assert!(fm.contains_key("next_steps"));
        assert!(body.contains("Body"));
    }
}
