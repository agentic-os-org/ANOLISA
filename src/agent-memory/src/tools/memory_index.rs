//! MEMORY.md index file management.
//!
//! Maintains a compact index file at the mount root (`MEMORY.md`) that lists
//! all memory entries with one-line descriptions. This enables:
//! - Fast context assembly without scanning the entire file tree
//! - A browsable "table of contents" for human users
//! - Compatibility with Claude Code's `.claude/memory/MEMORY.md` format
//!
//! Format (one line per entry, ≤150 chars):
//! ```markdown
//! - [title](relative-path) — one-line description
//! ```
//!
//! Capacity: ≤200 lines, ≤25KB. Entries are sorted by path (alphabetical)
//! and truncated when limits are reached. Run `mem_index_refresh` after
//! bulk writes to rebuild the index.

use walkdir::WalkDir;

use crate::audit::AuditEntry;
use crate::error::Result;
use crate::service::MemoryService;

const INDEX_FILE: &str = "MEMORY.md";
const MAX_LINES: usize = 200;
const MAX_BYTES: usize = 25_600; // 25KB
const MAX_ENTRY_BYTES: usize = 150;

/// A single entry in the MEMORY.md index.
#[derive(Debug, Clone)]
pub struct IndexEntry {
    pub title: String,
    pub path: String,
    pub description: String,
}

impl IndexEntry {
    /// Format as a MEMORY.md line: `- [title](path) — description`
    fn to_line(&self) -> String {
        let line = format!("- [{}]({}) — {}", self.title, self.path, self.description);
        if line.len() > MAX_ENTRY_BYTES {
            // '…' is 3 bytes in UTF-8; reserve space for it
            let mut end = MAX_ENTRY_BYTES - 3;
            while end > 0 && !line.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}…", &line[..end])
        } else {
            line
        }
    }
}

/// Parse the existing MEMORY.md index into entries.
pub fn parse_index(content: &str) -> Vec<IndexEntry> {
    let mut entries = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if !line.starts_with("- [") {
            continue;
        }
        // Format: - [title](path) — description
        let rest = &line[3..]; // skip "- ["
        if let Some(bracket_end) = rest.find("](") {
            let title = &rest[..bracket_end];
            let after_bracket = &rest[bracket_end + 2..];
            if let Some(paren_end) = after_bracket.find(')') {
                let path = &after_bracket[..paren_end];
                let description = after_bracket[paren_end + 1..]
                    .trim_start_matches(" — ")
                    .trim_start_matches(" - ")
                    .to_string();
                entries.push(IndexEntry {
                    title: title.to_string(),
                    path: path.to_string(),
                    description,
                });
            }
        }
    }
    entries
}

