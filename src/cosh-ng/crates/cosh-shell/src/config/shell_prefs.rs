//! Saved/effective/source status and persistence for `shell.analysis_mode`
//! and `shell.recommendations.bash_history`, mirroring the language config
//! contract. The bash history key lives under `[shell.recommendations]`, the
//! only level the shared parser consumes.

use std::path::{Path, PathBuf};

use super::load::{config_file_path, config_read_file_path, read_config_text};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AnalysisModeStatus {
    pub(crate) saved: Option<String>,
    pub(crate) effective: String,
    pub(crate) source: &'static str,
    pub(crate) unreadable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BashHistoryStatus {
    pub(crate) saved: Option<bool>,
    pub(crate) effective: bool,
    pub(crate) source: &'static str,
    pub(crate) unreadable: bool,
}

pub(crate) fn parse_analysis_mode(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "smart" => Some("smart"),
        "auto" => Some("auto"),
        "manual" => Some("manual"),
        _ => None,
    }
}

pub(crate) fn analysis_mode_status() -> AnalysisModeStatus {
    analysis_mode_status_at(
        config_read_file_path(),
        std::env::var("COSH_SHELL_ANALYSIS_MODE").ok().as_deref(),
    )
}

fn analysis_mode_status_at(path: Option<PathBuf>, env: Option<&str>) -> AnalysisModeStatus {
    let (saved, unreadable) = read_pref(path.as_deref(), |root| {
        root.get("shell")?
            .get("analysis_mode")?
            .as_str()
            .map(str::to_string)
    });
    let parsed = saved.as_deref().and_then(parse_analysis_mode);
    let mut effective = parsed.unwrap_or("smart").to_string();
    let mut source = if parsed.is_some() {
        "config"
    } else {
        "default"
    };
    if let Some(mode) = env.and_then(parse_analysis_mode) {
        effective = mode.to_string();
        source = "env";
    }
    AnalysisModeStatus {
        saved,
        effective,
        source,
        unreadable,
    }
}

pub(crate) fn bash_history_status() -> BashHistoryStatus {
    bash_history_status_at(
        config_read_file_path(),
        std::env::var("COSH_RECOMMENDATIONS_BASH_HISTORY")
            .ok()
            .as_deref(),
    )
}

fn bash_history_status_at(path: Option<PathBuf>, env: Option<&str>) -> BashHistoryStatus {
    let (saved, unreadable) = read_pref(path.as_deref(), |root| {
        root.get("shell")?
            .get("recommendations")?
            .get("bash_history")?
            .as_bool()
    });
    let mut effective = saved.unwrap_or(false);
    let mut source = if saved.is_some() { "config" } else { "default" };
    if let Some(value) = env {
        effective = super::parse::parse_bool_value(value);
        source = "env";
    }
    BashHistoryStatus {
        saved,
        effective,
        source,
        unreadable,
    }
}

pub(crate) fn write_analysis_mode(mode: &str) -> Result<PathBuf, String> {
    let mode = parse_analysis_mode(mode)
        .ok_or_else(|| format!("invalid analysis mode: {mode}; supported: smart, auto, manual"))?;
    let path =
        config_file_path().ok_or_else(|| "HOME is not set; cannot persist config".to_string())?;
    write_pref(
        &path,
        &["shell"],
        "analysis_mode",
        toml::Value::String(mode.to_string()),
    )?;
    Ok(path)
}

pub(crate) fn write_bash_history(enabled: bool) -> Result<PathBuf, String> {
    let path =
        config_file_path().ok_or_else(|| "HOME is not set; cannot persist config".to_string())?;
    write_pref(
        &path,
        &["shell", "recommendations"],
        "bash_history",
        toml::Value::Boolean(enabled),
    )?;
    Ok(path)
}

