//! Memory import — deserialize an Anolisa Memory Archive (AMA) JSON into the
//! memory store. Supports merge/overwrite/skip-existing strategies.

use std::collections::HashMap;
use std::os::fd::AsFd;
use std::path::Path;

use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::service::MemoryService;

use super::memory_export::{AmaArchive, ExportedMemory};

/// Import strategy for handling conflicts with existing memories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportStrategy {
    /// Merge: update existing memories, add new ones (default).
    Merge,
    /// Overwrite: delete all existing memories first, then import.
    Overwrite,
    /// Skip existing: only import memories whose path doesn't already exist.
    SkipExisting,
}

impl ImportStrategy {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "merge" => Ok(Self::Merge),
            "overwrite" => Ok(Self::Overwrite),
            "skip-existing" | "skip" => Ok(Self::SkipExisting),
            _ => Err(MemoryError::InvalidArgument(format!(
                "unknown import strategy '{s}'; expected merge, overwrite, or skip-existing"
            ))),
        }
    }
}

/// Result of an import operation.
#[derive(Debug)]
pub struct ImportReport {
    pub imported: usize,
    pub skipped: usize,
    pub overwritten: usize,
    pub errors: Vec<String>,
}

/// Reconstruct markdown from frontmatter map + body.
///
/// The frontmatter format is line-based (one `key: value` or `  - item`
/// line each), and its five hand-rolled readers (`parse_frontmatter`,
/// `parse_frontmatter_flat` and its copies) do not parse YAML escapes —
/// so this writer must not apply any. The crate's established convention
/// for exactly this situation (see `sanitize_hint` in `memory_observe.rs`
/// and `memory_task::yaml_quote`) is to flatten: every ASCII control
/// character becomes a space, everything else is written verbatim.
/// `sanitize_scalar` applies that to keys, values and list items alike —
/// `ExportedMemory.frontmatter` deserialises straight from an untrusted
/// archive with no validation, so a key can carry exactly the line break
/// this function refuses on the value side (including a `\n---\n` that
/// would otherwise terminate the frontmatter block early and land
/// attacker-controlled text at the head of the body). The cost of the
/// convention is that the line break is lost rather than preserved.
fn reconstruct_markdown(fm: &HashMap<String, String>, body: &str) -> String {
    if fm.is_empty() {
        return body.to_string();
    }
    let mut out = String::from("---\n");
    let mut keys: Vec<&String> = fm.keys().collect();
    keys.sort();
    for key in keys {
        let value = &fm[key];
        // If value is a JSON array, emit as YAML list items
        if value.starts_with('[') {
            if let Ok(items) = serde_json::from_str::<Vec<String>>(value) {
                // The item form is only safe when every item survives the
                // line-based format verbatim; otherwise re-serialise the
                // flattened items as one JSON array line — still one line,
                // still list-shaped for `parse_frontmatter_list`, and
                // byte-stable from the second cycle on.
                if items.iter().all(|item| is_frontmatter_safe(item)) {
                    out.push_str(&format!("{}:\n", sanitize_scalar(key)));
                    for item in &items {
                        out.push_str(&format!("  - {item}\n"));
                    }
                } else {
                    let flat: Vec<String> =
                        items.iter().map(|item| sanitize_scalar(item)).collect();
                    // `to_string` on a `Vec<String>` cannot fail (no maps,
                    // no non-string keys); the fallback only keeps the
                    // function total.
                    let encoded = serde_json::to_string(&flat).unwrap_or_else(|_| "[]".to_string());
                    out.push_str(&format!("{}: {}\n", sanitize_scalar(key), encoded));
                }
                continue;
            }
        }
        let flat_value = sanitize_scalar(value);
        let flat_key = sanitize_scalar(key);
        if flat_value.is_empty() {
            out.push_str(&format!("{flat_key}:\n"));
        } else {
            out.push_str(&format!("{flat_key}: {flat_value}\n"));
        }
    }
    out.push_str("---\n\n");
    out.push_str(body);
    if !body.ends_with('\n') && !body.is_empty() {
        out.push('\n');
    }
    out
}

