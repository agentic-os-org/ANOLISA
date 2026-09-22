//! Pure, side-effect-free helpers shared by the built-in framework
//! drivers.
//!
//! These never spawn a process or mutate the filesystem, so they are safe to
//! call from `plan`/`status`/`prepare` paths. Built-in drivers share them here
//! rather than each re-declaring timestamp and formatting logic.

use super::driver::{CliOutput, ConditionStatus, FrameworkCommand};

/// Compare link targets without requiring the source to survive package removal.
pub(crate) fn symlink_matches(
    link: &std::path::Path,
    target: &std::path::Path,
) -> Result<bool, super::AdapterError> {
    symlink_matches_at(link, link, target)
}

/// Resolve a detached symlink relative to its original location.
pub(crate) fn symlink_matches_at(
    link: &std::path::Path,
    original: &std::path::Path,
    target: &std::path::Path,
) -> Result<bool, super::AdapterError> {
    use std::path::{Component, PathBuf};
    let referent = match std::fs::read_link(link) {
        Ok(path) => path,
        Err(source)
            if matches!(
                source.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput
            ) =>
        {
            return Ok(false);
        }
        Err(source) => {
            return Err(super::AdapterError::Io {
                path: link.to_path_buf(),
                source,
            });
        }
    };
    let absolute = original
        .parent()
        .unwrap_or_else(|| std::path::Path::new(""))
        .join(referent);
    let mut normalized = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            part => normalized.push(part.as_os_str()),
        }
    }
    // An alias that only matches through canonicalization cannot be recognized
    // after package removal. Only adopt a stable lexical referent; while the
    // source exists, also reject traversal through a differently resolved path.
    if normalized != target {
        return Ok(false);
    }
    match (
        std::fs::canonicalize(&absolute),
        std::fs::canonicalize(target),
    ) {
        (Ok(actual), Ok(expected)) => Ok(actual == expected),
        _ => Ok(true),
    }
}

/// ISO 8601 UTC timestamp, second precision.
pub(crate) fn now_iso8601() -> String {
    use chrono::{SecondsFormat, Utc};
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Map a bool to a [`ConditionStatus`] (`true` -> `True`, `false` -> `False`).
pub(crate) fn bool_status(b: bool) -> ConditionStatus {
    if b {
        ConditionStatus::True
    } else {
        ConditionStatus::False
    }
}

/// Compose a failure reason string from a non-success [`CliOutput`].
pub(crate) fn cli_failure_reason(verb: &str, output: &CliOutput) -> String {
    if output.timed_out {
        return format!("'{verb}' timed out");
    }
    let code = output
        .status
        .map(|c| c.to_string())
        .unwrap_or_else(|| "killed".to_string());
    let mut reason = format!("'{verb}' exited with {code}");
    let stderr = output.stderr.trim();
    if !stderr.is_empty() {
        reason.push_str(": ");
        reason.push_str(stderr);
    }
    reason
}

/// Human-readable form of a command for dry-run/preview output. Display
/// only — never parsed back into an argv.
pub(crate) fn display_command(cmd: &FrameworkCommand) -> String {
    let mut s = String::new();
    for (k, v) in &cmd.env_set {
        s.push_str(&format!("{k}={v} "));
    }
    s.push_str(&cmd.program);
    for a in &cmd.args {
        s.push(' ');
        s.push_str(a);
    }
    s
}
