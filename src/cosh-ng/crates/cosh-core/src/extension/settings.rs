//! Typed extension settings with fail-closed secret and workspace handling.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Extension, SettingDefinition, SettingType};

const SETTINGS_SCHEMA_VERSION: u32 = 1;
const USER_SETTINGS_DIR: &str = "extension-settings";
const WORKSPACE_SETTINGS_FILE: &str = "extension-settings.json";
const KEYRING_SERVICE: &str = "cosh-ng.extensions";
const SETTINGS_TRANSACTION_DIR: &str = ".transactions";
const WORKSPACE_TRANSACTION_DIR: &str = ".extension-settings-transactions";
const SETTINGS_TRANSACTION_LOCK: &str = ".transactions.lock";
const SETTINGS_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

mod secret;
mod store;
mod transaction;

pub use secret::{KeyringSecretBackend, SecretBackend};
use store::{
    display_value, find_definition, is_workspace_trusted, load_store, mutate_store, parse_value,
    validate_value_type,
};
use transaction::{
    SettingsTransactionAction, SettingsTransactionJournal, SettingsTransactionPhase,
};

/// Persisted setting scope selected by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingScope {
    /// User-level value shared across workspaces.
    User,
    /// Value local to the trusted current workspace.
    Workspace,
}

impl SettingScope {
    /// Parses the stable registry scope spelling.
    pub fn parse(value: &str) -> Result<Self, SettingsError> {
        match value {
            "user" => Ok(Self::User),
            "workspace" => Ok(Self::Workspace),
            _ => Err(SettingsError::new(
                "extension_setting_scope_invalid",
                "setting scope must be user or workspace",
            )),
        }
    }
}

/// Redaction-safe setting response returned by the registry.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SettingView {
    /// Manifest setting key.
    pub key: String,
    /// Declared scalar type.
    pub setting_type: SettingType,
    /// Effective source, or the requested scope for an unset scoped query.
    pub scope: Option<SettingScope>,
    /// True when a persisted or default value exists.
    pub configured: bool,
    /// True when the value is held by the secret backend.
    pub sensitive: bool,
    /// Effective non-sensitive value. Sensitive values are always omitted.
    pub value: Option<Value>,
    /// Safe display value.
    pub display: String,
    /// Whether activation requires a resolved value.
    pub required: bool,
}

/// Stable settings failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsError {
    code: &'static str,
    message: String,
}

impl SettingsError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Returns the stable machine-readable failure code.
    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for SettingsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SettingsError {}

/// Typed settings owner for one current workspace.
pub struct ExtensionSettings {
    user_root: PathBuf,
    workspace_root: PathBuf,
    workspace_trusted: bool,
    secret_backend: Arc<dyn SecretBackend>,
    overlay: Option<SettingsTransactionJournal>,
}

impl ExtensionSettings {
    /// Creates the production settings service.
    pub fn new(workspace_root: PathBuf) -> Result<Self, SettingsError> {
        let home = dirs::home_dir().ok_or_else(|| {
            SettingsError::new(
                "extension_settings_path_unavailable",
                "cannot determine the user home directory",
            )
        })?;
        let user_root = home.join(".copilot-shell").join(USER_SETTINGS_DIR);
        let workspace_trusted = is_workspace_trusted(&workspace_root, &home);
        let settings = Self {
            user_root,
            workspace_root,
            workspace_trusted,
            secret_backend: Arc::new(KeyringSecretBackend),
            overlay: None,
        };
        settings.recover_pending()?;
        Ok(settings)
    }

    /// Returns whether the current workspace is present in the shell trust store.
    pub fn workspace_trusted(&self) -> bool {
        self.workspace_trusted
    }

    /// Creates an isolated service with an injected secret backend.
    #[cfg(test)]
    pub fn new_isolated(
        user_root: PathBuf,
        workspace_root: PathBuf,
        workspace_trusted: bool,
        secret_backend: Arc<dyn SecretBackend>,
    ) -> Self {
        Self {
            user_root,
            workspace_root,
            workspace_trusted,
            secret_backend,
            overlay: None,
        }
    }

    /// Stages a parsed value without changing the active setting store.
    /// Lists all declared settings using effective precedence.
    pub fn list(&self, extension: &Extension) -> Result<Vec<SettingView>, SettingsError> {
        extension
            .settings
            .iter()
            .map(|definition| self.effective_view(extension, definition))
            .collect()
    }

