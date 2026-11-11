use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{CoshConfig, HealthConfig};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HealthLoginConfig {
    pub(crate) health: HealthConfig,
    pub(crate) language: String,
    pub(crate) ai_enabled: bool,
}

pub(crate) fn read_login_health_config() -> io::Result<HealthLoginConfig> {
    let mut projection = match super::load::config_read_file_path() {
        Some(path) => read_projection_file(&path)?,
        None => return Err(io::Error::from_raw_os_error(nix::libc::ENOENT)),
    };
    if let Ok(value) = std::env::var("COSH_SHELL_LANG") {
        let mut config = CoshConfig {
            language: projection.language.clone(),
            ..CoshConfig::default()
        };
        super::language::apply_language_value(&mut config, &value);
        projection.language = config.language;
    }
    if let Ok(value) = std::env::var("COSH_SHELL_AI") {
        projection.ai_enabled = value != "off";
    }
    Ok(projection)
}

fn read_projection_file(path: &Path) -> io::Result<HealthLoginConfig> {
    let mut config = CoshConfig::default();
    let content = match super::load::read_config_text(path) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let simple = content
        .lines()
        .filter(|line| {
            let key = line
                .split_once('=')
                .map(|(key, _)| key.trim())
                .unwrap_or("");
            key.starts_with("health.") || key == "ui.language"
        })
        .collect::<Vec<_>>()
        .join("\n");
    super::parse::parse_simple_config(&simple, &mut config);
    match content.parse::<toml::Value>() {
        Ok(value) => {
            super::parse::parse_health_toml_config(&value, &mut config);
            if let Some(language) = value
                .get("ui")
                .and_then(|ui| ui.get("language"))
                .and_then(toml::Value::as_str)
            {
                super::language::apply_language_value(&mut config, language);
            }
        }
        Err(_)
            if content
                .lines()
                .any(|line| line.trim_start().starts_with('[')) =>
        {
            return Err(io::Error::from_raw_os_error(nix::libc::EINVAL));
        }
        Err(_) => {}
    }
    Ok(HealthLoginConfig {
        health: config.health,
        language: config.language,
        ai_enabled: config.ai_enabled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn invalid_language_override_keeps_the_configured_language() {
        let _guard = crate::diagnostics::test_env::env_guard();
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join(".copilot-shell")).unwrap();
        std::fs::write(
            directory.path().join(".copilot-shell/config.toml"),
            "[ui]\nlanguage='zh-CN'\n",
        )
        .unwrap();
        let previous = ["HOME", "COSH_SHELL_LANG"].map(|name| (name, std::env::var_os(name)));
        std::env::set_var("HOME", directory.path());
        std::env::set_var("COSH_SHELL_LANG", "invalid-language");
        let result = read_login_health_config();
        for (name, value) in previous {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        assert_eq!(result.unwrap().language, "zh-CN");
    }

    #[test]
    fn login_projection_reads_health_and_language_without_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        std::fs::write(
            &path,
            "[health]\nenabled=false\n[ui]\nlanguage='zh-CN'\n[ai]\napi_key='private-test-value'\n",
        )
        .unwrap();
        let config = read_projection_file(&path).unwrap();
        assert!(!config.health.enabled);
        assert_eq!(config.language, "zh-CN");
        let wire = serde_json::to_string(&config).unwrap();
        assert!(!wire.contains("private-test-value"));
        assert!(!wire.contains("api_key"));
    }

    #[test]
    fn login_projection_refuses_symlinks_and_oversized_config() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let link = directory.path().join("linked.toml");
        std::fs::write(&path, "[health]\nenabled=false").unwrap();
        symlink(&path, &link).unwrap();
        assert!(read_projection_file(&link).is_err());
        std::fs::write(&path, vec![b'#'; 256 * 1024 + 1]).unwrap();
        assert!(read_projection_file(&path).is_err());
    }
}
