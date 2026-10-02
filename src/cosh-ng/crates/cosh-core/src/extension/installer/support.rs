//! Shared validation, receipt, metadata, and filesystem helpers.

use super::*;

/// Stable lifecycle error for registry and slash protocol consumers.
#[derive(Debug)]
pub struct InstallerError {
    code: &'static str,
    message: String,
}

impl InstallerError {
    pub(super) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(super) fn from_source(error: SourceError) -> Self {
        Self::new(error.code(), error.to_string())
    }

    pub(super) fn from_git(error: GitSourceError) -> Self {
        Self::new(error.code(), error.to_string())
    }

    /// Returns the stable diagnostic code.
    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for InstallerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for InstallerError {}

pub(super) fn mutation_result(
    operation_id: &str,
    expected: &ExtensionPreflight,
) -> ExtensionMutationResult {
    ExtensionMutationResult {
        operation_id: operation_id.to_string(),
        action: match expected.action {
            OperationAction::Install => "install",
            OperationAction::Link => "link",
            OperationAction::Update => "update",
        }
        .to_string(),
        name: expected.name.clone(),
        version: Some(expected.version.clone()),
        source_kind: expected.source_kind,
        source_identity: expected.source_identity.clone(),
        resolved_revision: expected.resolved_revision.clone(),
        content_digest: expected.content_digest.clone(),
        capability_fingerprint: expected.capability_fingerprint.clone(),
        changed: expected.changed,
        activation: if expected.changed {
            "next_session".to_string()
        } else {
            "immediate".to_string()
        },
        consent_reference: operation_id.to_string(),
        consent_accepted_at: expected.consent_required.then(Utc::now),
        consent_policy_version: CONSENT_POLICY_VERSION,
        reused_consent_reference: expected.reused_consent_reference.clone(),
        risk_summary: expected.risk_summary.clone(),
    }
}

pub(super) fn uninstall_mutation_result(
    operation_id: &str,
    name: &str,
    metadata: &ManagedInstallationMetadata,
    risk_summary: CapabilityRiskSummary,
) -> ExtensionMutationResult {
    ExtensionMutationResult {
        operation_id: operation_id.to_string(),
        action: "uninstall".to_string(),
        name: name.to_string(),
        version: Some(metadata.version.clone()),
        source_kind: metadata.source_kind,
        source_identity: metadata.source_identity.clone(),
        resolved_revision: metadata.resolved_revision.clone(),
        content_digest: metadata.content_digest.clone(),
        capability_fingerprint: metadata.capability_fingerprint.clone(),
        changed: true,
        activation: "next_session".to_string(),
        consent_reference: metadata.consent_reference.clone(),
        consent_accepted_at: None,
        consent_policy_version: metadata.consent_policy_version,
        reused_consent_reference: Some(metadata.consent_reference.clone()),
        risk_summary,
    }
}

pub(super) fn capability_risk_summary(
    parsed: &ParsedManifest,
    package_root: &Path,
) -> CapabilityRiskSummary {
    let mut summary = CapabilityRiskSummary::default();
    let hooks = &parsed.config.hooks;
    for groups in [
        &hooks.pre_tool_use,
        &hooks.post_tool_use,
        &hooks.post_tool_use_failure,
        &hooks.user_prompt_submit,
        &hooks.session_start,
        &hooks.stop,
        &hooks.before_model,
        &hooks.after_model,
    ] {
        for hook in super::super::config::flatten_hook_groups(groups) {
            summary.execution.push(format!(
                "hook command={} matcher={}",
                hook.command,
                hook.matcher.as_deref().unwrap_or("*")
            ));
        }
    }
    for server in &parsed.mcp_servers {
        summary.execution.push(format!(
            "{} command={} args={}",
            server.id,
            server.command,
            serde_json::to_string(&server.args).unwrap_or_else(|_| "[]".to_string())
        ));
        for (key, value) in &server.env {
            if let Some(setting) = value
                .strip_prefix("${setting:")
                .and_then(|value| value.strip_suffix('}'))
            {
                summary.credential.push(format!(
                    "{}/{} -> {} env {key}",
                    parsed.config.name, setting, server.id
                ));
            }
        }
    }
    for capability in &parsed.capabilities {
        if capability.contains("/skill/")
            || capability.contains("/context/")
            || capability.contains("/agent/")
        {
            summary.instruction.push(capability.clone());
        }
    }
    if let Ok(agents) = super::super::agent::load_agent_files(
        &parsed.config.name,
        package_root,
        &parsed.agent_directories,
    ) {
        for agent in agents {
            summary.authorization.push(format!(
                "{} tools={} skills={} mcp={}",
                agent.id,
                agent.tools.join(","),
                agent.skills.join(","),
                agent.mcp_servers.join(",")
            ));
        }
    }
    for setting in &parsed.settings {
        summary.credential.push(format!(
            "{}/{} required={} sensitive={}",
            parsed.config.name, setting.key, setting.required, setting.sensitive
        ));
    }
    for context in &parsed.contexts {
        let path = context
            .path
            .strip_prefix(package_root)
            .unwrap_or(&context.path)
            .to_string_lossy();
        summary.filesystem.push(format!(
            "{} path={} required={}",
            context.id, path, context.required
        ));
    }
    for values in [
        &mut summary.execution,
        &mut summary.instruction,
        &mut summary.authorization,
        &mut summary.credential,
        &mut summary.filesystem,
    ] {
        values.sort();
        values.dedup();
    }
    summary
}

pub(super) fn validate_prepared_metadata(
    prepared_root: &Path,
    operation_id: &str,
    expected: &ExtensionPreflight,
) -> Result<(), InstallerError> {
    let metadata = read_installation_metadata(prepared_root)?;
    if metadata.name != expected.name
        || metadata.version != expected.version
        || metadata.source_kind != expected.source_kind
        || metadata.source_identity != expected.source_identity
        || metadata.requested_ref != expected.requested_ref
        || metadata.resolved_revision != expected.resolved_revision
        || metadata.content_digest != expected.content_digest
        || metadata.capability_fingerprint != expected.capability_fingerprint
        || metadata.consent_reference != operation_id
    {
        return Err(InstallerError::new(
            "extension_preflight_stale",
            "prepared installation metadata changed after preflight",
        ));
    }
    Ok(())
}

pub(super) fn validate_current_update(
    current_root: &Path,
    expected: &ExtensionPreflight,
) -> Result<(), InstallerError> {
    let metadata = read_installation_metadata(current_root)?;
    if metadata.name != expected.name
        || metadata.source_kind != ManagedSourceKind::GitHttps
        || metadata.source_identity != expected.source_identity
        || Some(metadata.version.as_str()) != expected.previous_version.as_deref()
        || metadata.resolved_revision.as_deref() != expected.previous_revision.as_deref()
        || Some(metadata.content_digest.as_str()) != expected.previous_content_digest.as_deref()
        || Some(metadata.capability_fingerprint.as_str())
            != expected.previous_capability_fingerprint.as_deref()
    {
        return Err(InstallerError::new(
            "extension_update_precondition_changed",
            "installed extension changed after update preflight",
        ));
    }
    let payload = current_root.join(PAYLOAD_DIR);
    let parsed = read_manifest(&payload)?;
    let digest = content_digest(&payload).map_err(InstallerError::from_source)?;
    if parsed.config.name != metadata.name
        || parsed.config.version != metadata.version
        || parsed.capability_fingerprint != metadata.capability_fingerprint
        || digest != metadata.content_digest
    {
        return Err(InstallerError::new(
            "extension_update_precondition_changed",
            "installed payload no longer matches committed metadata",
        ));
    }
    Ok(())
}

pub(super) fn read_manifest(root: &Path) -> Result<ParsedManifest, InstallerError> {
    let path = root.join(EXTENSION_CONFIG_FILENAME);
    let content = fs::read_to_string(&path).map_err(|error| {
        InstallerError::new(
            "extension_manifest_unreadable",
            format!("failed to read {}: {error}", path.display()),
        )
    })?;
    parse_manifest(&content, root)
        .map_err(|error| InstallerError::new(error.code(), error.to_string()))
}

pub(super) fn legacy_user_installation_exists(
    root: &Path,
    name: &str,
) -> Result<bool, InstallerError> {
    let entries = fs::read_dir(root).map_err(|error| {
        InstallerError::new(
            "extension_store_unavailable",
            format!("failed to scan {}: {error}", root.display()),
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            InstallerError::new(
                "extension_store_unavailable",
                format!("failed to read user extension entry: {error}"),
            )
        })?;
        if entry.file_name().to_string_lossy().starts_with('.') || !entry.path().is_dir() {
            continue;
        }
        let manifest = entry.path().join(EXTENSION_CONFIG_FILENAME);
        let content = match fs::read_to_string(&manifest) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(InstallerError::new(
                    "extension_manifest_unreadable",
                    format!("failed to read {}: {error}", manifest.display()),
                ));
            }
        };
        let parsed = parse_manifest(&content, &entry.path())
            .map_err(|error| InstallerError::new(error.code(), error.to_string()))?;
        if parsed.config.name == name {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn read_installation_metadata(
    installation: &Path,
) -> Result<ManagedInstallationMetadata, InstallerError> {
    let path = installation.join(super::super::MANAGED_INSTALL_METADATA_FILENAME);
    let bytes = fs::read(&path).map_err(|error| {
        InstallerError::new(
            "extension_installation_not_managed",
            format!("failed to read {}: {error}", path.display()),
        )
    })?;
    let metadata: ManagedInstallationMetadata =
        serde_json::from_slice(&bytes).map_err(|error| {
            InstallerError::new(
                "extension_install_metadata_invalid",
                format!("failed to parse {}: {error}", path.display()),
            )
        })?;
    if metadata.schema_version != INSTALLATION_SCHEMA_VERSION {
        return Err(InstallerError::new(
            "extension_install_metadata_schema_unsupported",
            format!(
                "unsupported installation metadata schema {}",
                metadata.schema_version
            ),
        ));
    }
    Ok(metadata)
}

#[cfg(unix)]
pub(super) fn create_directory_link(
    source: &Path,
    destination: &Path,
) -> Result<(), InstallerError> {
    std::os::unix::fs::symlink(source, destination).map_err(|error| {
        InstallerError::new(
            "extension_staging_failed",
            format!(
                "failed to link {} to {}: {error}",
                destination.display(),
                source.display()
            ),
        )
    })
}

#[cfg(not(unix))]
pub(super) fn create_directory_link(
    _source: &Path,
    _destination: &Path,
) -> Result<(), InstallerError> {
    Err(InstallerError::new(
        "extension_link_unsupported",
        "extension link is only supported on Unix hosts",
    ))
}

pub(super) fn validate_operation_id(operation_id: &str) -> Result<(), InstallerError> {
    Uuid::parse_str(operation_id).map_err(|_| {
        InstallerError::new(
            "extension_operation_id_invalid",
            "operation id must be a UUID",
        )
    })?;
    Ok(())
}

pub(super) fn write_json_atomic<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), InstallerError> {
    let parent = path.parent().ok_or_else(|| {
        InstallerError::new(
            "extension_operation_path_invalid",
            format!("path has no parent: {}", path.display()),
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        InstallerError::new(
            "extension_operation_write_failed",
            format!("failed to create {}: {error}", parent.display()),
        )
    })?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        InstallerError::new(
            "extension_operation_write_failed",
            format!("failed to serialize operation: {error}"),
        )
    })?;
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes).map_err(|error| {
        InstallerError::new(
            "extension_operation_write_failed",
            format!("failed to write {}: {error}", temporary.display()),
        )
    })?;
    fs::rename(&temporary, path).map_err(|error| {
        InstallerError::new(
            "extension_operation_write_failed",
            format!("failed to replace {}: {error}", path.display()),
        )
    })
}

pub(super) fn remove_dir_if_exists(path: &Path) -> Result<(), InstallerError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(InstallerError::new(
            "extension_operation_cleanup_failed",
            format!("failed to remove {}: {error}", path.display()),
        )),
    }
}

pub(super) fn remove_file_if_exists(path: &Path) -> Result<(), InstallerError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(InstallerError::new(
            "extension_operation_cleanup_failed",
            format!("failed to remove {}: {error}", path.display()),
        )),
    }
}