    /// Lists all declared settings at one exact persisted scope.
    pub fn list_scoped(
        &self,
        extension: &Extension,
        scope: SettingScope,
    ) -> Result<Vec<SettingView>, SettingsError> {
        extension
            .settings
            .iter()
            .map(|definition| self.scoped_view(extension, definition, scope))
            .collect()
    }

    /// Reads one effective setting.
    pub fn get(&self, extension: &Extension, key: &str) -> Result<SettingView, SettingsError> {
        let definition = find_definition(extension, key)?;
        self.effective_view(extension, definition)
    }

    /// Reads one setting at an exact persisted scope without fallback.
    pub fn get_scoped(
        &self,
        extension: &Extension,
        key: &str,
        scope: SettingScope,
    ) -> Result<SettingView, SettingsError> {
        let definition = find_definition(extension, key)?;
        self.scoped_view(extension, definition, scope)
    }

    /// Parses and persists one scoped value.
    pub fn set(
        &self,
        extension: &Extension,
        key: &str,
        raw_value: &str,
        scope: SettingScope,
    ) -> Result<SettingView, SettingsError> {
        let definition = find_definition(extension, key)?;
        if definition.sensitive {
            if scope == SettingScope::Workspace {
                return Err(SettingsError::new(
                    "extension_sensitive_workspace_forbidden",
                    "sensitive settings cannot use workspace scope",
                ));
            }
            self.secret_backend.set(&extension.name, key, raw_value)?;
            return self.effective_view(extension, definition);
        }
        let parsed = parse_value(definition.setting_type, raw_value)?;
        self.mutate_file(extension, scope, |values| {
            values.insert(key.to_string(), parsed);
        })?;
        self.effective_view(extension, definition)
    }

    /// Removes one scoped value and returns the fallback effective view.
    pub fn unset(
        &self,
        extension: &Extension,
        key: &str,
        scope: SettingScope,
    ) -> Result<SettingView, SettingsError> {
        let definition = find_definition(extension, key)?;
        if definition.sensitive {
            if scope == SettingScope::Workspace {
                return Err(SettingsError::new(
                    "extension_sensitive_workspace_forbidden",
                    "sensitive settings cannot use workspace scope",
                ));
            }
            self.secret_backend.delete(&extension.name, key)?;
            return self.effective_view(extension, definition);
        }
        self.mutate_file(extension, scope, |values| {
            values.remove(key);
        })?;
        self.effective_view(extension, definition)
    }

    /// Resolves a value for runtime substitution.
    pub fn resolve(
        &self,
        extension: &Extension,
        key: &str,
    ) -> Result<Option<Value>, SettingsError> {
        let definition = find_definition(extension, key)?;
        if definition.sensitive {
            if let Some(overlay) = self.matching_overlay(extension, key) {
                return match overlay.action {
                    SettingsTransactionAction::Set => {
                        let staged_extension =
                            overlay.staged_secret_extension.as_deref().ok_or_else(|| {
                                SettingsError::new(
                                    "extension_settings_journal_invalid",
                                    "sensitive candidate is missing its staged secret reference",
                                )
                            })?;
                        self.secret_backend
                            .get(staged_extension, key)
                            .map(|value| value.map(Value::String))
                    }
                    SettingsTransactionAction::Unset => Ok(None),
                };
            }
            return self
                .secret_backend
                .get(&extension.name, key)
                .map(|value| value.map(Value::String));
        }
        self.resolve_plain(extension, definition)
            .map(|resolved| resolved.map(|(_, value)| value))
    }

    /// Validates all required values before activation.
    pub fn validate_required(&self, extension: &Extension) -> Result<(), SettingsError> {
        for definition in extension
            .settings
            .iter()
            .filter(|definition| definition.required)
        {
            if self.resolve(extension, &definition.key)?.is_none() {
                return Err(SettingsError::new(
                    "extension_setting_required_missing",
                    format!(
                        "required setting {} is not configured for {}",
                        definition.key, extension.name
                    ),
                ));
            }
        }
        Ok(())
    }

