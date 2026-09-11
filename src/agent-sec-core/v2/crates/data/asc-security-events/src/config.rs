//! Data-directory and stream-path resolution (v1 `security_events/config.py`).
//!
//! All directories are created with mode `0o700` and then explicitly chmodded,
//! because `mkdir(mode=...)` is masked by the process umask.
//!
//! Resolution is split into a pure [`DataDirEnv`]-driven form and a thin wrapper
//! that reads the real process environment. Tests drive the pure form, so they
//! never mutate `AGENT_SEC_DATA_DIR` or `HOME` and stay safe under the default
//! parallel test harness.

use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::error::ConfigError;

/// System-wide `JSONL` path used to derive the tier-1 data directory.
pub const PRIMARY_LOG_PATH: &str = "/var/log/agent-sec/security-events.jsonl";
/// Per-user directory name used for the tier-2 fallback.
pub const FALLBACK_DIR_NAME: &str = ".agent-sec-core";
/// Default logical stream for security events.
pub const DEFAULT_SECURITY_STREAM: &str = "security-events";

const DIR_MODE: u32 = 0o700;

/// Inputs consumed by data-directory resolution.
///
/// Making these explicit keeps the tiering logic testable without mutating
/// process-global state.
#[derive(Debug, Clone)]
pub struct DataDirEnv {
    /// Value of `AGENT_SEC_DATA_DIR`; an empty value counts as absent.
    pub override_dir: Option<PathBuf>,
    /// Value of `HOME`, used for the tier-2 fallback.
    pub home: Option<PathBuf>,
    /// Tier-1 directory, normally the parent of [`PRIMARY_LOG_PATH`].
    pub primary_dir: PathBuf,
    /// Directory holding the tier-3 per-user fallback, normally `/tmp`.
    pub tmp_root: PathBuf,
    /// Effective user id, used for the tier-3 directory name and ownership check.
    pub uid: u32,
}

impl DataDirEnv {
    /// Captures the resolution inputs from the real process environment.
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            override_dir: non_empty_path("AGENT_SEC_DATA_DIR"),
            home: non_empty_path("HOME"),
            primary_dir: Path::new(PRIMARY_LOG_PATH)
                .parent()
                .unwrap_or_else(|| Path::new("/var/log/agent-sec"))
                .to_path_buf(),
            tmp_root: PathBuf::from("/tmp"),
            uid: rustix::process::getuid().as_raw(),
        }
    }

    fn tmp_dir(&self) -> PathBuf {
        self.tmp_root.join(format!("agent-sec-{}", self.uid))
    }
}

/// Returns the fallback `JSONL` path (`$HOME/.agent-sec-core/security-events.jsonl`).
///
/// v1 computes `FALLBACK_LOG_PATH` once at import time; exposing it as a
/// function keeps the value correct when `HOME` differs per process.
#[must_use]
pub fn fallback_log_path() -> Option<PathBuf> {
    non_empty_path("HOME").map(|home| home.join(FALLBACK_DIR_NAME).join("security-events.jsonl"))
}

/// Resolves the data directory from `env` using v1's tier order.
///
/// Tiers: `AGENT_SEC_DATA_DIR` override, `primary_dir`, `$HOME/.agent-sec-core`,
/// then a validated `{tmp_root}/agent-sec-{uid}`.
///
/// # Errors
///
/// Only the override can fail: v1 lets its `OSError` propagate there while
/// wrapping every built-in tier in `try/except`, so an unusable override is a
/// hard error rather than a silent downgrade.
pub fn resolve_data_dir_with(env: &DataDirEnv) -> Result<PathBuf, ConfigError> {
    if let Some(override_dir) = env.override_dir.as_deref() {
        prepare_dir(override_dir).map_err(|source| ConfigError::DataDirUnusable {
            path: override_dir.display().to_string(),
            source,
        })?;
        return Ok(override_dir.to_path_buf());
    }

    // Tier 1: system-wide directory. Unlike tier 2 this also requires write
    // access, so a root-owned directory falls through instead of being used.
    if prepare_dir(&env.primary_dir).is_ok()
        && env.primary_dir.is_dir()
        && is_writable(&env.primary_dir)
    {
        return Ok(env.primary_dir.clone());
    }

    // Tier 2: user home directory. v1 performs no writability probe here.
    if let Some(home) = env.home.as_deref() {
        let fallback = home.join(FALLBACK_DIR_NAME);
        if prepare_dir(&fallback).is_ok() {
            return Ok(fallback);
        }
    }

    // Tier 3: validated per-user tmp directory.
    if let Ok(tmp) = safe_tmp_dir(env) {
        return Ok(tmp);
    }

    // Last resort: hand back the tmp path without creating it, exactly as v1
    // does. Callers surface the resulting write failure instead.
    Ok(env.tmp_dir())
}

/// Resolves the data directory from the process environment.
///
/// # Errors
///
/// Propagates [`resolve_data_dir_with`] failures.
pub fn resolve_data_dir() -> Result<PathBuf, ConfigError> {
    resolve_data_dir_with(&DataDirEnv::from_process())
}

