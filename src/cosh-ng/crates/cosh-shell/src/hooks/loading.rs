use std::path::{Path, PathBuf};

pub(crate) fn dirs_for_hook_loading() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let dir = user_hooks_dir_for_home(&PathBuf::from(home));
    if dir.is_dir() {
        Some(dir)
    } else {
        None
    }
}

pub(crate) fn user_hooks_dir_for_home(home: &Path) -> PathBuf {
    home.join(".copilot-shell/cosh/hooks")
}

pub(crate) fn project_hook_root_from_cwd(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|candidate| candidate.join(".cosh/hooks").is_dir())
        .map(canonical_project_root)
}

pub(crate) fn is_trusted_project_root(project_root: &Path, trusted_roots: &[PathBuf]) -> bool {
    let project_root = canonical_project_root(project_root);
    trusted_roots
        .iter()
        .map(|root| canonical_project_root(root))
        .any(|trusted_root| trusted_root == project_root)
}

fn canonical_project_root(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn user_hooks_dir_uses_copilot_shell_cosh_dir() {
        let home = PathBuf::from("/tmp/cosh-shell-home");

        assert_eq!(
            user_hooks_dir_for_home(&home),
            PathBuf::from("/tmp/cosh-shell-home/.copilot-shell/cosh/hooks")
        );
    }

    #[test]
    fn project_hook_root_from_cwd_walks_up_to_cosh_hooks() {
        let root = std::env::temp_dir().join("cosh-shell-project-root-walk");
        let nested = root.join("src/bin");
        let hooks_dir = root.join(".cosh/hooks");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&nested).expect("create nested dir");
        fs::create_dir_all(&hooks_dir).expect("create hooks dir");

        let found = project_hook_root_from_cwd(&nested).expect("project hook root");
        assert_eq!(found, root.canonicalize().expect("canonical root"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn project_hook_root_from_cwd_returns_none_without_hooks_dir() {
        let root = std::env::temp_dir().join("cosh-shell-project-root-missing");
        let nested = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&nested).expect("create nested dir");

        assert!(project_hook_root_from_cwd(&nested).is_none());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn trusted_project_root_matches_canonical_root() {
        let root = std::env::temp_dir().join("cosh-shell-project-root-trusted");
        let nested = root.join("src");
        let other = std::env::temp_dir().join("cosh-shell-project-root-other");
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&other);
        fs::create_dir_all(&nested).expect("create nested dir");
        fs::create_dir_all(&other).expect("create other dir");

        assert!(is_trusted_project_root(&root, &[nested.join("..")]));
        assert!(!is_trusted_project_root(
            &root,
            std::slice::from_ref(&other)
        ));

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&other);
    }
}
