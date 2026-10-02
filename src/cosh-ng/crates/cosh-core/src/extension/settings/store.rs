//! Plain-value parsing, validation, and crash-safe JSON persistence.

use super::*;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct SettingsStore {
    pub(super) schema_version: u32,
    #[serde(default)]
    pub(super) extensions: BTreeMap<String, BTreeMap<String, Value>>,
}

pub(super) fn find_definition<'a>(
    extension: &'a Extension,
    key: &str,
) -> Result<&'a SettingDefinition, SettingsError> {
    extension
        .settings
        .iter()
        .find(|definition| definition.key == key)
        .ok_or_else(|| {
            SettingsError::new(
                "extension_setting_unknown",
                format!("extension setting not found: {}/{}", extension.name, key),
            )
        })
}

pub(super) fn parse_value(setting_type: SettingType, raw: &str) -> Result<Value, SettingsError> {
    let parsed = match setting_type {
        SettingType::String => Value::String(raw.to_string()),
        SettingType::Boolean => match raw {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => {
                return Err(SettingsError::new(
                    "extension_setting_type_invalid",
                    "boolean setting value must be true or false",
                ))
            }
        },
        SettingType::Integer => Value::Number(
            raw.parse::<i64>()
                .map_err(|_| {
                    SettingsError::new(
                        "extension_setting_type_invalid",
                        "integer setting value must be a signed 64-bit integer",
                    )
                })?
                .into(),
        ),
    };
    Ok(parsed)
}

pub(super) fn validate_value_type(
    setting_type: SettingType,
    value: &Value,
) -> Result<(), SettingsError> {
    let valid = match setting_type {
        SettingType::String => value.is_string(),
        SettingType::Boolean => value.is_boolean(),
        SettingType::Integer => value.as_i64().is_some(),
    };
    if valid {
        Ok(())
    } else {
        Err(SettingsError::new(
            "extension_setting_store_invalid",
            "persisted setting value does not match its manifest type",
        ))
    }
}

pub(super) fn display_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        _ => value.to_string(),
    }
}

pub(super) fn load_store(path: &Path) -> Result<SettingsStore, SettingsError> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SettingsStore {
                schema_version: SETTINGS_SCHEMA_VERSION,
                extensions: BTreeMap::new(),
            })
        }
        Err(error) => {
            return Err(SettingsError::new(
                "extension_settings_unreadable",
                format!("failed to read {}: {error}", path.display()),
            ))
        }
    };
    let store: SettingsStore = serde_json::from_str(&content).map_err(|error| {
        SettingsError::new(
            "extension_settings_invalid",
            format!("failed to parse {}: {error}", path.display()),
        )
    })?;
    if store.schema_version != SETTINGS_SCHEMA_VERSION {
        return Err(SettingsError::new(
            "extension_settings_schema_unsupported",
            format!(
                "unsupported settings schema {} in {}",
                store.schema_version,
                path.display()
            ),
        ));
    }
    Ok(store)
}

pub(super) fn mutate_store(
    path: &Path,
    extension: &str,
    mutation: impl FnOnce(&mut BTreeMap<String, Value>),
) -> Result<(), SettingsError> {
    let parent = path.parent().ok_or_else(|| {
        SettingsError::new(
            "extension_settings_path_invalid",
            format!("settings path has no parent: {}", path.display()),
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        SettingsError::new(
            "extension_settings_write_failed",
            format!("failed to create {}: {error}", parent.display()),
        )
    })?;
    let lock_path = path.with_extension("lock");
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| {
            SettingsError::new(
                "extension_settings_lock_failed",
                format!("failed to open {}: {error}", lock_path.display()),
            )
        })?;
    lock.lock_exclusive().map_err(|error| {
        SettingsError::new(
            "extension_settings_lock_failed",
            format!("failed to lock {}: {error}", lock_path.display()),
        )
    })?;
    let mut store = load_store(path)?;
    mutation(store.extensions.entry(extension.to_string()).or_default());
    if store
        .extensions
        .get(extension)
        .is_some_and(BTreeMap::is_empty)
    {
        store.extensions.remove(extension);
    }
    let bytes = serde_json::to_vec_pretty(&store).map_err(|error| {
        SettingsError::new(
            "extension_settings_write_failed",
            format!("failed to serialize settings: {error}"),
        )
    })?;
    let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|error| {
            SettingsError::new(
                "extension_settings_write_failed",
                format!("failed to create {}: {error}", temporary.display()),
            )
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                SettingsError::new(
                    "extension_settings_write_failed",
                    format!("failed to secure {}: {error}", temporary.display()),
                )
            })?;
    }
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| {
            SettingsError::new(
                "extension_settings_write_failed",
                format!("failed to write {}: {error}", temporary.display()),
            )
        })?;
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        SettingsError::new(
            "extension_settings_write_failed",
            format!("failed to replace {}: {error}", path.display()),
        )
    })?;
    Ok(())
}

pub(super) fn is_workspace_trusted(workspace_root: &Path, home: &Path) -> bool {
    let trust_store = std::env::var_os("COSH_SHELL_PROJECT_TRUST_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            home.join(".copilot-shell")
                .join("cosh")
                .join("trusted-project-hooks")
        });
    let expected = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    fs::read_to_string(trust_store)
        .ok()
        .into_iter()
        .flat_map(|content| {
            content
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(PathBuf::from)
                .collect::<Vec<_>>()
        })
        .map(|root| root.canonicalize().unwrap_or(root))
        .any(|root| root == expected)
}