    fn effective_view(
        &self,
        extension: &Extension,
        definition: &SettingDefinition,
    ) -> Result<SettingView, SettingsError> {
        if definition.sensitive {
            let configured = self
                .secret_backend
                .get(&extension.name, &definition.key)?
                .is_some();
            return Ok(SettingView {
                key: definition.key.clone(),
                setting_type: definition.setting_type,
                scope: configured.then_some(SettingScope::User),
                configured,
                sensitive: true,
                value: None,
                display: if configured {
                    "[redacted]".to_string()
                } else {
                    "[not configured]".to_string()
                },
                required: definition.required,
            });
        }
        let resolved = self.resolve_plain(extension, definition)?;
        let (scope, value) = match resolved {
            Some((scope, value)) => (scope, Some(value)),
            None => (None, None),
        };
        Ok(SettingView {
            key: definition.key.clone(),
            setting_type: definition.setting_type,
            scope,
            configured: value.is_some(),
            sensitive: false,
            display: value
                .as_ref()
                .map(display_value)
                .unwrap_or_else(|| "[not configured]".to_string()),
            value,
            required: definition.required,
        })
    }

    fn scoped_view(
        &self,
        extension: &Extension,
        definition: &SettingDefinition,
        scope: SettingScope,
    ) -> Result<SettingView, SettingsError> {
        if scope == SettingScope::Workspace && !self.workspace_trusted {
            return Err(SettingsError::new(
                "extension_workspace_untrusted",
                "workspace settings require an explicitly trusted project root",
            ));
        }
        if definition.sensitive {
            if scope == SettingScope::Workspace {
                return Err(SettingsError::new(
                    "extension_sensitive_workspace_forbidden",
                    "sensitive settings cannot use workspace scope",
                ));
            }
            let configured = self
                .secret_backend
                .get(&extension.name, &definition.key)?
                .is_some();
            return Ok(SettingView {
                key: definition.key.clone(),
                setting_type: definition.setting_type,
                scope: Some(scope),
                configured,
                sensitive: true,
                value: None,
                display: if configured {
                    "[redacted]".to_string()
                } else {
                    "[not configured]".to_string()
                },
                required: definition.required,
            });
        }
        let store = match scope {
            SettingScope::User => load_store(&self.user_store_path(&extension.name))?,
            SettingScope::Workspace => load_store(&self.workspace_store_path())?,
        };
        let value = store
            .extensions
            .get(&extension.name)
            .and_then(|values| values.get(&definition.key))
            .cloned();
        if let Some(value) = &value {
            validate_value_type(definition.setting_type, value)?;
        }
        Ok(SettingView {
            key: definition.key.clone(),
            setting_type: definition.setting_type,
            scope: Some(scope),
            configured: value.is_some(),
            sensitive: false,
            display: value
                .as_ref()
                .map(display_value)
                .unwrap_or_else(|| "[not configured]".to_string()),
            value,
            required: definition.required,
        })
    }

    fn resolve_plain(
        &self,
        extension: &Extension,
        definition: &SettingDefinition,
    ) -> Result<Option<(Option<SettingScope>, Value)>, SettingsError> {
        if self.workspace_trusted {
            let workspace_overlay = self
                .matching_overlay(extension, &definition.key)
                .filter(|overlay| overlay.scope == SettingScope::Workspace);
            match workspace_overlay.map(|overlay| overlay.action) {
                Some(SettingsTransactionAction::Set) => {
                    let value = workspace_overlay
                        .and_then(|overlay| overlay.plain_value.clone())
                        .ok_or_else(|| {
                            SettingsError::new(
                                "extension_settings_journal_invalid",
                                "plain set candidate is missing its value",
                            )
                        })?;
                    validate_value_type(definition.setting_type, &value)?;
                    return Ok(Some((Some(SettingScope::Workspace), value)));
                }
                Some(SettingsTransactionAction::Unset) => {}
                None => {
                    let workspace = load_store(&self.workspace_store_path())?;
                    if let Some(value) = workspace
                        .extensions
                        .get(&extension.name)
                        .and_then(|values| values.get(&definition.key))
                    {
                        validate_value_type(definition.setting_type, value)?;
                        return Ok(Some((Some(SettingScope::Workspace), value.clone())));
                    }
                }
            }
        }
        let user_overlay = self
            .matching_overlay(extension, &definition.key)
            .filter(|overlay| overlay.scope == SettingScope::User);
        match user_overlay.map(|overlay| overlay.action) {
            Some(SettingsTransactionAction::Set) => {
                let value = user_overlay
                    .and_then(|overlay| overlay.plain_value.clone())
                    .ok_or_else(|| {
                        SettingsError::new(
                            "extension_settings_journal_invalid",
                            "plain set candidate is missing its value",
                        )
                    })?;
                validate_value_type(definition.setting_type, &value)?;
                return Ok(Some((Some(SettingScope::User), value)));
            }
            Some(SettingsTransactionAction::Unset) => {}
            None => {
                let user = load_store(&self.user_store_path(&extension.name))?;
                if let Some(value) = user
                    .extensions
                    .get(&extension.name)
                    .and_then(|values| values.get(&definition.key))
                {
                    validate_value_type(definition.setting_type, value)?;
                    return Ok(Some((Some(SettingScope::User), value.clone())));
                }
            }
        }
        if let Some(default) = &definition.default {
            validate_value_type(definition.setting_type, default)?;
            return Ok(Some((None, default.clone())));
        }
        Ok(None)
    }

