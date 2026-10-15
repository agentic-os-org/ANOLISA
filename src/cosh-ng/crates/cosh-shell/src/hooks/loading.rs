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

/// Pins a project's hooks and trusts them only if the pinned snapshot matches
/// recorded consent.
///
/// The load comes first so the fingerprint covers the very descriptors that
/// would run. Deciding before the load would measure a second copy opened by
/// path, leaving a project that can write its own `.cosh/hooks` free to swap the
/// file in between: the old content would pass the consent check while the new
/// content got pinned and executed as trusted.
pub(crate) fn load_project_hooks_with_consent(
    engine: &mut crate::hooks::HookEngine,
    project_root: &Path,
    policy_roots: &[PathBuf],
    consented: &[crate::config::ConsentedProjectHook],
) -> crate::config::ProjectHookTrust {
    use crate::config::ProjectHookTrust;

    engine.load_project_hooks_from_root(project_root, false);
    let trust = crate::config::project_hook_trust(
        project_root,
        policy_roots,
        consented,
        engine.project_hooks_pinned_digest().as_deref(),
    );
    if trust == ProjectHookTrust::ContentChanged {
        tracing::warn!(
            target: "cosh_hooks",
            project = %project_root.display(),
            "project hook content differs from the approved fingerprint; \
             hooks withheld until `/hooks trust-project` runs again"
        );
    }
    engine.set_project_hooks_trusted(trust == ProjectHookTrust::Trusted);
    trust
}

fn canonical_project_root(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

#[cfg(test)]
#[path = "loading/tests.rs"]
mod tests;
