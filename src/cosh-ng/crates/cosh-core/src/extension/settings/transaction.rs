//! Crash-safe staging, commit, rollback, and recovery for setting mutations.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SettingsTransactionAction {
    Set,
    Unset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SettingsTransactionPhase {
    Staged,
    CommitIntent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct SettingsTransactionJournal {
    pub(super) schema_version: u32,
    pub(super) operation_id: String,
    pub(super) extension: String,
    pub(super) key: String,
    pub(super) scope: SettingScope,
    pub(super) sensitive: bool,
    pub(super) action: SettingsTransactionAction,
    pub(super) plain_value: Option<Value>,
    pub(super) staged_secret_extension: Option<String>,
    pub(super) phase: SettingsTransactionPhase,
}

/// Staged setting mutation kept invisible to the active store until candidate validation passes.
pub struct PendingSettingMutation {
    pub(super) journal: SettingsTransactionJournal,
    pub(super) lock: SettingsTransactionLock,
}

#[derive(Debug)]
pub(super) struct SettingsTransactionLock {
    file: File,
}

impl Drop for SettingsTransactionLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SettingsRecoveryResult {
    pub(crate) rolled_back: usize,
    pub(crate) finalized: usize,
}

impl ExtensionSettings {
    /// Stages a parsed value without changing the active setting store.
    pub fn begin_set(
        &self,
        operation_id: &str,
        extension: &Extension,
        key: &str,
        raw_value: &str,
        scope: SettingScope,
    ) -> Result<PendingSettingMutation, SettingsError> {
        self.begin_mutation(
            operation_id,
            extension,
            key,
            scope,
            SettingsTransactionAction::Set,
            Some(raw_value),
        )
    }

    /// Stages removal of one scoped value without changing the active setting store.
    pub fn begin_unset(
        &self,
        operation_id: &str,
        extension: &Extension,
        key: &str,
        scope: SettingScope,
    ) -> Result<PendingSettingMutation, SettingsError> {
        self.begin_mutation(
            operation_id,
            extension,
            key,
            scope,
            SettingsTransactionAction::Unset,
            None,
        )
    }

    /// Creates a read-only candidate view backed by the staged mutation.
    pub fn with_candidate(&self, pending: &PendingSettingMutation) -> Self {
        Self {
            user_root: self.user_root.clone(),
            workspace_root: self.workspace_root.clone(),
            workspace_trusted: self.workspace_trusted,
            secret_backend: Arc::clone(&self.secret_backend),
            overlay: Some(pending.journal.clone()),
        }
    }

    /// Makes a health-validated staged value durable and returns its effective safe view.
    pub fn commit(
        &self,
        mut pending: PendingSettingMutation,
        extension: &Extension,
    ) -> Result<SettingView, SettingsError> {
        pending.journal.phase = SettingsTransactionPhase::CommitIntent;
        self.write_journal(&pending.journal)?;
        self.apply_journal(&pending.journal)?;
        self.cleanup_journal(&pending.journal)?;
        let key = pending.journal.key.clone();
        drop(pending.lock);
        self.get(extension, &key)
    }

    /// Discards one unvalidated staged value without touching the active setting store.
    pub fn rollback(&self, pending: PendingSettingMutation) -> Result<(), SettingsError> {
        self.cleanup_journal(&pending.journal)?;
        drop(pending.lock);
        Ok(())
    }

    /// Recovers abandoned setting transactions according to their persisted commit intent.
    pub(crate) fn recover_pending(&self) -> Result<SettingsRecoveryResult, SettingsError> {
        let _lock = self.lock_transactions()?;
        self.recover_pending_locked()
    }

    fn begin_mutation(
        &self,
        operation_id: &str,
        extension: &Extension,
        key: &str,
        scope: SettingScope,
        action: SettingsTransactionAction,
        raw_value: Option<&str>,
    ) -> Result<PendingSettingMutation, SettingsError> {
        uuid::Uuid::parse_str(operation_id).map_err(|_| {
            SettingsError::new(
                "extension_setting_operation_id_invalid",
                "setting transaction id must be a UUID",
            )
        })?;
        let definition = find_definition(extension, key)?;
        self.validate_mutation_scope(definition, scope)?;
        let plain_value = if definition.sensitive {
            None
        } else {
            match action {
                SettingsTransactionAction::Set => Some(parse_value(
                    definition.setting_type,
                    raw_value.ok_or_else(|| {
                        SettingsError::new(
                            "extension_setting_value_missing",
                            "setting value is required for set",
                        )
                    })?,
                )?),
                SettingsTransactionAction::Unset => None,
            }
        };
        let staged_secret_extension = (definition.sensitive
            && action == SettingsTransactionAction::Set)
            .then(|| format!("pending.{operation_id}.{}", extension.name));
        let journal = SettingsTransactionJournal {
            schema_version: SETTINGS_SCHEMA_VERSION,
            operation_id: operation_id.to_string(),
            extension: extension.name.clone(),
            key: key.to_string(),
            scope,
            sensitive: definition.sensitive,
            action,
            plain_value,
            staged_secret_extension,
            phase: SettingsTransactionPhase::Staged,
        };
        validate_transaction_journal(&journal)?;

        let lock = self.lock_transactions()?;
        self.recover_pending_locked()?;
        self.write_journal(&journal)?;
        if let Some(staged_extension) = &journal.staged_secret_extension {
            if let Err(error) = self.secret_backend.set(
                staged_extension,
                &journal.key,
                raw_value.unwrap_or_default(),
            ) {
                let _ = self.remove_journal_file(&journal);
                return Err(error);
            }
        }
        Ok(PendingSettingMutation { journal, lock })
    }

    fn validate_mutation_scope(
        &self,
        definition: &SettingDefinition,
        scope: SettingScope,
    ) -> Result<(), SettingsError> {
        if definition.sensitive && scope == SettingScope::Workspace {
            return Err(SettingsError::new(
                "extension_sensitive_workspace_forbidden",
                "sensitive settings cannot use workspace scope",
            ));
        }
        if scope == SettingScope::Workspace && !self.workspace_trusted {
            return Err(SettingsError::new(
                "extension_workspace_untrusted",
                "workspace settings require an explicitly trusted project root",
            ));
        }
        Ok(())
    }

    fn lock_transactions(&self) -> Result<SettingsTransactionLock, SettingsError> {
        fs::create_dir_all(&self.user_root).map_err(|error| {
            SettingsError::new(
                "extension_settings_lock_failed",
                format!("failed to create {}: {error}", self.user_root.display()),
            )
        })?;
        let path = self.user_root.join(SETTINGS_TRANSACTION_LOCK);
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| {
                SettingsError::new(
                    "extension_settings_lock_failed",
                    format!("failed to open {}: {error}", path.display()),
                )
            })?;
        let started = Instant::now();
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(SettingsTransactionLock { file }),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if started.elapsed() >= SETTINGS_LOCK_TIMEOUT {
                        return Err(SettingsError::new(
                            "extension_settings_lock_timeout",
                            format!("timed out waiting for {}", path.display()),
                        ));
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => {
                    return Err(SettingsError::new(
                        "extension_settings_lock_failed",
                        format!("failed to lock {}: {error}", path.display()),
                    ))
                }
            }
        }
    }

    fn recover_pending_locked(&self) -> Result<SettingsRecoveryResult, SettingsError> {
        let mut result = SettingsRecoveryResult::default();
        for (expected_scope, root) in self.transaction_roots() {
            let entries = match fs::read_dir(&root) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(SettingsError::new(
                        "extension_settings_recovery_failed",
                        format!("failed to scan {}: {error}", root.display()),
                    ))
                }
            };
            for entry in entries {
                let entry = entry.map_err(|error| {
                    SettingsError::new(
                        "extension_settings_recovery_failed",
                        format!("failed to read {}: {error}", root.display()),
                    )
                })?;
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let journal = read_transaction_journal(&path)?;
                if journal.scope != expected_scope {
                    return Err(SettingsError::new(
                        "extension_settings_journal_invalid",
                        format!(
                            "journal scope does not match transaction root: {}",
                            path.display()
                        ),
                    ));
                }
                let expected = format!("{}.json", journal.operation_id);
                if entry.file_name().to_string_lossy() != expected {
                    return Err(SettingsError::new(
                        "extension_settings_journal_invalid",
                        format!(
                            "journal filename does not match operation id: {}",
                            path.display()
                        ),
                    ));
                }
                match journal.phase {
                    SettingsTransactionPhase::Staged => {
                        self.cleanup_journal(&journal)?;
                        result.rolled_back += 1;
                    }
                    SettingsTransactionPhase::CommitIntent => {
                        self.apply_journal(&journal)?;
                        self.cleanup_journal(&journal)?;
                        result.finalized += 1;
                    }
                }
            }
        }
        Ok(result)
    }

    fn apply_journal(&self, journal: &SettingsTransactionJournal) -> Result<(), SettingsError> {
        validate_transaction_journal(journal)?;
        if journal.sensitive {
            return match journal.action {
                SettingsTransactionAction::Set => {
                    let staged_extension =
                        journal.staged_secret_extension.as_deref().ok_or_else(|| {
                            SettingsError::new(
                                "extension_settings_journal_invalid",
                                "sensitive set journal is missing its staged secret reference",
                            )
                        })?;
                    let value = self
                        .secret_backend
                        .get(staged_extension, &journal.key)?
                        .ok_or_else(|| {
                            SettingsError::new(
                                "extension_settings_staged_secret_missing",
                                "staged secret is unavailable; active value was not changed",
                            )
                        })?;
                    self.secret_backend
                        .set(&journal.extension, &journal.key, &value)
                }
                SettingsTransactionAction::Unset => {
                    self.secret_backend.delete(&journal.extension, &journal.key)
                }
            };
        }
        let path = self.store_path(journal.scope, &journal.extension);
        mutate_store(&path, &journal.extension, |values| match journal.action {
            SettingsTransactionAction::Set => {
                if let Some(value) = &journal.plain_value {
                    values.insert(journal.key.clone(), value.clone());
                }
            }
            SettingsTransactionAction::Unset => {
                values.remove(&journal.key);
            }
        })
    }

    fn cleanup_journal(&self, journal: &SettingsTransactionJournal) -> Result<(), SettingsError> {
        // Once the active value is applied, recovery must never depend on staged data that
        // cleanup may already have removed. Orphan cleanup is therefore best-effort only.
        self.remove_journal_file(journal)?;
        if let Some(staged_extension) = &journal.staged_secret_extension {
            if let Err(error) = self.secret_backend.delete(staged_extension, &journal.key) {
                tracing::warn!(
                    code = error.code(),
                    extension = %journal.extension,
                    key = %journal.key,
                    "failed to remove staged extension secret"
                );
            }
        }
        Ok(())
    }

    fn remove_journal_file(
        &self,
        journal: &SettingsTransactionJournal,
    ) -> Result<(), SettingsError> {
        let path = self.journal_path(journal);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(SettingsError::new(
                "extension_settings_write_failed",
                format!("failed to remove {}: {error}", path.display()),
            )),
        }
    }

    pub(super) fn write_journal(
        &self,
        journal: &SettingsTransactionJournal,
    ) -> Result<(), SettingsError> {
        validate_transaction_journal(journal)?;
        write_json_atomic_secure(&self.journal_path(journal), journal)
    }

    pub(super) fn journal_path(&self, journal: &SettingsTransactionJournal) -> PathBuf {
        self.transaction_root(journal.scope)
            .join(format!("{}.json", journal.operation_id))
    }

    pub(super) fn transaction_root(&self, scope: SettingScope) -> PathBuf {
        match scope {
            SettingScope::User => self.user_root.join(SETTINGS_TRANSACTION_DIR),
            SettingScope::Workspace => self
                .workspace_root
                .join(".copilot-shell")
                .join(WORKSPACE_TRANSACTION_DIR),
        }
    }

    fn transaction_roots(&self) -> Vec<(SettingScope, PathBuf)> {
        let mut roots = vec![(
            SettingScope::User,
            self.transaction_root(SettingScope::User),
        )];
        if self.workspace_trusted {
            roots.push((
                SettingScope::Workspace,
                self.transaction_root(SettingScope::Workspace),
            ));
        }
        roots
    }

    fn store_path(&self, scope: SettingScope, extension: &str) -> PathBuf {
        match scope {
            SettingScope::User => self.user_store_path(extension),
            SettingScope::Workspace => self.workspace_store_path(),
        }
    }
}

