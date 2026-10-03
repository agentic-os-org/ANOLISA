use std::os::fd::AsFd;
use std::path::Path;

use walkdir::WalkDir;

use crate::audit::AuditEntry;
use crate::error::Result;
use crate::index::extractor::is_indexable;
use crate::ns::paths::relative_to_mount;
use crate::safe_fs;
use crate::service::MemoryService;

const TOOL: &str = "memory_get_context";
const PER_FILE_PREVIEW_BYTES: usize = 256;
const TOKEN_TO_BYTES: usize = 4;

/// Tier B: build a compact context summary by concatenating previews of the
/// most recently modified text files in the mount, capped to roughly
/// `max_tokens * 4` bytes.
///
/// This is a deliberately simple heuristic: it doesn't hit the index — it
/// walks the file tree, sorts by mtime desc, and emits markdown sections.
pub fn memory_get_context(svc: &MemoryService, max_tokens: usize) -> Result<String> {
    let max_bytes = max_tokens.saturating_mul(TOKEN_TO_BYTES);
    let meta_dir = svc.mount.meta_dir.clone();

    // Collect candidates with mtime. WalkDir gives us absolute paths;
    // we compute the mount-relative form early so we can route the actual
    // read through safe_fs (openat2 RESOLVE_BENEATH|RESOLVE_NO_SYMLINKS),
    // closing the symlink-swap TOCTOU window that std::fs::read_to_string
    // would leave open.
    let mut entries: Vec<(std::time::SystemTime, String, u64)> = Vec::new();
    let root = svc.mount.root.clone();
    for entry in WalkDir::new(&svc.mount.root)
        .follow_links(false)
        .into_iter()
        // Skip the OS-managed meta dir (.anolisa/) and the git mirror
        // (.git/) — neither is user memory. `.git/` was previously
        // leaked here (only the index worker excluded it), surfacing
        // git internals like `.git/logs/HEAD` as context. Share the
        // predicate with the worker via safe_fs::is_under_git.
        .filter_entry(|e| {
            !e.path().starts_with(&meta_dir) && !crate::safe_fs::is_under_git(e.path(), &root)
        })
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = relative_to_mount(&svc.mount, entry.path());
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !is_indexable(Path::new(&rel), meta.len()) {
            continue;
        }
        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        entries.push((mtime, rel, meta.len()));
    }

    // Newest first (descending by mtime).
    entries.sort_by_key(|e| std::cmp::Reverse(e.0));

    let mut out = String::new();
    let mut bytes_used = 0;
    for (_, rel, _size) in entries {
        if bytes_used >= max_bytes {
            break;
        }
        let body = match safe_fs::read_to_string(svc.mount.root_fd.as_fd(), Path::new(&rel)) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let preview = take_chars(&body, PER_FILE_PREVIEW_BYTES);

        let section = format!(
            "## {rel}\n\n{preview}{ellipsis}\n\n",
            ellipsis = if body.len() > preview.len() {
                "…"
            } else {
                ""
            }
        );
        if bytes_used + section.len() > max_bytes {
            // Truncate this last section to fit
            let remaining = max_bytes.saturating_sub(bytes_used);
            if remaining > 0 {
                out.push_str(&take_chars(&section, remaining));
            }
            break;
        }
        out.push_str(&section);
        bytes_used += section.len();
    }

    if out.is_empty() {
        out.push_str("(no memory files yet)");
    }

    svc.audit_log(AuditEntry::new(TOOL).bytes(out.len() as u64));
    Ok(out)
}

fn take_chars(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // Find a safe char boundary at or below max_bytes
    let mut idx = max_bytes;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    s[..idx].to_string()
}
