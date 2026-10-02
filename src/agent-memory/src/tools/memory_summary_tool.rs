//! Memory summary — provide a structured overview of all stored memories.
//!
//! Corresponds to Dreaming V3's "Memory Summary" page: users can see
//! what the system remembers, how memories were created (auto vs manual),
//! and which topics/categories dominate.

use std::collections::HashMap;

use serde::Serialize;
use walkdir::WalkDir;

use crate::audit::AuditEntry;
use crate::error::Result;
use crate::service::MemoryService;

/// One entry in the recent memories list.
#[derive(Debug, Serialize)]
pub struct MemorySummaryEntry {
    pub path: String,
    pub title: String,
    pub category: String,
    pub source: String,
    pub created_at: String,
}

/// Structured memory overview.
#[derive(Debug, Serialize)]
pub struct MemorySummary {
    /// Total memory files found.
    pub total_memories: usize,
    /// Created by auto-consolidation.
    pub auto_created: usize,
    /// Created by manual memory_observe or mem_write.
    pub manual_created: usize,
    /// Source unknown (no `source` field in frontmatter).
    pub unknown_source: usize,
    /// Count per category (e.g. "lesson" → 5).
    pub by_category: HashMap<String, usize>,
    /// Count per source (e.g. "auto-consolidation" → 12).
    pub by_source: HashMap<String, usize>,
    /// Most frequently occurring concepts (from frontmatter `concepts` field).
    pub top_concepts: Vec<(String, usize)>,
    /// Most frequently referenced files (from frontmatter `files` field).
    pub top_files: Vec<(String, usize)>,
    /// Most recent memories (up to `limit`).
    pub recent_memories: Vec<MemorySummaryEntry>,
    /// Total bytes of all memory files.
    pub total_bytes: u64,
}

/// Simple frontmatter parser — extracts key: value pairs from YAML block.
fn parse_frontmatter_flat(content: &str) -> HashMap<String, String> {
    let mut fm = HashMap::new();
    if let Some(rest) = content.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---") {
            for line in rest[..end].lines() {
                if let Some((key, value)) = line.split_once(": ") {
                    let key = key.trim();
                    let value = value.trim().trim_matches('"');
                    // Skip list items and nested keys
                    if !key.starts_with(' ') && !key.starts_with('-') {
                        fm.insert(key.to_string(), value.to_string());
                    }
                }
            }
        }
    }
    fm
}

