use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};

use super::load::copilot_shell_cosh_dir;
use super::CoshConfig;

pub fn trust_project_root(root: &Path) -> Result<(), String> {
    let path = project_trust_store_path()
        .ok_or_else(|| "HOME is not set; cannot persist trust".to_string())?;
    add_trusted_project_root_to_store_path(&path, root)
}

pub fn untrust_project_root(root: &Path) -> Result<(), String> {
    let path = project_trust_store_path()
        .ok_or_else(|| "HOME is not set; cannot persist trust".to_string())?;
    remove_trusted_project_root_from_store_path(&path, root)
}

pub fn clear_project_trust_store() -> Result<(), String> {
    let path = project_trust_store_path()
        .ok_or_else(|| "HOME is not set; cannot persist trust".to_string())?;
    write_trusted_project_roots_to_store_path(&path, &[])
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
    let roots = read_trusted_project_roots_from_store_path(path)?;
    config.trusted_project_roots.extend(roots);
    Ok(())
}

pub(super) fn read_trusted_project_roots_from_store_path(
    path: &Path,
) -> Result<Vec<PathBuf>, String> {
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
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(PathBuf::from)
        .collect())
}

pub(super) fn add_trusted_project_root_to_store_path(
    path: &Path,
    root: &Path,
) -> Result<(), String> {
    let root = canonical_project_root(root);
    let mut roots = read_trusted_project_roots_from_store_path(path)?
        .into_iter()
        .map(|root| canonical_project_root(&root))
        .collect::<Vec<_>>();
    if !roots.iter().any(|existing| existing == &root) {
        roots.push(root);
    }
    write_trusted_project_roots_to_store_path(path, &roots)
}

pub(super) fn remove_trusted_project_root_from_store_path(
    path: &Path,
    root: &Path,
) -> Result<(), String> {
    let root = canonical_project_root(root);
    let mut roots = read_trusted_project_roots_from_store_path(path)?
        .into_iter()
        .map(|root| canonical_project_root(&root))
        .collect::<Vec<_>>();
    roots.retain(|existing| existing != &root);
    write_trusted_project_roots_to_store_path(path, &roots)
}

pub(super) fn write_trusted_project_roots_to_store_path(
    path: &Path,
    roots: &[PathBuf],
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
    for root in roots {
        content.push_str(&root.to_string_lossy());
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
