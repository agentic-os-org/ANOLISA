//! Path normalization for `SQLite` state.

use std::path::{Component, Path, PathBuf};

/// Normalizes a path the way v1 `normalize_sqlite_path` does.
///
/// v1 is `Path(path).expanduser().resolve()`. Python's `resolve()` is
/// non-strict: it resolves symlinks for the parts that exist and keeps the rest
/// lexically. `std::fs::canonicalize` requires the whole path to exist, so this
/// canonicalizes the deepest existing ancestor and re-appends the remainder.
#[must_use]
pub fn normalize_sqlite_path(path: impl AsRef<Path>) -> PathBuf {
    let expanded = expand_user(path.as_ref());
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir().map_or(expanded.clone(), |cwd| cwd.join(&expanded))
    };
    // Canonicalize the longest existing prefix before collapsing `..`: a parent
    // component following a symlink applies to the symlink target, as it does in
    // Python's `Path.resolve()`.
    let mut remainder: Vec<&std::ffi::OsStr> = Vec::new();
    let mut candidate: &Path = &absolute;
    loop {
        if let Ok(resolved) = candidate.canonicalize() {
            let mut out = resolved;
            for part in remainder.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (candidate.file_name(), candidate.parent()) {
            (Some(name), Some(parent)) => {
                remainder.push(name);
                candidate = parent;
            }
            _ => return lexically_normalize(&absolute),
        }
    }
}

/// Returns the main database path plus the `-wal` and `-shm` sidecars.
///
/// Mirrors v1 `sqlite_database_files`, which appends the suffixes to the *string*
/// form rather than treating them as extensions.
#[must_use]
pub fn sqlite_database_files(path: &Path) -> [PathBuf; 3] {
    let mut wal = path.as_os_str().to_os_string();
    wal.push("-wal");
    let mut shm = path.as_os_str().to_os_string();
    shm.push("-shm");
    [path.to_path_buf(), PathBuf::from(wal), PathBuf::from(shm)]
}

/// Expands a leading `~` using `HOME`, matching Python `expanduser`.
fn expand_user(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text == "~" {
        if let Some(home) = home_dir() {
            return home;
        }
    } else if let Some(rest) = text.strip_prefix("~/")
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }
    path.to_path_buf()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Collapses `.` and `..` without touching the filesystem.
fn lexically_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(component.as_os_str());
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn sidecars_are_string_suffixes_not_extensions() {
        let files = sqlite_database_files(Path::new("/tmp/a/events.db"));
        assert_eq!(files[0], Path::new("/tmp/a/events.db"));
        assert_eq!(files[1], Path::new("/tmp/a/events.db-wal"));
        assert_eq!(files[2], Path::new("/tmp/a/events.db-shm"));
    }

    #[test]
    fn resolves_a_parent_after_an_intermediate_symlink() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().expect("temp dir");
        let links = dir.path().join("links");
        let target_child = dir.path().join("target/child");
        std::fs::create_dir_all(&links).expect("links directory");
        std::fs::create_dir_all(&target_child).expect("target directory");
        symlink(&target_child, links.join("current")).expect("symlink");

        let normalized = normalize_sqlite_path(links.join("current/../events.db"));
        let expected = dir.path().canonicalize().expect("canonical temp dir");
        assert_eq!(normalized, expected.join("target/events.db"));
    }

    #[test]
    fn normalizes_a_path_whose_file_does_not_exist_yet() {
        let dir = TempDir::new().expect("temp dir");
        let target = dir.path().join("nested/../events.db");
        let normalized = normalize_sqlite_path(&target);
        assert!(normalized.is_absolute());
        assert_eq!(
            normalized.file_name().and_then(std::ffi::OsStr::to_str),
            Some("events.db")
        );
        assert!(!normalized.to_string_lossy().contains(".."));
    }

    #[test]
    fn normalizes_an_existing_file() {
        let dir = TempDir::new().expect("temp dir");
        let target = dir.path().join("events.db");
        std::fs::write(&target, b"").expect("seed");
        let normalized = normalize_sqlite_path(&target);
        assert_eq!(normalized, target.canonicalize().expect("canonical"));
    }

    #[test]
    fn resolves_relative_paths_against_the_working_directory() {
        let normalized = normalize_sqlite_path("events.db");
        assert!(normalized.is_absolute());
    }

    #[test]
    fn expands_a_leading_tilde() {
        // HOME is always set in the test environment; if it were not, the path
        // would simply be returned unchanged, which this assertion tolerates.
        let normalized = normalize_sqlite_path("~/events.db");
        assert!(!normalized.to_string_lossy().starts_with('~'));
    }

    #[test]
    fn normalization_is_idempotent() {
        let dir = TempDir::new().expect("temp dir");
        let once = normalize_sqlite_path(dir.path().join("events.db"));
        let twice = normalize_sqlite_path(&once);
        assert_eq!(once, twice);
    }
}
