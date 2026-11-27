//! Bounded, metadata-only first-input inventory of workspace-relative paths.

use std::ffi::OsString;
use std::fs::{File, Metadata};
use std::os::fd::AsRawFd;
use std::os::unix::fs::DirEntryExt;
use std::process::Command;

use semantic_arbiter::deadline::Ingress;
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use super::fs::{self, Identity};

const PATH_LIMIT: usize = 256;
const PATH_BYTES: usize = 32 * 1024;
const MAX_DEPTH: usize = 12;
// Leave room for worst-case JSON escaping of all 16 KiB of task text, 64 turn
// digests and framing, without increasing storage's existing 128 KiB quota.
const ENCODED_LIMIT: usize = 20 * 1024;
const EXCLUDED: &[&str] = &[".git", "node_modules", "target", ".venv"];
const GIT: &str = "/usr/bin/git";

/// Bounded first-submit inventory, not an atomic whole-tree snapshot.
/// Changes after a subtree's final check and inode reuse can escape detection.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Inventory {
    captured_at_ms: u64,
    scope: Scope,
    root: Option<Identity>,
    complete: bool,
    entries: Vec<Entry>,
    gaps: Vec<String>,
    /// HEAD commit at session start; presence selects per-query git existence checks.
    git_head: Option<String>,
}

#[derive(Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Scope {
    #[default]
    Unknown,
    // Persisted root-only inventories must never imply recursive coverage.
    RootChildren,
    Recursive,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: String,
    identity: Identity,
}

impl Inventory {
    /// Enumerates confined paths without reading content, executing commands,
    /// writing to the workspace or following symlink targets.
    pub(crate) fn capture(workspace: &str, ingress: Ingress, deadline: Instant) -> Self {
        Self::capture_with(workspace, ingress, deadline, |_| {})
    }

    /// Runs a callback before each directory's final stability check, allowing
    /// deterministic mutation fixtures without changing the filesystem reader.
    pub(super) fn capture_with(
        workspace: &str,
        ingress: Ingress,
        deadline: Instant,
        after_directory: impl FnMut(&str),
    ) -> Self {
        let mut result = Self {
            captured_at_ms: ingress.now_ms(),
            ..Self::default()
        };
        let root = match fs::root(workspace, deadline) {
            Ok(root) => root,
            Err(code) => {
                result.gap(code);
                return result;
            }
        };
        let before = match root.metadata() {
            Ok(meta) => meta,
            Err(_) => {
                result.gap("inventory_stat_failed");
                return result;
            }
        };
        result.root = Some(Identity::of(&before));
        if let Some(head) = git_head(workspace, deadline) {
            result.git_head = Some(head);
            result.scope = Scope::Recursive;
            result.complete = true;
            return result;
        }
        result.scope = Scope::Recursive;
        result.complete = true;
        Scan {
            inventory: &mut result,
            root: &root,
            deadline,
            visited: 0,
            path_bytes: 0,
            exhausted: false,
            after_directory,
        }
        .directory(".", &before, 0);
        // The pinned root may have been renamed/replaced at its workspace name.
        let named_root = fs::root(workspace, deadline)
            .and_then(|root| root.metadata().map_err(|_| "inventory_stat_failed"));
        match named_root {
            Ok(after) if fs::stable(&before, &after) => {}
            other => {
                result.entries.clear();
                result.incomplete(other.err().unwrap_or("inventory_changed"));
            }
        }
        if let Err(code) = fs::timely(deadline) {
            result.entries.clear();
            result.incomplete(code);
        }
        result.bound_encoded();
        result
    }

    fn gap(&mut self, code: &str) {
        if !self.gaps.iter().any(|gap| gap == code) {
            self.gaps.push(code.into());
        }
    }

    fn incomplete(&mut self, code: &str) {
        self.complete = false;
        self.gap(code);
    }

    fn bound_encoded(&mut self) {
        while serde_json::to_vec(self).map_or(true, |bytes| bytes.len() > ENCODED_LIMIT) {
            self.incomplete("inventory_encoded_limit");
            if self.entries.pop().is_none() {
                break;
            }
        }
    }

