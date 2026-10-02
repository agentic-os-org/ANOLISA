//! Store-locked commit, rollback, uninstall, and crash recovery.

use super::support::*;
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct PreparedOperation {
    pub(super) schema_version: u32,
    pub(super) preflight: ExtensionPreflight,
    pub(super) prepared_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum UninstallPhase {
    Rollback,
    CommitIntent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct UninstallJournal {
    schema_version: u32,
    operation_id: String,
    name: String,
    source_identity: String,
    #[serde(default)]
    previous_state: Option<state::ExtensionState>,
    phase: UninstallPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PendingCommitPhase {
    PublishedUnvalidated,
    CommitIntent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PendingCommitJournal {
    schema_version: u32,
    operation_id: String,
    name: String,
    action: OperationAction,
    changed: bool,
    previous_state: state::ExtensionState,
    phase: PendingCommitPhase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PendingStateJournal {
    schema_version: u32,
    operation_id: String,
    previous_state: state::ExtensionState,
    phase: PendingCommitPhase,
}

/// Provisional filesystem/catalog mutation held under the store lock.
pub struct PendingMutation {
    result: ExtensionMutationResult,
    lock: StoreLock,
}

/// Provisional uninstall held until the removal candidate passes runtime health.
pub struct PendingUninstall {
    result: ExtensionMutationResult,
    lock: StoreLock,
}

impl PendingUninstall {
    /// Returns the provisional typed result used to validate the removal candidate.
    pub fn result(&self) -> &ExtensionMutationResult {
        &self.result
    }
}

/// Desired-state mutation held under the package store lock until candidate health passes.
pub struct PendingStateMutation {
    operation_id: String,
    lock: StoreLock,
}

/// Settings mutation held under the package store lock until candidate health passes.
pub struct PendingSettingsMutationLock {
    operation_id: String,
    lock: StoreLock,
}

impl PendingSettingsMutationLock {
    /// Returns the transaction identity shared with the settings journal.
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }
}

impl PendingMutation {
    /// Returns the provisional typed result used to build a runtime candidate.
    pub fn result(&self) -> &ExtensionMutationResult {
        &self.result
    }
}

/// Result of deterministic managed-store recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RecoveryResult {
    /// Desired-state/source-selection mutations restored before candidate validation.
    pub rolled_back_state_mutations: usize,
    /// Health-validated state commit intents finalized after interruption.
    pub finalized_state_mutations: usize,
    /// Provisional package/state publications rolled back before health validation.
    pub rolled_back_pending_commits: usize,
    /// Health-validated commit intents completed after interruption.
    pub finalized_pending_commits: usize,
    /// Incomplete uninstalls restored to their previous installation.
    pub restored_uninstalls: usize,
    /// Uninstalls completed from a persisted commit intent.
    pub completed_uninstalls: usize,
    /// Updates restored to the old installation before publication completed.
    pub restored_updates: usize,
    /// Updates finalized after the new installation had already been published.
    pub completed_updates: usize,
    /// Staging directories discarded because no operation record owned them.
    pub removed_orphan_staging: usize,
}

impl ExtensionInstaller {
    /// Commits exactly the prepared fingerprint accepted by the user.
    pub fn commit(
        &self,
        operation_id: &str,
        accepted_fingerprint: &str,
    ) -> Result<ExtensionMutationResult, InstallerError> {
        let pending = self.commit_pending(operation_id, accepted_fingerprint)?;
        self.finalize_pending(pending)
    }

    /// Publishes package and state provisionally while retaining rollback state and lock.
    pub fn commit_pending(
        &self,
        operation_id: &str,
        accepted_fingerprint: &str,
    ) -> Result<PendingMutation, InstallerError> {
        validate_operation_id(operation_id)?;
        let lock = self
            .store
            .lock(STORE_LOCK_TIMEOUT)
            .map_err(InstallerError::from_source)?;
        let operation = self.read_operation(operation_id)?;
        if Utc::now().signed_duration_since(operation.prepared_at)
            > chrono::Duration::minutes(OPERATION_TTL_MINUTES)
        {
            return Err(InstallerError::new(
                "extension_operation_expired",
                "prepared operation expired; run preflight again",
            ));
        }
        let expected = &operation.preflight;
        if accepted_fingerprint != expected.capability_fingerprint {
            return Err(InstallerError::new(
                "extension_consent_fingerprint_mismatch",
                "accepted fingerprint does not match the prepared security projection",
            ));
        }

        let staging = self.store.staging(operation_id);
        let destination = self.store.installation(&expected.name);
        let rollback = self.store.rollback(operation_id);
        let (prepared_root, already_published) = if staging.exists() {
            (staging.as_path(), false)
        } else if destination.exists() {
            let metadata = read_installation_metadata(&destination)?;
            if metadata.consent_reference != operation_id {
                return Err(InstallerError::new(
                    "extension_preflight_stale",
                    format!(
                        "prepared payload is no longer available for {}",
                        expected.name
                    ),
                ));
            }
            (destination.as_path(), true)
        } else {
            return Err(InstallerError::new(
                "extension_preflight_stale",
                "prepared payload is missing; run preflight again",
            ));
        };
        let payload = prepared_root.join(PAYLOAD_DIR);
        let parsed = read_manifest(&payload)?;
        let digest = content_digest(&payload).map_err(InstallerError::from_source)?;
        if parsed.config.name != expected.name
            || parsed.config.version != expected.version
            || parsed.capability_fingerprint != expected.capability_fingerprint
            || digest != expected.content_digest
        {
            return Err(InstallerError::new(
                "extension_preflight_stale",
                "prepared package changed after preflight; run preflight again",
            ));
        }
        validate_prepared_metadata(prepared_root, operation_id, expected)?;

        if expected.action != OperationAction::Update
            && !already_published
            && (destination.exists()
                || legacy_user_installation_exists(&self.store.root, &expected.name)?)
        {
            remove_dir_if_exists(&staging)?;
            remove_file_if_exists(&self.store.operation(operation_id))?;
            return Err(InstallerError::new(
                "extension_already_installed",
                format!("a user installation already exists for {}", expected.name),
            ));
        }

        let previous_state = state::load(self.state_dir_override.as_deref())
            .map_err(|error| InstallerError::new(error.code(), error.to_string()))?
            .state;
        let journal_path = self.store.pending_commit_journal(operation_id);
        let journal = PendingCommitJournal {
            schema_version: OPERATION_SCHEMA_VERSION,
            operation_id: operation_id.to_string(),
            name: expected.name.clone(),
            action: expected.action,
            changed: expected.changed,
            previous_state: previous_state.clone(),
            phase: PendingCommitPhase::PublishedUnvalidated,
        };
        write_json_atomic(&journal_path, &journal)?;

        if expected.action == OperationAction::Update {
            if !already_published {
                let current = if destination.exists() {
                    destination.as_path()
                } else if rollback.exists() {
                    rollback.as_path()
                } else {
                    return Err(InstallerError::new(
                        "extension_update_recovery_conflict",
                        "both current installation and rollback candidate are missing",
                    ));
                };
                validate_current_update(current, expected)?;
                if !expected.changed {
                    let result = mutation_result(operation_id, expected);
                    return Ok(PendingMutation { result, lock });
                }
                if destination.exists() && !rollback.exists() {
                    fs::rename(&destination, &rollback).map_err(|error| {
                        InstallerError::new(
                            "extension_update_switch_failed",
                            format!("failed to preserve previous installation: {error}"),
                        )
                    })?;
                } else if destination.exists() || !rollback.exists() {
                    return Err(InstallerError::new(
                        "extension_update_recovery_conflict",
                        "update switch state is ambiguous; keep rollback and operation for doctor",
                    ));
                }
                if let Err(error) = fs::rename(&staging, &destination) {
                    let restore_error = fs::rename(&rollback, &destination).err();
                    let detail = restore_error.map_or_else(
                        || format!("failed to publish update: {error}"),
                        |restore| {
                            format!(
                                "failed to publish update: {error}; restore also failed: {restore}"
                            )
                        },
                    );
                    return Err(InstallerError::new("extension_update_rolled_back", detail));
                }
            }
        } else {
            if !already_published {
                fs::rename(&staging, &destination).map_err(|error| {
                    InstallerError::new(
                        "extension_commit_failed",
                        format!(
                            "failed to publish {} as {}: {error}",
                            staging.display(),
                            destination.display()
                        ),
                    )
                })?;
            }

            let mut next_state = previous_state.clone();
            next_state.source_selections.insert(
                expected.name.clone(),
                SourceSelectionRecord {
                    source: SourceSelection::User,
                    source_identity: expected.source_identity.clone(),
                },
            );
            if let Err(error) = state::save(&next_state, self.state_dir_override.as_deref()) {
                let rollback_error = fs::rename(&destination, &staging).err();
                let detail = match rollback_error {
                    Some(rollback) => format!(
                        "failed to persist extension state: {error}; rollback also failed: {rollback}"
                    ),
                    None => format!("failed to persist extension state: {error}"),
                };
                return Err(InstallerError::new("extension_commit_rolled_back", detail));
            }
        }

        let result = mutation_result(operation_id, expected);
        Ok(PendingMutation { result, lock })
    }

    /// Makes a health-validated provisional mutation durable and releases rollback state.
    pub fn finalize_pending(
        &self,
        pending: PendingMutation,
    ) -> Result<ExtensionMutationResult, InstallerError> {
        let operation_id = pending.result.operation_id.clone();
        let journal_path = self.store.pending_commit_journal(&operation_id);
        let mut journal = self.read_pending_commit_journal(&operation_id)?;
        journal.phase = PendingCommitPhase::CommitIntent;
        write_json_atomic(&journal_path, &journal)?;
        self.finalize_pending_locked(&pending.result)?;
        let result = pending.result;
        drop(pending.lock);
        Ok(result)
    }

    /// Restores package and state after candidate construction or health validation fails.
    pub fn rollback_pending(&self, pending: PendingMutation) -> Result<(), InstallerError> {
        let operation_id = pending.result.operation_id.clone();
        self.rollback_pending_locked(&operation_id)?;
        drop(pending.lock);
        Ok(())
    }

    /// Starts a crash-recoverable desired-state or source-selection mutation.
    pub fn begin_state_mutation(&self) -> Result<PendingStateMutation, InstallerError> {
        self.recover()?;
        let lock = self
            .store
            .lock(STORE_LOCK_TIMEOUT)
            .map_err(InstallerError::from_source)?;
        let operation_id = Uuid::new_v4().to_string();
        let previous_state = state::load(self.state_dir_override.as_deref())
            .map_err(|error| InstallerError::new(error.code(), error.to_string()))?
            .state;
        write_json_atomic(
            &self.store.pending_state_journal(&operation_id),
            &PendingStateJournal {
                schema_version: OPERATION_SCHEMA_VERSION,
                operation_id: operation_id.clone(),
                previous_state,
                phase: PendingCommitPhase::PublishedUnvalidated,
            },
        )?;
        Ok(PendingStateMutation { operation_id, lock })
    }

    /// Commits a health-validated desired-state mutation.
    pub fn finalize_state_mutation(
        &self,
        pending: PendingStateMutation,
    ) -> Result<(), InstallerError> {
        let path = self.store.pending_state_journal(&pending.operation_id);
        let mut journal = self.read_pending_state_journal(&pending.operation_id)?;
        journal.phase = PendingCommitPhase::CommitIntent;
        write_json_atomic(&path, &journal)?;
        remove_file_if_exists(&path)?;
        drop(pending.lock);
        Ok(())
    }

    /// Restores the exact state document after candidate validation fails.
    pub fn rollback_state_mutation(
        &self,
        pending: PendingStateMutation,
    ) -> Result<(), InstallerError> {
        self.rollback_state_mutation_locked(&pending.operation_id)?;
        drop(pending.lock);
        Ok(())
    }

    /// Serializes one settings transaction with package and desired-state mutations.
    pub fn begin_settings_mutation(&self) -> Result<PendingSettingsMutationLock, InstallerError> {
        self.recover()?;
        let lock = self
            .store
            .lock(STORE_LOCK_TIMEOUT)
            .map_err(InstallerError::from_source)?;
        Ok(PendingSettingsMutationLock {
            operation_id: Uuid::new_v4().to_string(),
            lock,
        })
    }

    /// Releases the package store lock after the settings journal is committed or rolled back.
    pub fn finish_settings_mutation(&self, pending: PendingSettingsMutationLock) {
        drop(pending.lock);
    }

    /// Cancels a prepared operation and removes its staged payload.
    pub fn cancel(&self, operation_id: &str) -> Result<(), InstallerError> {
        self.recover()?;
        validate_operation_id(operation_id)?;
        let _lock = self
            .store
            .lock(STORE_LOCK_TIMEOUT)
            .map_err(InstallerError::from_source)?;
        if self.store.rollback(operation_id).exists() {
            return Err(InstallerError::new(
                "extension_operation_commit_in_progress",
                "operation has entered atomic switch and cannot be cancelled",
            ));
        }
        remove_dir_if_exists(&self.store.staging(operation_id))?;
        remove_file_if_exists(&self.store.operation(operation_id))
    }

    /// Removes one managed installation and rolls back if state persistence fails.
    pub fn uninstall(&self, name: &str) -> Result<ExtensionMutationResult, InstallerError> {
        let pending = self.uninstall_pending(name)?;
        self.finalize_uninstall(pending)
    }

    /// Removes package/state provisionally while retaining the old installation and lock.
    pub fn uninstall_pending(&self, name: &str) -> Result<PendingUninstall, InstallerError> {
        self.recover()?;
        super::super::identity::validate_package_name(name)
            .map_err(|error| InstallerError::new(error.code(), error.to_string()))?;
        let lock = self
            .store
            .lock(STORE_LOCK_TIMEOUT)
            .map_err(InstallerError::from_source)?;
        let installation = self.store.installation(name);
        let metadata = read_installation_metadata(&installation)?;
        let parsed = read_manifest(&installation.join(PAYLOAD_DIR))?;
        let risk_summary = capability_risk_summary(&parsed, &installation.join(PAYLOAD_DIR));
        let operation_id = Uuid::new_v4().to_string();
        let rollback = self.store.rollback(&operation_id);
        let journal_path = self.store.rollback_journal(&operation_id);
        let previous_state = state::load(self.state_dir_override.as_deref())
            .map_err(|error| InstallerError::new(error.code(), error.to_string()))?
            .state;
        let journal = UninstallJournal {
            schema_version: OPERATION_SCHEMA_VERSION,
            operation_id: operation_id.clone(),
            name: name.to_string(),
            source_identity: metadata.source_identity.clone(),
            previous_state: Some(previous_state),
            phase: UninstallPhase::Rollback,
        };
        write_json_atomic(&journal_path, &journal)?;
        fs::rename(&installation, &rollback).map_err(|error| {
            InstallerError::new(
                "extension_uninstall_failed",
                format!("failed to stage uninstall for {name}: {error}"),
            )
        })?;

        let next_state = self.state_without_user_selection(name, &metadata.source_identity)?;
        if let Err(error) = state::save(&next_state, self.state_dir_override.as_deref()) {
            return Err(InstallerError::new(
                "extension_uninstall_recovery_required",
                format!(
                    "failed to persist extension state: {error}; commit-intent journal retained"
                ),
            ));
        }
        let result = uninstall_mutation_result(&operation_id, name, &metadata, risk_summary);
        self.store
            .ensure_internal_dirs()
            .map_err(InstallerError::from_source)?;
        Ok(PendingUninstall { result, lock })
    }

    /// Finalizes a runtime-validated uninstall and removes its rollback candidate.
    pub fn finalize_uninstall(
        &self,
        pending: PendingUninstall,
    ) -> Result<ExtensionMutationResult, InstallerError> {
        let operation_id = pending.result.operation_id.clone();
        let journal_path = self.store.rollback_journal(&operation_id);
        let mut journal = self.read_uninstall_journal(&operation_id)?;
        journal.phase = UninstallPhase::CommitIntent;
        write_json_atomic(&journal_path, &journal)?;
        self.write_receipt(&operation_id, &pending.result)?;
        remove_dir_if_exists(&self.store.rollback(&operation_id))?;
        remove_file_if_exists(&journal_path)?;
        let result = pending.result;
        drop(pending.lock);
        Ok(result)
    }

    /// Restores a provisionally removed package and its exact prior state.
    pub fn rollback_uninstall(&self, pending: PendingUninstall) -> Result<(), InstallerError> {
        let operation_id = pending.result.operation_id.clone();
        self.rollback_uninstall_locked(&operation_id)?;
        drop(pending.lock);
        Ok(())
    }

    /// Recovers interrupted transactions according to their persisted intent.
    pub fn recover(&self) -> Result<RecoveryResult, InstallerError> {
        let _lock = self
            .store
            .lock(STORE_LOCK_TIMEOUT)
            .map_err(InstallerError::from_source)?;
        self.store
            .ensure_internal_dirs()
            .map_err(InstallerError::from_source)?;
        let mut result = RecoveryResult {
            rolled_back_state_mutations: 0,
            finalized_state_mutations: 0,
            rolled_back_pending_commits: 0,
            finalized_pending_commits: 0,
            restored_uninstalls: 0,
            completed_uninstalls: 0,
            restored_updates: 0,
            completed_updates: 0,
            removed_orphan_staging: 0,
        };

        let rollback_root = self.store.root.join(ROLLBACK_DIR);
        for entry in fs::read_dir(&rollback_root).map_err(|error| {
            InstallerError::new(
                "extension_recovery_failed",
                format!("failed to scan {}: {error}", rollback_root.display()),
            )
        })? {
            let entry = entry.map_err(|error| {
                InstallerError::new(
                    "extension_recovery_failed",
                    format!("failed to read pending state journal entry: {error}"),
                )
            })?;
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let Some(operation_id) = file_name.strip_suffix(".state.json") else {
                continue;
            };
            validate_operation_id(operation_id)?;
            let journal = self.read_pending_state_journal(operation_id)?;
            match journal.phase {
                PendingCommitPhase::PublishedUnvalidated => {
                    self.rollback_state_mutation_locked(operation_id)?;
                    result.rolled_back_state_mutations += 1;
                }
                PendingCommitPhase::CommitIntent => {
                    remove_file_if_exists(&entry.path())?;
                    result.finalized_state_mutations += 1;
                }
            }
        }

        for entry in fs::read_dir(&rollback_root).map_err(|error| {
            InstallerError::new(
                "extension_recovery_failed",
                format!("failed to scan {}: {error}", rollback_root.display()),
            )
        })? {
            let entry = entry.map_err(|error| {
                InstallerError::new(
                    "extension_recovery_failed",
                    format!("failed to read pending commit journal entry: {error}"),
                )
            })?;
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let Some(operation_id) = file_name.strip_suffix(".commit.json") else {
                continue;
            };
            validate_operation_id(operation_id)?;
            let journal = self.read_pending_commit_journal(operation_id)?;
            match journal.phase {
                PendingCommitPhase::PublishedUnvalidated => {
                    self.rollback_pending_locked(operation_id)?;
                    result.rolled_back_pending_commits += 1;
                }
                PendingCommitPhase::CommitIntent => {
                    let mutation = if self.store.receipt(operation_id).exists() {
                        self.result(operation_id)?
                    } else {
                        let operation = self.read_operation(operation_id)?;
                        mutation_result(operation_id, &operation.preflight)
                    };
                    self.finalize_pending_locked(&mutation)?;
                    result.finalized_pending_commits += 1;
                }
            }
        }

        for entry in fs::read_dir(&rollback_root).map_err(|error| {
            InstallerError::new(
                "extension_recovery_failed",
                format!("failed to scan {}: {error}", rollback_root.display()),
            )
        })? {
            let entry = entry.map_err(|error| {
                InstallerError::new(
                    "extension_recovery_failed",
                    format!("failed to read rollback journal entry: {error}"),
                )
            })?;
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if !file_name.ends_with(".uninstall.json") {
                continue;
            }
            let bytes = fs::read(entry.path()).map_err(|error| {
                InstallerError::new(
                    "extension_recovery_failed",
                    format!("failed to read {}: {error}", entry.path().display()),
                )
            })?;
            let journal: UninstallJournal = serde_json::from_slice(&bytes).map_err(|error| {
                InstallerError::new(
                    "extension_recovery_journal_invalid",
                    format!("failed to parse {}: {error}", entry.path().display()),
                )
            })?;
            if journal.schema_version != OPERATION_SCHEMA_VERSION {
                return Err(InstallerError::new(
                    "extension_recovery_journal_schema_unsupported",
                    format!("unsupported recovery schema {}", journal.schema_version),
                ));
            }
            validate_operation_id(&journal.operation_id)?;
            let rollback = self.store.rollback(&journal.operation_id);
            match journal.phase {
                UninstallPhase::Rollback => {
                    self.rollback_uninstall_locked(&journal.operation_id)?;
                    result.restored_uninstalls += 1;
                }
                UninstallPhase::CommitIntent => {
                    let next_state =
                        self.state_without_user_selection(&journal.name, &journal.source_identity)?;
                    state::save(&next_state, self.state_dir_override.as_deref())
                        .map_err(|error| InstallerError::new(error.code(), error.to_string()))?;
                    if !self.store.receipt(&journal.operation_id).exists() {
                        let metadata = read_installation_metadata(&rollback)?;
                        let parsed = read_manifest(&rollback.join(PAYLOAD_DIR))?;
                        let risk_summary =
                            capability_risk_summary(&parsed, &rollback.join(PAYLOAD_DIR));
                        let mutation = uninstall_mutation_result(
                            &journal.operation_id,
                            &journal.name,
                            &metadata,
                            risk_summary,
                        );
                        self.write_receipt(&journal.operation_id, &mutation)?;
                    }
                    remove_dir_if_exists(&rollback)?;
                    remove_file_if_exists(&entry.path())?;
                    result.completed_uninstalls += 1;
                }
            }
        }

        for entry in fs::read_dir(&rollback_root).map_err(|error| {
            InstallerError::new(
                "extension_recovery_failed",
                format!("failed to rescan {}: {error}", rollback_root.display()),
            )
        })? {
            let entry = entry.map_err(|error| {
                InstallerError::new(
                    "extension_recovery_failed",
                    format!("failed to read update rollback entry: {error}"),
                )
            })?;
            if !entry.path().is_dir() {
                continue;
            }
            let operation_id = entry.file_name().to_string_lossy().into_owned();
            if Uuid::parse_str(&operation_id).is_err()
                || !self.store.operation(&operation_id).exists()
            {
                continue;
            }
            let operation = self.read_operation(&operation_id)?;
            if operation.preflight.action != OperationAction::Update {
                continue;
            }
            let expected = &operation.preflight;
            let staging = self.store.staging(&operation_id);
            let destination = self.store.installation(&expected.name);
            if staging.exists() && !destination.exists() {
                validate_current_update(&entry.path(), expected)?;
                fs::rename(entry.path(), &destination).map_err(|error| {
                    InstallerError::new(
                        "extension_recovery_failed",
                        format!("failed to restore interrupted update: {error}"),
                    )
                })?;
                result.restored_updates += 1;
            } else if !staging.exists() && destination.exists() {
                validate_prepared_metadata(&destination, &operation_id, expected)?;
                validate_current_update(&entry.path(), expected)?;
                let mutation = mutation_result(&operation_id, expected);
                self.write_receipt(&operation_id, &mutation)?;
                remove_dir_if_exists(&entry.path())?;
                remove_file_if_exists(&self.store.operation(&operation_id))?;
                result.completed_updates += 1;
            } else {
                return Err(InstallerError::new(
                    "extension_update_recovery_conflict",
                    format!(
                        "cannot recover update {} with staging={} destination={}",
                        operation_id,
                        staging.exists(),
                        destination.exists()
                    ),
                ));
            }
        }

        let staging_root = self.store.root.join(STAGING_DIR);
        for entry in fs::read_dir(&staging_root).map_err(|error| {
            InstallerError::new(
                "extension_recovery_failed",
                format!("failed to scan {}: {error}", staging_root.display()),
            )
        })? {
            let entry = entry.map_err(|error| {
                InstallerError::new(
                    "extension_recovery_failed",
                    format!("failed to read staging entry: {error}"),
                )
            })?;
            if !entry.path().is_dir() {
                continue;
            }
            let operation_id = entry.file_name().to_string_lossy().into_owned();
            if Uuid::parse_str(&operation_id).is_err() {
                continue;
            }
            if !self.store.operation(&operation_id).exists() {
                remove_dir_if_exists(&entry.path())?;
                result.removed_orphan_staging += 1;
            }
        }
        Ok(result)
    }

    pub(super) fn read_operation(
        &self,
        operation_id: &str,
    ) -> Result<PreparedOperation, InstallerError> {
        let path = self.store.operation(operation_id);
        let bytes = fs::read(&path).map_err(|error| {
            InstallerError::new(
                "extension_operation_not_found",
                format!("failed to read {}: {error}", path.display()),
            )
        })?;
        let operation: PreparedOperation = serde_json::from_slice(&bytes).map_err(|error| {
            InstallerError::new(
                "extension_operation_invalid",
                format!("failed to parse {}: {error}", path.display()),
            )
        })?;
        if operation.schema_version != OPERATION_SCHEMA_VERSION {
            return Err(InstallerError::new(
                "extension_operation_schema_unsupported",
                format!("unsupported operation schema {}", operation.schema_version),
            ));
        }
        Ok(operation)
    }

    fn write_receipt(
        &self,
        operation_id: &str,
        result: &ExtensionMutationResult,
    ) -> Result<(), InstallerError> {
        write_json_atomic(&self.store.receipt(operation_id), result)
    }

    fn state_without_user_selection(
        &self,
        name: &str,
        source_identity: &str,
    ) -> Result<super::super::state::ExtensionState, InstallerError> {
        let mut next_state = state::load(self.state_dir_override.as_deref())
            .map_err(|error| InstallerError::new(error.code(), error.to_string()))?
            .state;
        if next_state
            .source_selections
            .get(name)
            .is_some_and(|selection| {
                selection.source == SourceSelection::User
                    && selection.source_identity == source_identity
            })
        {
            next_state.source_selections.remove(name);
        }
        Ok(next_state)
    }

    fn read_pending_commit_journal(
        &self,
        operation_id: &str,
    ) -> Result<PendingCommitJournal, InstallerError> {
        let path = self.store.pending_commit_journal(operation_id);
        let bytes = fs::read(&path).map_err(|error| {
            InstallerError::new(
                "extension_pending_commit_missing",
                format!("failed to read {}: {error}", path.display()),
            )
        })?;
        let journal: PendingCommitJournal = serde_json::from_slice(&bytes).map_err(|error| {
            InstallerError::new(
                "extension_pending_commit_invalid",
                format!("failed to parse {}: {error}", path.display()),
            )
        })?;
        if journal.schema_version != OPERATION_SCHEMA_VERSION
            || journal.operation_id != operation_id
        {
            return Err(InstallerError::new(
                "extension_pending_commit_invalid",
                "pending commit journal identity or schema does not match",
            ));
        }
        Ok(journal)
    }

    fn read_uninstall_journal(
        &self,
        operation_id: &str,
    ) -> Result<UninstallJournal, InstallerError> {
        let path = self.store.rollback_journal(operation_id);
        let bytes = fs::read(&path).map_err(|error| {
            InstallerError::new(
                "extension_uninstall_journal_missing",
                format!("failed to read {}: {error}", path.display()),
            )
        })?;
        let journal: UninstallJournal = serde_json::from_slice(&bytes).map_err(|error| {
            InstallerError::new(
                "extension_recovery_journal_invalid",
                format!("failed to parse {}: {error}", path.display()),
            )
        })?;
        if journal.schema_version != OPERATION_SCHEMA_VERSION
            || journal.operation_id != operation_id
        {
            return Err(InstallerError::new(
                "extension_recovery_journal_invalid",
                "uninstall journal identity or schema does not match",
            ));
        }
        Ok(journal)
    }

    fn read_pending_state_journal(
        &self,
        operation_id: &str,
    ) -> Result<PendingStateJournal, InstallerError> {
        let path = self.store.pending_state_journal(operation_id);
        let bytes = fs::read(&path).map_err(|error| {
            InstallerError::new(
                "extension_state_transaction_missing",
                format!("failed to read {}: {error}", path.display()),
            )
        })?;
        let journal: PendingStateJournal = serde_json::from_slice(&bytes).map_err(|error| {
            InstallerError::new(
                "extension_state_transaction_invalid",
                format!("failed to parse {}: {error}", path.display()),
            )
        })?;
        if journal.schema_version != OPERATION_SCHEMA_VERSION
            || journal.operation_id != operation_id
        {
            return Err(InstallerError::new(
                "extension_state_transaction_invalid",
                "state transaction identity or schema does not match",
            ));
        }
        Ok(journal)
    }

    fn rollback_state_mutation_locked(&self, operation_id: &str) -> Result<(), InstallerError> {
        let journal = self.read_pending_state_journal(operation_id)?;
        state::save(&journal.previous_state, self.state_dir_override.as_deref()).map_err(
            |error| {
                InstallerError::new(
                    "extension_state_transaction_rollback_failed",
                    format!("failed to restore previous extension state: {error}"),
                )
            },
        )?;
        remove_file_if_exists(&self.store.pending_state_journal(operation_id))
    }

    fn rollback_uninstall_locked(&self, operation_id: &str) -> Result<(), InstallerError> {
        let journal = self.read_uninstall_journal(operation_id)?;
        let rollback = self.store.rollback(operation_id);
        let installation = self.store.installation(&journal.name);
        if rollback.exists() && installation.exists() {
            return Err(InstallerError::new(
                "extension_recovery_conflict",
                format!("both installation and rollback exist for {}", journal.name),
            ));
        }
        if rollback.exists() {
            fs::rename(&rollback, &installation).map_err(|error| {
                InstallerError::new(
                    "extension_uninstall_rollback_failed",
                    format!("failed to restore {}: {error}", journal.name),
                )
            })?;
        }
        if let Some(previous_state) = &journal.previous_state {
            state::save(previous_state, self.state_dir_override.as_deref()).map_err(|error| {
                InstallerError::new(
                    "extension_uninstall_rollback_failed",
                    format!("failed to restore previous extension state: {error}"),
                )
            })?;
        }
        remove_file_if_exists(&self.store.rollback_journal(operation_id))
    }

    fn finalize_pending_locked(
        &self,
        result: &ExtensionMutationResult,
    ) -> Result<(), InstallerError> {
        let operation_id = &result.operation_id;
        self.write_receipt(operation_id, result)?;
        remove_dir_if_exists(&self.store.rollback(operation_id))?;
        remove_dir_if_exists(&self.store.staging(operation_id))?;
        remove_file_if_exists(&self.store.operation(operation_id))?;
        remove_file_if_exists(&self.store.pending_commit_journal(operation_id))
    }

    fn rollback_pending_locked(&self, operation_id: &str) -> Result<(), InstallerError> {
        let journal = self.read_pending_commit_journal(operation_id)?;
        let staging = self.store.staging(operation_id);
        let destination = self.store.installation(&journal.name);
        let rollback = self.store.rollback(operation_id);

        match journal.action {
            OperationAction::Update if journal.changed => {
                if rollback.exists() {
                    if destination.exists() {
                        if staging.exists() {
                            return Err(InstallerError::new(
                                "extension_pending_rollback_conflict",
                                "candidate destination and staging both exist",
                            ));
                        }
                        fs::rename(&destination, &staging).map_err(|error| {
                            InstallerError::new(
                                "extension_pending_rollback_failed",
                                format!("failed to preserve rejected candidate: {error}"),
                            )
                        })?;
                    }
                    fs::rename(&rollback, &destination).map_err(|error| {
                        InstallerError::new(
                            "extension_pending_rollback_failed",
                            format!("failed to restore previous installation: {error}"),
                        )
                    })?;
                }
            }
            OperationAction::Install | OperationAction::Link
                if journal.changed && destination.exists() =>
            {
                if staging.exists() {
                    return Err(InstallerError::new(
                        "extension_pending_rollback_conflict",
                        "candidate destination and staging both exist",
                    ));
                }
                fs::rename(&destination, &staging).map_err(|error| {
                    InstallerError::new(
                        "extension_pending_rollback_failed",
                        format!("failed to unpublish rejected installation: {error}"),
                    )
                })?;
            }
            _ => {}
        }

        state::save(&journal.previous_state, self.state_dir_override.as_deref()).map_err(
            |error| {
                InstallerError::new(
                    "extension_pending_rollback_failed",
                    format!("failed to restore previous extension state: {error}"),
                )
            },
        )?;
        remove_file_if_exists(&self.store.pending_commit_journal(operation_id))
    }
}
