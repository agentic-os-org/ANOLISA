//! Phase 6.2: optional git versioning for memory mounts.
//!
//! When `[memory.git].enabled = true`:
//! - On startup, `<mount.root>` is initialized as a git repo if it isn't
//!   already, with `.anolisa/` (audit / index / snapshots) excluded via
//!   the repo's own `.gitignore`.
//! - When `auto_commit = true`, every successful audit-emitting tool call
//!   performs a best-effort inline `commit -am "<tool> <path>"`. Failures are
//!   logged at debug level and never block the foreground tool.
//! - `mem_log` returns recent commits; `mem_revert` checks out a path
//!   from the previous commit.

use std::os::fd::BorrowedFd;
use std::path::Path;
use std::sync::Arc;

use git2::{IndexAddOption, Repository, Signature};
use serde::{Deserialize, Serialize};

use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitConfig {
    /// Master switch. Default `false` so existing mounts aren't suddenly
    /// turned into git repos behind the user's back.
    #[serde(default)]
    pub enabled: bool,
    /// Auto-commit after every successful audit entry.
    #[serde(default = "default_auto_commit")]
    pub auto_commit: bool,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            auto_commit: default_auto_commit(),
        }
    }
}

fn default_auto_commit() -> bool {
    true
}

const GITIGNORE_BODY: &str = ".anolisa/\n";
const AUTHOR_NAME: &str = "agent-memory";
const AUTHOR_EMAIL: &str = "anolisa@local";

/// Initialize the mount root as a git repo if it isn't one already, and
/// install a `.gitignore` that hides `.anolisa/`. Idempotent.
pub fn init(root: &Path) -> Result<()> {
    if root.join(".git").exists() {
        // Existing repo — only refresh .gitignore so .anolisa/ is hidden.
        ensure_gitignore(root)?;
        return Ok(());
    }
    Repository::init(root).map_err(|e| MemoryError::Other(format!("git init: {e}")))?;
    ensure_gitignore(root)?;
    // Create an initial commit so subsequent commits have a parent.
    commit_all(root, "initial commit")?;
    Ok(())
}

fn ensure_gitignore(root: &Path) -> Result<()> {
    let p = root.join(".gitignore");
    if !p.exists() {
        std::fs::write(&p, GITIGNORE_BODY)?;
    } else {
        let body = std::fs::read_to_string(&p)?;
        if !body
            .lines()
            .any(|l| l.trim() == ".anolisa/" || l.trim() == ".anolisa")
        {
            let mut joined = body;
            if !joined.ends_with('\n') {
                joined.push('\n');
            }
            joined.push_str(".anolisa/\n");
            std::fs::write(&p, joined)?;
        }
    }
    Ok(())
}

/// Stage everything (sans .gitignore'd paths) and commit. Returns the new
/// commit id, or `None` if the staged tree matches the current HEAD's tree
/// (no-op write like `mem_write` of identical content) — skipping these
/// avoids polluting the log with thousands of empty commits.
pub(crate) fn commit_all(root: &Path, message: &str) -> Result<Option<String>> {
    let repo = Repository::open(root).map_err(|e| MemoryError::Other(format!("git open: {e}")))?;
    let mut index = repo
        .index()
        .map_err(|e| MemoryError::Other(format!("git index: {e}")))?;
    index
        .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
        .map_err(|e| MemoryError::Other(format!("git add_all: {e}")))?;
    index
        .write()
        .map_err(|e| MemoryError::Other(format!("git index write: {e}")))?;
    let tree_oid = index
        .write_tree()
        .map_err(|e| MemoryError::Other(format!("git write_tree: {e}")))?;
    let tree = repo
        .find_tree(tree_oid)
        .map_err(|e| MemoryError::Other(format!("git find_tree: {e}")))?;

    let sig = Signature::now(AUTHOR_NAME, AUTHOR_EMAIL)
        .map_err(|e| MemoryError::Other(format!("git sig: {e}")))?;

    let parents: Vec<git2::Commit> = match repo.head() {
        Ok(h) => h
            .target()
            .and_then(|oid| repo.find_commit(oid).ok())
            .into_iter()
            .collect(),
        Err(_) => Vec::new(),
    };

    // Empty-commit guard: identical tree as parent => skip. Initial commit
    // (no parent) always proceeds so the repo gets a HEAD.
    if let Some(parent) = parents.first() {
        if parent.tree_id() == tree_oid {
            return Ok(None);
        }
    }

    let parent_refs: Vec<&git2::Commit> = parents.iter().collect();

    let oid = repo
        .commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)
        .map_err(|e| MemoryError::Other(format!("git commit: {e}")))?;
    Ok(Some(oid.to_string()))
}

