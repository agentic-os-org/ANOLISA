//! Standalone audit configuration mirror for `cosh-shell`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::types::audit::AuditMode;

/// Fixed audit settings shared with Core and CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuditSettings {
    pub(crate) mode: AuditMode,
    pub(crate) retention_days: u32,
    pub(crate) max_disk_bytes: u64,
}

impl Default for AuditSettings {
    fn default() -> Self {
        Self {
            mode: AuditMode::BestEffort,
            retention_days: 30,
            max_disk_bytes: 1024 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAuditSettings {
    mode: Option<AuditMode>,
    retention_days: Option<u32>,
    max_disk_bytes: Option<u64>,
}

/// Loads the existing system/user config files; no audit-specific file is used.
pub(crate) fn load_audit_settings() -> Result<AuditSettings, String> {
    warn_ignored_project_table();
    let system = read_table(Path::new("/etc/copilot-shell/config.toml"))?;
    if system.is_some() {
        return apply(system);
    }
    let user_path = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(".copilot-shell/config.toml"));
    let user = user_path.as_deref().map(read_table).transpose()?.flatten();
    apply(user)
}

fn warn_ignored_project_table() {
    let Some(path) = std::env::current_dir()
        .ok()
        .map(|root| root.join(".copilot-shell/config.toml"))
    else {
        return;
    };
    if read_table(&path).ok().flatten().is_some() {
        tracing::warn!(
            target: "cosh_audit",
            "ignored project [audit] settings in .copilot-shell/config.toml"
        );
    }
}

fn apply(raw: Option<RawAuditSettings>) -> Result<AuditSettings, String> {
    let mut settings = AuditSettings::default();
    let Some(raw) = raw else {
        return Ok(settings);
    };
    if let Some(mode) = raw.mode {
        settings.mode = mode;
    }
    if let Some(days) = raw.retention_days {
        if days == 0 {
            return Err("audit retention_days must be greater than zero".to_string());
        }
        settings.retention_days = days;
    }
    if let Some(bytes) = raw.max_disk_bytes {
        if bytes == 0 {
            return Err("audit max_disk_bytes must be greater than zero".to_string());
        }
        settings.max_disk_bytes = bytes;
    }
    Ok(settings)
}

fn read_table(path: &Path) -> Result<Option<RawAuditSettings>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot read audit config: {error}")),
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| "invalid audit config UTF-8".to_string())?;
    let document: toml::Value =
        toml::from_str(text).map_err(|error| format!("invalid audit config TOML: {error}"))?;
    document
        .get("audit")
        .cloned()
        .map(|table| {
            table
                .try_into()
                .map_err(|error| format!("invalid [audit] settings: {error}"))
        })
        .transpose()
}

/// Resolves the same non-temporary audit root used by Core and CLI.
pub(crate) fn resolve_audit_root() -> Result<PathBuf, String> {
    if let Some(path) = nonempty_env_path("COSH_AUDIT_DIR") {
        return require_absolute(path);
    }
    if let Some(path) = nonempty_env_path("XDG_STATE_HOME") {
        return require_absolute(path.join("cosh/audit"));
    }
    if let Some(path) = nonempty_env_path("HOME") {
        return require_absolute(path.join(".local/state/cosh/audit"));
    }
    Err("no safe audit root; set COSH_AUDIT_DIR, XDG_STATE_HOME, or HOME".to_string())
}

fn require_absolute(path: PathBuf) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Err("audit root must be absolute".to_string())
    }
}

fn nonempty_env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_defaults_and_validation_match_platform_contract() {
        assert_eq!(apply(None).unwrap(), AuditSettings::default());
        assert!(apply(Some(RawAuditSettings {
            retention_days: Some(0),
            ..RawAuditSettings::default()
        }))
        .is_err());
        assert!(apply(Some(RawAuditSettings {
            max_disk_bytes: Some(0),
            ..RawAuditSettings::default()
        }))
        .is_err());
    }

    #[test]
    fn system_table_is_complete_and_does_not_merge_user_omissions() {
        let settings = apply(Some(RawAuditSettings {
            mode: Some(AuditMode::Required),
            ..RawAuditSettings::default()
        }))
        .unwrap();
        assert_eq!(settings.mode, AuditMode::Required);
        assert_eq!(settings.retention_days, 30);
        assert_eq!(settings.max_disk_bytes, 1024 * 1024 * 1024);
    }
}
