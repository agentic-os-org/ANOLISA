//! Path validation for Tokenless SQLite state.
//!
//! Directory-level relocation may target any absolute filesystem location,
//! while file-level overrides remain confined to explicitly trusted roots.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

/// Failure returned when a Tokenless state path violates the path policy.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PathPolicyError {
    /// No passwd-backed home is available for the default data directory.
    #[error("no trusted home directory available and TOKENLESS_DATA_DIR is not set")]
    MissingDefaultHome,
    /// The configured path is relative.
    #[error("path '{path}' is not absolute")]
    NotAbsolute {
        /// Rejected path.
        path: PathBuf,
    },
    /// The configured path contains a parent-directory component.
    #[error("path '{path}' contains parent traversal")]
    ParentTraversal {
        /// Rejected path.
        path: PathBuf,
    },
    /// The filesystem root cannot be used as the Tokenless data directory.
    #[error("path '{path}' cannot be used as the data directory")]
    RootDataDirectory {
        /// Rejected path.
        path: PathBuf,
    },
    /// The data-directory path resolves to a non-directory.
    #[error("path '{path}' is not a directory")]
    NotDirectory {
        /// Rejected path.
        path: PathBuf,
    },
    /// A database path resolves to a non-regular file.
    #[error("path '{path}' is not a regular file")]
    NotRegularFile {
        /// Rejected path.
        path: PathBuf,
    },
    /// A database override names a symbolic link instead of a database file.
    #[error("database path '{path}' is a symbolic link")]
    DatabaseSymlink {
        /// Rejected path.
        path: PathBuf,
    },
    /// An existing path component could not be resolved.
    #[error("path '{path}' cannot be resolved: {source}")]
    CannotResolve {
        /// Path being resolved.
        path: PathBuf,
        /// Filesystem error returned while resolving the path.
        #[source]
        source: std::io::Error,
    },
    /// No trusted root is available for a file-level database override.
    #[error("no trusted root is available for database path '{path}'")]
    NoTrustedRoot {
        /// Rejected database path.
        path: PathBuf,
    },
    /// A file-level override escapes both the real home and selected data directory.
    #[error("database path '{path}' is outside the trusted home and data directory")]
    OutsideTrustedRoots {
        /// Rejected database path.
        path: PathBuf,
    },
}

/// Create a Tokenless state directory with private permissions when supported.
///
/// Existing directories retain their permissions. On Unix, every directory
/// created by this call uses mode `0700`; other platforms use the standard
/// recursive directory creation behavior.
///
/// # Errors
///
/// Returns the filesystem error raised while creating the directory tree.
pub fn ensure_state_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;

        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

/// Resolve the Tokenless data directory.
///
/// A non-empty override may point anywhere on the filesystem, subject to
/// absolute-path, traversal, and directory-type checks. Without an override,
/// the default remains `.tokenless` beneath the passwd-backed home.
///
/// # Errors
///
/// Returns [`PathPolicyError`] when the override is unsafe or unusable, or
/// when neither an override nor a trusted default home is available.
pub fn resolve_data_dir(
    home: Option<&Path>,
    override_path: Option<&str>,
) -> Result<PathBuf, PathPolicyError> {
    if let Some(path) = override_path.filter(|path| !path.is_empty()) {
        return validate_data_dir(Path::new(path));
    }

    let home = home
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or(PathPolicyError::MissingDefaultHome)?;
    validate_data_dir(&home.join(".tokenless"))
}

