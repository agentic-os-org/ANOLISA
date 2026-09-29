//! Legacy ilogtail channel decommission (upgrade migration).
//!
//! Pre-self-hosted ANOLISA shipped telemetry through the shared ilogtail
//! daemon, authorizing uploads by dropping a per-account file under the
//! daemon's `users/` directory (the file name is the SLS account id) and by
//! tagging the host with an `anolisa-livetrace` line in
//! `/etc/ilogtail/user_defined_id`. After migrating to the self-hosted
//! uploader both are orphaned: the shared daemon keeps shipping to the legacy
//! project, which double-uploads alongside the new channel and — worse —
//! keeps flowing after a consent withdrawal.
//!
//! [`LegacyIlogtail::decommission`] removes exactly the account files listed in
//! `/etc/anolisa/legacy-accounts.json` (or another configured path) and strips
//! the `anolisa-livetrace` line from `/etc/ilogtail/user_defined_id`. It is
//! idempotent and never touches the shared daemon or unrelated tenants' files
//! or other `user_defined_id` entries.
//!
//! The account ids and `users/` directories are kept out of source code so
//! downstream distributions can inject their own values without forking the code.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::telemetry::{TelemetryConfig, TelemetryError};

/// Path to ilogtail's `user_defined_id` file.
const USER_DEFINED_ID_PATH: &str = "/etc/ilogtail/user_defined_id";
/// Marker line that identifies this host as an ANOLISA telemetry source.
const USER_DEFINED_ID_MARKER: &str = "anolisa-livetrace";

/// Legacy ilogtail account configuration.
///
/// Read from `/etc/anolisa/legacy-accounts.json` by default. Missing fields
/// default to empty vectors, so a missing file simply makes decommission a
/// no-op rather than an error.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LegacyAccountsConfig {
    /// SLS account ids whose presence under an ilogtail `users/` directory
    /// authorizes the shared daemon to ship to the legacy project.
    #[serde(default)]
    pub account_ids: Vec<String>,
    /// ilogtail `users/` directories to scan for the configured account ids.
    #[serde(default)]
    pub users_dirs: Vec<PathBuf>,
}

impl LegacyAccountsConfig {
    /// Read the configuration from `path`.
    ///
    /// Returns the default (empty) config when the file is missing or malformed,
    /// so a host without this file is unaffected.
    pub fn read_from(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }
}

/// Decommissioner for the legacy ilogtail upload channel.
pub struct LegacyIlogtail {
    config: LegacyAccountsConfig,
}

impl Default for LegacyIlogtail {
    fn default() -> Self {
        Self::from_config(&TelemetryConfig::default())
    }
}

impl LegacyIlogtail {
    /// Production decommissioner using the configured legacy accounts path.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build one from an explicit [`TelemetryConfig`].
    pub fn from_config(telemetry_config: &TelemetryConfig) -> Self {
        Self {
            config: LegacyAccountsConfig::read_from(&telemetry_config.legacy_accounts_path),
        }
    }

    /// Build one targeting an explicit configuration (tests).
    pub fn with_config(config: LegacyAccountsConfig) -> Self {
        Self { config }
    }