fn read_pref<T>(
    path: Option<&Path>,
    extract: impl FnOnce(&toml::map::Map<String, toml::Value>) -> Option<T>,
) -> (Option<T>, bool) {
    let Some(path) = path else {
        return (None, false);
    };
    // Only a genuinely absent file is "unconfigured". Permission errors, a
    // non-directory component, or a dangling symlink must stay Unknown rather
    // than masquerade as a default the wizard would offer to overwrite.
    match std::fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (None, false),
        Err(_) => return (None, true),
    }
    let Ok(content) = read_config_text(path) else {
        return (None, true);
    };
    let Ok(value) = content.parse::<toml::Value>() else {
        return (None, true);
    };
    (value.as_table().and_then(extract), false)
}

fn write_pref(path: &Path, section: &[&str], key: &str, value: toml::Value) -> Result<(), String> {
    let label = section.join(".");
    let mut root = if path.exists() {
        let content = std::fs::read_to_string(path)
            .map_err(|err| format!("read config file failed: {err}"))?;
        content
            .parse::<toml::Value>()
            .map_err(|err| {
                format!("config file is not valid TOML; edit {label}.{key} manually: {err}")
            })?
            .as_table()
            .cloned()
            .ok_or_else(|| {
                format!("config root is not a TOML table; edit {label}.{key} manually")
            })?
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("create config directory failed: {err}"))?;
        }
        toml::map::Map::new()
    };
    let mut table = &mut root;
    for part in section {
        let node = table
            .entry(part.to_string())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        table = node
            .as_table_mut()
            .ok_or_else(|| format!("[{label}] is not a TOML table; edit {label}.{key} manually"))?;
    }
    table.insert(key.to_string(), value);
    let rendered = toml::to_string_pretty(&toml::Value::Table(root))
        .map_err(|err| format!("render config TOML failed: {err}"))?;
    std::fs::write(path, rendered).map_err(|err| format!("write config file failed: {err}"))?;
    Ok(())
}