fn validate_transaction_journal(journal: &SettingsTransactionJournal) -> Result<(), SettingsError> {
    if journal.schema_version != SETTINGS_SCHEMA_VERSION {
        return Err(SettingsError::new(
            "extension_settings_journal_schema_unsupported",
            format!(
                "unsupported settings transaction schema {}",
                journal.schema_version
            ),
        ));
    }
    uuid::Uuid::parse_str(&journal.operation_id).map_err(|_| {
        SettingsError::new(
            "extension_settings_journal_invalid",
            "settings journal operation id must be a UUID",
        )
    })?;
    super::super::identity::validate_package_name(&journal.extension).map_err(|error| {
        SettingsError::new("extension_settings_journal_invalid", error.to_string())
    })?;
    super::super::identity::validate_setting_key(&journal.key).map_err(|error| {
        SettingsError::new("extension_settings_journal_invalid", error.to_string())
    })?;
    if journal.sensitive && journal.scope != SettingScope::User {
        return Err(SettingsError::new(
            "extension_settings_journal_invalid",
            "sensitive setting journal must use user scope",
        ));
    }
    match (journal.sensitive, journal.action) {
        (true, SettingsTransactionAction::Set)
            if journal.plain_value.is_some() || journal.staged_secret_extension.is_none() =>
        {
            Err(SettingsError::new(
                "extension_settings_journal_invalid",
                "sensitive set journal must contain only a staged secret reference",
            ))
        }
        (true, SettingsTransactionAction::Unset)
            if journal.plain_value.is_some() || journal.staged_secret_extension.is_some() =>
        {
            Err(SettingsError::new(
                "extension_settings_journal_invalid",
                "sensitive unset journal contains unexpected staged data",
            ))
        }
        (false, SettingsTransactionAction::Set)
            if journal.plain_value.is_none() || journal.staged_secret_extension.is_some() =>
        {
            Err(SettingsError::new(
                "extension_settings_journal_invalid",
                "plain set journal is missing its value or contains a secret reference",
            ))
        }
        (false, SettingsTransactionAction::Unset)
            if journal.plain_value.is_some() || journal.staged_secret_extension.is_some() =>
        {
            Err(SettingsError::new(
                "extension_settings_journal_invalid",
                "plain unset journal contains unexpected staged data",
            ))
        }
        _ => Ok(()),
    }
}

fn read_transaction_journal(path: &Path) -> Result<SettingsTransactionJournal, SettingsError> {
    let bytes = fs::read(path).map_err(|error| {
        SettingsError::new(
            "extension_settings_recovery_failed",
            format!("failed to read {}: {error}", path.display()),
        )
    })?;
    let journal = serde_json::from_slice(&bytes).map_err(|error| {
        SettingsError::new(
            "extension_settings_journal_invalid",
            format!("failed to parse {}: {error}", path.display()),
        )
    })?;
    validate_transaction_journal(&journal)?;
    Ok(journal)
}

fn write_json_atomic_secure<T: Serialize>(path: &Path, value: &T) -> Result<(), SettingsError> {
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
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        SettingsError::new(
            "extension_settings_write_failed",
            format!("failed to serialize settings transaction: {error}"),
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
    })
}
