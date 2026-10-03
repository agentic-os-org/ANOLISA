//! Deployment-owned telemetry paths. The disable sentinel is checked per write.
use nix::unistd::{Uid, User};
use std::ffi::OsString;
use std::path::PathBuf;

/// Existing uploader-owned telemetry file.
pub const DEFAULT_TELEMETRY_PATH: &str = "/var/log/anolisa/sls/ops/agent-sec-core.jsonl";
/// Presence (including a dangling symlink) disables L1 telemetry.
pub const DISABLED_SENTINEL: &str = "/etc/anolisa/.telemetry_disabled";

/// Explicit paths make gates testable without modifying host policy files.
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// Existing file; the writer never creates it or its directory.
    pub path: PathBuf,
    /// Deployment-owned disable marker, not controlled by request parameters.
    pub disabled_sentinel: PathBuf,
}

impl TelemetryConfig {
    /// Loads the V1 path override; sentinel location remains fixed in production.
    #[must_use]
    pub fn from_process() -> Self {
        let path = std::env::var_os("AGENT_SEC_TELEMETRY_LOG_PATH")
            .filter(|p| !p.is_empty())
            .map_or_else(|| PathBuf::from(DEFAULT_TELEMETRY_PATH), PathBuf::from);
        Self {
            path: expand_user(path, std::env::var_os("HOME")),
            disabled_sentinel: PathBuf::from(DISABLED_SENTINEL),
        }
    }

    /// Only an absent sentinel enables telemetry; every other stat failure disables it.
    #[must_use]
    pub fn enabled(&self) -> bool {
        matches!(std::fs::symlink_metadata(&self.disabled_sentinel), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    }
}

/// Resolve V1 tilde forms through the OS user database, including NSS providers.
fn expand_user(path: PathBuf, home: Option<OsString>) -> PathBuf {
    let mut components = path.components();
    let Some(user) = components
        .next()
        .and_then(|part| part.as_os_str().to_str())
        .and_then(|part| part.strip_prefix('~'))
    else {
        return path;
    };
    let home = if user.is_empty() {
        home.map(PathBuf::from).or_else(|| {
            User::from_uid(Uid::current())
                .ok()
                .flatten()
                .map(|user| user.dir)
        })
    } else {
        User::from_name(user).ok().flatten().map(|user| user.dir)
    };
    home.map_or_else(
        || path.clone(),
        |home| {
            // Python expanduser treats an explicitly empty home as the root directory.
            let home = if home.as_os_str().is_empty() {
                PathBuf::from("/")
            } else {
                home
            };
            home.join(components.as_path())
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_user_ignores_home_override() {
        let user = User::from_uid(Uid::current()).unwrap().unwrap();
        for suffix in ["", "/telemetry.jsonl"] {
            assert_eq!(
                expand_user(
                    PathBuf::from(format!("~{}{suffix}", user.name)),
                    Some(OsString::from("/different-home")),
                ),
                user.dir.join(suffix.trim_start_matches('/')),
            );
        }
    }

    #[test]
    fn absent_home_uses_current_user_database_entry() {
        let user = User::from_uid(Uid::current()).unwrap().unwrap();
        assert_eq!(
            expand_user(PathBuf::from("~/telemetry.jsonl"), None),
            user.dir.join("telemetry.jsonl"),
        );
    }

    #[test]
    fn home_override_and_non_tilde_paths_keep_v1_behavior() {
        for (input, home, expected) in [
            (
                "~/telemetry.jsonl",
                "/custom-home",
                "/custom-home/telemetry.jsonl",
            ),
            ("~/telemetry.jsonl", "", "/telemetry.jsonl"),
            (
                DEFAULT_TELEMETRY_PATH,
                "/custom-home",
                DEFAULT_TELEMETRY_PATH,
            ),
            (
                "relative/telemetry.jsonl",
                "/custom-home",
                "relative/telemetry.jsonl",
            ),
        ] {
            assert_eq!(
                expand_user(PathBuf::from(input), Some(OsString::from(home))),
                PathBuf::from(expected),
            );
        }
    }
}
