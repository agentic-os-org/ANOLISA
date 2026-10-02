//! Canonical audit configuration layering and storage-root resolution.

use std::path::{Path, PathBuf};

use cosh_types::audit::{AuditMode, AuditSettingSource, AuditSettings};
use cosh_types::error::{CoshError, ErrorCode};
use serde::Deserialize;

/// File locations used by the audit settings loader.
#[derive(Debug, Clone)]
pub struct AuditConfigPaths {
    /// Host-wide configuration file.
    pub system: PathBuf,
    /// Per-user configuration file.
    pub user: Option<PathBuf>,
    /// Workspace configuration inspected only for rejected `[audit]` input.
    pub project: Option<PathBuf>,
}

impl AuditConfigPaths {
    /// Resolves the production configuration locations.
    pub fn detect(workspace: Option<&Path>) -> Self {
        let user = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .map(|home| home.join(".copilot-shell/config.toml"));
        let project = workspace.map(|root| root.join(".copilot-shell/config.toml"));
        Self {
            system: PathBuf::from("/etc/copilot-shell/config.toml"),
            user,
            project,
        }
    }
}

/// Effective settings plus non-fatal project-scope warnings.
#[derive(Debug, Clone)]
pub struct LoadedAuditSettings {
    /// Canonically resolved settings.
    pub settings: AuditSettings,
    /// Safe warnings that never include rejected configuration values.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAuditSettings {
    mode: Option<AuditMode>,
    retention_days: Option<u32>,
    max_disk_bytes: Option<u64>,
}

/// Loads audit settings using the production file locations.
///
/// # Errors
///
/// Returns a stable audit configuration error for malformed or unsafe system
/// and user settings. A system error never falls back to user configuration.
pub fn load_audit_settings(workspace: Option<&Path>) -> Result<LoadedAuditSettings, CoshError> {
    load_audit_settings_from(&AuditConfigPaths::detect(workspace))
}

/// Loads audit settings from explicit locations for deterministic tests.
///
/// # Errors
///
/// Returns a stable audit configuration error for unreadable, malformed,
/// unknown, zero, or overflowing audit settings.
pub fn load_audit_settings_from(
    paths: &AuditConfigPaths,
) -> Result<LoadedAuditSettings, CoshError> {
    let system = read_audit_table(&paths.system)?;
    // A system table is authoritative as a whole. Do not let an ignored user
    // file make valid host policy unavailable.
    let user = if system.is_none() {
        match &paths.user {
            Some(path) => read_audit_table(path)?,
            None => None,
        }
    } else {
        None
    };

    let mut warnings = Vec::new();
    if let Some(project) = &paths.project {
        if file_has_audit_table(project)? {
            warnings.push(format!(
                "ignored project [audit] settings in {}",
                safe_config_label(project)
            ));
        }
    }

    let mut settings = AuditSettings::default();
    if let Some(raw) = system {
        apply_raw(&mut settings, raw, AuditSettingSource::System)?;
    } else if let Some(raw) = user {
        apply_raw(&mut settings, raw, AuditSettingSource::User)?;
    }
    Ok(LoadedAuditSettings { settings, warnings })
}

fn apply_raw(
    settings: &mut AuditSettings,
    raw: RawAuditSettings,
    source: AuditSettingSource,
) -> Result<(), CoshError> {
    if let Some(mode) = raw.mode {
        settings.mode = mode;
        settings.mode_source = source;
    }
    if let Some(days) = raw.retention_days {
        if days == 0 {
            return Err(config_error("retention_days must be greater than zero"));
        }
        settings.retention_days = days;
        settings.retention_days_source = source;
    }
    if let Some(bytes) = raw.max_disk_bytes {
        if bytes == 0 {
            return Err(config_error("max_disk_bytes must be greater than zero"));
        }
        settings.max_disk_bytes = bytes;
        settings.max_disk_bytes_source = source;
    }
    Ok(())
}

fn read_audit_table(path: &Path) -> Result<Option<RawAuditSettings>, CoshError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(config_error(format!(
                "cannot read {}: {error}",
                safe_config_label(path)
            )))
        }
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| config_error(format!("invalid UTF-8 in {}", safe_config_label(path))))?;
    let document: toml::Value = toml::from_str(text).map_err(|error| {
        config_error(format!(
            "invalid TOML in {}: {error}",
            safe_config_label(path)
        ))
    })?;
    let Some(table) = document.get("audit") else {
        return Ok(None);
    };
    let raw = table.clone().try_into().map_err(|error| {
        config_error(format!(
            "invalid [audit] in {}: {error}",
            safe_config_label(path)
        ))
    })?;
    Ok(Some(raw))
}

fn file_has_audit_table(path: &Path) -> Result<bool, CoshError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(config_error(format!(
                "cannot inspect {}: {error}",
                safe_config_label(path)
            )))
        }
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| config_error(format!("invalid UTF-8 in {}", safe_config_label(path))))?;
    let document: toml::Value = toml::from_str(text).map_err(|error| {
        config_error(format!(
            "invalid TOML in {}: {error}",
            safe_config_label(path)
        ))
    })?;
    Ok(document.get("audit").is_some())
}

fn safe_config_label(path: &Path) -> String {
    if path == Path::new("/etc/copilot-shell/config.toml") {
        return "system config".to_string();
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml")
        .to_string()
}

fn config_error(message: impl Into<String>) -> CoshError {
    CoshError::new(ErrorCode::AuditUnavailable, message, "audit")
        .with_hint("fix the existing [audit] table; no separate audit config file is used")
}

/// Source used to resolve the version 1 audit root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditRootSource {
    /// Explicit `COSH_AUDIT_DIR` override.
    Environment,
    /// XDG state directory.
    XdgStateHome,
    /// Default state directory below `HOME`.
    Home,
}

