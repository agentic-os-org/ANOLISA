//! Sandbox policy types for linux-sandbox.
//!
//! This module contains all policy-related types needed to configure
//! filesystem and network sandboxing.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tracing::error;

use crate::path::AbsolutePathBuf;

// ============================================================================
// Network Policy
// ============================================================================

/// Network sandbox policy (used by seccomp).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkSandboxPolicy {
    #[default]
    Restricted,
    Enabled,
}

impl NetworkSandboxPolicy {
    pub fn is_enabled(self) -> bool {
        matches!(self, NetworkSandboxPolicy::Enabled)
    }
}

impl FromStr for NetworkSandboxPolicy {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
    }
}

// ============================================================================
// Filesystem Policy
// ============================================================================

/// Access mode for a filesystem entry.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum FileSystemAccessMode {
    Read,
    Write,
    None,
}

impl FileSystemAccessMode {
    pub fn can_read(self) -> bool {
        !matches!(self, FileSystemAccessMode::None)
    }

    pub fn can_write(self) -> bool {
        matches!(self, FileSystemAccessMode::Write)
    }
}

/// Special filesystem paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileSystemSpecialPath {
    Root,
    Minimal,
    CurrentWorkingDirectory,
    ProjectRoots {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subpath: Option<PathBuf>,
    },
    Tmpdir,
    SlashTmp,
    Unknown {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subpath: Option<PathBuf>,
    },
}

/// Filesystem path specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FileSystemPath {
    Path { path: AbsolutePathBuf },
    Special { value: FileSystemSpecialPath },
}

/// A single filesystem sandbox entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileSystemSandboxEntry {
    pub path: FileSystemPath,
    pub access: FileSystemAccessMode,
}

/// Kind of filesystem sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum FileSystemSandboxKind {
    #[default]
    Restricted,
    Unrestricted,
    ExternalSandbox,
}

/// Filesystem sandbox policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileSystemSandboxPolicy {
    pub kind: FileSystemSandboxKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<FileSystemSandboxEntry>,
}

impl Default for FileSystemSandboxPolicy {
    fn default() -> Self {
        Self {
            kind: FileSystemSandboxKind::Restricted,
            entries: vec![FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Read,
            }],
        }
    }
}

impl FileSystemSandboxPolicy {
    /// Create a restricted policy with given entries.
    pub fn restricted(entries: Vec<FileSystemSandboxEntry>) -> Self {
        Self {
            kind: FileSystemSandboxKind::Restricted,
            entries,
        }
    }

    /// Create an unrestricted policy (full disk access).
    pub fn unrestricted() -> Self {
        Self {
            kind: FileSystemSandboxKind::Unrestricted,
            entries: vec![],
        }
    }

    pub fn has_full_disk_write_access(&self) -> bool {
        matches!(
            self.kind,
            FileSystemSandboxKind::Unrestricted | FileSystemSandboxKind::ExternalSandbox
        )
    }

    pub fn has_full_disk_read_access(&self) -> bool {
        if !matches!(self.kind, FileSystemSandboxKind::Restricted) {
            return true;
        }
        self.entries.iter().any(|entry| {
            matches!(
                &entry.path,
                FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root
                }
            ) && entry.access.can_read()
        })
    }

    pub fn include_platform_defaults(&self) -> bool {
        // For now, always include platform defaults for restricted policies
        matches!(self.kind, FileSystemSandboxKind::Restricted)
    }

    pub fn get_readable_roots_with_cwd(&self, cwd: &Path) -> Vec<AbsolutePathBuf> {
        if self.has_full_disk_read_access() {
            return Vec::new();
        }

        let mut roots: Vec<AbsolutePathBuf> = self
            .entries
            .iter()
            .filter(|e| e.access.can_read())
            .filter_map(|e| self.resolve_path(&e.path, cwd))
            .collect();

        // Add cwd
        if let Ok(cwd_path) = AbsolutePathBuf::from_absolute_path(cwd) {
            roots.push(cwd_path);
        }

        let mut seen = HashSet::new();
        roots.retain(|root| seen.insert(root.to_path_buf()));
        roots
    }

    pub fn get_writable_roots_with_cwd(&self, cwd: &Path) -> Vec<WritableRoot> {
        let mut roots: Vec<AbsolutePathBuf> = self
            .entries
            .iter()
            .filter(|e| e.access.can_write())
            .filter_map(|e| self.resolve_path(&e.path, cwd))
            .collect();

        // For CWD entries, add cwd
        for entry in &self.entries {
            if entry.access.can_write()
                && let FileSystemPath::Special {
                    value: FileSystemSpecialPath::CurrentWorkingDirectory,
                } = &entry.path
                && let Ok(cwd_path) = AbsolutePathBuf::from_absolute_path(cwd)
            {
                roots.push(cwd_path);
            }
        }

        roots
            .into_iter()
            .map(|writable_root| WritableRoot {
                read_only_subpaths: default_read_only_subpaths_for_writable_root(&writable_root),
                root: writable_root,
            })
            .collect()
    }

    pub fn get_unreadable_roots_with_cwd(&self, cwd: &Path) -> Vec<AbsolutePathBuf> {
        self.entries
            .iter()
            .filter(|e| matches!(e.access, FileSystemAccessMode::None))
            .filter_map(|e| self.resolve_path(&e.path, cwd))
            .collect()
    }

    pub fn needs_direct_runtime_enforcement(
        &self,
        _network_sandbox_policy: NetworkSandboxPolicy,
        _cwd: &Path,
    ) -> bool {
        // Check if there are any entries that need runtime enforcement
        // (e.g., mixed read/write entries on same paths)
        if !matches!(self.kind, FileSystemSandboxKind::Restricted) {
            return false;
        }

        // If there are any None entries or complex patterns, need runtime enforcement
        self.entries
            .iter()
            .any(|e| matches!(e.access, FileSystemAccessMode::None))
    }

    fn resolve_path(&self, path: &FileSystemPath, cwd: &Path) -> Option<AbsolutePathBuf> {
        match path {
            FileSystemPath::Path { path } => Some(path.clone()),
            FileSystemPath::Special { value } => match value {
                FileSystemSpecialPath::Root => AbsolutePathBuf::from_absolute_path("/").ok(),
                FileSystemSpecialPath::CurrentWorkingDirectory => {
                    AbsolutePathBuf::from_absolute_path(cwd).ok()
                }
                FileSystemSpecialPath::SlashTmp => AbsolutePathBuf::from_absolute_path("/tmp").ok(),
                FileSystemSpecialPath::Tmpdir => std::env::var_os("TMPDIR")
                    .and_then(|p| AbsolutePathBuf::from_absolute_path(PathBuf::from(p)).ok()),
                _ => None,
            },
        }
    }
}