/// Validate and normalize a directory-level Tokenless state override.
///
/// Unlike file-level overrides, the directory is not confined to the user's
/// home. Existing symlink prefixes are canonicalized before the path is used.
///
/// # Errors
///
/// Returns [`PathPolicyError`] for relative paths, parent traversal, the
/// filesystem root, non-directory targets, or unresolvable components.
pub fn validate_data_dir(path: &Path) -> Result<PathBuf, PathPolicyError> {
    validate_absolute_syntax(path)?;
    let normalized = normalize_with_existing_ancestor(path)?;
    if normalized.parent().is_none() {
        return Err(PathPolicyError::RootDataDirectory {
            path: path.to_path_buf(),
        });
    }

    match std::fs::metadata(path) {
        Ok(metadata) if !metadata.is_dir() => Err(PathPolicyError::NotDirectory {
            path: path.to_path_buf(),
        }),
        Ok(_) => Ok(normalized),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(normalized),
        Err(source) => Err(PathPolicyError::CannotResolve {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Validate a database file against the real home and selected data directory.
///
/// Each supplied root is normalized before comparison. Existing database
/// files must be regular files and may not be symbolic links.
///
/// # Errors
///
/// Returns [`PathPolicyError`] when the database path is malformed, cannot be
/// resolved, names an unsafe file type, or lies outside every trusted root.
pub fn validate_database_path(
    path: &Path,
    trusted_roots: &[&Path],
) -> Result<PathBuf, PathPolicyError> {
    validate_absolute_syntax(path)?;

    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(PathPolicyError::DatabaseSymlink {
                path: path.to_path_buf(),
            });
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(PathPolicyError::NotRegularFile {
                path: path.to_path_buf(),
            });
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(PathPolicyError::CannotResolve {
                path: path.to_path_buf(),
                source,
            });
        }
    }

    let normalized = normalize_with_existing_ancestor(path)?;
    let mut normalized_roots = Vec::with_capacity(trusted_roots.len());
    for root in trusted_roots
        .iter()
        .filter(|root| !root.as_os_str().is_empty())
    {
        let normalized_root = normalize_with_existing_ancestor(root)?;
        if normalized_root.parent().is_some() {
            normalized_roots.push(normalized_root);
        }
    }
    if normalized_roots.is_empty() {
        return Err(PathPolicyError::NoTrustedRoot {
            path: path.to_path_buf(),
        });
    }
    if normalized_roots
        .iter()
        .any(|root| normalized.starts_with(root))
    {
        Ok(normalized)
    } else {
        Err(PathPolicyError::OutsideTrustedRoots {
            path: path.to_path_buf(),
        })
    }
}

fn validate_absolute_syntax(path: &Path) -> Result<(), PathPolicyError> {
    if !path.is_absolute() {
        return Err(PathPolicyError::NotAbsolute {
            path: path.to_path_buf(),
        });
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(PathPolicyError::ParentTraversal {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn normalize_with_existing_ancestor(path: &Path) -> Result<PathBuf, PathPolicyError> {
    let mut existing_ancestor = path;
    let mut missing_components: Vec<OsString> = Vec::new();

    loop {
        match std::fs::symlink_metadata(existing_ancestor) {
            Ok(_) => {
                if existing_ancestor != path {
                    let metadata = std::fs::metadata(existing_ancestor).map_err(|source| {
                        PathPolicyError::CannotResolve {
                            path: existing_ancestor.to_path_buf(),
                            source,
                        }
                    })?;
                    if !metadata.is_dir() {
                        return Err(PathPolicyError::NotDirectory {
                            path: existing_ancestor.to_path_buf(),
                        });
                    }
                }
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let component = existing_ancestor.file_name().ok_or_else(|| {
                    PathPolicyError::CannotResolve {
                        path: path.to_path_buf(),
                        source: error,
                    }
                })?;
                missing_components.push(component.to_os_string());
                existing_ancestor =
                    existing_ancestor
                        .parent()
                        .ok_or_else(|| PathPolicyError::CannotResolve {
                            path: path.to_path_buf(),
                            source: std::io::Error::from(std::io::ErrorKind::NotFound),
                        })?;
            }
            Err(source) => {
                return Err(PathPolicyError::CannotResolve {
                    path: existing_ancestor.to_path_buf(),
                    source,
                });
            }
        }
    }

    let mut normalized =
        existing_ancestor
            .canonicalize()
            .map_err(|source| PathPolicyError::CannotResolve {
                path: existing_ancestor.to_path_buf(),
                source,
            })?;
    for component in missing_components.iter().rev() {
        normalized.push(component);
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_accepts_absolute_path_outside_home() {
        let external = tempfile::tempdir().unwrap();
        let resolved =
            resolve_data_dir(Some(Path::new("/home/example")), external.path().to_str()).unwrap();
        assert_eq!(resolved, external.path().canonicalize().unwrap());
    }

    #[test]
    fn data_dir_override_does_not_require_home() {
        let external = tempfile::tempdir().unwrap();
        let resolved = resolve_data_dir(None, external.path().to_str()).unwrap();
        assert_eq!(resolved, external.path().canonicalize().unwrap());
    }

    #[test]
    fn data_dir_default_requires_home() {
        let error = resolve_data_dir(None, None).unwrap_err();
        assert!(matches!(error, PathPolicyError::MissingDefaultHome));
    }

    #[test]
    fn data_dir_rejects_relative_and_parent_paths() {
        assert!(matches!(
            validate_data_dir(Path::new("relative/data")),
            Err(PathPolicyError::NotAbsolute { .. })
        ));
        assert!(matches!(
            validate_data_dir(Path::new("/tmp/../var/tokenless")),
            Err(PathPolicyError::ParentTraversal { .. })
        ));
    }

    #[test]
    fn data_dir_rejects_root_and_existing_file() {
        assert!(matches!(
            validate_data_dir(Path::new("/")),
            Err(PathPolicyError::RootDataDirectory { .. })
        ));
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, "not a directory").unwrap();
        assert!(matches!(
            validate_data_dir(&file),
            Err(PathPolicyError::NotDirectory { .. })
        ));
    }

    #[test]
    fn database_path_accepts_home_or_selected_data_dir() {
        let home = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let home_db = home.path().join("stats.db");
        let external_db = external.path().join("stats.db");
        let roots = [home.path(), external.path()];

        assert!(validate_database_path(&home_db, &roots).is_ok());
        assert!(validate_database_path(&external_db, &roots).is_ok());
    }

    #[test]
    fn database_path_rejects_outside_roots() {
        let home = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let roots = [home.path(), external.path()];
        let error = validate_database_path(&outside.path().join("stats.db"), &roots).unwrap_err();
        assert!(matches!(error, PathPolicyError::OutsideTrustedRoots { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn state_dir_is_private_and_preserves_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let created = root.path().join("created/nested");
        ensure_state_dir(&created).unwrap();
        assert_eq!(
            created
                .parent()
                .unwrap()
                .metadata()
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            created.metadata().unwrap().permissions().mode() & 0o777,
            0o700
        );

        std::fs::set_permissions(&created, std::fs::Permissions::from_mode(0o750)).unwrap();
        ensure_state_dir(&created).unwrap();
        assert_eq!(
            created.metadata().unwrap().permissions().mode() & 0o777,
            0o750
        );
    }

    #[cfg(unix)]
    #[test]
    fn database_path_rejects_symlink_file() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.db");
        let link = dir.path().join("stats.db");
        std::fs::write(&target, "db").unwrap();
        symlink(&target, &link).unwrap();

        let error = validate_database_path(&link, &[dir.path()]).unwrap_err();
        assert!(matches!(error, PathPolicyError::DatabaseSymlink { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn data_dir_canonicalizes_symlink_prefix() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let link = root.path().join("data");
        symlink(target.path(), &link).unwrap();

        let resolved = validate_data_dir(&link).unwrap();
        assert_eq!(resolved, target.path().canonicalize().unwrap());
    }
}
