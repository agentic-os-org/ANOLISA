use std::path::{Path, PathBuf};

use super::load::{config_file_path, config_read_file_path};
use super::CoshConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageConfigStatus {
    pub setting: String,
    pub effective: Language,
    pub source: &'static str,
    pub config_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    EnUs,
    ZhCn,
}

impl Language {
    pub fn as_config_value(self) -> &'static str {
        match self {
            Self::EnUs => "en-US",
            Self::ZhCn => "zh-CN",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguageSetting {
    Auto,
    Language(Language),
}

impl LanguageSetting {
    pub fn as_config_value(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Language(language) => language.as_config_value(),
        }
    }
}

pub fn parse_language_setting(value: &str) -> Option<LanguageSetting> {
    match value.trim() {
        "auto" => Some(LanguageSetting::Auto),
        "en" | "en_US" | "en-US" => Some(LanguageSetting::Language(Language::EnUs)),
        "zh" | "zh_CN" | "zh-CN" | "zh-Hans" => Some(LanguageSetting::Language(Language::ZhCn)),
        _ => None,
    }
}

pub fn resolve_language_setting(setting: LanguageSetting) -> Language {
    match setting {
        LanguageSetting::Auto => detect_language_from_env(),
        LanguageSetting::Language(language) => language,
    }
}

pub fn detect_language_from_env() -> Language {
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(value) = std::env::var_os(key).filter(|value| !value.is_empty()) {
            return value
                .to_str()
                .and_then(parse_locale_language)
                .unwrap_or(Language::EnUs);
        }
    }
    Language::EnUs
}

pub fn write_user_language_config(language: &str) -> Result<PathBuf, String> {
    let path =
        config_file_path().ok_or_else(|| "HOME is not set; cannot persist config".to_string())?;
    write_language_config_to_path(&path, language)?;
    Ok(path)
}

pub fn language_config_status() -> LanguageConfigStatus {
    let config_path = config_file_path();
    let config_setting = config_read_file_path()
        .as_ref()
        .and_then(|path| language_setting_from_config_path(path));
    let mut setting = config_setting.clone().unwrap_or_else(|| "auto".to_string());
    let mut source = if config_setting.is_some() {
        "config"
    } else {
        "default"
    };

    if let Ok(value) = std::env::var("COSH_SHELL_LANG") {
        if let Some(parsed) = parse_language_setting(&value) {
            setting = parsed.as_config_value().to_string();
            source = "env";
        }
    }

    let parsed = parse_language_setting(&setting).unwrap_or(LanguageSetting::Auto);
    LanguageConfigStatus {
        setting,
        effective: resolve_language_setting(parsed),
        source,
        config_path,
    }
}

pub(crate) fn read_guidance_language() -> std::io::Result<Language> {
    if let Some(setting) = std::env::var("COSH_SHELL_LANG")
        .ok()
        .and_then(|value| parse_language_setting(&value))
    {
        return Ok(resolve_language_setting(setting));
    }
    let path = config_read_file_path()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
    let content = match super::load::read_config_text(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let setting = language_setting_from_config_content(&content)
        .as_deref()
        .and_then(parse_language_setting)
        .unwrap_or(LanguageSetting::Auto);
    Ok(resolve_language_setting(setting))
}

pub(super) fn apply_language_value(config: &mut CoshConfig, value: &str) {
    if let Some(setting) = parse_language_setting(value) {
        config.language = setting.as_config_value().to_string();
    }
}

pub(super) fn language_setting_from_config_content(content: &str) -> Option<String> {
    let mut setting = None;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') || !line.contains('=') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == "ui.language" {
            setting = parse_language_setting(value.trim().trim_matches('"'))
                .map(LanguageSetting::as_config_value)
                .map(str::to_string);
        }
    }
    if let Ok(value) = content.parse::<toml::Value>() {
        if let Some(language) = value
            .get("ui")
            .and_then(toml::Value::as_table)
            .and_then(|ui| ui.get("language"))
            .and_then(toml::Value::as_str)
        {
            setting = parse_language_setting(language)
                .map(LanguageSetting::as_config_value)
                .map(str::to_string);
        }
    }
    setting
}

pub(super) fn write_language_config_to_path(path: &Path, language: &str) -> Result<(), String> {
    let setting = parse_language_setting(language)
        .ok_or_else(|| format!("invalid language: {language}; supported: auto, en-US, zh-CN"))?;
    let language = setting.as_config_value();

    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("create config directory failed: {err}"))?;
        }
        let content = format!("[ui]\nlanguage = \"{language}\"\n");
        return std::fs::write(path, content)
            .map_err(|err| format!("write config file failed: {err}"));
    }

    let content =
        std::fs::read_to_string(path).map_err(|err| format!("read config file failed: {err}"))?;
    let mut value = content.parse::<toml::Value>().map_err(|err| {
        format!("config file is not valid TOML; edit ui.language manually: {err}")
    })?;
    let Some(root) = value.as_table_mut() else {
        return Err("config file root is not a TOML table; edit ui.language manually".to_string());
    };
    let ui = root
        .entry("ui".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    let Some(ui) = ui.as_table_mut() else {
        return Err("[ui] is not a TOML table; edit ui.language manually".to_string());
    };
    ui.insert(
        "language".to_string(),
        toml::Value::String(language.to_string()),
    );
    let rendered = toml::to_string_pretty(&value)
        .map_err(|err| format!("render config TOML failed: {err}"))?;
    std::fs::write(path, rendered).map_err(|err| format!("write config file failed: {err}"))
}

