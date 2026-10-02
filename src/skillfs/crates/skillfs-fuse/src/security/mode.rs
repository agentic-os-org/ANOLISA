//! M0 Security Mount Mode validation.
//!
//! Background. SkillFS supports two mount layouts (see `HANDOFF.md`):
//!
//! * **normal**   — `source` is outside the mountpoint; the virtual skill
//!   view appears under `/<mountpoint>/skills/...`. The physical source
//!   directory remains reachable by its real path, so FUSE callbacks only
//!   observe operations that go through `/<mountpoint>/`. Any process that
//!   knows the source path can bypass FUSE entirely.
//! * **in-place** — `source == mountpoint`; FUSE over-mounts the source
//!   directory. Once the mount is up, the only userspace path to the
//!   skills directory is through FUSE, so the `.skill-meta` policy
//!   (Package S1) and the JSONL audit stream (Packages S2 / S2.1) cover
//!   every operation visible to userspace.
//!
//! Package M0 makes that distinction explicit at startup. When an operator
//! wants the strong guarantee that audit and policy actually cover every
//! mutation, they opt in with `--security-mode`. SkillFS then refuses to
//! start a non-in-place mount instead of silently providing weaker
//! enforcement.
//!
//! Default behavior is **unchanged**. Operators who do not pass
//! `--security-mode` keep the existing normal-mount UX. The CLI still
//! prints a visible warning that direct writes to the physical source path
//! bypass SkillFS in that mode.
//!
//! This module owns the validation primitive only. CLI flag parsing,
//! audit-runtime wiring, and the mount entry points stay in their own
//! modules so security wiring stays separable from POSIX behavior.

use std::path::{Path, PathBuf};

/// Operator-visible security mount mode.
///
/// The default is disabled — equivalent to the pre-M0 behavior. When
/// enabled, [`validate`](Self::validate) requires that `source` and
/// `mountpoint` canonicalize to the same path before any FUSE mount may
/// proceed.
#[derive(Debug, Clone, Default)]
pub struct SecurityModeConfig {
    /// `true` when the operator passed `--security-mode`. When set, mounts
    /// that are not in-place are rejected before the FUSE event loop
    /// starts.
    pub enabled: bool,
}

impl SecurityModeConfig {
    /// Disabled (compat) config — the default.
    ///
    /// The CLI continues to accept both in-place and normal mounts.
    /// Non-in-place mounts only enforce SkillFS policy/audit for syscalls
    /// that go through the FUSE mountpoint; direct writes to the physical
    /// source path are not intercepted.
    pub fn disabled() -> Self {
        Self { enabled: false }
    }

    /// Enabled security-mode config.
    ///
    /// [`validate`](Self::validate) will reject any source/mountpoint pair
    /// that does not canonicalize to the same path.
    pub fn enabled_mode() -> Self {
        Self { enabled: true }
    }

    /// Whether the security-mode constraint is active.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Validate the source/mountpoint pair against the configured mode.
    ///
    /// * When [`enabled`](Self::enabled) is `false`: always returns
    ///   `Ok(())`. The CLI/embedder remains responsible for the existing
    ///   normal vs. in-place detection it does today.
    /// * When [`enabled`](Self::enabled) is `true`: both paths are
    ///   canonicalized and compared. The mount is accepted iff they refer
    ///   to the same on-disk directory; otherwise a
    ///   [`SecurityModeError::NotInPlace`] error is returned and the
    ///   caller must refuse to mount.
    ///
    /// Canonicalization is required when the security guarantee is
    /// requested: comparing the user-supplied paths textually would let
    /// `source = "./skills"` and `mountpoint = "/abs/skills"` pass even
    /// though they refer to the same directory, or — worse — let trailing
    /// slashes, `..`, or symlink trickery slip a non-in-place mount past
    /// the check. Canonicalize errors therefore become [`SecurityModeError`]
    /// values rather than being swallowed: an operator who asked for
    /// `--security-mode` must not end up running without it because one of
    /// the paths could not be resolved.
    pub fn validate(&self, source: &Path, mountpoint: &Path) -> Result<(), SecurityModeError> {
        if !self.enabled {
            return Ok(());
        }

        let source_canonical =
            source
                .canonicalize()
                .map_err(|e| SecurityModeError::SourceCanonicalize {
                    path: source.to_path_buf(),
                    source: e,
                })?;
        let mountpoint_canonical =
            mountpoint
                .canonicalize()
                .map_err(|e| SecurityModeError::MountpointCanonicalize {
                    path: mountpoint.to_path_buf(),
                    source: e,
                })?;

        if source_canonical == mountpoint_canonical {
            Ok(())
        } else {
            Err(SecurityModeError::NotInPlace {
                source: source.to_path_buf(),
                mountpoint: mountpoint.to_path_buf(),
                source_canonical,
                mountpoint_canonical,
            })
        }
    }
}