    pub(super) fn gaps(&self) -> impl Iterator<Item = &str> {
        self.gaps.iter().map(String::as_str)
    }

    pub(super) fn existed(
        &self,
        root: &Metadata,
        path: &str,
        current: Option<&Metadata>,
    ) -> Option<bool> {
        if self.git_head.is_some() {
            return None;
        }
        if self.scope == Scope::Unknown
            || self.root != Some(Identity::of(root))
            || fs::relative(path, "").as_deref() != Some(path)
            || (self.scope == Scope::RootChildren && path.contains('/'))
        {
            return None;
        }
        if let Some(entry) = self.entries.iter().find(|entry| entry.path == path) {
            // A replacement inode is not asserted to be the original object.
            return current
                .filter(|meta| Identity::of(meta) == entry.identity)
                .map(|_| true);
        }
        (self.scope == Scope::Recursive && self.complete && self.gaps.is_empty()).then_some(false)
    }

    pub(crate) fn is_git(&self) -> bool {
        self.git_head.is_some()
    }

    /// Per-query existence via git index; no full-tree scan or stored file list.
    pub(crate) fn existed_git(
        &self,
        root: &Metadata,
        workspace: &str,
        path: &str,
        deadline: Instant,
    ) -> Option<bool> {
        if self.root != Some(Identity::of(root)) || fs::relative(path, "").as_deref() != Some(path)
        {
            return None;
        }
        if fs::timely(deadline).is_err() {
            return None;
        }
        let tracked = Command::new(GIT)
            .args(["-C", workspace, "ls-files", "--error-unmatch", "--", path])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        if fs::timely(deadline).is_err() {
            return None;
        }
        if tracked.success() {
            return Some(true);
        }
        let untracked = Command::new(GIT)
            .args([
                "-C",
                workspace,
                "ls-files",
                "--others",
                "--exclude-standard",
                "--",
                path,
            ])
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        if fs::timely(deadline).is_err() {
            return None;
        }
        if untracked.status.success() && !untracked.stdout.is_empty() {
            return Some(true);
        }
        None
    }
}

struct Scan<'a, F> {
    inventory: &'a mut Inventory,
    root: &'a File,
    deadline: Instant,
    visited: usize,
    path_bytes: usize,
    exhausted: bool,
    after_directory: F,
}