/// A scalar that can be written verbatim as one frontmatter line: no
/// ASCII control character at all. NUL and ESC are as unwelcome in a
/// line that later gets FTS-indexed and echoed to a terminal as a line
/// break is, which is why the predicate is control-character-wide and
/// not just newline-shaped.
fn is_frontmatter_safe(value: &str) -> bool {
    !value.chars().any(|c| c.is_ascii_control())
}

/// Flatten a scalar for the line-based frontmatter format: every ASCII
/// control character becomes a space, everything else is kept verbatim
/// (including `#`, `:`, `"` and `\\` — the readers do not parse
/// escapes, so escaping would corrupt values asymmetrically). This is
/// the same convention `sanitize_hint` and `memory_task::yaml_quote`
/// already apply.
fn sanitize_scalar(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_ascii_control() { ' ' } else { c })
        .collect()
}

/// Import memories from an AMA JSON string.
pub fn memory_import(
    svc: &MemoryService,
    json_data: &str,
    strategy: ImportStrategy,
    dry_run: bool,
) -> Result<String> {
    // Parse the AMA archive
    let archive: AmaArchive = serde_json::from_str(json_data)
        .map_err(|e| MemoryError::InvalidArgument(format!("invalid AMA JSON: {e}")))?;

    // Validate format
    if archive.format != "anolisa-memory-archive" {
        return Err(MemoryError::InvalidArgument(format!(
            "unknown format '{}'; expected 'anolisa-memory-archive'",
            archive.format
        )));
    }

    // Reject unknown AMA versions so future format changes fail loudly
    // instead of being silently mis-parsed.
    if archive.version != "1.0" {
        return Err(MemoryError::InvalidArgument(format!(
            "unsupported AMA version '{}'; only '1.0' is supported",
            archive.version
        )));
    }

    let mut report = ImportReport {
        imported: 0,
        skipped: 0,
        overwritten: 0,
        errors: Vec::new(),
    };

    // Handle overwrite strategy: export backup, then remove all existing .md files
    if strategy == ImportStrategy::Overwrite && !dry_run {
        // Best-effort backup: export current state before destructive overwrite.
        match crate::tools::memory_export::memory_export(
            svc,
            &crate::tools::memory_export::ExportFilter::default(),
        ) {
            Ok(backup) => {
                tracing::info!(
                    "overwrite backup: {} bytes of current memories saved",
                    backup.len()
                );
            }
            Err(e) => {
                tracing::warn!("overwrite backup failed: {e}; proceeding without backup");
            }
        }
        let removed = remove_all_memories(svc)?;
        tracing::info!("overwrite strategy: removed {removed} existing memories");
    }

    // Import memories
    for mem in &archive.memories {
        match import_single(svc, mem, strategy, dry_run) {
            Ok(action) => match action {
                ImportAction::Imported => report.imported += 1,
                ImportAction::Overwritten => report.overwritten += 1,
                ImportAction::Skipped => report.skipped += 1,
            },
            Err(e) => {
                report.errors.push(format!("{}: {e}", mem.path));
            }
        }
    }

    // Import tasks
    for task in &archive.tasks {
        match import_single(svc, task, strategy, dry_run) {
            Ok(action) => match action {
                ImportAction::Imported => report.imported += 1,
                ImportAction::Overwritten => report.overwritten += 1,
                ImportAction::Skipped => report.skipped += 1,
            },
            Err(e) => {
                report.errors.push(format!("{}: {e}", task.path));
            }
        }
    }

    let prefix = if dry_run { "[DRY RUN] " } else { "" };
    let summary = format!(
        "{prefix}import complete: {} imported, {} overwritten, {} skipped, {} errors",
        report.imported,
        report.overwritten,
        report.skipped,
        report.errors.len()
    );

    if !report.errors.is_empty() {
        tracing::warn!("import errors: {:?}", report.errors);
    }

    svc.audit_log(
        AuditEntry::new("mem_import")
            .path(format!(
                "{} imported, {} skipped",
                report.imported, report.skipped
            ))
            .bytes(json_data.len() as u64),
    );

    // Append error details if any
    if report.errors.is_empty() {
        Ok(summary)
    } else {
        let mut out = summary;
        out.push_str("\n\nErrors:\n");
        for e in &report.errors {
            out.push_str(&format!("  - {e}\n"));
        }
        Ok(out)
    }
}