/// Errors returned by [`SecurityModeConfig::validate`].
///
/// All variants are produced **before** any FUSE mount begins, so the CLI
/// can surface them as a startup error and exit non-zero without leaving
/// the user with a partially-initialized mount.
#[derive(Debug)]
pub enum SecurityModeError {
    /// Security mode was requested but `source` and `mountpoint` resolve
    /// to different directories on disk. Returned with both the
    /// user-supplied paths and their canonical forms so the operator can
    /// see exactly which inputs disagreed.
    NotInPlace {
        source: PathBuf,
        mountpoint: PathBuf,
        source_canonical: PathBuf,
        mountpoint_canonical: PathBuf,
    },
    /// Security mode was requested but the source path could not be
    /// canonicalized (e.g. it does not exist, or a parent component is
    /// not a directory). The underlying `std::io::Error` is preserved.
    SourceCanonicalize {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Security mode was requested but the mountpoint path could not be
    /// canonicalized. The underlying `std::io::Error` is preserved.
    MountpointCanonicalize {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for SecurityModeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecurityModeError::NotInPlace {
                source,
                mountpoint,
                source_canonical,
                mountpoint_canonical,
            } => {
                write!(
                    f,
                    "--security-mode requires an in-place mount (source must equal mountpoint), \
                     but source '{}' (canonical '{}') and mountpoint '{}' (canonical '{}') \
                     resolve to different directories. Re-run with source and mountpoint \
                     pointing at the same directory, or omit --security-mode to use the \
                     compatibility (non-in-place) mount layout.",
                    source.display(),
                    source_canonical.display(),
                    mountpoint.display(),
                    mountpoint_canonical.display(),
                )
            }
            SecurityModeError::SourceCanonicalize { path, source } => {
                write!(
                    f,
                    "--security-mode could not canonicalize source path '{}': {}",
                    path.display(),
                    source
                )
            }
            SecurityModeError::MountpointCanonicalize { path, source } => {
                write!(
                    f,
                    "--security-mode could not canonicalize mountpoint path '{}': {}",
                    path.display(),
                    source
                )
            }
        }
    }
}