impl<F: FnMut(&str)> Scan<'_, F> {
    fn directory(&mut self, path: &str, before: &Metadata, depth: usize) {
        let start = self.inventory.entries.len();
        if depth != 0 {
            self.record(path, before);
        }
        if let Err(code) = self.read_directory(path, before, depth) {
            // No initial-presence claim survives an unstable or unverified
            // subtree. Budget consumption is deliberately not refunded.
            self.inventory.entries.truncate(start);
            self.inventory.incomplete(code);
        }
    }

    fn read_directory(
        &mut self,
        path: &str,
        before: &Metadata,
        depth: usize,
    ) -> Result<(), &'static str> {
        fs::timely(self.deadline)?;
        // Reopen every full relative path from the original root, never from a
        // child descriptor that could have been moved outside the workspace.
        let directory = fs::open_beneath(self.root, path, libc::O_RDONLY | libc::O_DIRECTORY)
            .map_err(|_| "inventory_directory_open_failed")?;
        let opened = directory.metadata().map_err(|_| "inventory_stat_failed")?;
        if !fs::stable(before, &opened) {
            return Err("inventory_changed");
        }
        fs::timely(self.deadline)?;
        // This procfs link denotes our live fd, not a workspace-controlled
        // name. Only names, never DirEntry paths or metadata, are consumed.
        let mut entries = std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))
            .map_err(|_| "inventory_enumeration_failed")?;
        let mut members = Vec::new();
        while !self.exhausted {
            fs::timely(self.deadline)?;
            // Conservatively stop at the cap without visiting one extra entry
            // to establish EOF. Exclusions and invalid names also consume it.
            if self.visited >= PATH_LIMIT {
                self.exhausted = true;
                self.inventory.incomplete("inventory_path_limit");
                break;
            }
            let Some(entry) = entries.next() else { break };
            self.visited += 1;
            let entry = entry.map_err(|_| "inventory_enumeration_failed")?;
            let name = entry.file_name();
            let bytes = name.as_encoded_bytes().len() + if depth == 0 { 0 } else { path.len() + 1 };
            if self.path_bytes + bytes > PATH_BYTES {
                self.exhausted = true;
                self.inventory.incomplete("inventory_path_bytes");
                break;
            }
            self.path_bytes += bytes;
            members.push((name.clone(), entry.ino()));
            let Some(name) = name.to_str() else {
                self.inventory.incomplete("inventory_non_utf8");
                continue;
            };
            if EXCLUDED.contains(&name) {
                self.inventory.incomplete("inventory_excluded");
                continue;
            }
            let child = if depth == 0 {
                name.into()
            } else {
                format!("{path}/{name}")
            };
            fs::timely(self.deadline)?;
            let meta = match fs::metadata(self.root, &child) {
                Ok(meta) => meta,
                Err(_) => {
                    self.inventory.incomplete("inventory_stat_failed");
                    continue;
                }
            };
            if meta.is_dir() && depth + 1 < MAX_DEPTH {
                self.directory(&child, &meta, depth + 1);
            } else {
                self.record(&child, &meta);
                if meta.is_dir() {
                    self.inventory.incomplete("inventory_depth_limit");
                } else if meta.file_type().is_symlink() {
                    // The target's type is unknown; it might hide a subtree.
                    self.inventory.incomplete("inventory_symlink");
                }
            }
        }
        if self.exhausted {
            return Err("inventory_verification_incomplete");
        }
        (self.after_directory)(path);
        self.verify_members(&directory, members)?;
        fs::timely(self.deadline)?;
        let after = directory.metadata().map_err(|_| "inventory_stat_failed")?;
        fs::timely(self.deadline)?;
        let named = fs::metadata(self.root, path).map_err(|_| "inventory_changed")?;
        fs::timely(self.deadline)?;
        if !fs::stable(before, &after) || !fs::stable(&after, &named) {
            return Err("inventory_changed");
        }
        Ok(())
    }

    fn verify_members(
        &mut self,
        directory: &File,
        mut members: Vec<(OsString, u64)>,
    ) -> Result<(), &'static str> {
        // Directory timestamps can stay equal across rapid membership changes.
        // Compare names/inodes too, charging every recheck to the global cap.
        fs::timely(self.deadline)?;
        let mut entries = std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))
            .map_err(|_| "inventory_enumeration_failed")?;
        loop {
            fs::timely(self.deadline)?;
            if self.visited >= PATH_LIMIT {
                self.exhausted = true;
                return Err("inventory_path_limit");
            }
            let Some(entry) = entries.next() else { break };
            self.visited += 1;
            let entry = entry.map_err(|_| "inventory_enumeration_failed")?;
            let name = entry.file_name();
            let Some(index) = members
                .iter()
                .position(|(old, ino)| *old == name && *ino == entry.ino())
            else {
                return Err("inventory_changed");
            };
            members.swap_remove(index);
        }
        if !members.is_empty() {
            return Err("inventory_changed");
        }
        Ok(())
    }

    fn record(&mut self, path: &str, meta: &Metadata) {
        self.inventory.entries.push(Entry {
            path: path.into(),
            identity: Identity::of(meta),
        });
    }
}

/// Resolves HEAD via the system git binary; returns None for non-repos or failures.
fn git_head(workspace: &str, deadline: Instant) -> Option<String> {
    if fs::timely(deadline).is_err() {
        return None;
    }
    let output = Command::new(GIT)
        .args(["-C", workspace, "rev-parse", "HEAD"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let head = String::from_utf8(output.stdout).ok()?;
    let head = head.trim();
    if head.len() == 40 && head.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(head.to_owned())
    } else {
        None
    }
}