/// Returns the resolved data directory (v1 `get_data_dir`).
///
/// # Errors
///
/// Propagates [`resolve_data_dir`] failures.
pub fn get_data_dir() -> Result<PathBuf, ConfigError> {
    resolve_data_dir()
}

/// Returns the `JSONL` path for a logical stream, rooted at `data_dir`.
///
/// # Errors
///
/// Returns [`ConfigError::InvalidStreamName`] for names failing
/// `^[A-Za-z0-9][A-Za-z0-9_-]*$`.
pub fn stream_log_path_in(data_dir: &Path, stream: &str) -> Result<PathBuf, ConfigError> {
    validate_stream_name(stream)?;
    Ok(data_dir.join(format!("{stream}.jsonl")))
}

/// Returns the `SQLite` path for a logical stream, rooted at `data_dir`.
///
/// # Errors
///
/// Returns [`ConfigError::InvalidStreamName`] for names failing
/// `^[A-Za-z0-9][A-Za-z0-9_-]*$`.
pub fn stream_db_path_in(data_dir: &Path, stream: &str) -> Result<PathBuf, ConfigError> {
    validate_stream_name(stream)?;
    Ok(data_dir.join(format!("{stream}.db")))
}

/// Returns the `JSONL` path for a logical stream in the resolved data directory.
///
/// # Errors
///
/// Propagates stream-name validation and data-directory failures.
pub fn get_stream_log_path(stream: &str) -> Result<PathBuf, ConfigError> {
    validate_stream_name(stream)?;
    stream_log_path_in(&resolve_data_dir()?, stream)
}

/// Returns the `SQLite` path for a logical stream in the resolved data directory.
///
/// # Errors
///
/// Propagates stream-name validation and data-directory failures.
pub fn get_stream_db_path(stream: &str) -> Result<PathBuf, ConfigError> {
    validate_stream_name(stream)?;
    stream_db_path_in(&resolve_data_dir()?, stream)
}

/// Returns the security-events `JSONL` path.
///
/// # Errors
///
/// Propagates [`get_stream_log_path`] failures.
pub fn get_log_path() -> Result<PathBuf, ConfigError> {
    get_stream_log_path(DEFAULT_SECURITY_STREAM)
}

/// Returns the security-events `SQLite` path.
///
/// # Errors
///
/// Propagates [`get_stream_db_path`] failures.
pub fn get_db_path() -> Result<PathBuf, ConfigError> {
    get_stream_db_path(DEFAULT_SECURITY_STREAM)
}

/// Validates a logical local-event stream name.
///
/// # Errors
///
/// Returns [`ConfigError::InvalidStreamName`] when the name is empty, starts
/// with `_`/`-`, or contains anything outside `[A-Za-z0-9_-]`.
pub fn validate_stream_name(stream: &str) -> Result<(), ConfigError> {
    let mut chars = stream.chars();
    let valid = match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        }
        _ => false,
    };

    if valid {
        Ok(())
    } else {
        Err(ConfigError::InvalidStreamName(stream.to_owned()))
    }
}

fn non_empty_path(key: &str) -> Option<PathBuf> {
    // v1 tests the value for truthiness, so an empty value is ignored.
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn prepare_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(DIR_MODE))
}

fn safe_tmp_dir(env: &DataDirEnv) -> io::Result<PathBuf> {
    let dir = env.tmp_dir();
    fs::create_dir_all(&dir)?;

    let meta = fs::symlink_metadata(&dir)?;
    if meta.file_type().is_symlink() {
        return Err(io::Error::other(format!(
            "{} is a symlink \u{2014} refusing to use",
            dir.display()
        )));
    }
    if meta.uid() != env.uid {
        return Err(io::Error::other(format!(
            "{} not owned by uid {}",
            dir.display(),
            env.uid
        )));
    }

    fs::set_permissions(&dir, fs::Permissions::from_mode(DIR_MODE))?;
    Ok(dir)
}

