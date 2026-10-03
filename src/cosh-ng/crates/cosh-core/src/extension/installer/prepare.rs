//! Local and Git source preparation for reviewable installer operations.

use super::support::*;
use super::transaction::PreparedOperation;
use super::*;

impl ExtensionInstaller {
    pub(super) fn prepare_local(
        &self,
        source: &Path,
        action: OperationAction,
    ) -> Result<ExtensionPreflight, InstallerError> {
        let canonical = canonical_local_source(source).map_err(InstallerError::from_source)?;
        let _lock = self
            .store
            .lock(STORE_LOCK_TIMEOUT)
            .map_err(InstallerError::from_source)?;
        self.store
            .ensure_internal_dirs()
            .map_err(InstallerError::from_source)?;
        let canonical_store = self.store.root.canonicalize().map_err(|error| {
            InstallerError::new(
                "extension_store_unavailable",
                format!("failed to resolve {}: {error}", self.store.root.display()),
            )
        })?;
        if canonical.starts_with(&canonical_store) || canonical_store.starts_with(&canonical) {
            return Err(InstallerError::new(
                "extension_source_overlaps_store",
                "local extension source must not contain or reside inside the managed store",
            ));
        }
        let operation_id = Uuid::new_v4().to_string();
        let staging = self.store.staging(&operation_id);
        fs::create_dir(&staging).map_err(|error| {
            InstallerError::new(
                "extension_staging_failed",
                format!("failed to create {}: {error}", staging.display()),
            )
        })?;
        let payload = staging.join(PAYLOAD_DIR);
        let prepared = (|| {
            match action {
                OperationAction::Install => {
                    copy_package_tree(&canonical, &payload).map_err(InstallerError::from_source)?
                }
                OperationAction::Link => create_directory_link(&canonical, &payload)?,
                OperationAction::Update => {
                    return Err(InstallerError::new(
                        "extension_source_not_updatable",
                        "local path-copy and link sources cannot be updated",
                    ));
                }
            }
            let parsed = read_manifest(&payload)?;
            let digest = content_digest(&payload).map_err(InstallerError::from_source)?;
            if self.store.installation(&parsed.config.name).exists()
                || legacy_user_installation_exists(&self.store.root, &parsed.config.name)?
            {
                return Err(InstallerError::new(
                    "extension_already_installed",
                    format!(
                        "a user installation already exists for {}",
                        parsed.config.name
                    ),
                ));
            }
            let source_kind = match action {
                OperationAction::Install => ManagedSourceKind::PathCopy,
                OperationAction::Link => ManagedSourceKind::Link,
                OperationAction::Update => {
                    return Err(InstallerError::new(
                        "extension_source_not_updatable",
                        "local path-copy and link sources cannot be updated",
                    ));
                }
            };
            let preflight = ExtensionPreflight {
                operation_id: operation_id.clone(),
                action,
                name: parsed.config.name.clone(),
                version: parsed.config.version.clone(),
                source_kind,
                source_identity: canonical.to_string_lossy().into_owned(),
                requested_ref: None,
                resolved_revision: None,
                previous_version: None,
                previous_revision: None,
                previous_content_digest: None,
                previous_capability_fingerprint: None,
                content_digest: digest.clone(),
                capability_fingerprint: parsed.capability_fingerprint.clone(),
                capabilities: parsed.capabilities.clone(),
                capabilities_added: parsed.capabilities.clone(),
                capabilities_removed: Vec::new(),
                capability_fingerprint_changed: true,
                consent_required: true,
                changed: true,
                diagnostics: parsed.diagnostics.clone(),
                risk_summary: capability_risk_summary(&parsed, &payload),
                expected_desired_state: "enabled".to_string(),
                expected_effective_state: "unchanged".to_string(),
                expected_activation: "next_session".to_string(),
                reused_consent_reference: None,
            };
            let now = Utc::now();
            write_metadata(
                &staging,
                &ManagedInstallationMetadata {
                    schema_version: INSTALLATION_SCHEMA_VERSION,
                    name: preflight.name.clone(),
                    version: preflight.version.clone(),
                    source_kind,
                    source_identity: preflight.source_identity.clone(),
                    requested_ref: None,
                    resolved_revision: None,
                    content_digest: digest,
                    capability_fingerprint: preflight.capability_fingerprint.clone(),
                    consent_reference: operation_id.clone(),
                    consent_policy_version: CONSENT_POLICY_VERSION,
                    installed_at: now,
                    updated_at: now,
                },
            )
            .map_err(InstallerError::from_source)?;
            let operation = PreparedOperation {
                schema_version: OPERATION_SCHEMA_VERSION,
                preflight: preflight.clone(),
                prepared_at: now,
            };
            write_json_atomic(&self.store.operation(&operation_id), &operation)?;
            Ok(preflight)
        })();
        if prepared.is_err() {
            let _ = fs::remove_dir_all(&staging);
            let _ = fs::remove_file(self.store.operation(&operation_id));
        }
        prepared
    }