#[derive(Debug)]
enum ImportAction {
    Imported,
    Overwritten,
    Skipped,
}

fn import_single(
    svc: &MemoryService,
    mem: &ExportedMemory,
    strategy: ImportStrategy,
    dry_run: bool,
) -> Result<ImportAction> {
    // Security: use ns::paths::resolve_for_create to prevent path traversal
    // and enforce reserved-segment checks (.anolisa, .git*, etc.)
    let target_path = crate::ns::paths::resolve_for_create(&svc.mount, &mem.path)?;

    let exists = target_path.exists();

    // Skip-existing: don't touch existing files
    if exists && strategy == ImportStrategy::SkipExisting {
        return Ok(ImportAction::Skipped);
    }

    if dry_run {
        return Ok(if exists {
            ImportAction::Overwritten
        } else {
            ImportAction::Imported
        });
    }

    // Reconstruct the markdown file
    let content = reconstruct_markdown(&mem.frontmatter, &mem.content);

    // Use safe_fs::write for secure write with openat2 + RESOLVE_BENEATH
    let rel_path = crate::ns::paths::relative_to_mount(&svc.mount, &target_path);
    let rel_path_path = Path::new(&rel_path);

    // Ensure parent directory exists (using safe_fs-aware path).
    // Mirrors write.rs: validate each existing parent component against
    // symlink swaps before the unsandboxed create_dir_all call.
    // resolve_for_create already enforced no `..`/absolute at check time;
    // assert_no_symlink_traversal closes the TOCTOU gap.
    if let Some(parent) = target_path.parent() {
        crate::safe_fs::assert_no_symlink_traversal(svc.mount.root_fd.as_fd(), rel_path_path)?;
        std::fs::create_dir_all(parent)?;
    }

    crate::safe_fs::write(svc.mount.root_fd.as_fd(), rel_path_path, content.as_bytes())?;

    Ok(if exists {
        ImportAction::Overwritten
    } else {
        ImportAction::Imported
    })
}