#[cfg(test)]
library_tests! {
mod tests {
    use super::*;

    fn temp_config(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, content).unwrap();
        (dir, path)
    }

    #[test]
    fn analysis_absent_defaults_to_smart() {
        let (_dir, path) = temp_config("");
        let status = analysis_mode_status_at(Some(path), None);
        assert!(status.saved.is_none());
        assert_eq!(status.effective, "smart");
        assert_eq!(status.source, "default");
    }

    #[test]
    fn analysis_invalid_saved_reports_default_source() {
        let (_dir, path) = temp_config("[shell]\nanalysis_mode = \"bogus\"\n");
        let status = analysis_mode_status_at(Some(path), None);
        assert_eq!(status.saved.as_deref(), Some("bogus"));
        assert_eq!(status.effective, "smart");
        assert_eq!(status.source, "default");
    }

    #[test]
    fn analysis_valid_saved_reports_config_source() {
        let (_dir, path) = temp_config("[shell]\nanalysis_mode = \"manual\"\n");
        let status = analysis_mode_status_at(Some(path), None);
        assert_eq!(status.effective, "manual");
        assert_eq!(status.source, "config");
    }

    #[test]
    fn analysis_env_overrides_saved() {
        let (_dir, path) = temp_config("[shell]\nanalysis_mode = \"manual\"\n");
        let status = analysis_mode_status_at(Some(path), Some("auto"));
        assert_eq!(status.effective, "auto");
        assert_eq!(status.source, "env");
    }

    #[test]
    fn write_preserves_unrelated_keys() {
        let (_dir, path) = temp_config("[ui]\nlanguage = \"zh-CN\"\n");
        write_pref(
            &path,
            &["shell"],
            "analysis_mode",
            toml::Value::String("auto".into()),
        )
        .unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let root: toml::Value = content.parse().unwrap();
        assert_eq!(root["ui"]["language"].as_str(), Some("zh-CN"));
        assert_eq!(root["shell"]["analysis_mode"].as_str(), Some("auto"));
        let status = analysis_mode_status_at(Some(path), None);
        assert_eq!(status.source, "config");
    }

    #[test]
    fn bash_history_roundtrip_and_env_override() {
        let (_dir, path) = temp_config("");
        write_pref(
            &path,
            &["shell", "recommendations"],
            "bash_history",
            toml::Value::Boolean(true),
        )
        .unwrap();
        let status = bash_history_status_at(Some(path.clone()), None);
        assert_eq!(status.saved, Some(true));
        assert_eq!(status.source, "config");
        let overridden = bash_history_status_at(Some(path), Some("false"));
        assert!(!overridden.effective);
        assert_eq!(overridden.source, "env");
    }

    #[test]
    fn analysis_invalid_toml_is_unreadable_not_default() {
        let (_dir, path) = temp_config("this is = = not toml");
        let status = analysis_mode_status_at(Some(path), None);
        assert!(status.unreadable);
        assert!(status.saved.is_none());
        assert_eq!(status.effective, "smart");
        assert_eq!(status.source, "default");
    }

    #[test]
    fn bash_invalid_toml_is_unreadable_not_off() {
        let (_dir, path) = temp_config("recommendations = broken =");
        let status = bash_history_status_at(Some(path), None);
        assert!(status.unreadable);
        assert!(status.saved.is_none());
        assert!(!status.effective);
    }

    #[test]
    fn oversized_config_is_unreadable() {
        let (_dir, path) = temp_config(&"# padding\n".repeat(40_000));
        let status = analysis_mode_status_at(Some(path), None);
        assert!(status.unreadable);
    }

    #[test]
    fn readable_or_absent_config_is_not_unreadable() {
        let (dir, path) = temp_config("[shell]\nanalysis_mode = \"bogus\"\n");
        let status = analysis_mode_status_at(Some(path), None);
        assert!(!status.unreadable);
        let absent = analysis_mode_status_at(Some(dir.path().join("missing.toml")), None);
        assert!(!absent.unreadable);
        assert!(absent.saved.is_none());
    }

    #[test]
    fn bash_history_write_is_read_by_the_real_parser() {
        let (_dir, path) = temp_config("[shell.recommendations]\nenabled = true\n");
        write_pref(
            &path,
            &["shell", "recommendations"],
            "bash_history",
            toml::Value::Boolean(true),
        )
        .unwrap();
        let mut config = crate::config::CoshConfig::default();
        crate::config::load::load_config_file_into(&path, &mut config);
        assert!(config.recommendations.bash_history);
        assert!(config.recommendations.enabled);
        let status = bash_history_status_at(Some(path), None);
        assert_eq!(status.saved, Some(true));
        assert_eq!(status.source, "config");
    }

    #[test]
    fn bash_history_top_level_table_is_ignored_like_the_parser() {
        let (_dir, path) = temp_config("[recommendations]\nbash_history = true\n");
        let status = bash_history_status_at(Some(path.clone()), None);
        assert_eq!(status.saved, None);
        let mut config = crate::config::CoshConfig::default();
        crate::config::load::load_config_file_into(&path, &mut config);
        assert!(!config.recommendations.bash_history);
    }

    #[test]
    fn bash_history_write_preserves_nested_sibling_keys() {
        let (_dir, path) = temp_config(
            "[shell]\nanalysis_mode = \"manual\"\n[shell.recommendations]\nenabled = true\n",
        );
        write_pref(
            &path,
            &["shell", "recommendations"],
            "bash_history",
            toml::Value::Boolean(false),
        )
        .unwrap();
        let mut config = crate::config::CoshConfig::default();
        crate::config::load::load_config_file_into(&path, &mut config);
        assert!(!config.recommendations.bash_history);
        assert!(config.recommendations.enabled);
        assert_eq!(config.analysis_mode, "manual");
    }

    #[test]
    fn non_directory_config_path_is_unreadable() {
        let (_dir, path) = temp_config("");
        let bad = path.join("config.toml");
        let status = analysis_mode_status_at(Some(bad), None);
        assert!(status.unreadable);
    }

    #[test]
    fn dangling_symlink_config_is_unreadable_not_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::os::unix::fs::symlink(dir.path().join("missing-target"), &path).unwrap();
        let status = analysis_mode_status_at(Some(path), None);
        assert!(status.unreadable);
        assert!(status.saved.is_none());
    }

    #[test]
    fn permission_denied_config_is_unreadable() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { nix::libc::geteuid() } == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let status = analysis_mode_status_at(Some(locked.join("config.toml")), None);
        assert!(status.unreadable);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}
}