    pub(super) fn prepare_git(
        &self,
        source: &str,
        requested_ref: Option<&str>,
        update_name: Option<&str>,
    ) -> Result<ExtensionPreflight, InstallerError> {
        let _lock = self
            .store
            .lock(STORE_LOCK_TIMEOUT)
            .map_err(InstallerError::from_source)?;
        self.store
            .ensure_internal_dirs()
            .map_err(InstallerError::from_source)?;

        let (action, source, requested_ref, previous_metadata, previous_capabilities) =
            if let Some(name) = update_name {
                let installation = self.store.installation(name);
                let metadata = read_installation_metadata(&installation)?;
                if metadata.source_kind != ManagedSourceKind::GitHttps {
                    return Err(InstallerError::new(
                        "extension_source_not_updatable",
                        format!(
                            "extension {name} uses {:?}; only git-https sources are updatable",
                            metadata.source_kind
                        ),
                    ));
                }
                let current = read_manifest(&installation.join(PAYLOAD_DIR))?;
                (
                    OperationAction::Update,
                    metadata.source_identity.clone(),
                    metadata.requested_ref.clone(),
                    Some(metadata),
                    current.capabilities,
                )
            } else {
                (
                    OperationAction::Install,
                    source.to_string(),
                    requested_ref.map(str::to_string),
                    None,
                    Vec::new(),
                )
            };

        let operation_id = Uuid::new_v4().to_string();
        let staging = self.store.staging(&operation_id);
        fs::create_dir(&staging).map_err(|error| {
            InstallerError::new(
                "extension_staging_failed",
                format!("failed to create {}: {error}", staging.display()),
            )
        })?;
        let payload = staging.join(PAYLOAD_DIR);
        let prepared = (|| {
            let materialized = self
                .git_materializer
                .materialize(&source, requested_ref.as_deref(), &payload)
                .map_err(InstallerError::from_git)?;
            let parsed = read_manifest(&payload)?;
            let digest = content_digest(&payload).map_err(InstallerError::from_source)?;

            if let Some(name) = update_name {
                if parsed.config.name != name {
                    return Err(InstallerError::new(
                        "extension_update_identity_changed",
                        format!(
                            "update resolved package {}, expected {name}",
                            parsed.config.name
                        ),
                    ));
                }
            } else if self.store.installation(&parsed.config.name).exists()
                || legacy_user_installation_exists(&self.store.root, &parsed.config.name)?
            {
                return Err(InstallerError::new(
                    "extension_already_installed",
                    format!(
                        "a user installation already exists for {}",
                        parsed.config.name
                    ),
                ));
            }

            let previous_set = previous_capabilities
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>();
            let next_set = parsed.capabilities.iter().cloned().collect::<BTreeSet<_>>();
            let capabilities_added = next_set.difference(&previous_set).cloned().collect();
            let capabilities_removed = previous_set.difference(&next_set).cloned().collect();
            let previous_fingerprint = previous_metadata
                .as_ref()
                .map(|metadata| metadata.capability_fingerprint.as_str());
            let consent_required = previous_fingerprint
                != Some(parsed.capability_fingerprint.as_str())
                || previous_metadata.as_ref().is_some_and(|metadata| {
                    metadata.consent_policy_version != CONSENT_POLICY_VERSION
                });
            let reused_consent_reference = previous_metadata
                .as_ref()
                .filter(|_| !consent_required)
                .map(|metadata| metadata.consent_reference.clone());
            let expected_desired_state = state::load(self.state_dir_override.as_deref())
                .map(|loaded| {
                    if loaded.state.disabled.contains(&parsed.config.name) {
                        "disabled"
                    } else {
                        "enabled"
                    }
                })
                .unwrap_or("unknown")
                .to_string();
            let changed = match previous_metadata.as_ref() {
                Some(metadata) => {
                    metadata.version != parsed.config.version
                        || metadata.resolved_revision.as_deref()
                            != Some(materialized.resolved_revision.as_str())
                        || metadata.content_digest != digest
                        || metadata.capability_fingerprint != parsed.capability_fingerprint
                }
                None => true,
            };
            let preflight = ExtensionPreflight {
                operation_id: operation_id.clone(),
                action,
                name: parsed.config.name.clone(),
                version: parsed.config.version.clone(),
                source_kind: ManagedSourceKind::GitHttps,
                source_identity: materialized.source_identity.clone(),
                requested_ref: materialized.requested_ref.clone(),
                resolved_revision: Some(materialized.resolved_revision.clone()),
                previous_version: previous_metadata
                    .as_ref()
                    .map(|metadata| metadata.version.clone()),
                previous_revision: previous_metadata
                    .as_ref()
                    .and_then(|metadata| metadata.resolved_revision.clone()),
                previous_content_digest: previous_metadata
                    .as_ref()
                    .map(|metadata| metadata.content_digest.clone()),
                previous_capability_fingerprint: previous_metadata
                    .as_ref()
                    .map(|metadata| metadata.capability_fingerprint.clone()),
                content_digest: digest.clone(),
                capability_fingerprint: parsed.capability_fingerprint.clone(),
                capabilities: parsed.capabilities.clone(),
                capabilities_added,
                capabilities_removed,
                capability_fingerprint_changed: consent_required,
                consent_required,
                changed,
                diagnostics: parsed.diagnostics.clone(),
                risk_summary: capability_risk_summary(&parsed, &payload),
                expected_desired_state,
                expected_effective_state: "unchanged".to_string(),
                expected_activation: if changed {
                    "next_session".to_string()
                } else {
                    "immediate".to_string()
                },
                reused_consent_reference,
            };
            let now = Utc::now();
            write_metadata(
                &staging,
                &ManagedInstallationMetadata {
                    schema_version: INSTALLATION_SCHEMA_VERSION,
                    name: preflight.name.clone(),
                    version: preflight.version.clone(),
                    source_kind: ManagedSourceKind::GitHttps,
                    source_identity: preflight.source_identity.clone(),
                    requested_ref: preflight.requested_ref.clone(),
                    resolved_revision: preflight.resolved_revision.clone(),
                    content_digest: digest,
                    capability_fingerprint: preflight.capability_fingerprint.clone(),
                    consent_reference: operation_id.clone(),
                    consent_policy_version: CONSENT_POLICY_VERSION,
                    installed_at: previous_metadata
                        .as_ref()
                        .map_or_else(|| now, |metadata| metadata.installed_at),
                    updated_at: now,
                },
            )
            .map_err(InstallerError::from_source)?;
            write_json_atomic(
                &self.store.operation(&operation_id),
                &PreparedOperation {
                    schema_version: OPERATION_SCHEMA_VERSION,
                    preflight: preflight.clone(),
                    prepared_at: now,
                },
            )?;
            Ok(preflight)
        })();
        if prepared.is_err() {
            let _ = fs::remove_dir_all(&staging);
            let _ = fs::remove_file(self.store.operation(&operation_id));
        }
        prepared
    }
}