/// Extract a list value from frontmatter (e.g. concepts: ["a", "b"]).
fn parse_frontmatter_list(fm: &HashMap<String, String>, key: &str) -> Vec<String> {
    let Some(val) = fm.get(key) else {
        return Vec::new();
    };
    // Simple parser: strip brackets, split by comma, trim quotes
    let inner = val.trim_start_matches('[').trim_end_matches(']');
    inner
        .split(',')
        .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Generate a memory summary by scanning the mount root.
pub fn memory_summary(svc: &MemoryService, recent_limit: usize) -> Result<MemorySummary> {
    let meta_dir = svc.mount.meta_dir.clone();
    let mut summary = MemorySummary {
        total_memories: 0,
        auto_created: 0,
        manual_created: 0,
        unknown_source: 0,
        by_category: HashMap::new(),
        by_source: HashMap::new(),
        top_concepts: Vec::new(),
        top_files: Vec::new(),
        recent_memories: Vec::new(),
        total_bytes: 0,
    };

    let mut concept_counts: HashMap<String, usize> = HashMap::new();
    let mut file_counts: HashMap<String, usize> = HashMap::new();
    let mut all_entries: Vec<(String, MemorySummaryEntry)> = Vec::new(); // (created_at, entry)

    for dir_entry in WalkDir::new(&svc.mount.root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !e.path().starts_with(&meta_dir))
    {
        let dir_entry = match dir_entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !dir_entry.file_type().is_file() {
            continue;
        }
        let path = dir_entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        let rel_path = path
            .strip_prefix(&svc.mount.root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        // Skip non-memory files (MEMORY.md index, README.md, etc.)
        if rel_path == "MEMORY.md" || rel_path == "README.md" {
            continue;
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let meta = dir_entry.metadata().ok();
        let file_size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        summary.total_bytes += file_size;
        summary.total_memories += 1;

        let fm = parse_frontmatter_flat(&content);

        // Source tracking
        let source = fm.get("source").cloned().unwrap_or_default();
        match source.as_str() {
            "auto-consolidation" | "auto-capture" => summary.auto_created += 1,
            "manual-observe" | "manual-write" => summary.manual_created += 1,
            _ => summary.unknown_source += 1,
        }
        if !source.is_empty() {
            *summary.by_source.entry(source.clone()).or_insert(0) += 1;
        }

        // Category tracking
        let category = fm.get("category").cloned().unwrap_or_else(|| {
            // Infer from path: facts/<category>/...
            rel_path
                .split('/')
                .nth(1)
                .unwrap_or("uncategorized")
                .to_string()
        });
        *summary.by_category.entry(category.clone()).or_insert(0) += 1;

        // Concept tracking
        for concept in parse_frontmatter_list(&fm, "concepts") {
            *concept_counts.entry(concept).or_insert(0) += 1;
        }

        // File reference tracking
        for file_ref in parse_frontmatter_list(&fm, "files") {
            *file_counts.entry(file_ref).or_insert(0) += 1;
        }

        // Collect for recent memories sorting
        let created_at = fm.get("created_at").cloned().unwrap_or_default();
        let title = fm.get("title").cloned().unwrap_or_else(|| rel_path.clone());

        all_entries.push((
            created_at.clone(),
            MemorySummaryEntry {
                path: rel_path,
                title,
                category,
                source,
                created_at,
            },
        ));
    }

    // Sort by created_at descending, take top N
    all_entries.sort_by(|a, b| b.0.cmp(&a.0));
    summary.recent_memories = all_entries
        .into_iter()
        .take(recent_limit)
        .map(|(_, entry)| entry)
        .collect();

    // Top concepts (sorted by count descending)
    let mut concepts: Vec<(String, usize)> = concept_counts.into_iter().collect();
    concepts.sort_by_key(|b| std::cmp::Reverse(b.1));
    summary.top_concepts = concepts.into_iter().take(10).collect();

    // Top files (sorted by count descending)
    let mut files: Vec<(String, usize)> = file_counts.into_iter().collect();
    files.sort_by_key(|b| std::cmp::Reverse(b.1));
    summary.top_files = files.into_iter().take(10).collect();

    svc.audit_log(
        AuditEntry::new("memory_summary")
            .path(format!("{} memories", summary.total_memories))
            .bytes(summary.total_bytes),
    );

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_frontmatter_basic() {
        let content = "---\nid: abc\ncategory: lesson\nsource: auto-consolidation\ntitle: \"Test title\"\n---\nBody here";
        let fm = parse_frontmatter_flat(content);
        assert_eq!(fm.get("id").unwrap(), "abc");
        assert_eq!(fm.get("category").unwrap(), "lesson");
        assert_eq!(fm.get("source").unwrap(), "auto-consolidation");
        assert_eq!(fm.get("title").unwrap(), "Test title");
    }

    #[test]
    fn parse_frontmatter_no_frontmatter() {
        let content = "# Just a heading\nSome content";
        let fm = parse_frontmatter_flat(content);
        assert!(fm.is_empty());
    }

    #[test]
    fn parse_list_values() {
        let mut fm = HashMap::new();
        fm.insert(
            "concepts".into(),
            "[\"rust\", \"memory\", \"safety\"]".into(),
        );
        let list = parse_frontmatter_list(&fm, "concepts");
        assert_eq!(list, vec!["rust", "memory", "safety"]);
    }

    #[test]
    fn parse_list_empty() {
        let fm = HashMap::new();
        let list = parse_frontmatter_list(&fm, "concepts");
        assert!(list.is_empty());
    }
}
