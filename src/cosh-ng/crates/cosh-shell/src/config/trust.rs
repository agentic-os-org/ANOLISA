use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};

use super::load::copilot_shell_cosh_dir;
use super::{ConsentedProjectHook, CoshConfig};

/// Whether a project's `.cosh/hooks` tree can be pinned at all.
///
/// The loader opens `.cosh` and `hooks` with `O_NOFOLLOW` and refuses a
/// symlinked component, so a tree reached through one can never be fingerprinted
/// or executed. Consent for it is refused rather than recorded as an approval of
/// content nobody can pin.
fn project_hooks_tree_is_pinnable(project_root: &Path) -> bool {
    is_real_directory(&project_root.join(".cosh"))
        && is_real_directory(&project_root.join(".cosh/hooks"))
}

fn is_real_directory(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(metadata) => metadata.file_type().is_dir(),
        Err(_) => false,
    }
}

/// Whether a project's hooks may run under consent that is already recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectHookTrust {
    /// Approved content is unchanged, or an administrator policy root covers it.
    Trusted,
    /// A consent entry exists but does not cover the current content, so the
    /// hooks stay withheld until the user consents again.
    ContentChanged,
    /// No consent entry exists for this project.
    Untrusted,
}

/// Decides project hook trust from both consent sources.
///
/// `policy_roots` are administrator-declared roots. They can only come from
/// `~/.copilot-shell/config.toml`, which a project cannot write, so they keep
/// covering content updates; that is the one path with no fingerprint check.
/// `consented` are user approvals recorded in the trust store; those bind the
/// approved content, and a legacy entry without a fingerprint never counts as
/// consent for content nobody approved.
///
/// `pinned_digest` is the fingerprint of the hooks the loader has *already
/// pinned* for this project, from [`HookEngine::project_hooks_pinned_digest`].
/// It is passed in rather than recomputed here so the bytes being compared
/// against the approval are the same objects that would run: measuring the tree
/// by path again would reopen a second copy and leave a swap window between the
/// decision and the execution.
///
/// [`HookEngine::project_hooks_pinned_digest`]: crate::hooks::HookEngine::project_hooks_pinned_digest
pub(crate) fn project_hook_trust(
    project_root: &Path,
    policy_roots: &[PathBuf],
    consented: &[ConsentedProjectHook],
    pinned_digest: Option<&str>,
) -> ProjectHookTrust {
    let project_root = canonical_project_root(project_root);
    if policy_roots
        .iter()
        .map(|root| canonical_project_root(root))
        .any(|root| root == project_root)
    {
        return ProjectHookTrust::Trusted;
    }
    let matching: Vec<&ConsentedProjectHook> = consented
        .iter()
        .filter(|entry| canonical_project_root(&entry.root) == project_root)
        .collect();
    // A rollback leaves a bare legacy line beside the fingerprinted one for the
    // same root, so pick the fingerprinted entry deliberately instead of taking
    // whichever line happens to come first: the decision must not depend on
    // store line order.
    let Some(entry) = matching
        .iter()
        .find(|entry| entry.digest.is_some())
        .or_else(|| matching.first())
    else {
        return ProjectHookTrust::Untrusted;
    };
    let Some(approved) = entry.digest.as_deref() else {
        // A legacy path-only entry never counts as consent for content nobody
        // fingerprinted, so it must not be upgraded silently.
        return ProjectHookTrust::ContentChanged;
    };
    match pinned_digest {
        Some(current) if current == approved => ProjectHookTrust::Trusted,
        // A tree that cannot be pinned is not consent, so fail closed.
        _ => ProjectHookTrust::ContentChanged,
    }
}

pub fn trust_project_root(root: &Path, pinned_digest: Option<&str>) -> Result<(), String> {
    let path = project_trust_store_path()
        .ok_or_else(|| "HOME is not set; cannot persist trust".to_string())?;
    trust_project_root_to_store_path(&path, root, pinned_digest)
}

