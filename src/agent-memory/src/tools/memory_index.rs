//! MEMORY.md index file management.
//!
//! Maintains a compact index file at the mount root (`MEMORY.md`) that lists
//! all memory entries with one-line descriptions. This enables:
//! - Fast context assembly without scanning the entire file tree
//! - A browsable "table of contents" for human users
//! - Compatibility with Claude Code's `.claude/memory/MEMORY.md` format
//!
//! Format (one line per entry, targeting 150 bytes):
//! ```markdown
//! - [title](relative-path) — one-line description
//! ```
//! Shorten display text to meet the entry target. Complete links take priority
//! when their paths require longer lines; the whole-file byte cap still applies.
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
        let link_bytes = format!("- []({}) — ", self.path).len();
        let display_budget = MAX_ENTRY_BYTES.saturating_sub(link_bytes);
        // Keep a visible label even when the path consumes the entry target.
        let title_budget = if display_budget == 0 {
            '…'.len_utf8()
        } else {
            display_budget
        };
        let title = truncate_display(&self.title, title_budget);
        let description_budget = MAX_ENTRY_BYTES.saturating_sub(link_bytes + title.len());
        let description = truncate_display(&self.description, description_budget);
        format!("- [{title}]({}) — {description}", self.path)
    }
}

fn truncate_display(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    if max_bytes == 0 {
        return String::new();
    }
    let marker = if max_bytes >= '…'.len_utf8() {
        "…"
    } else {
        "."
    };
    let mut end = max_bytes - marker.len();
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{marker}", &text[..end])
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
    fn index_entry_keeps_long_ascii_title_link() {
        let entry = IndexEntry {
            title: "A".repeat(200),
            path: "notes/ascii.md".into(),
            description: "A short description".into(),
        };
        let line = entry.to_line();
        assert!(line.len() <= MAX_ENTRY_BYTES, "{line}");
        let parsed = parse_index(&line);
        assert_eq!(parsed.len(), 1, "long title broke the link: {line}");
        assert_eq!(parsed[0].path, entry.path);
        assert!(parsed[0].title.ends_with('…'));
    }

    #[test]
    fn index_entry_keeps_long_utf8_title_link() {
        let entry = IndexEntry {
            title: "记".repeat(80),
            path: "notes/chinese.md".into(),
            description: "中文说明".into(),
        };
        let line = entry.to_line();
        assert!(line.len() <= MAX_ENTRY_BYTES, "{line}");
        let parsed = parse_index(&line);
        assert_eq!(parsed.len(), 1, "UTF-8 title broke the link: {line}");
        assert_eq!(parsed[0].path, entry.path);
        assert!(parsed[0].title.starts_with('记'));
        assert!(parsed[0].title.ends_with('…'));
    }

    #[test]
    fn index_entry_keeps_path_beyond_line_target() {
        let entry = IndexEntry {
            title: "Long path".into(),
            path: format!("notes/{}.md", "p".repeat(170)),
            description: "Description".into(),
        };
        let line = entry.to_line();
        assert!(line.len() > MAX_ENTRY_BYTES);
        let parsed = parse_index(&line);
        assert_eq!(parsed.len(), 1, "long path broke the link: {line}");
        assert_eq!(parsed[0].path, entry.path);
        assert!(!parsed[0].title.is_empty());
    }

    #[test]
    fn index_entry_keeps_utf8_label_at_tiny_display_budgets() {
        for path_bytes in [137, 138, 139] {
            let entry = IndexEntry {
                title: "记忆标题".into(),
                path: "p".repeat(path_bytes),
                description: "中文说明".into(),
            };
            let line = entry.to_line();
            let parsed = parse_index(&line);
            assert_eq!(parsed.len(), 1, "tiny budget broke the link: {line}");
            assert_eq!(parsed[0].path, entry.path);
            assert!(!parsed[0].title.is_empty());
            if path_bytes < 139 {
                assert!(line.len() <= MAX_ENTRY_BYTES, "{line}");
            }
        }
    }

    fn setup_index_service() -> (tempfile::TempDir, MemoryService) {
        let temporary = tempfile::tempdir().unwrap();
        let mut config = crate::config::AppConfig::default();
        config.memory.paths.base_dir = temporary.path().to_string_lossy().into_owned();
        config.memory.session.base_dir = temporary
            .path()
            .join("sessions")
            .to_string_lossy()
            .into_owned();
        config.memory.mount.strategy = crate::mount::MountStrategyKind::Userland;
        config.memory.index.enabled = false;
        config.memory.git.enabled = false;
        config.memory.cgroup.enabled = false;
        config.memory.consolidation.enabled = false;
        let service = MemoryService::new(config).unwrap();
        (temporary, service)
    }

    #[test]
    fn index_helpers_retain_long_title_targets() {
        let (_temporary, service) = setup_index_service();
        let original = IndexEntry {
            title: "A".repeat(200),
            path: "notes/original.md".into(),
            description: "Original memory".into(),
        };
        write_index(&service, std::slice::from_ref(&original)).unwrap();
        let added = IndexEntry {
            title: "New memory".into(),
            path: "notes/new.md".into(),
            description: "Another memory".into(),
        };
        update_index_entry(&service, &added).unwrap();
        let content = service.read(INDEX_FILE).unwrap();
        let updated = parse_index(&content);
        assert_eq!(updated.len(), 2, "upsert lost an existing link: {content}");
        assert!(updated.iter().any(|entry| entry.path == original.path));
        assert!(updated.iter().any(|entry| entry.path == added.path));

        remove_index_entry(&service, &added.path).unwrap();
        let remaining = parse_index(&service.read(INDEX_FILE).unwrap());
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].path, original.path);
    }

    #[test]
    fn long_link_entries_still_obey_global_byte_cap() {
        let (_temporary, service) = setup_index_service();
        let segment = "p".repeat(200);
        let entries: Vec<_> = (0..MAX_LINES)
            .map(|index| IndexEntry {
                title: format!("Memory {index}"),
                path: format!("notes/{segment}/{segment}/{index}.md"),
                description: "Description".into(),
            })
            .collect();
        write_index(&service, &entries).unwrap();
        let content = service.read(INDEX_FILE).unwrap();
        assert!(content.len() <= MAX_BYTES);
        let parsed = parse_index(&content);
        assert!(!parsed.is_empty(), "no complete links were emitted");
        assert!(
            parsed.len() < entries.len(),
            "byte cap did not stop long entries"
        );
        for entry in parsed {
            assert!(entries.iter().any(|original| original.path == entry.path));
        }
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