impl std::error::Error for SecurityModeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SecurityModeError::SourceCanonicalize { source, .. }
            | SecurityModeError::MountpointCanonicalize { source, .. } => Some(source),
            SecurityModeError::NotInPlace { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled() {
        let cfg = SecurityModeConfig::default();
        assert!(!cfg.is_enabled());
        assert!(!cfg.enabled);
    }

    #[test]
    fn disabled_helper_matches_default() {
        let cfg = SecurityModeConfig::disabled();
        assert!(!cfg.is_enabled());
    }

    #[test]
    fn enabled_helper_sets_flag() {
        let cfg = SecurityModeConfig::enabled_mode();
        assert!(cfg.is_enabled());
    }

    #[test]
    fn disabled_validation_accepts_non_in_place_paths() {
        let source = tempfile::tempdir().unwrap();
        let mountpoint = tempfile::tempdir().unwrap();
        let cfg = SecurityModeConfig::disabled();
        cfg.validate(source.path(), mountpoint.path())
            .expect("disabled mode must accept any pair");
    }

    #[test]
    fn disabled_validation_does_not_require_paths_to_exist() {
        // Disabled is the "compatibility" path: don't canonicalize, don't
        // touch the filesystem, never reject. This is what the existing
        // CLI does today.
        let cfg = SecurityModeConfig::disabled();
        cfg.validate(
            Path::new("/this/does/not/exist/source"),
            Path::new("/this/does/not/exist/mountpoint"),
        )
        .expect("disabled mode must not require canonicalization");
    }

    #[test]
    fn enabled_validation_accepts_identical_paths() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = SecurityModeConfig::enabled_mode();
        cfg.validate(dir.path(), dir.path())
            .expect("source == mountpoint must satisfy security mode");
    }

    #[test]
    fn enabled_validation_accepts_distinct_paths_that_canonicalize_equal() {
        let dir = tempfile::tempdir().unwrap();
        // Re-form the same path with a `.` so the user-facing strings
        // differ even though canonicalize() resolves both to the same
        // directory. This pins that we compare canonical, not literal,
        // paths.
        let with_dot = dir.path().join(".");
        let cfg = SecurityModeConfig::enabled_mode();
        cfg.validate(dir.path(), &with_dot)
            .expect("canonicalize-equal paths must satisfy security mode");
        cfg.validate(&with_dot, dir.path())
            .expect("canonicalize-equal paths must satisfy security mode");
    }

    #[test]
    fn enabled_validation_rejects_distinct_directories() {
        let source = tempfile::tempdir().unwrap();
        let mountpoint = tempfile::tempdir().unwrap();
        let cfg = SecurityModeConfig::enabled_mode();
        let err = cfg
            .validate(source.path(), mountpoint.path())
            .expect_err("distinct directories must be rejected under security mode");
        match err {
            SecurityModeError::NotInPlace {
                source_canonical,
                mountpoint_canonical,
                ..
            } => {
                assert_ne!(source_canonical, mountpoint_canonical);
            }
            other => panic!("expected NotInPlace, got {other:?}"),
        }
    }

    #[test]
    fn enabled_validation_reports_canonicalize_error_for_missing_source() {
        let mountpoint = tempfile::tempdir().unwrap();
        let cfg = SecurityModeConfig::enabled_mode();
        let bogus = std::path::PathBuf::from("/nonexistent/skillfs-m0/source");
        let err = cfg
            .validate(&bogus, mountpoint.path())
            .expect_err("missing source must surface canonicalize error");
        assert!(matches!(err, SecurityModeError::SourceCanonicalize { .. }));
    }

    #[test]
    fn enabled_validation_reports_canonicalize_error_for_missing_mountpoint() {
        let source = tempfile::tempdir().unwrap();
        let cfg = SecurityModeConfig::enabled_mode();
        let bogus = std::path::PathBuf::from("/nonexistent/skillfs-m0/mountpoint");
        let err = cfg
            .validate(source.path(), &bogus)
            .expect_err("missing mountpoint must surface canonicalize error");
        assert!(matches!(
            err,
            SecurityModeError::MountpointCanonicalize { .. }
        ));
    }

    #[test]
    fn not_in_place_error_message_names_both_paths() {
        let source = tempfile::tempdir().unwrap();
        let mountpoint = tempfile::tempdir().unwrap();
        let cfg = SecurityModeConfig::enabled_mode();
        let err = cfg
            .validate(source.path(), mountpoint.path())
            .expect_err("distinct directories must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("--security-mode"),
            "error message should mention the flag: {msg}"
        );
        assert!(
            msg.contains(&source.path().display().to_string()),
            "error message should mention source: {msg}"
        );
        assert!(
            msg.contains(&mountpoint.path().display().to_string()),
            "error message should mention mountpoint: {msg}"
        );
    }

    #[test]
    fn canonicalize_error_preserves_underlying_io_error() {
        let mountpoint = tempfile::tempdir().unwrap();
        let cfg = SecurityModeConfig::enabled_mode();
        let bogus = std::path::PathBuf::from("/nonexistent/skillfs-m0/source");
        let err = cfg
            .validate(&bogus, mountpoint.path())
            .expect_err("missing source must surface error");
        use std::error::Error as _;
        let inner = err
            .source()
            .expect("canonicalize error should chain io::Error");
        // We don't pin a specific errno (the OS may return ENOENT or
        // ENOTDIR depending on what part of the path is missing); we
        // only require an underlying io::Error is preserved.
        assert!(
            inner.downcast_ref::<std::io::Error>().is_some(),
            "expected std::io::Error in chain"
        );
    }
}