/// Records consent for `root` in the store at `path`.
///
/// `pinned_digest` is the fingerprint of the snapshot being approved. Split out
/// from [`trust_project_root`] so the failure path is testable without owning
/// the process environment.
pub(crate) fn trust_project_root_to_store_path(
    path: &Path,
    root: &Path,
    pinned_digest: Option<&str>,
) -> Result<(), String> {
    // Recording consent without a fingerprint would write a line that
    // `project_hook_trust` can never accept, so the user would be told the trust
    // was persisted while the hooks stay withheld on every later start.
    let Some(digest) = pinned_digest.filter(|_| project_hooks_tree_is_pinnable(root)) else {
        return Err(format!(
            "cannot fingerprint the .cosh/hooks tree of {}: it is missing, unreadable, \
             or contains a symbolic link",
            root.display()
        ));
    };
    add_consented_project_hook_to_store_path(path, root, Some(digest))
}

pub fn untrust_project_root(root: &Path) -> Result<(), String> {
    let path = project_trust_store_path()
        .ok_or_else(|| "HOME is not set; cannot persist trust".to_string())?;
    remove_consented_project_hook_from_store_path(&path, root)
}

pub fn clear_project_trust_store() -> Result<(), String> {
    let path = project_trust_store_path()
        .ok_or_else(|| "HOME is not set; cannot persist trust".to_string())?;
    write_consented_project_hooks_to_store_path(&path, &[])
}

pub(super) fn project_trust_store_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("COSH_SHELL_PROJECT_TRUST_STORE") {
        return Some(PathBuf::from(path));
    }
    copilot_shell_cosh_dir().map(|d| project_trust_store_path_in_dir(&d))
}

pub(super) fn project_trust_store_path_in_dir(cosh_dir: &Path) -> PathBuf {
    cosh_dir.join("trusted-project-hooks")
}

pub(super) fn load_project_trust_store(config: &mut CoshConfig, path: &Path) -> Result<(), String> {
    let entries = read_consented_project_hooks_from_store_path(path)?;
    config.consented_project_hooks.extend(entries);
    Ok(())
}

// Older Shell and Core path readers ignore comments; a versioned record can
// never become a path-level grant on rollback.
const PROJECT_HOOK_RECORD_PREFIX: &str = "# cosh-project-hook-v2 ";

