use std::path::Path;

use globset::{Glob, GlobMatcher};
use serde::Serialize;
use walkdir::WalkDir;

use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::ns::paths::{relative_to_mount, resolve_path};
use crate::service::MemoryService;

const TOOL: &str = "mem_list";
const DEFAULT_MAX_DEPTH: usize = 1;
const RECURSIVE_MAX_DEPTH: usize = 16;
const MAX_ENTRIES: usize = 5000;

#[derive(Debug, Clone, Default)]
pub struct ListOptions {
    pub recursive: bool,
    /// Optional glob like `**/*.md` applied to the path relative to the mount.
    pub glob: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListEntry {
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

/// List entries under `dir`. Empty `dir` (or `"."`) means mount root.
pub fn list(svc: &MemoryService, dir: &str, opts: ListOptions) -> Result<Vec<ListEntry>> {
    // Empty / "." → mount root (special case, since resolve_path forbids ".")
    let resolved = if dir.is_empty() || dir == "." {
        svc.mount.root.clone()
    } else {
        match resolve_path(&svc.mount, dir) {
            Ok(p) => p,
            Err(e) => {
                svc.audit_log(
                    AuditEntry::new(TOOL)
                        .path(dir.to_string())
                        .error(e.to_string()),
                );
                return Err(e);
            }
        }
    };

    let rel = relative_to_mount(&svc.mount, &resolved);

    if !resolved.exists() {
        let err = MemoryError::NotFound(rel.clone());
        svc.audit_log(AuditEntry::new(TOOL).path(rel).error(err.to_string()));
        return Err(err);
    }
    if !resolved.is_dir() {
        let err = MemoryError::InvalidArgument(format!("'{rel}' is not a directory"));
        svc.audit_log(AuditEntry::new(TOOL).path(rel).error(err.to_string()));
        return Err(err);
    }

    let matcher: Option<GlobMatcher> = match opts.glob.as_deref() {
        Some(pat) => Some(Glob::new(pat)?.compile_matcher()),
        None => None,
    };

    let max_depth = if opts.recursive {
        RECURSIVE_MAX_DEPTH
    } else {
        DEFAULT_MAX_DEPTH
    };
    let mut out = Vec::new();
    let meta_dir = svc.mount.root.join(svc.mount.meta_dir_name());

    for entry in WalkDir::new(&resolved)
        .min_depth(1)
        .max_depth(max_depth)
        // Explicit: never follow symlinks. The path sandbox already
        // rejects symlink escapes, but a benign symlink inside the mount
        // (e.g. a user's own `latest -> notes/2026-04`) must not be
        // traversed twice or expose external trees if planted.
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Always hide the .anolisa meta dir
            !e.path().starts_with(&meta_dir)
        })
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let p = entry.path();
        let rel_path = relative_to_mount(&svc.mount, p);

        if let Some(m) = &matcher {
            if !m.is_match(Path::new(&rel_path)) {
                continue;
            }
        }

        let ft = entry.file_type();
        let kind = if ft.is_dir() {
            EntryKind::Dir
        } else if ft.is_symlink() {
            EntryKind::Symlink
        } else {
            EntryKind::File
        };
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);

        out.push(ListEntry {
            path: rel_path,
            kind,
            size,
        });
        if out.len() >= MAX_ENTRIES {
            break;
        }
    }

    svc.audit_log(AuditEntry::new(TOOL).path(rel).bytes(out.len() as u64));

    Ok(out)
}