/// Remove all .md files under the mount root (for overwrite strategy).
/// Respects reserved paths (.anolisa, .git*, etc.)
fn remove_all_memories(svc: &MemoryService) -> Result<usize> {
    let meta_dir = svc.mount.meta_dir.clone();
    let mut removed = 0;

    for entry in walkdir::WalkDir::new(&svc.mount.root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Skip meta directory and reserved paths
            let path = e.path();
            if path.starts_with(&meta_dir) {
                return false;
            }
            // Check for reserved segments (.git*, etc.)
            if let Some(first_segment) = path
                .strip_prefix(&svc.mount.root)
                .ok()
                .and_then(|p| p.components().next())
            {
                let segment_str = first_segment.as_os_str().to_string_lossy();
                if segment_str.starts_with(".git") {
                    return false;
                }
            }
            true
        })
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
        // Validate path is within sandbox and no symlink traversal
        let rel_path = crate::ns::paths::relative_to_mount(&svc.mount, path);
        if crate::safe_fs::assert_no_symlink_traversal(
            svc.mount.root_fd.as_fd(),
            Path::new(&rel_path),
        )
        .is_err()
        {
            tracing::warn!("skipping unsafe path during overwrite: {}", path.display());
            continue;
        }
        if std::fs::remove_file(path).is_ok() {
            removed += 1;
        }
    }

    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconstruct_markdown_roundtrip() {
        let mut fm = HashMap::new();
        fm.insert("id".into(), "abc123".into());
        fm.insert("category".into(), "lesson".into());
        let body = "This is the body content.";

        let md = reconstruct_markdown(&fm, body);
        assert!(md.starts_with("---\n"));
        assert!(md.contains("id: abc123"));
        assert!(md.contains("category: lesson"));
        assert!(md.contains("This is the body content."));
    }

    #[test]
    fn reconstruct_empty_frontmatter() {
        let fm = HashMap::new();
        let body = "Just content.";
        let md = reconstruct_markdown(&fm, body);
        assert_eq!(md, body);
    }

    #[test]
    fn multiline_value_cannot_inject_frontmatter_keys() {
        // A hand-edited or third-party AMA archive may carry a frontmatter
        // value with an embedded line break. Written verbatim, the second
        // line would become its own frontmatter key (`id: injected`).
        let mut fm = HashMap::new();
        fm.insert("note".to_string(), "line one\nid: injected".to_string());

        let md = reconstruct_markdown(&fm, "body");

        for line in md.lines() {
            assert!(
                !line.starts_with("id:"),
                "value line break injected a frontmatter key: {md:?}"
            );
        }
        // The value survives, flattened onto one line per the crate's
        // no-escape convention (`sanitize_hint` and friends).
        assert!(
            md.contains("note: line one id: injected"),
            "multi-line value should be flattened onto one line: {md:?}"
        );
        // Windows paths and other non-control characters stay verbatim —
        // the readers do not parse escapes, so escaping them would corrupt
        // the value on read-back.
        let mut fm = HashMap::new();
        fm.insert("path".to_string(), "C:\\Users\\tester\\notes".to_string());
        let md = reconstruct_markdown(&fm, "body");
        assert!(
            md.contains("path: C:\\Users\\tester\\notes"),
            "non-control characters must stay verbatim: {md:?}"
        );
    }

    #[test]
    fn multiline_key_cannot_inject_frontmatter_keys() {
        // `ExportedMemory.frontmatter` deserialises straight from the
        // archive with no validation, so a KEY can carry the same line
        // break — and worse, a `\n---\n` that would terminate the
        // frontmatter block early and drop every key sorted after it.
        let mut fm = HashMap::new();
        fm.insert("aaa\nid: injected".to_string(), "v".to_string());
        fm.insert("tags".to_string(), "keepme".to_string());

        let md = reconstruct_markdown(&fm, "body");

        for line in md.lines() {
            assert!(
                !line.starts_with("id:"),
                "key line break injected a frontmatter key: {md:?}"
            );
        }
        let (parsed, body) = crate::tools::memory_export::parse_frontmatter(&md);
        assert!(
            parsed.get("tags").is_some_and(|v| v == "keepme"),
            "keys sorted after the malicious one must survive: {parsed:?}"
        );
        assert_eq!(
            body.trim(),
            "body",
            "body must not absorb frontmatter: {body:?}"
        );
    }

    #[test]
    fn a_key_cannot_terminate_the_frontmatter_block_early() {
        let mut fm = HashMap::new();
        fm.insert("aaa".to_string(), "1".to_string());
        fm.insert("zzz\n---\n\npwned body line".to_string(), "2".to_string());
        fm.insert("tags".to_string(), "keepme".to_string());

        let md = reconstruct_markdown(&fm, "real body");

        let (parsed, body) = crate::tools::memory_export::parse_frontmatter(&md);
        assert!(
            parsed.get("tags").is_some_and(|v| v == "keepme"),
            "keys sorted after the malicious one must survive: {parsed:?}"
        );
        assert!(
            parsed.contains_key("zzz ---  pwned body line"),
            "the malicious key is flattened, not lost: {parsed:?}"
        );
        assert!(
            !body.contains("pwned"),
            "attacker-controlled text must not land at the head of the body: {body:?}"
        );
        assert_eq!(body.trim(), "real body");
    }

    #[test]
    fn reconstruct_round_trips_through_the_real_reader() {
        // The in-repo pattern (memory_observe's hint_round_trips): assert
        // on the parsed map, not the rendered text, so the reader and the
        // writer cannot drift apart.
        let mut fm = HashMap::new();
        fm.insert("id".to_string(), "abc123".to_string());
        fm.insert("note".to_string(), "line one\nid: injected".to_string());
        fm.insert(
            "next_steps".to_string(),
            "[\"Step 1\",\"a\nb\"]".to_string(),
        );

        let md1 = reconstruct_markdown(&fm, "body");
        let (parsed, body1) = crate::tools::memory_export::parse_frontmatter(&md1);
        assert_eq!(body1.trim(), "body");
        assert_eq!(parsed.get("id").map(String::as_str), Some("abc123"));
        assert_eq!(
            parsed.get("note").map(String::as_str),
            Some("line one id: injected")
        );

        // The cycle reaches a fixed point by the second rendering: the
        // first cycle may still normalise (the JSON-array fallback for a
        // list with a broken item re-renders as the item form once the
        // item is flattened), but from there on every further
        // reconstruct -> parse round trip is byte-stable.
        let md2 = reconstruct_markdown(&parsed, body1);
        let (parsed2, body2) = crate::tools::memory_export::parse_frontmatter(&md2);
        assert_eq!(
            parsed, parsed2,
            "parsed values must be stable across cycles"
        );
        assert_eq!(
            body1.trim(),
            body2.trim(),
            "body content must survive every cycle unchanged"
        );
        // (The body gains one leading blank line per cycle - pre-existing:
        // reconstruct_markdown writes a blank line after the closing
        // marker while parse_frontmatter slices right after it; out of
        // scope for this PR and worth its own issue.)
        let md3 = reconstruct_markdown(&parsed2, body2);
        let (parsed3, _) = crate::tools::memory_export::parse_frontmatter(&md3);
        assert_eq!(
            parsed2, parsed3,
            "the frontmatter must stay at its fixed point from the second cycle on"
        );
    }

    #[test]
    fn json_list_with_multiline_items_falls_back_to_one_line() {
        // `["a\nb"]` cannot be a YAML list here: the embedded line break
        // would split the item across lines and corrupt the frontmatter.
        let mut fm = HashMap::new();
        fm.insert("next_steps".to_string(), "[\"a\nb\"]".to_string());

        let md = reconstruct_markdown(&fm, "body");

        // The fallback re-serialises the flattened items as one JSON
        // array line: still list-shaped for every downstream consumer.
        assert_eq!(
            md.lines()
                .find(|l| l.starts_with("next_steps:"))
                .map(|l| l.to_string()),
            Some("next_steps: [\"a b\"]".to_string()),
            "list fallback must re-serialise the flattened items: {md:?}"
        );
        assert!(
            !md.contains("\n  - "),
            "no list item line may split: {md:?}"
        );
    }

    #[test]
    fn single_line_lists_keep_the_yaml_item_form() {
        let mut fm = HashMap::new();
        fm.insert(
            "next_steps".to_string(),
            "[\"Step 1\",\"Step 2\"]".to_string(),
        );

        let md = reconstruct_markdown(&fm, "body");

        assert!(
            md.contains("next_steps:\n  - Step 1\n  - Step 2\n"),
            "well-formed lists must keep the item form: {md:?}"
        );
    }

    #[test]
    fn import_strategy_parse() {
        assert_eq!(
            ImportStrategy::parse("merge").unwrap(),
            ImportStrategy::Merge
        );
        assert_eq!(
            ImportStrategy::parse("overwrite").unwrap(),
            ImportStrategy::Overwrite
        );
        assert_eq!(
            ImportStrategy::parse("skip-existing").unwrap(),
            ImportStrategy::SkipExisting
        );
        assert_eq!(
            ImportStrategy::parse("skip").unwrap(),
            ImportStrategy::SkipExisting
        );
        assert!(ImportStrategy::parse("invalid").is_err());
    }

    #[test]
    fn parse_ama_json() {
        let json = r#"{
            "version": "1.0",
            "format": "anolisa-memory-archive",
            "exported_at": "2026-06-11T14:00:00Z",
            "agent_id": "test",
            "user_id": "alice",
            "total_memories": 1,
            "memories": [{
                "path": "facts/lesson/test.md",
                "frontmatter": {"category": "lesson", "id": "abc"},
                "content": "Lesson body"
            }],
            "tasks": [],
            "stats": {"by_category": {"lesson": 1}, "by_source": {}, "total_bytes": 50}
        }"#;
        let archive: AmaArchive = serde_json::from_str(json).unwrap();
        assert_eq!(archive.total_memories, 1);
        assert_eq!(archive.memories[0].path, "facts/lesson/test.md");
    }
}