/// Build a complete index by scanning all .md files in the mount.
pub fn build_index(svc: &MemoryService) -> Result<Vec<IndexEntry>> {
    let meta_dir = svc.mount.meta_dir.clone();
    let mut entries = Vec::new();

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
        // Skip the index file itself
        let rel_path = path
            .strip_prefix(&svc.mount.root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        if rel_path == INDEX_FILE {
            continue;
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Extract title and description from frontmatter
        let (title, description) = extract_title_and_description(&content, &rel_path);

        entries.push(IndexEntry {
            title,
            path: rel_path,
            description,
        });
    }

    // Sort by path for deterministic output
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    // Truncate to MAX_LINES
    entries.truncate(MAX_LINES);

    Ok(entries)
}

/// Write the index to MEMORY.md with capacity protection.
pub fn write_index(svc: &MemoryService, entries: &[IndexEntry]) -> Result<()> {
    let mut content = String::new();
    content.push_str("# Memory Index\n\n");
    content.push_str(&format!(
        "_Auto-generated index of {} memories. Do not edit manually — use `mem_index_refresh`._\n\n",
        entries.len()
    ));

    for entry in entries {
        let line = entry.to_line();
        // Byte limit check
        if content.len() + line.len() + 1 > MAX_BYTES {
            break;
        }
        content.push_str(&line);
        content.push('\n');
    }

    let index_path = svc.mount.root.join(INDEX_FILE);
    std::fs::write(&index_path, &content)?;

    svc.audit_log(
        AuditEntry::new("mem_index_refresh")
            .path(INDEX_FILE.to_string())
            .bytes(content.len() as u64),
    );

    Ok(())
}

/// Refresh the MEMORY.md index: scan all files and rebuild.
pub fn refresh_index(svc: &MemoryService) -> Result<usize> {
    let entries = build_index(svc)?;
    let count = entries.len();
    write_index(svc, &entries)?;
    Ok(count)
}

/// Update a single entry in the index (upsert).
/// Called after memory_observe or mem_write to keep the index current.
pub fn update_index_entry(svc: &MemoryService, entry: &IndexEntry) -> Result<()> {
    let index_path = svc.mount.root.join(INDEX_FILE);

    let mut entries = if index_path.exists() {
        let content = std::fs::read_to_string(&index_path)?;
        parse_index(&content)
    } else {
        Vec::new()
    };

    // Upsert: replace if path matches, otherwise append
    if let Some(pos) = entries.iter().position(|e| e.path == entry.path) {
        entries[pos] = entry.clone();
    } else {
        entries.push(entry.clone());
    }

    // Enforce capacity
    entries.truncate(MAX_LINES);

    write_index(svc, &entries)
}

/// Remove an entry from the index by path.
pub fn remove_index_entry(svc: &MemoryService, path: &str) -> Result<()> {
    let index_path = svc.mount.root.join(INDEX_FILE);

    if !index_path.exists() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&index_path)?;
    let entries: Vec<IndexEntry> = parse_index(&content)
        .into_iter()
        .filter(|e| e.path != path)
        .collect();

    write_index(svc, &entries)
}

/// Extract title and one-line description from markdown frontmatter.
fn extract_title_and_description(content: &str, fallback_path: &str) -> (String, String) {
    let mut title = fallback_path.to_string();
    let mut description = String::new();

    if let Some(rest) = content.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---") {
            let fm = &rest[..end];
            for line in fm.lines() {
                if let Some((key, value)) = line.split_once(": ") {
                    match key.trim() {
                        "title" => title = value.trim().trim_matches('"').to_string(),
                        "hint" | "description" => {
                            description = value.trim().trim_matches('"').to_string();
                        }
                        "category" if description.is_empty() => {
                            description = format!("[{}]", value.trim());
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // Fallback description: first line of body
    if description.is_empty() {
        let body = content
            .find("\n---\n")
            .map(|pos| &content[pos + 5..])
            .unwrap_or(content);
        let first_line = body.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        description = first_line.chars().take(80).collect::<String>();
    }

    (title, description)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_index_basic() {
        let content = "# Memory Index\n\n- [My Title](path/to/file.md) — A description\n- [Another](other.md) — Second entry\n";
        let entries = parse_index(content);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].title, "My Title");
        assert_eq!(entries[0].path, "path/to/file.md");
        assert_eq!(entries[0].description, "A description");
        assert_eq!(entries[1].title, "Another");
    }

    #[test]
    fn parse_index_empty() {
        let entries = parse_index("# Memory Index\n\n_No entries yet._\n");
        assert!(entries.is_empty());
    }

    #[test]
    fn index_entry_truncates_long_lines() {
        let entry = IndexEntry {
            title: "A very long title that goes on and on".into(),
            path: "path/to/file.md".into(),
            description: "A".repeat(200),
        };
        let line = entry.to_line();
        assert!(line.len() <= MAX_ENTRY_BYTES);
    }

    #[test]
    fn extract_title_from_frontmatter() {
        let content = "---\ntitle: \"JWT Auth Decision\"\ncategory: project\n---\nWe chose JWT for mobile support.";
        let (title, desc) = extract_title_and_description(content, "fallback.md");
        assert_eq!(title, "JWT Auth Decision");
        assert_eq!(desc, "[project]");
    }

    #[test]
    fn extract_title_fallback_to_path() {
        let content = "Just plain markdown content here.";
        let (title, desc) = extract_title_and_description(content, "notes/observed/abc.md");
        assert_eq!(title, "notes/observed/abc.md");
        assert_eq!(desc, "Just plain markdown content here.");
    }
}