fn is_writable(path: &Path) -> bool {
    rustix::fs::access(path, rustix::fs::Access::WRITE_OK).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an environment whose every tier lives under `root`, so tier
    /// selection can be observed without touching the real filesystem layout.
    fn env_under(root: &Path) -> DataDirEnv {
        DataDirEnv {
            override_dir: None,
            home: Some(root.join("home")),
            primary_dir: root.join("primary"),
            tmp_root: root.join("tmp"),
            uid: rustix::process::getuid().as_raw(),
        }
    }

    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn stream_names_follow_v1_regex() {
        for good in ["security-events", "a", "A0", "obs_stream", "x-y_z9"] {
            assert!(validate_stream_name(good).is_ok(), "{good} should be valid");
        }
        for bad in [
            "",
            "_leading",
            "-leading",
            "has space",
            "dot.name",
            "sl/ash",
            "\u{4f60}",
        ] {
            assert!(
                validate_stream_name(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn invalid_stream_name_message_matches_v1() {
        let err = validate_stream_name("bad name").expect_err("must reject");
        assert_eq!(err.to_string(), "Invalid stream name: 'bad name'");
    }

    #[test]
    fn override_wins_over_every_tier_and_is_created_0700() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("nested/data");
        let mut env = env_under(temp.path());
        env.override_dir = Some(target.clone());
        // Make tier 1 viable so the assertion proves precedence, not fallback.
        fs::create_dir_all(&env.primary_dir).unwrap();

        assert_eq!(resolve_data_dir_with(&env).unwrap(), target);
        assert_eq!(mode_of(&target), DIR_MODE);
    }

    #[test]
    fn unusable_override_is_a_hard_error() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("not-a-dir");
        fs::write(&file, b"x").unwrap();
        let mut env = env_under(temp.path());
        env.override_dir = Some(file.join("child"));

        let err = resolve_data_dir_with(&env).expect_err("override failure must propagate");
        assert!(matches!(err, ConfigError::DataDirUnusable { .. }));
    }

    #[test]
    fn primary_tier_wins_when_writable() {
        let temp = tempfile::tempdir().unwrap();
        let env = env_under(temp.path());
        assert_eq!(resolve_data_dir_with(&env).unwrap(), env.primary_dir);
        assert_eq!(mode_of(&env.primary_dir), DIR_MODE);
    }

    #[test]
    fn home_tier_is_used_when_primary_cannot_be_created() {
        let temp = tempfile::tempdir().unwrap();
        let mut env = env_under(temp.path());
        // A regular file at the tier-1 path makes `create_dir_all` fail.
        env.primary_dir = temp.path().join("blocked/primary");
        fs::create_dir_all(temp.path().join("blocked")).unwrap();
        fs::write(temp.path().join("blocked/primary"), b"x").unwrap();

        let resolved = resolve_data_dir_with(&env).unwrap();
        assert_eq!(resolved, temp.path().join("home").join(FALLBACK_DIR_NAME));
        assert_eq!(mode_of(&resolved), DIR_MODE);
    }

    #[test]
    fn tmp_tier_is_used_when_primary_and_home_fail() {
        let temp = tempfile::tempdir().unwrap();
        let mut env = env_under(temp.path());
        env.primary_dir = temp.path().join("blocked/primary");
        fs::create_dir_all(temp.path().join("blocked")).unwrap();
        fs::write(temp.path().join("blocked/primary"), b"x").unwrap();
        env.home = None;

        let resolved = resolve_data_dir_with(&env).unwrap();
        assert_eq!(
            resolved,
            env.tmp_root.join(format!("agent-sec-{}", env.uid))
        );
        assert_eq!(mode_of(&resolved), DIR_MODE);
    }

    #[test]
    fn tmp_tier_rejects_a_directory_owned_by_another_uid() {
        let temp = tempfile::tempdir().unwrap();
        let mut env = env_under(temp.path());
        env.primary_dir = temp.path().join("blocked/primary");
        fs::create_dir_all(temp.path().join("blocked")).unwrap();
        fs::write(temp.path().join("blocked/primary"), b"x").unwrap();
        env.home = None;
        // Claiming a different uid makes the ownership probe fail without
        // needing root to chown anything.
        env.uid = env.uid.wrapping_add(1);

        let resolved = resolve_data_dir_with(&env).unwrap();
        // Ownership rejection falls through to the uncreated last-resort path.
        assert_eq!(
            resolved,
            env.tmp_root.join(format!("agent-sec-{}", env.uid))
        );
    }

    #[test]
    fn last_resort_path_is_returned_without_being_created() {
        let temp = tempfile::tempdir().unwrap();
        let mut env = env_under(temp.path());
        env.primary_dir = temp.path().join("blocked/primary");
        fs::create_dir_all(temp.path().join("blocked")).unwrap();
        fs::write(temp.path().join("blocked/primary"), b"x").unwrap();
        env.home = None;
        // A file where the tmp root should be makes tier 3 fail outright.
        fs::write(&env.tmp_root, b"x").unwrap();

        let resolved = resolve_data_dir_with(&env).unwrap();
        assert_eq!(
            resolved,
            env.tmp_root.join(format!("agent-sec-{}", env.uid))
        );
        assert!(
            !resolved.exists(),
            "last resort must not create the directory"
        );
    }

    #[test]
    fn stream_paths_are_built_under_the_data_dir() {
        let dir = Path::new("/data");
        assert_eq!(
            stream_log_path_in(dir, "security-events").unwrap(),
            Path::new("/data/security-events.jsonl")
        );
        assert_eq!(
            stream_db_path_in(dir, "security-events").unwrap(),
            Path::new("/data/security-events.db")
        );
        assert_eq!(
            stream_log_path_in(dir, "observability").unwrap(),
            Path::new("/data/observability.jsonl")
        );
    }

    #[test]
    fn stream_path_helpers_reject_invalid_names() {
        let dir = Path::new("/data");
        assert!(stream_log_path_in(dir, "../escape").is_err());
        assert!(stream_db_path_in(dir, "").is_err());
    }
}