impl FromStr for FileSystemSandboxPolicy {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
    }
}

// ============================================================================
// Writable Root
// ============================================================================

/// A writable root path with read-only subpaths.
#[derive(Debug, Clone, PartialEq, Eq, JsonSchema)]
pub struct WritableRoot {
    pub root: AbsolutePathBuf,
    pub read_only_subpaths: Vec<AbsolutePathBuf>,
}

impl WritableRoot {
    pub fn is_path_writable(&self, path: &Path) -> bool {
        if !path.starts_with(&self.root) {
            return false;
        }
        for subpath in &self.read_only_subpaths {
            if path.starts_with(subpath) {
                return false;
            }
        }
        true
    }
}

// ============================================================================
// Helper functions
// ============================================================================

fn default_read_only_subpaths_for_writable_root(
    writable_root: &AbsolutePathBuf,
) -> Vec<AbsolutePathBuf> {
    let mut subpaths: Vec<AbsolutePathBuf> = Vec::new();

    #[allow(clippy::expect_used)]
    let top_level_git = writable_root
        .join(".git")
        .expect(".git is a valid relative path");

    let top_level_git_is_file = top_level_git.as_path().is_file();
    let top_level_git_is_dir = top_level_git.as_path().is_dir();

    if top_level_git_is_dir || top_level_git_is_file {
        if top_level_git_is_file
            && is_git_pointer_file(&top_level_git)
            && let Some(gitdir) = resolve_gitdir_from_file(&top_level_git)
        {
            subpaths.push(gitdir);
        }
        subpaths.push(top_level_git);
    }

    // Make .agents and .copilot-shell read-only
    for subdir in &[".agents", ".copilot-shell"] {
        #[allow(clippy::expect_used)]
        let top_level_copilot = writable_root.join(subdir).expect("valid relative path");
        if top_level_copilot.as_path().is_dir() {
            subpaths.push(top_level_copilot);
        }
    }

    let mut deduped = Vec::with_capacity(subpaths.len());
    let mut seen = HashSet::new();
    for path in subpaths {
        if seen.insert(path.to_path_buf()) {
            deduped.push(path);
        }
    }
    deduped
}

fn is_git_pointer_file(path: &AbsolutePathBuf) -> bool {
    path.as_path().is_file() && path.as_path().file_name() == Some(OsStr::new(".git"))
}

fn resolve_gitdir_from_file(dot_git: &AbsolutePathBuf) -> Option<AbsolutePathBuf> {
    let contents = match std::fs::read_to_string(dot_git.as_path()) {
        Ok(contents) => contents,
        Err(err) => {
            error!(
                "Failed to read {path} for gitdir pointer: {err}",
                path = dot_git.as_path().display()
            );
            return None;
        }
    };

    let trimmed = contents.trim();
    let (_, gitdir_raw) = match trimmed.split_once(':') {
        Some(parts) => parts,
        None => {
            error!(
                "Expected {path} to contain a gitdir pointer",
                path = dot_git.as_path().display()
            );
            return None;
        }
    };

    let gitdir_raw = gitdir_raw.trim();
    if gitdir_raw.is_empty() {
        error!(
            "Expected {path} gitdir pointer was empty",
            path = dot_git.as_path().display()
        );
        return None;
    }

    let base = match dot_git.as_path().parent() {
        Some(base) => base,
        None => {
            error!(
                "Unable to resolve parent for {path}",
                path = dot_git.as_path().display()
            );
            return None;
        }
    };

    let gitdir_path = match AbsolutePathBuf::resolve_path_against_base(gitdir_raw, base) {
        Ok(path) => path,
        Err(err) => {
            error!(
                "Failed to resolve gitdir path {gitdir_raw} from {path}: {err}",
                path = dot_git.as_path().display()
            );
            return None;
        }
    };

    if !gitdir_path.as_path().exists() {
        error!(
            "Resolved gitdir path {path} does not exist",
            path = gitdir_path.as_path().display()
        );
        return None;
    }

    Some(gitdir_path)
}