pub(crate) fn read_consented_project_hooks_from_store_path(
    path: &Path,
) -> Result<Vec<ConsentedProjectHook>, String> {
    let metadata = match fs::symlink_metadata(path) {
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

    let bytes = fs::read(path).map_err(|error| {
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
        .filter(|line| !line.is_empty())
        .filter_map(parse_store_line)
        .collect())
}

fn parse_store_line(line: &str) -> Option<ConsentedProjectHook> {
    if let Some(record) = line.strip_prefix(PROJECT_HOOK_RECORD_PREFIX) {
        let (root, digest): (PathBuf, Option<String>) = serde_json::from_str(record).ok()?;
        if digest.as_ref().is_some_and(|value| {
            value.strip_prefix("sha256:").is_none_or(|hex| {
                hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        }) {
            return None;
        }
        return Some(ConsentedProjectHook { root, digest });
    }
    if line.starts_with('#') {
        return None;
    }
    Some(ConsentedProjectHook {
        root: PathBuf::from(line),
        digest: None,
    })
}

pub(crate) fn add_consented_project_hook_to_store_path(
    path: &Path,
    root: &Path,
    digest: Option<&str>,
) -> Result<(), String> {
    let root = canonical_project_root(root);
    let entry = ConsentedProjectHook {
        root: root.clone(),
        digest: digest.map(str::to_string),
    };
    let mut entries = read_consented_project_hooks_from_store_path(path)?
        .into_iter()
        .map(|existing| ConsentedProjectHook {
            root: canonical_project_root(&existing.root),
            ..existing
        })
        .collect::<Vec<_>>();
    // Drop every line for this root before appending, so a duplicate left behind
    // by a rollback does not survive the next write.
    entries.retain(|existing| existing.root != root);
    entries.push(entry);
    write_consented_project_hooks_to_store_path(path, &entries)
}

pub(super) fn remove_consented_project_hook_from_store_path(
    path: &Path,
    root: &Path,
) -> Result<(), String> {
    let root = canonical_project_root(root);
    let mut entries = read_consented_project_hooks_from_store_path(path)?
        .into_iter()
        .map(|existing| ConsentedProjectHook {
            root: canonical_project_root(&existing.root),
            ..existing
        })
        .collect::<Vec<_>>();
    entries.retain(|existing| existing.root != root);
    write_consented_project_hooks_to_store_path(path, &entries)
}

pub(super) fn write_consented_project_hooks_to_store_path(
    path: &Path,
    entries: &[ConsentedProjectHook],
) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("project trust store has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|err| format!("create trust store directory failed: {err}"))?;

    let existing = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(format!(
                "project trust store is a symbolic link: {}",
                path.display()
            ));
        }
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(format!(
                "project trust store is not a regular file: {}",
                path.display()
            ));
        }
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "inspect project trust store failed for {}: {error}",
                path.display()
            ));
        }
    };

    let mut content = String::new();
    content.push_str("# cosh-shell trusted project hook roots\n");
    for entry in entries {
        let root = entry.root.to_string_lossy();
        if entry.digest.is_none() && !root.contains(['\n', '\r']) {
            content.push_str(&root);
            content.push('\n');
            continue;
        }
        let record = serde_json::to_string(&(&entry.root, &entry.digest))
            .map_err(|error| format!("encode project hook consent failed: {error}"))?;
        content.push_str(PROJECT_HOOK_RECORD_PREFIX);
        content.push_str(&record);
        content.push('\n');
    }

    #[cfg(unix)]
    let mode = existing
        .as_ref()
        .map(|metadata| metadata.permissions().mode() & 0o7777)
        .unwrap_or(0o600);

    let mut temporary_path = None;
    let mut temporary_file = None;
    for _ in 0..16 {
        let candidate = temporary_store_path(path);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(mode);
        match options.open(&candidate) {
            Ok(file) => {
                #[cfg(unix)]
                if let Err(error) = file.set_permissions(fs::Permissions::from_mode(mode)) {
                    let _ = fs::remove_file(&candidate);
                    return Err(format!(
                        "set project trust store temporary file permissions failed: {error}"
                    ));
                }
                temporary_path = Some(candidate);
                temporary_file = Some(file);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "create project trust store temporary file failed: {error}"
                ));
            }
        }
    }
    let Some(temporary_path) = temporary_path else {
        return Err("create project trust store temporary file failed: name collision".to_string());
    };
    let Some(mut temporary_file) = temporary_file else {
        return Err("create project trust store temporary file failed: missing handle".to_string());
    };

    let result = (|| {
        temporary_file
            .write_all(content.as_bytes())
            .map_err(|error| format!("write project trust store failed: {error}"))?;
        temporary_file
            .sync_data()
            .map_err(|error| format!("sync project trust store failed: {error}"))?;
        drop(temporary_file);
        fs::rename(&temporary_path, path)
            .map_err(|error| format!("replace project trust store failed: {error}"))?;
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("sync project trust store directory failed: {error}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

#[cfg(unix)]
fn temporary_store_path(path: &Path) -> PathBuf {
    static NEXT_TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| std::borrow::Cow::Borrowed("trusted-project-hooks"));
    path.with_file_name(format!(".{name}.tmp-{}-{id}", std::process::id()))
}

#[cfg(not(unix))]
fn temporary_store_path(path: &Path) -> PathBuf {
    static NEXT_TEMPORARY_ID: std::sync::OnceLock<std::sync::atomic::AtomicU64> =
        std::sync::OnceLock::new();
    let id = NEXT_TEMPORARY_ID
        .get_or_init(|| std::sync::atomic::AtomicU64::new(0))
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| std::borrow::Cow::Borrowed("trusted-project-hooks"));
    path.with_file_name(format!(".{name}.tmp-{}-{id}", std::process::id()))
}

fn canonical_project_root(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}