#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub hash: String,
    pub summary: String,
    pub author: String,
    /// RFC3339 UTC commit time.
    pub time: String,
}

/// Return at most `limit` most-recent commits. `path` filters to commits
/// touching that path (mount-relative); empty/None = whole repo.
pub fn log(root: &Path, limit: usize, path: Option<&str>) -> Result<Vec<LogEntry>> {
    let repo = Repository::open(root).map_err(|e| MemoryError::Other(format!("git open: {e}")))?;
    let head = match repo.head() {
        Ok(h) => h,
        Err(_) => return Ok(Vec::new()), // empty repo
    };
    let mut walk = repo
        .revwalk()
        .map_err(|e| MemoryError::Other(format!("git revwalk: {e}")))?;
    // `head.target()` returns None for a symbolic reference whose target
    // branch is missing. Don't panic — treat it as an empty log so a
    // partially-initialized repo doesn't take down the tokio worker.
    let head_oid = match head.target() {
        Some(o) => o,
        None => return Ok(Vec::new()),
    };
    walk.push(head_oid)
        .map_err(|e| MemoryError::Other(format!("git push head: {e}")))?;
    walk.set_sorting(git2::Sort::TIME)
        .map_err(|e| MemoryError::Other(format!("git sort: {e}")))?;

    let path_filter: Option<std::path::PathBuf> = path.map(std::path::PathBuf::from);
    let mut out = Vec::new();
    for oid in walk.flatten() {
        let commit = match repo.find_commit(oid) {
            Ok(c) => c,
            Err(_) => continue,
        };

        if let Some(p) = &path_filter {
            // Skip commits that don't touch the path.
            let touched = commit_touches_path(&repo, &commit, p);
            if !touched {
                continue;
            }
        }

        let secs = commit.time().seconds();
        let time = chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default();
        let summary = commit.summary().unwrap_or("(no summary)").to_string();
        let author = commit.author().name().unwrap_or("(unknown)").to_string();

        out.push(LogEntry {
            hash: commit.id().to_string(),
            summary,
            author,
            time,
        });
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

fn commit_touches_path(repo: &Repository, commit: &git2::Commit, path: &Path) -> bool {
    let tree = match commit.tree() {
        Ok(t) => t,
        Err(_) => return false,
    };
    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());

    let diff = match parent_tree.as_ref() {
        Some(pt) => repo.diff_tree_to_tree(Some(pt), Some(&tree), None),
        None => repo.diff_tree_to_tree(None, Some(&tree), None),
    };
    let diff = match diff {
        Ok(d) => d,
        Err(_) => return false,
    };

    let mut hit = false;
    let _ = diff.foreach(
        &mut |delta, _| {
            let p = delta.new_file().path().or_else(|| delta.old_file().path());
            if let Some(p) = p {
                if p == path {
                    hit = true;
                }
            }
            true
        },
        None,
        None,
        None,
    );
    hit
}

/// Restore `path` to its content in the most recent commit, then commit
/// the revert. Returns the hash of the revert commit (or the existing
/// HEAD hash if nothing changed).
///
/// `root_fd` is the mount's O_PATH dirfd; the write-back routes through
/// `safe_fs` (openat2 RESOLVE_BENEATH|RESOLVE_NO_SYMLINKS) so a
/// concurrent process planting a symlink at `path` cannot redirect the
/// blob bytes outside the mount.
pub fn revert(root: &Path, root_fd: BorrowedFd<'_>, path: &str) -> Result<String> {
    let repo = Repository::open(root).map_err(|e| MemoryError::Other(format!("git open: {e}")))?;
    let head = repo
        .head()
        .map_err(|e| MemoryError::Other(format!("git head: {e}")))?;
    let head_commit = head
        .peel_to_commit()
        .map_err(|e| MemoryError::Other(format!("git peel commit: {e}")))?;
    let tree = head_commit
        .tree()
        .map_err(|e| MemoryError::Other(format!("git head tree: {e}")))?;

    // Find blob for path in HEAD tree.
    let entry = tree
        .get_path(Path::new(path))
        .map_err(|_| MemoryError::NotFound(format!("path '{path}' in HEAD")))?;
    // git stores symlinks as mode 0o120000 blobs whose content is the
    // link target string. Reverting such an entry would write that
    // string as a regular file (e.g. "/etc/passwd") — confusing and
    // potentially dangerous. Refuse outright.
    if entry.filemode() == 0o120000 {
        return Err(MemoryError::InvalidArgument(format!(
            "path '{path}' is a symlink at HEAD; refuse to revert"
        )));
    }
    let blob_obj = entry
        .to_object(&repo)
        .map_err(|e| MemoryError::Other(format!("git blob obj: {e}")))?;
    let blob = blob_obj.as_blob().ok_or_else(|| {
        MemoryError::InvalidArgument(format!("'{path}' is not a regular file at HEAD"))
    })?;

    // Write back through the sandbox. assert_no_symlink_traversal on the
    // parent closes the gap that std::fs::create_dir_all leaves open;
    // safe_fs::write itself uses openat2 with NO_SYMLINKS so the final
    // open cannot follow a leaf symlink either (live or dangling).
    let rel_path = Path::new(path);
    if let Some(parent) = rel_path.parent() {
        if !parent.as_os_str().is_empty() {
            crate::safe_fs::assert_no_symlink_traversal(root_fd, parent)?;
            let parent_abs = root.join(parent);
            std::fs::create_dir_all(&parent_abs)?;
        }
    }
    crate::safe_fs::write(root_fd, rel_path, blob.content())?;

    // commit_all returns None when the file already matched HEAD (revert
    // of unchanged content); surface the existing HEAD oid as the caller
    // contract is "return the relevant commit id".
    match commit_all(root, &format!("revert {path} to HEAD"))? {
        Some(oid) => Ok(oid),
        None => {
            let repo =
                Repository::open(root).map_err(|e| MemoryError::Other(format!("git open: {e}")))?;
            let head = repo
                .head()
                .map_err(|e| MemoryError::Other(format!("git head: {e}")))?
                .target()
                .ok_or_else(|| MemoryError::Other("git head detached".to_string()))?;
            Ok(head.to_string())
        }
    }
}

/// Lightweight handle MemoryService can hold. Today this is just config —
/// every operation re-opens the repo. That's fine: git2 is fast enough,
/// and avoids long-lived open handles across tokio tasks.
///
/// The `commit_mutex` serializes every entry point that opens the repo
/// for writing (auto_commit_for, revert). Without it, concurrent MCP
/// tool calls race on git's index.lock file: the loser used to be
/// silently dropped at `debug!` and the user lost commits.
pub struct GitHandle {
    pub config: GitConfig,
    pub root: std::path::PathBuf,
    commit_mutex: std::sync::Mutex<()>,
}

impl GitHandle {
    pub fn open(config: GitConfig, root: &Path) -> Result<Option<Arc<Self>>> {
        if !config.enabled {
            return Ok(None);
        }
        init(root)?;
        Ok(Some(Arc::new(Self {
            config,
            root: root.to_path_buf(),
            commit_mutex: std::sync::Mutex::new(()),
        })))
    }

    /// Best-effort auto-commit driven by an AuditEntry. Errors surface at
    /// `warn!` so operators can see them in journald — losing a commit
    /// because of a transient lock conflict used to be invisible.
    ///
    /// NOTE on synchronicity: git2 commit + index.lock fsync is blocking
    /// I/O; this runs inline on the caller's tokio worker. Measured cost
    /// on ext4 is sub-100 ms for typical mounts, acceptable for the
    /// current stdio (single-client) usage. For multi-client / HTTP
    /// transports the future move is to a dedicated git worker thread
    /// with a bounded channel (TODO(memory-git): P6.6); this signature
    /// stays as-is so the migration is internal.
    pub fn auto_commit_for(&self, entry: &AuditEntry) {
        if !self.config.auto_commit {
            return;
        }
        if !entry.ok {
            return; // don't commit on failed tool calls
        }
        // Only commit for write-side tools; reads shouldn't bump HEAD.
        if !is_write_tool(entry.tool) {
            return;
        }
        let msg = if entry.path.is_empty() {
            entry.tool.to_string()
        } else {
            format!("{} {}", entry.tool, entry.path)
        };
        let _g = self.commit_mutex.lock().unwrap_or_else(|p| p.into_inner());
        match commit_all(&self.root, &msg) {
            Ok(Some(_oid)) => {}
            Ok(None) => {
                // Tree identical to HEAD — typical for mem_write of unchanged
                // content. Silently skip; the audit log is the source of
                // truth, git is the secondary tape.
                tracing::debug!(
                    "auto-commit for {} {} skipped: no tree change",
                    entry.tool,
                    entry.path
                );
            }
            Err(e) => {
                tracing::warn!("auto-commit for {} {} failed: {e}", entry.tool, entry.path);
            }
        }
    }

    /// Restore `path` to its content at HEAD, then commit the revert.
    /// Holds the same mutex as auto_commit_for so the two never race on
    /// git's index.lock. `root_fd` is the mount's O_PATH dirfd used to
    /// route the blob write through `safe_fs`.
    pub fn revert(&self, root_fd: BorrowedFd<'_>, path: &str) -> Result<String> {
        let _g = self.commit_mutex.lock().unwrap_or_else(|p| p.into_inner());
        revert(&self.root, root_fd, path)
    }
}

fn is_write_tool(name: &str) -> bool {
    matches!(
        name,
        "mem_write"
            | "mem_append"
            | "mem_edit"
            | "mem_mkdir"
            | "mem_remove"
            | "mem_promote"
            | "memory_observe"
            | "mem_snapshot_restore"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn init_creates_repo_and_gitignore() {
        let tmp = tempdir().unwrap();
        std::fs::write(tmp.path().join("seed.md"), "hello").unwrap();
        init(tmp.path()).unwrap();
        assert!(tmp.path().join(".git").is_dir());
        assert!(tmp.path().join(".gitignore").exists());
        let body = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert!(body.contains(".anolisa/"));
    }

    #[test]
    fn commit_all_records_changes() {
        let tmp = tempdir().unwrap();
        std::fs::write(tmp.path().join("a.md"), "alpha").unwrap();
        init(tmp.path()).unwrap();

        std::fs::write(tmp.path().join("a.md"), "alpha v2").unwrap();
        let h = commit_all(tmp.path(), "v2").unwrap();
        let h = h.expect("commit should produce an oid when tree changed");
        assert_eq!(h.len(), 40); // SHA-1 hex

        let entries = log(tmp.path(), 10, None).unwrap();
        assert!(entries.len() >= 2);
        assert_eq!(entries[0].summary, "v2");
    }

    #[test]
    fn commit_all_skips_empty_commits() {
        let tmp = tempdir().unwrap();
        std::fs::write(tmp.path().join("a.md"), "stable").unwrap();
        init(tmp.path()).unwrap();

        // No file change between this commit_all and the initial seed:
        // the initial init() already committed "init"; calling commit_all
        // with no tree change must return None instead of creating an
        // empty commit.
        let result = commit_all(tmp.path(), "no-op").unwrap();
        assert!(
            result.is_none(),
            "expected None on unchanged tree, got {result:?}"
        );

        let entries = log(tmp.path(), 10, None).unwrap();
        // Only the initial "init" commit, no empty "no-op" entry.
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn revert_restores_previous_content() {
        use std::os::fd::AsFd;
        let tmp = tempdir().unwrap();
        std::fs::write(tmp.path().join("a.md"), "v1").unwrap();
        init(tmp.path()).unwrap();
        std::fs::write(tmp.path().join("a.md"), "v2").unwrap();
        commit_all(tmp.path(), "to v2").unwrap();
        std::fs::write(tmp.path().join("a.md"), "v3 (uncommitted)").unwrap();

        let root_fd = crate::safe_fs::open_root(tmp.path()).unwrap();
        let _ = revert(tmp.path(), root_fd.as_fd(), "a.md").unwrap();
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("a.md")).unwrap(),
            "v2"
        );
    }
}