    fn matching_overlay<'a>(
        &'a self,
        extension: &Extension,
        key: &str,
    ) -> Option<&'a SettingsTransactionJournal> {
        self.overlay.as_ref().filter(|overlay| {
            overlay.extension == extension.name
                && overlay.key == key
                && overlay.phase == SettingsTransactionPhase::Staged
        })
    }

    fn mutate_file(
        &self,
        extension: &Extension,
        scope: SettingScope,
        mutation: impl FnOnce(&mut BTreeMap<String, Value>),
    ) -> Result<(), SettingsError> {
        if scope == SettingScope::Workspace && !self.workspace_trusted {
            return Err(SettingsError::new(
                "extension_workspace_untrusted",
                "workspace settings require an explicitly trusted project root",
            ));
        }
        let path = match scope {
            SettingScope::User => self.user_store_path(&extension.name),
            SettingScope::Workspace => self.workspace_store_path(),
        };
        mutate_store(&path, &extension.name, mutation)
    }

    fn user_store_path(&self, extension: &str) -> PathBuf {
        self.user_root.join(format!("{extension}.json"))
    }

    fn workspace_store_path(&self) -> PathBuf {
        self.workspace_root
            .join(".copilot-shell")
            .join(WORKSPACE_SETTINGS_FILE)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::extension::{ExtensionManager, EXTENSION_CONFIG_FILENAME};

    #[derive(Default)]
    struct MemorySecrets {
        values: Mutex<BTreeMap<String, String>>,
    }

    impl SecretBackend for MemorySecrets {
        fn set(&self, extension: &str, key: &str, value: &str) -> Result<(), SettingsError> {
            self.values
                .lock()
                .unwrap()
                .insert(format!("{extension}/{key}"), value.to_string());
            Ok(())
        }

        fn get(&self, extension: &str, key: &str) -> Result<Option<String>, SettingsError> {
            Ok(self
                .values
                .lock()
                .unwrap()
                .get(&format!("{extension}/{key}"))
                .cloned())
        }

        fn delete(&self, extension: &str, key: &str) -> Result<(), SettingsError> {
            self.values
                .lock()
                .unwrap()
                .remove(&format!("{extension}/{key}"));
            Ok(())
        }
    }

    struct UnavailableSecrets;

    impl SecretBackend for UnavailableSecrets {
        fn set(&self, _extension: &str, _key: &str, _value: &str) -> Result<(), SettingsError> {
            Err(SettingsError::new(
                "extension_secret_backend_unavailable",
                "secret backend unavailable",
            ))
        }

        fn get(&self, _extension: &str, _key: &str) -> Result<Option<String>, SettingsError> {
            Err(SettingsError::new(
                "extension_secret_backend_unavailable",
                "secret backend unavailable",
            ))
        }

        fn delete(&self, _extension: &str, _key: &str) -> Result<(), SettingsError> {
            Err(SettingsError::new(
                "extension_secret_backend_unavailable",
                "secret backend unavailable",
            ))
        }
    }

    #[derive(Default)]
    struct DeleteFailSecrets {
        values: Mutex<BTreeMap<String, String>>,
    }

    impl SecretBackend for DeleteFailSecrets {
        fn set(&self, extension: &str, key: &str, value: &str) -> Result<(), SettingsError> {
            self.values
                .lock()
                .unwrap()
                .insert(format!("{extension}/{key}"), value.to_string());
            Ok(())
        }

        fn get(&self, extension: &str, key: &str) -> Result<Option<String>, SettingsError> {
            Ok(self
                .values
                .lock()
                .unwrap()
                .get(&format!("{extension}/{key}"))
                .cloned())
        }

        fn delete(&self, _extension: &str, _key: &str) -> Result<(), SettingsError> {
            Err(SettingsError::new(
                "extension_secret_cleanup_failed",
                "injected secret cleanup failure",
            ))
        }
    }

    fn extension(root: &Path) -> Extension {
        let user = root.join("extensions");
        let package = user.join("example.ops");
        let system = root.join("system");
        fs::create_dir_all(&package).unwrap();
        fs::create_dir_all(&system).unwrap();
        fs::write(
            package.join(EXTENSION_CONFIG_FILENAME),
            r#"{
                "schemaVersion": 1,
                "name": "example.ops",
                "version": "1.0.0",
                "compatibility": {"cosh": ">=0.12.0"},
                "settings": [
                    {
                        "key": "region",
                        "type": "string",
                        "description": "region",
                        "default": "default-region"
                    },
                    {
                        "key": "retries",
                        "type": "integer",
                        "description": "retry count"
                    },
                    {
                        "key": "token",
                        "type": "string",
                        "description": "access token",
                        "required": true,
                        "sensitive": true
                    }
                ]
            }"#,
        )
        .unwrap();
        let mut manager = ExtensionManager::new_isolated_with_state(
            root.join("workspace"),
            Some(user),
            Some(system),
            root.join("state"),
        );
        manager.refresh();
        manager.list()[0].clone()
    }

    #[test]
    fn workspace_overrides_user_and_unset_falls_back() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let extension = extension(temporary.path());
        let settings = ExtensionSettings::new_isolated(
            temporary.path().join("settings"),
            workspace,
            true,
            Arc::new(MemorySecrets::default()),
        );

        settings
            .set(&extension, "region", "user-region", SettingScope::User)
            .unwrap();
        settings
            .set(
                &extension,
                "region",
                "workspace-region",
                SettingScope::Workspace,
            )
            .unwrap();
        let view = settings.get(&extension, "region").unwrap();
        assert_eq!(view.value, Some(Value::String("workspace-region".into())));
        assert_eq!(view.scope, Some(SettingScope::Workspace));

        let view = settings
            .unset(&extension, "region", SettingScope::Workspace)
            .unwrap();
        assert_eq!(view.value, Some(Value::String("user-region".into())));
        assert_eq!(view.scope, Some(SettingScope::User));

        let view = settings
            .unset(&extension, "region", SettingScope::User)
            .unwrap();
        assert_eq!(view.value, Some(Value::String("default-region".into())));
        assert_eq!(view.scope, None);
    }

    #[test]
    fn type_error_has_zero_mutation() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let extension = extension(temporary.path());
        let settings = ExtensionSettings::new_isolated(
            temporary.path().join("settings"),
            workspace,
            true,
            Arc::new(MemorySecrets::default()),
        );
        let error = settings
            .set(&extension, "retries", "many", SettingScope::User)
            .unwrap_err();
        assert_eq!(error.code(), "extension_setting_type_invalid");
        assert!(!temporary.path().join("settings/example.ops.json").exists());
    }

    #[test]
    fn sensitive_values_are_redacted_and_never_written_to_files() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let extension = extension(temporary.path());
        let secrets = Arc::new(MemorySecrets::default());
        let settings = ExtensionSettings::new_isolated(
            temporary.path().join("settings"),
            workspace,
            true,
            secrets,
        );

        let view = settings
            .set(&extension, "token", "top-secret-value", SettingScope::User)
            .unwrap();
        assert!(view.configured);
        assert_eq!(view.value, None);
        assert_eq!(view.display, "[redacted]");
        settings.validate_required(&extension).unwrap();

        let files = fs::read_dir(temporary.path().join("settings"))
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|entry| fs::read_to_string(entry.path()).ok())
            .collect::<String>();
        assert!(!files.contains("top-secret-value"));

        let view = settings
            .unset(&extension, "token", SettingScope::User)
            .unwrap();
        assert!(!view.configured);
        let error = settings.validate_required(&extension).unwrap_err();
        assert_eq!(error.code(), "extension_setting_required_missing");
    }

    #[test]
    fn workspace_secrets_and_untrusted_workspace_fail_closed() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let extension = extension(temporary.path());
        let settings = ExtensionSettings::new_isolated(
            temporary.path().join("settings"),
            workspace,
            false,
            Arc::new(MemorySecrets::default()),
        );

        let error = settings
            .set(&extension, "region", "local", SettingScope::Workspace)
            .unwrap_err();
        assert_eq!(error.code(), "extension_workspace_untrusted");
        let error = settings
            .set(&extension, "token", "secret", SettingScope::Workspace)
            .unwrap_err();
        assert_eq!(error.code(), "extension_sensitive_workspace_forbidden");
    }

    #[test]
    fn unavailable_secret_backend_fails_closed_without_value_echo() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let extension = extension(temporary.path());
        let settings = ExtensionSettings::new_isolated(
            temporary.path().join("settings"),
            workspace,
            true,
            Arc::new(UnavailableSecrets),
        );
        let error = settings
            .set(&extension, "token", "must-not-echo", SettingScope::User)
            .unwrap_err();
        assert_eq!(error.code(), "extension_secret_backend_unavailable");
        assert!(!error.to_string().contains("must-not-echo"));
    }

    #[test]
    fn plain_transaction_is_visible_only_to_candidate_until_commit() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let extension = extension(temporary.path());
        let settings = ExtensionSettings::new_isolated(
            temporary.path().join("settings"),
            workspace,
            true,
            Arc::new(MemorySecrets::default()),
        );
        let operation_id = uuid::Uuid::new_v4().to_string();
        let pending = settings
            .begin_set(
                &operation_id,
                &extension,
                "region",
                "candidate-region",
                SettingScope::User,
            )
            .unwrap();

        assert_eq!(
            settings.get(&extension, "region").unwrap().value,
            Some(Value::String("default-region".into()))
        );
        let candidate = settings.with_candidate(&pending);
        assert_eq!(
            candidate.get(&extension, "region").unwrap().value,
            Some(Value::String("candidate-region".into()))
        );

        let committed = settings.commit(pending, &extension).unwrap();
        assert_eq!(
            committed.value,
            Some(Value::String("candidate-region".into()))
        );
        assert!(!settings
            .transaction_root(SettingScope::User)
            .join(format!("{operation_id}.json"))
            .exists());
    }

    #[test]
    fn sensitive_transaction_journal_contains_only_a_staging_reference() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let extension = extension(temporary.path());
        let secrets = Arc::new(MemorySecrets::default());
        let settings = ExtensionSettings::new_isolated(
            temporary.path().join("settings"),
            workspace,
            true,
            secrets.clone(),
        );
        settings
            .set(&extension, "token", "old-secret", SettingScope::User)
            .unwrap();
        let operation_id = uuid::Uuid::new_v4().to_string();
        let pending = settings
            .begin_set(
                &operation_id,
                &extension,
                "token",
                "new-secret",
                SettingScope::User,
            )
            .unwrap();
        let journal = fs::read_to_string(settings.journal_path(&pending.journal)).unwrap();
        assert!(!journal.contains("new-secret"));
        assert!(!journal.contains("old-secret"));
        assert_eq!(
            settings.resolve(&extension, "token").unwrap(),
            Some(Value::String("old-secret".into()))
        );
        let candidate = settings.with_candidate(&pending);
        assert_eq!(
            candidate.resolve(&extension, "token").unwrap(),
            Some(Value::String("new-secret".into()))
        );

        settings.rollback(pending).unwrap();
        assert_eq!(
            settings.resolve(&extension, "token").unwrap(),
            Some(Value::String("old-secret".into()))
        );
        assert_eq!(secrets.values.lock().unwrap().len(), 1);
    }

    #[test]
    fn recovery_discards_unvalidated_stage_and_finishes_commit_intent() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let extension = extension(temporary.path());
        let settings = ExtensionSettings::new_isolated(
            temporary.path().join("settings"),
            workspace,
            true,
            Arc::new(MemorySecrets::default()),
        );

        let abandoned_id = uuid::Uuid::new_v4().to_string();
        let abandoned = settings
            .begin_set(
                &abandoned_id,
                &extension,
                "region",
                "abandoned",
                SettingScope::User,
            )
            .unwrap();
        drop(abandoned);
        let recovery = settings.recover_pending().unwrap();
        assert_eq!(recovery.rolled_back, 1);
        assert_eq!(recovery.finalized, 0);
        assert_eq!(
            settings.get(&extension, "region").unwrap().value,
            Some(Value::String("default-region".into()))
        );

        let committed_id = uuid::Uuid::new_v4().to_string();
        let mut interrupted = settings
            .begin_set(
                &committed_id,
                &extension,
                "region",
                "recovered",
                SettingScope::User,
            )
            .unwrap();
        interrupted.journal.phase = SettingsTransactionPhase::CommitIntent;
        settings.write_journal(&interrupted.journal).unwrap();
        drop(interrupted);
        let recovery = settings.recover_pending().unwrap();
        assert_eq!(recovery.rolled_back, 0);
        assert_eq!(recovery.finalized, 1);
        assert_eq!(
            settings.get(&extension, "region").unwrap().value,
            Some(Value::String("recovered".into()))
        );
    }

    #[test]
    fn recovery_skips_untrusted_workspace_journals() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        let user_root = temporary.path().join("settings");
        let transaction_root = workspace
            .join(".copilot-shell")
            .join(WORKSPACE_TRANSACTION_DIR);
        fs::create_dir_all(&transaction_root).unwrap();
        let operation_id = uuid::Uuid::new_v4().to_string();
        let journal = SettingsTransactionJournal {
            schema_version: SETTINGS_SCHEMA_VERSION,
            operation_id: operation_id.clone(),
            extension: "example.ops".to_string(),
            key: "region".to_string(),
            scope: SettingScope::User,
            sensitive: false,
            action: SettingsTransactionAction::Set,
            plain_value: Some(Value::String("forged".to_string())),
            staged_secret_extension: None,
            phase: SettingsTransactionPhase::CommitIntent,
        };
        let journal_path = transaction_root.join(format!("{operation_id}.json"));
        fs::write(&journal_path, serde_json::to_vec(&journal).unwrap()).unwrap();
        let settings = ExtensionSettings::new_isolated(
            user_root.clone(),
            workspace,
            false,
            Arc::new(MemorySecrets::default()),
        );

        let recovery = settings.recover_pending().unwrap();
        assert_eq!(recovery.rolled_back, 0);
        assert_eq!(recovery.finalized, 0);
        assert!(journal_path.exists());
        assert!(!user_root.join("example.ops.json").exists());
    }

    #[test]
    fn recovery_rejects_scope_that_does_not_match_scanned_root() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        let user_root = temporary.path().join("settings");
        let transaction_root = workspace
            .join(".copilot-shell")
            .join(WORKSPACE_TRANSACTION_DIR);
        fs::create_dir_all(&transaction_root).unwrap();
        let operation_id = uuid::Uuid::new_v4().to_string();
        let journal = SettingsTransactionJournal {
            schema_version: SETTINGS_SCHEMA_VERSION,
            operation_id: operation_id.clone(),
            extension: "example.ops".to_string(),
            key: "region".to_string(),
            scope: SettingScope::User,
            sensitive: false,
            action: SettingsTransactionAction::Set,
            plain_value: Some(Value::String("forged".to_string())),
            staged_secret_extension: None,
            phase: SettingsTransactionPhase::CommitIntent,
        };
        fs::write(
            transaction_root.join(format!("{operation_id}.json")),
            serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        let settings = ExtensionSettings::new_isolated(
            user_root.clone(),
            workspace,
            true,
            Arc::new(MemorySecrets::default()),
        );

        let error = settings.recover_pending().unwrap_err();
        assert_eq!(error.code(), "extension_settings_journal_invalid");
        assert!(!user_root.join("example.ops.json").exists());
    }

    #[test]
    fn sensitive_cleanup_failure_cannot_leave_commit_intent_replay() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let extension = extension(temporary.path());
        let secrets = Arc::new(DeleteFailSecrets::default());
        let settings = ExtensionSettings::new_isolated(
            temporary.path().join("settings"),
            workspace,
            true,
            secrets,
        );
        let operation_id = uuid::Uuid::new_v4().to_string();
        let pending = settings
            .begin_set(
                &operation_id,
                &extension,
                "token",
                "new-secret",
                SettingScope::User,
            )
            .unwrap();

        settings.commit(pending, &extension).unwrap();

        assert!(!settings
            .transaction_root(SettingScope::User)
            .join(format!("{operation_id}.json"))
            .exists());
        let recovery = settings.recover_pending().unwrap();
        assert_eq!(recovery.rolled_back, 0);
        assert_eq!(recovery.finalized, 0);
    }
}