    /// Remove the configured legacy SLS account files and strip the
    /// `anolisa-livetrace` line from `/etc/ilogtail/user_defined_id`.
    ///
    /// Idempotent: absent files and absent markers are skipped. Returns the
    /// account-file paths actually removed. Only the configured
    /// [`LegacyAccountsConfig::account_ids`] are touched — never the directory
    /// itself or other tenants' account files, so the shared daemon keeps
    /// serving unrelated accounts. Only the exact `anolisa-livetrace` line is
    /// removed from `user_defined_id`; other entries (e.g. `sysom_*`) are
    /// preserved.
    ///
    /// # Errors
    /// Returns the first unexpected filesystem error (e.g. a file that exists
    /// but cannot be removed); a not-found file is not an error.
    pub fn decommission(&self) -> Result<Vec<PathBuf>, TelemetryError> {
        let mut removed = Vec::new();
        for dir in &self.config.users_dirs {
            for id in &self.config.account_ids {
                let path = dir.join(id);
                match fs::remove_file(&path) {
                    Ok(()) => removed.push(path),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        Self::clean_user_defined_id_at(Path::new(USER_DEFINED_ID_PATH), USER_DEFINED_ID_MARKER)?;
        Ok(removed)
    }

    /// Remove a single marker line from a `user_defined_id`-style file.
    ///
    /// Idempotent: a missing file or absent marker is a no-op. The file is
    /// rewritten in place only when the marker was removed, and the original
    /// trailing-newline state is preserved. Exposed as a free helper so tests
    /// can exercise the logic without touching the production path.
    fn clean_user_defined_id_at(path: &Path, marker: &str) -> Result<(), TelemetryError> {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };

        let mut changed = false;
        let kept: Vec<&str> = content
            .lines()
            .filter(|line| {
                if *line == marker {
                    changed = true;
                    false
                } else {
                    true
                }
            })
            .collect();

        if !changed {
            return Ok(());
        }

        let mut new_content = kept.join("\n");
        if !new_content.is_empty() && content.ends_with('\n') {
            new_content.push('\n');
        }
        fs::write(path, new_content)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_config(users: &Path) -> LegacyAccountsConfig {
        LegacyAccountsConfig {
            account_ids: vec!["1808078950770264".into(), "1644215368948677".into()],
            users_dirs: vec![users.into()],
        }
    }

    #[test]
    fn decommission_removes_only_configured_account_files_idempotently() {
        let dir = TempDir::new().unwrap();
        let users = dir.path().join("users");
        fs::create_dir_all(&users).unwrap();
        let cfg = sample_config(&users);
        for id in &cfg.account_ids {
            fs::write(users.join(id), "").unwrap();
        }
        // An unrelated tenant's account file must survive.
        let other = users.join("9999999999999999");
        fs::write(&other, "").unwrap();

        let legacy = LegacyIlogtail::with_config(cfg);
        let removed = legacy.decommission().unwrap();

        assert_eq!(removed.len(), 2);
        for id in &legacy.config.account_ids {
            assert!(!users.join(id).exists());
        }
        assert!(other.exists());

        // Idempotent: a second pass finds nothing to remove.
        assert!(legacy.decommission().unwrap().is_empty());
    }

    #[test]
    fn decommission_missing_dirs_is_noop() {
        let dir = TempDir::new().unwrap();
        let cfg = LegacyAccountsConfig {
            account_ids: vec!["1808078950770264".into()],
            users_dirs: vec![dir.path().join("absent/users")],
        };
        let legacy = LegacyIlogtail::with_config(cfg);
        assert!(legacy.decommission().unwrap().is_empty());
    }

    #[test]
    fn decommission_empty_config_is_noop() {
        let dir = TempDir::new().unwrap();
        let users = dir.path().join("users");
        fs::create_dir_all(&users).unwrap();
        fs::write(users.join("1808078950770264"), "").unwrap();

        let legacy = LegacyIlogtail::with_config(LegacyAccountsConfig::default());
        let removed = legacy.decommission().unwrap();

        assert!(removed.is_empty());
    }

    #[test]
    fn read_from_json_file_parses_account_ids_and_users_dirs() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("legacy-accounts.json");
        fs::write(
            &path,
            r#"{
                "account_ids": ["1111111111111111", "2222222222222222"],
                "users_dirs": ["/etc/ilogtail/users", "/opt/custom/users"]
            }"#,
        )
        .unwrap();

        let cfg = LegacyAccountsConfig::read_from(&path);
        assert_eq!(
            cfg.account_ids,
            vec![
                "1111111111111111".to_string(),
                "2222222222222222".to_string()
            ]
        );
        assert_eq!(
            cfg.users_dirs,
            vec![
                PathBuf::from("/etc/ilogtail/users"),
                PathBuf::from("/opt/custom/users")
            ]
        );
    }

    #[test]
    fn read_from_missing_file_returns_default() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("does-not-exist.json");

        let cfg = LegacyAccountsConfig::read_from(&path);
        assert!(cfg.account_ids.is_empty());
        assert!(cfg.users_dirs.is_empty());
    }

    #[test]
    fn clean_user_defined_id_strips_marker_and_preserves_others() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("user_defined_id");
        fs::write(
            &path,
            "anolisa-livetrace\nsysom_unity_metrics\nsysom_livetrace_oncpu\n",
        )
        .unwrap();

        LegacyIlogtail::clean_user_defined_id_at(&path, "anolisa-livetrace").unwrap();

        let after = fs::read_to_string(&path).unwrap();
        assert!(!after.contains("anolisa-livetrace"));
        assert!(after.contains("sysom_unity_metrics"));
        assert!(after.contains("sysom_livetrace_oncpu"));
        assert!(after.ends_with('\n'));
    }

    #[test]
    fn clean_user_defined_id_is_noop_when_marker_absent() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("user_defined_id");
        fs::write(&path, "sysom_unity_metrics\n").unwrap();

        LegacyIlogtail::clean_user_defined_id_at(&path, "anolisa-livetrace").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "sysom_unity_metrics\n");
    }

    #[test]
    fn clean_user_defined_id_missing_file_is_noop() {
        let dir = TempDir::new().unwrap();
        LegacyIlogtail::clean_user_defined_id_at(&dir.path().join("absent"), "anolisa-livetrace")
            .unwrap();
    }
}