/// Safe version 1 audit root and its resolution source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAuditRoot {
    /// Audit root before the fixed `v1` child.
    pub path: PathBuf,
    /// Source of the resolved path.
    pub source: AuditRootSource,
}

impl ResolvedAuditRoot {
    /// Returns the fixed version 1 directory.
    pub fn v1_dir(&self) -> PathBuf {
        self.path.join("v1")
    }
}

/// Resolves the version 1 storage root without a `/tmp` fallback.
///
/// # Errors
///
/// Returns `AuditUnavailable` when no non-empty safe root input exists.
pub fn resolve_audit_root() -> Result<ResolvedAuditRoot, CoshError> {
    if let Some(path) = nonempty_env_path("COSH_AUDIT_DIR") {
        require_absolute_root(&path)?;
        return Ok(ResolvedAuditRoot {
            path,
            source: AuditRootSource::Environment,
        });
    }
    if let Some(path) = nonempty_env_path("XDG_STATE_HOME") {
        require_absolute_root(&path)?;
        return Ok(ResolvedAuditRoot {
            path: path.join("cosh/audit"),
            source: AuditRootSource::XdgStateHome,
        });
    }
    if let Some(path) = nonempty_env_path("HOME") {
        require_absolute_root(&path)?;
        return Ok(ResolvedAuditRoot {
            path: path.join(".local/state/cosh/audit"),
            source: AuditRootSource::Home,
        });
    }
    Err(CoshError::new(
        ErrorCode::AuditUnavailable,
        "no safe audit state root is available",
        "audit",
    )
    .recoverable(true)
    .with_hint("set COSH_AUDIT_DIR, XDG_STATE_HOME, or HOME"))
}

fn require_absolute_root(path: &Path) -> Result<(), CoshError> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(CoshError::new(
            ErrorCode::AuditUnavailable,
            "audit state root must be absolute",
            "audit",
        )
        .recoverable(true)
        .with_hint("use an absolute COSH_AUDIT_DIR, XDG_STATE_HOME, or HOME path"))
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

    fn paths(root: &Path) -> AuditConfigPaths {
        AuditConfigPaths {
            system: root.join("system.toml"),
            user: Some(root.join("user.toml")),
            project: Some(root.join("project.toml")),
        }
    }

    #[test]
    fn defaults_when_no_audit_table_exists() {
        let directory = tempfile::tempdir().unwrap();
        let loaded = load_audit_settings_from(&paths(directory.path())).unwrap();
        assert_eq!(loaded.settings, AuditSettings::default());
    }

    #[test]
    fn system_table_is_authoritative_for_omitted_values() {
        let directory = tempfile::tempdir().unwrap();
        let paths = paths(directory.path());
        std::fs::write(&paths.system, "[audit]\nmode = \"required\"\n").unwrap();
        std::fs::write(
            paths.user.as_ref().unwrap(),
            "[audit]\nretention_days = 7\nmax_disk_bytes = 9\n",
        )
        .unwrap();
        let loaded = load_audit_settings_from(&paths).unwrap();
        assert_eq!(loaded.settings.mode, AuditMode::Required);
        assert_eq!(loaded.settings.retention_days, 30);
        assert_eq!(loaded.settings.max_disk_bytes, 1024 * 1024 * 1024);
    }

    #[test]
    fn authoritative_system_table_does_not_parse_ignored_user_file() {
        let directory = tempfile::tempdir().unwrap();
        let paths = paths(directory.path());
        std::fs::write(&paths.system, "[audit]\nmode = \"required\"\n").unwrap();
        std::fs::write(paths.user.as_ref().unwrap(), "not valid toml = [").unwrap();

        let loaded = load_audit_settings_from(&paths).unwrap();

        assert_eq!(loaded.settings.mode, AuditMode::Required);
        assert_eq!(loaded.settings.retention_days, 30);
    }

    #[test]
    fn user_table_applies_when_system_table_is_absent() {
        let directory = tempfile::tempdir().unwrap();
        let paths = paths(directory.path());
        std::fs::write(&paths.system, "[logging]\nlevel = \"info\"\n").unwrap();
        std::fs::write(
            paths.user.as_ref().unwrap(),
            "[audit]\nretention_days = 7\nmax_disk_bytes = 99\n",
        )
        .unwrap();
        let loaded = load_audit_settings_from(&paths).unwrap();
        assert_eq!(loaded.settings.retention_days, 7);
        assert_eq!(loaded.settings.max_disk_bytes, 99);
        assert_eq!(
            loaded.settings.retention_days_source,
            AuditSettingSource::User
        );
    }

    #[test]
    fn project_table_is_ignored_with_one_safe_warning() {
        let directory = tempfile::tempdir().unwrap();
        let paths = paths(directory.path());
        std::fs::write(
            paths.project.as_ref().unwrap(),
            "[audit]\nmode = \"required\"\n",
        )
        .unwrap();
        let loaded = load_audit_settings_from(&paths).unwrap();
        assert_eq!(loaded.settings.mode, AuditMode::BestEffort);
        assert_eq!(loaded.warnings.len(), 1);
        assert!(!loaded.warnings[0].contains("required"));
    }

    #[test]
    fn unknown_and_zero_values_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let paths = paths(directory.path());
        std::fs::write(&paths.system, "[audit]\nenabled = false\n").unwrap();
        assert!(load_audit_settings_from(&paths).is_err());
        std::fs::write(&paths.system, "[audit]\nretention_days = 0\n").unwrap();
        assert!(load_audit_settings_from(&paths).is_err());
    }
}
