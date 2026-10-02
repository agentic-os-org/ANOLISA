use std::io::{BufRead, BufReader};
use std::os::fd::AsFd;
use std::path::Path;

use globset::{Glob, GlobMatcher};
use regex::Regex;
use serde::Serialize;
use walkdir::WalkDir;

use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::ns::paths::{relative_to_mount, resolve_path};
use crate::safe_fs;
use crate::service::MemoryService;

const TOOL: &str = "mem_grep";
const DEFAULT_MAX_HITS: usize = 200;
const MAX_LINE_LEN: usize = 4096;
const MAX_DEPTH: usize = 16;

#[derive(Debug, Clone, Default)]
pub struct GrepOptions {
    /// Directory to search under. Empty / "." means mount root.
    pub dir: String,
    /// Optional file glob (e.g. `**/*.md`).
    pub r#type: Option<String>,
    /// Maximum number of matches to return; defaults to 200.
    pub max: Option<usize>,
    /// Case-insensitive search.
    pub case_insensitive: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct GrepHit {
    pub path: String,
    pub line: usize,
    pub text: String,
}

/// Regex search across files under `opts.dir`. The `.anolisa` meta dir is
/// always excluded; non-UTF8 lines are skipped.
pub fn grep(svc: &MemoryService, pattern: &str, opts: GrepOptions) -> Result<Vec<GrepHit>> {
    if pattern.is_empty() {
        let err = MemoryError::InvalidArgument("empty pattern".into());
        svc.audit_log(AuditEntry::new(TOOL).error(err.to_string()));
        return Err(err);
    }

    let dir = if opts.dir.is_empty() || opts.dir == "." {
        svc.mount.root.clone()
    } else {
        match resolve_path(&svc.mount, &opts.dir) {
            Ok(p) => p,
            Err(e) => {
                svc.audit_log(
                    AuditEntry::new(TOOL)
                        .path(opts.dir.clone())
                        .error(e.to_string()),
                );
                return Err(e);
            }
        }
    };

    if !dir.is_dir() {
        let rel = relative_to_mount(&svc.mount, &dir);
        let err = MemoryError::InvalidArgument(format!("'{rel}' is not a directory"));
        svc.audit_log(AuditEntry::new(TOOL).path(rel).error(err.to_string()));
        return Err(err);
    }

    let re_src = if opts.case_insensitive {
        format!("(?i){pattern}")
    } else {
        pattern.to_string()
    };
    let re = Regex::new(&re_src)?;

    let glob_matcher: Option<GlobMatcher> = match opts.r#type.as_deref() {
        Some(g) => Some(Glob::new(g)?.compile_matcher()),
        None => None,
    };

    let max = opts.max.unwrap_or(DEFAULT_MAX_HITS);
    let meta_dir = svc.mount.root.join(svc.mount.meta_dir_name());
    let mut hits = Vec::new();

    'outer: for entry in WalkDir::new(&dir)
        .max_depth(MAX_DEPTH)
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

        let rel_path = relative_to_mount(&svc.mount, entry.path());

        if let Some(m) = &glob_matcher {
            if !m.is_match(&rel_path) {
                continue;
            }
        }

        // walkdir gives us absolute paths; route the open through
        // safe_fs so symlink swaps between entry discovery and open
        // cannot leak file content from outside the mount.
        let f = match safe_fs::open_read(svc.mount.root_fd.as_fd(), Path::new(&rel_path)) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let reader = BufReader::new(f);
        for (idx, line_result) in reader.lines().enumerate() {
            let mut line = match line_result {
                Ok(l) => l,
                Err(_) => break,
            };
            if line.len() > MAX_LINE_LEN {
                line.truncate(MAX_LINE_LEN);
            }
            if re.is_match(&line) {
                hits.push(GrepHit {
                    path: rel_path.clone(),
                    line: idx + 1,
                    text: line,
                });
                if hits.len() >= max {
                    break 'outer;
                }
            }
        }
    }

    svc.audit_log(
        AuditEntry::new(TOOL)
            .path(relative_to_mount(&svc.mount, &dir))
            .bytes(hits.len() as u64),
    );

    Ok(hits)
}