fn language_setting_from_config_path(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    language_setting_from_config_content(&content)
}

fn parse_locale_language(value: &str) -> Option<Language> {
    let value = value.trim();
    if value == "zh" || value.starts_with("zh_") || value.starts_with("zh-") {
        return Some(Language::ZhCn);
    }
    if value == "en" || value.starts_with("en_") || value.starts_with("en-") {
        return Some(Language::EnUs);
    }
    None
}

#[cfg(test)]
mod localization_tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::fs::symlink;

    struct Environment(Vec<(&'static str, Option<OsString>)>);
    impl Environment {
        fn set(values: &[(&'static str, Option<&str>)]) -> Self {
            let saved = values
                .iter()
                .map(|(name, value)| {
                    let old = std::env::var_os(name);
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                    (*name, old)
                })
                .collect();
            Self(saved)
        }
    }
    impl Drop for Environment {
        fn drop(&mut self) {
            for (name, value) in &self.0 {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    #[test]
    fn localization_first_nonempty_locale_has_priority_even_when_not_chinese_or_english() {
        let _guard = crate::diagnostics::test_env::env_guard();
        for (all, messages, lang, expected) in [
            (
                Some("C"),
                Some("zh_CN.UTF-8"),
                Some("zh_CN.UTF-8"),
                Language::EnUs,
            ),
            (Some("POSIX"), None, Some("zh_CN.UTF-8"), Language::EnUs),
            (Some("C.UTF-8"), None, Some("zh_CN.UTF-8"), Language::EnUs),
            (
                Some("ja_JP.UTF-8"),
                None,
                Some("zh_CN.UTF-8"),
                Language::EnUs,
            ),
            (
                Some(""),
                Some("zh_CN.UTF-8"),
                Some("en_US.UTF-8"),
                Language::ZhCn,
            ),
            (
                None,
                Some("en_US.UTF-8"),
                Some("zh_CN.UTF-8"),
                Language::EnUs,
            ),
            (None, None, Some("zh_CN.UTF-8"), Language::ZhCn),
            (None, None, None, Language::EnUs),
        ] {
            let _env =
                Environment::set(&[("LC_ALL", all), ("LC_MESSAGES", messages), ("LANG", lang)]);
            assert_eq!(
                detect_language_from_env(),
                expected,
                "{all:?}/{messages:?}/{lang:?}"
            );
        }
    }

    #[test]
    fn localization_native_settings_keep_oversized_and_symlinked_config_compatible() {
        let _guard = crate::diagnostics::test_env::env_guard();
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".copilot-shell")).unwrap();
        let path = home.path().join(".copilot-shell/config.toml");
        let _env = Environment::set(&[
            ("HOME", home.path().to_str()),
            ("COSH_SHELL_LANG", None),
            ("LC_ALL", Some("en_US.UTF-8")),
        ]);
        std::fs::write(
            &path,
            format!("[ui]\nlanguage='zh-CN'\n#{}", "x".repeat(256 * 1024)),
        )
        .unwrap();
        assert_eq!(language_config_status().effective, Language::ZhCn);
        assert!(read_guidance_language().is_err());
        std::fs::remove_file(&path).unwrap();
        let target = home.path().join("target.toml");
        std::fs::write(&target, "[ui]\nlanguage='zh-CN'\n").unwrap();
        symlink(target, &path).unwrap();
        assert_eq!(language_config_status().effective, Language::ZhCn);
        assert!(read_guidance_language().is_err());
        let _override = Environment::set(&[("COSH_SHELL_LANG", Some("zh-CN"))]);
        assert_eq!(read_guidance_language().unwrap(), Language::ZhCn);
    }
    #[test]
    fn localization_passive_reader_accepts_the_limit_and_rejects_fifo_without_a_writer() {
        let _guard = crate::diagnostics::test_env::env_guard();
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".copilot-shell")).unwrap();
        let path = home.path().join(".copilot-shell/config.toml");
        let _env = Environment::set(&[
            ("HOME", home.path().to_str()),
            ("COSH_SHELL_LANG", None),
            ("LC_ALL", Some("en_US.UTF-8")),
        ]);
        assert_eq!(read_guidance_language().unwrap(), Language::EnUs);
        let header = "[ui]\nlanguage='zh-CN'\n#";
        std::fs::write(
            &path,
            format!("{header}{}", "x".repeat(256 * 1024 - header.len())),
        )
        .unwrap();
        assert_eq!(read_guidance_language().unwrap(), Language::ZhCn);
        std::fs::remove_file(&path).unwrap();
        nix::unistd::mkfifo(&path, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
        assert!(read_guidance_language().is_err());
        let _override = Environment::set(&[("COSH_SHELL_LANG", Some("en-US"))]);
        assert_eq!(read_guidance_language().unwrap(), Language::EnUs);
    }
}
