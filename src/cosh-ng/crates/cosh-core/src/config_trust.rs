//! Project-layer trust, shared with cosh-shell's interactive hook trust
//! flow. A workspace the user explicitly trusted from cosh-shell
//! (`trusted-project-hooks` store) also unlocks project hooks in
//! headless cosh-core; every other workspace fails closed.

use std::path::{Path, PathBuf};

use crate::config::config_dir;

/// Location of the shared project trust store: the
/// `COSH_SHELL_PROJECT_TRUST_STORE` override first, then
/// `<config_dir>/cosh/trusted-project-hooks` — the same file
/// cosh-shell's `trust_project_root`/`untrust_project_root` maintain.
pub(crate) fn project_trust_store_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("COSH_SHELL_PROJECT_TRUST_STORE") {
        return Some(PathBuf::from(path));
    }
    Some(config_dir().join("cosh").join("trusted-project-hooks"))
}

/// Reads one canonical workspace root per line; blank lines and `#`
/// comments are skipped. A symlinked or non-regular store is rejected so
/// the trust decision cannot be redirected by another local user.
pub(crate) fn read_trusted_project_roots(path: &Path) -> Result<Vec<PathBuf>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "inspect project trust store failed for {}: {error}",
                path.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "project trust store is a symbolic link: {}",
            path.display()
        ));
    }
    if !metadata.file_type().is_file() {
        return Err(format!(
            "project trust store is not a regular file: {}",
            path.display()
        ));
    }
    let bytes = std::fs::read(path).map_err(|error| {
        format!(
            "read project trust store failed for {}: {error}",
            path.display()
        )
    })?;
    let content = String::from_utf8(bytes)
        .map_err(|_| format!("project trust store is not valid UTF-8: {}", path.display()))?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(PathBuf::from)
        .collect())
}

/// Whether `workspace` is one of the explicitly trusted roots. Every
/// store failure (unreadable, symlinked, non-UTF-8) fails closed — after
/// reporting the failure, so a workspace the user explicitly trusted does
/// not silently lose its hooks with no way to diagnose the cause.
pub(crate) fn workspace_is_trusted(workspace: &Path, store_path: &Path) -> bool {
    match read_trusted_project_roots(store_path) {
        Ok(roots) => {
            let workspace = canonical(workspace);
            roots
                .iter()
                .map(|root| canonical(root))
                .any(|trusted| trusted == workspace)
        }
        Err(reason) => {
            // Fail closed, but never silently: without this report the
            // caller only says "untrusted project" (or nothing), and the
            // operator cannot tell a never-trusted workspace from a broken
            // store. This runs once per config load, at the same choke
            // point as the other project-layer warnings.
            eprintln!("[cosh-core] Warning: treating the project layer as untrusted: {reason}");
            false
        }
    }
}

/// Resolves the shared trust store and answers for `workspace`; an absent
/// store means no workspace is trusted.
pub(crate) fn workspace_trusted_via_shared_store(workspace: &Path) -> bool {
    project_trust_store_path().is_some_and(|store| workspace_is_trusted(workspace, &store))
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_matches_canonical_roots_and_skips_comments() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("repo");
        std::fs::create_dir_all(&workspace).unwrap();
        let store = dir.path().join("store");
        std::fs::write(
            &store,
            format!(
                "# trusted workspaces\n\n{}\n  {}  \n",
                workspace.canonicalize().unwrap().display(),
                workspace.display()
            ),
        )
        .unwrap();

        assert!(workspace_is_trusted(&workspace, &store));
        let sibling = dir.path().join("other");
        std::fs::create_dir_all(&sibling).unwrap();
        assert!(!workspace_is_trusted(&sibling, &store));
    }

    #[test]
    fn absent_store_and_symlinked_store_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("repo");
        std::fs::create_dir_all(&workspace).unwrap();

        assert!(!workspace_is_trusted(
            &workspace,
            &dir.path().join("missing-store")
        ));

        let target = dir.path().join("target-store");
        std::fs::write(&target, format!("{}\n", workspace.display())).unwrap();
        let symlinked = dir.path().join("symlinked-store");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &symlinked).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, &symlinked).unwrap();

        assert!(!workspace_is_trusted(&workspace, &symlinked));
    }
}
