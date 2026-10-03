//! Extension catalog discovery and selected runtime contribution projection.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use super::config::{flatten_hook_groups, ExtensionHooks};
use super::manifest::parse_manifest;
use super::source::{
    content_digest, ManagedInstallationMetadata, ManagedSourceKind, MANAGED_DIR, PAYLOAD_DIR,
};
use super::state::{self, SourceSelection, SourceSelectionRecord, StateOrigin};
use super::variables::{hydrate_config, VariableContext};
use super::{
    Activation, DesiredState, EffectiveState, Extension, ExtensionDiagnostic, ExtensionHealth,
    ExtensionSourceKind, InstallMetadata, EXTENSION_CONFIG_FILENAME, INSTALL_METADATA_FILENAME,
    MANAGED_INSTALL_METADATA_FILENAME,
};

/// Discovers extension installations and builds the selected catalog snapshot.
pub struct ExtensionManager {
    extensions: Vec<Extension>,
    catalog_diagnostics: Vec<ExtensionDiagnostic>,
    workspace_dir: PathBuf,
    user_dir_override: Option<PathBuf>,
    system_dirs_override: Option<Vec<PathBuf>>,
    state_dir_override: Option<PathBuf>,
}

impl ExtensionManager {
    /// Creates a manager for the current user and system extension roots.
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self {
            extensions: Vec::new(),
            catalog_diagnostics: Vec::new(),
            workspace_dir,
            user_dir_override: None,
            system_dirs_override: None,
            state_dir_override: None,
        }
    }

    /// Creates an isolated manager that cannot read or write the real user state.
    #[cfg(test)]
    pub fn new_isolated(
        workspace_dir: PathBuf,
        user_dir: Option<PathBuf>,
        system_dir: Option<PathBuf>,
    ) -> Self {
        let state_dir_override = user_dir
            .as_ref()
            .and_then(|directory| directory.parent())
            .map(|parent| parent.join("states"))
            .or_else(|| Some(workspace_dir.join(".test-extension-states")));
        Self {
            extensions: Vec::new(),
            catalog_diagnostics: Vec::new(),
            workspace_dir,
            user_dir_override: user_dir,
            system_dirs_override: system_dir.map(|directory| vec![directory]),
            state_dir_override,
        }
    }

    /// Creates an isolated manager with an explicit extension state directory.
    #[cfg(test)]
    pub fn new_isolated_with_state(
        workspace_dir: PathBuf,
        user_dir: Option<PathBuf>,
        system_dir: Option<PathBuf>,
        state_dir: PathBuf,
    ) -> Self {
        Self {
            extensions: Vec::new(),
            catalog_diagnostics: Vec::new(),
            workspace_dir,
            user_dir_override: user_dir,
            system_dirs_override: system_dir.map(|directory| vec![directory]),
            state_dir_override: Some(state_dir),
        }
    }

    /// Creates an isolated manager with explicit system roots and state directory.
    #[cfg(test)]
    fn new_isolated_with_system_dirs(
        workspace_dir: PathBuf,
        user_dir: Option<PathBuf>,
        system_dirs: Vec<PathBuf>,
        state_dir: PathBuf,
    ) -> Self {
        Self {
            extensions: Vec::new(),
            catalog_diagnostics: Vec::new(),
            workspace_dir,
            user_dir_override: user_dir,
            system_dirs_override: Some(system_dirs),
            state_dir_override: Some(state_dir),
        }
    }

    /// Rebuilds the selected catalog and runtime contribution snapshot.
    pub fn refresh(&mut self) {
        self.catalog_diagnostics.clear();
        let mut candidates: BTreeMap<String, Vec<Extension>> = BTreeMap::new();
        let system_dirs = self
            .system_dirs_override
            .clone()
            .unwrap_or_else(super::system_extensions_dirs);
        for system_dir in canonical_unique_roots(system_dirs) {
            self.scan_directory(&system_dir, ExtensionSourceKind::System, &mut candidates);
        }
        if let Some(user_dir) = self
            .user_dir_override
            .clone()
            .or_else(super::user_extensions_dir)
        {
            self.scan_managed_directory(&user_dir.join(MANAGED_DIR), &mut candidates);
            self.scan_directory(&user_dir, ExtensionSourceKind::Legacy, &mut candidates);
        }

        let loaded_state = match state::load(self.state_dir_override.as_deref()) {
            Ok(loaded) => loaded,
            Err(error) => {
                self.catalog_diagnostics
                    .push(ExtensionDiagnostic::new(error.code(), error.to_string()));
                self.extensions = candidates
                    .into_values()
                    .filter_map(|group| group.into_iter().next())
                    .map(|mut extension| {
                        extension.desired_state = DesiredState::Disabled;
                        extension.effective_state = EffectiveState::Disabled;
                        extension.is_active = false;
                        extension.health = ExtensionHealth::Broken;
                        extension.diagnostics.push(ExtensionDiagnostic::new(
                            error.code(),
                            "extension activation failed closed because state is unreadable",
                        ));
                        extension
                    })
                    .collect();
                return;
            }
        };
        let mut persisted_state = loaded_state.state;
        let compatibility_selection = loaded_state.origin != StateOrigin::Versioned;
        if compatibility_selection {
            for (name, group) in &candidates {
                if has_source(group, ExtensionSourceKind::Legacy)
                    && has_source(group, ExtensionSourceKind::System)
                {
                    let source_identity = group
                        .iter()
                        .find(|extension| extension.source == ExtensionSourceKind::Legacy)
                        .map(|extension| extension.source_identity.clone())
                        .unwrap_or_default();
                    persisted_state.source_selections.insert(
                        name.clone(),
                        SourceSelectionRecord {
                            source: SourceSelection::User,
                            source_identity,
                        },
                    );
                    self.catalog_diagnostics.push(ExtensionDiagnostic::new(
                        "legacy_source_selection_migrated",
                        format!(
                            "preserved the legacy user-over-system selection for extension {name}"
                        ),
                    ));
                }
            }
            if let Err(error) = state::save(&persisted_state, self.state_dir_override.as_deref()) {
                self.catalog_diagnostics
                    .push(ExtensionDiagnostic::new(error.code(), error.to_string()));
                self.extensions = fail_closed_candidates(candidates, error.code());
                return;
            }
        }

        let mut resolved = Vec::new();
        for (name, mut group) in candidates {
            group.sort_by_key(|extension| {
                (
                    match extension.source {
                        ExtensionSourceKind::PathCopy
                        | ExtensionSourceKind::Link
                        | ExtensionSourceKind::GitHttps
                        | ExtensionSourceKind::Legacy => 0,
                        ExtensionSourceKind::System => 1,
                        ExtensionSourceKind::Conflict => 2,
                    },
                    extension.source_identity.clone(),
                )
            });
            let mut available_sources = Vec::new();
            let mut available_source_identities = BTreeMap::new();
            let system_source_identities = group
                .iter()
                .filter(|extension| extension.source == ExtensionSourceKind::System)
                .map(|extension| extension.source_identity.clone())
                .collect::<Vec<_>>();
            let system_source_conflict = system_source_identities.len() > 1;
            for extension in &group {
                if !available_sources.contains(&extension.source) {
                    available_sources.push(extension.source);
                }
                if system_source_conflict && extension.source == ExtensionSourceKind::System {
                    continue;
                }
                let key = match extension.source {
                    ExtensionSourceKind::PathCopy
                    | ExtensionSourceKind::Link
                    | ExtensionSourceKind::GitHttps
                    | ExtensionSourceKind::Legacy => "user",
                    ExtensionSourceKind::System => "system",
                    ExtensionSourceKind::Conflict => "conflict",
                };
                available_source_identities
                    .insert(key.to_string(), extension.source_identity.clone());
            }
            let selection = persisted_state.source_selections.get(&name);
            let exact_selection = selection.and_then(|selection| {
                group.iter().position(|extension| {
                    let source_matches = match selection.source {
                        SourceSelection::User => is_user_source(extension.source),
                        SourceSelection::System => extension.source == ExtensionSourceKind::System,
                    };
                    source_matches && extension.source_identity == selection.source_identity
                })
            });
            if let (Some(index), Some(selection)) = (exact_selection, selection) {
                let source_key = match selection.source {
                    SourceSelection::User => "user",
                    SourceSelection::System => "system",
                };
                available_source_identities
                    .insert(source_key.to_string(), group[index].source_identity.clone());
            }
            let selected_index = match exact_selection {
                Some(index) => Some(index),
                None if system_source_conflict => None,
                None if selection.is_none() && group.len() == 1 => Some(0),
                None => None,
            };

            let mut extension = match selected_index {
                Some(index) => group.remove(index),
                None if system_source_conflict => {
                    let mut conflict = group.remove(0);
                    conflict.source = ExtensionSourceKind::Conflict;
                    conflict.source_identity = format!("conflict:{name}");
                    conflict.health = ExtensionHealth::Conflict;
                    conflict.diagnostics.push(ExtensionDiagnostic::new(
                        "extension_system_source_conflict",
                        format!(
                            "extension {name} has multiple system installations: {}; uninstall all but one system installation before enabling it",
                            system_source_identities.join(", ")
                        ),
                    ));
                    conflict
                }
                None => {
                    let mut conflict = group.remove(0);
                    conflict.source = ExtensionSourceKind::Conflict;
                    conflict.source_identity = format!("conflict:{name}");
                    conflict.health = ExtensionHealth::Conflict;
                    conflict.diagnostics.push(ExtensionDiagnostic::new(
                        "extension_source_conflict",
                        format!(
                            "extension {name} has multiple sources; select user or system explicitly"
                        ),
                    ));
                    conflict
                }
            };
            if selection.is_some() && extension.source == ExtensionSourceKind::Conflict {
                extension.diagnostics.push(ExtensionDiagnostic::new(
                    "extension_source_selection_stale",
                    format!("saved source selection for {name} is no longer available"),
                ));
            }
            extension.available_sources = available_sources;
            extension.available_source_identities = available_source_identities;
            extension.desired_state = if persisted_state.disabled.contains(&name) {
                DesiredState::Disabled
            } else {
                DesiredState::Enabled
            };
            let can_activate = extension.desired_state == DesiredState::Enabled
                && matches!(
                    extension.health,
                    ExtensionHealth::Healthy | ExtensionHealth::Degraded
                );
            extension.is_active = can_activate;
            extension.effective_state = if can_activate {
                EffectiveState::Enabled
            } else {
                EffectiveState::Disabled
            };
            extension.activation = Activation::Immediate;
            resolved.push(extension);
        }
        self.extensions = resolved;
    }

    /// Returns active extension skill directories.
    pub fn skill_dirs(&self) -> Vec<PathBuf> {
        self.extensions
            .iter()
            .filter(|extension| extension.is_active)
            .flat_map(|extension| {
                extension.config.skills.0.iter().map(|skill_dir| {
                    if Path::new(skill_dir).is_absolute() {
                        PathBuf::from(skill_dir)
                    } else {
                        extension.path.join(skill_dir)
                    }
                })
            })
            .collect()
    }

    /// Collects active extension hook definitions.
    pub fn hook_definitions(&self) -> ExtensionHooks {
        let mut merged = ExtensionHooks::default();
        for extension in self
            .extensions
            .iter()
            .filter(|extension| extension.is_active)
        {
            merged.merge(&extension.config.hooks);
        }
        merged
    }

    /// Fingerprints the exact package content selected for this runtime snapshot.
    pub fn runtime_fingerprint(&self) -> Result<String, ExtensionDiagnostic> {
        let mut projection = Vec::new();
        for extension in self
            .extensions
            .iter()
            .filter(|extension| extension.is_active)
        {
            let digest = content_digest(&extension.path).map_err(|error| {
                ExtensionDiagnostic::new(
                    error.code(),
                    format!(
                        "failed to fingerprint runtime package {}: {error}",
                        extension.name
                    ),
                )
            })?;
            projection.push(serde_json::json!({
                "capability_fingerprint": extension.capability_fingerprint,
                "content_digest": digest,
                "name": extension.name,
                "source_identity": extension.source_identity,
            }));
        }
        super::identity::fingerprint_projection(serde_json::json!(projection)).map_err(|error| {
            ExtensionDiagnostic::new(
                "extension_generation_fingerprint_failed",
                format!("failed to fingerprint extension runtime generation: {error}"),
            )
        })
    }

    /// Returns the selected catalog entries sorted by package identity.
    pub fn list(&self) -> &[Extension] {
        &self.extensions
    }

    /// Returns mutable entries to the runtime snapshot validator.
    pub(crate) fn list_mut(&mut self) -> &mut [Extension] {
        &mut self.extensions
    }

    /// Returns the canonical workspace root used for runtime contributions.
    pub fn workspace_dir(&self) -> &Path {
        &self.workspace_dir
    }

    /// Returns catalog-wide diagnostics, including invalid packages and state migration.
    pub fn catalog_diagnostics(&self) -> &[ExtensionDiagnostic] {
        &self.catalog_diagnostics
    }

    /// Persists desired enabled state and rebuilds the catalog projection.
    pub fn set_enabled(&mut self, name: &str, enabled: bool) -> Result<Extension, String> {
        if !self
            .extensions
            .iter()
            .any(|extension| extension.name == name)
        {
            return Err(format!("extension not found: {name}"));
        }
        state::set_enabled(name, enabled, self.state_dir_override.as_deref())
            .map_err(|error| error.to_string())?;
        self.refresh();
        self.extensions
            .iter()
            .find(|extension| extension.name == name)
            .cloned()
            .ok_or_else(|| format!("extension not found after state update: {name}"))
    }

    /// Persists an explicit source selection and rebuilds the catalog projection.
    pub fn select_source(
        &mut self,
        name: &str,
        selection: SourceSelection,
    ) -> Result<Extension, String> {
        if !self
            .extensions
            .iter()
            .any(|extension| extension.name == name)
        {
            return Err(format!("extension not found: {name}"));
        }
        let source_key = match selection {
            SourceSelection::User => "user",
            SourceSelection::System => "system",
        };
        let source_identity = self
            .extensions
            .iter()
            .find(|extension| extension.name == name)
            .and_then(|extension| extension.available_source_identities.get(source_key))
            .cloned()
            .ok_or_else(|| format!("extension source not found: {name}/{source_key}"))?;
        state::select_source(
            name,
            selection,
            &source_identity,
            self.state_dir_override.as_deref(),
        )
        .map_err(|error| error.to_string())?;
        self.refresh();
        self.extensions
            .iter()
            .find(|extension| extension.name == name)
            .cloned()
            .ok_or_else(|| format!("extension not found after source selection: {name}"))
    }

    /// Returns hook local names for compatibility cleanup in hook state.
    pub fn extension_hook_names(&self, extension_name: &str) -> HashSet<String> {
        let Some(extension) = self
            .extensions
            .iter()
            .find(|extension| extension.name == extension_name)
        else {
            return HashSet::new();
        };
        let mut names = HashSet::new();
        let groups = [
            &extension.config.hooks.pre_tool_use,
            &extension.config.hooks.post_tool_use,
            &extension.config.hooks.post_tool_use_failure,
            &extension.config.hooks.user_prompt_submit,
            &extension.config.hooks.session_start,
            &extension.config.hooks.stop,
            &extension.config.hooks.before_model,
            &extension.config.hooks.after_model,
        ];
        for group in groups {
            for definition in flatten_hook_groups(group) {
                if let Some(name) = definition.name {
                    names.insert(name);
                }
            }
        }
        names
    }

    fn scan_directory(
        &mut self,
        directory: &Path,
        source: ExtensionSourceKind,
        candidates: &mut BTreeMap<String, Vec<Extension>>,
    ) {
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                self.catalog_diagnostics.push(ExtensionDiagnostic::new(
                    "extension_source_unreadable",
                    format!("failed to scan {}: {error}", directory.display()),
                ));
                return;
            }
        };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let entry_path = entry.path();
            if !entry_path.is_dir() {
                continue;
            }
            let resolved_path = match entry_path.canonicalize() {
                Ok(path) => path,
                Err(error) => {
                    self.catalog_diagnostics.push(ExtensionDiagnostic::new(
                        "extension_path_unreadable",
                        format!("failed to resolve {}: {error}", entry_path.display()),
                    ));
                    continue;
                }
            };
            if !resolved_path.join(EXTENSION_CONFIG_FILENAME).exists() {
                continue;
            }
            match self.load_extension(&resolved_path, source) {
                Ok(extension) => insert_candidate(candidates, extension),
                Err(diagnostic) => self.catalog_diagnostics.push(diagnostic),
            }
        }
    }

    fn scan_managed_directory(
        &mut self,
        directory: &Path,
        candidates: &mut BTreeMap<String, Vec<Extension>>,
    ) {
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                self.catalog_diagnostics.push(ExtensionDiagnostic::new(
                    "extension_managed_store_unreadable",
                    format!("failed to scan {}: {error}", directory.display()),
                ));
                return;
            }
        };
        for entry in entries.flatten() {
            let installation = entry.path();
            if !installation.is_dir() {
                continue;
            }
            match self.load_managed_extension(&installation) {
                Ok(extension) => insert_candidate(candidates, extension),
                Err(diagnostic) => {
                    if let Some(extension) =
                        self.broken_link_extension(&installation, diagnostic.clone())
                    {
                        insert_candidate(candidates, extension);
                    }
                    self.catalog_diagnostics.push(diagnostic);
                }
            }
        }
    }

    fn broken_link_extension(
        &self,
        installation: &Path,
        diagnostic: ExtensionDiagnostic,
    ) -> Option<Extension> {
        let metadata = std::fs::read(installation.join(MANAGED_INSTALL_METADATA_FILENAME))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ManagedInstallationMetadata>(&bytes).ok())?;
        if metadata.source_kind != ManagedSourceKind::Link {
            return None;
        }
        Some(Extension {
            name: metadata.name.clone(),
            version: metadata.version.clone(),
            path: installation.join(PAYLOAD_DIR),
            is_active: false,
            config: super::ExtensionConfig {
                name: metadata.name.clone(),
                version: metadata.version.clone(),
                skills: super::config::SkillsDirs(Vec::new()),
                hooks: ExtensionHooks::default(),
            },
            install_metadata: None,
            managed_install_metadata: Some(metadata.clone()),
            schema_version: super::ManifestSchemaVersion::V1,
            source: ExtensionSourceKind::Link,
            source_identity: metadata.source_identity,
            available_sources: Vec::new(),
            available_source_identities: BTreeMap::new(),
            desired_state: DesiredState::Enabled,
            effective_state: EffectiveState::Disabled,
            activation: Activation::NextSession,
            health: ExtensionHealth::Broken,
            capability_fingerprint: metadata.capability_fingerprint,
            capabilities: Vec::new(),
            diagnostics: vec![diagnostic],
            settings: Vec::new(),
            contexts: Vec::new(),
            mcp_servers: Vec::new(),
            agent_directories: Vec::new(),
        })
    }

    fn load_managed_extension(
        &self,
        installation: &Path,
    ) -> Result<Extension, ExtensionDiagnostic> {
        let metadata_path = installation.join(MANAGED_INSTALL_METADATA_FILENAME);
        let metadata_content = std::fs::read_to_string(&metadata_path).map_err(|error| {
            ExtensionDiagnostic::new(
                "extension_install_metadata_unreadable",
                format!("failed to read {}: {error}", metadata_path.display()),
            )
        })?;
        let metadata: ManagedInstallationMetadata = serde_json::from_str(&metadata_content)
            .map_err(|error| {
                ExtensionDiagnostic::new(
                    "extension_install_metadata_invalid",
                    format!("failed to parse {}: {error}", metadata_path.display()),
                )
            })?;
        if metadata.schema_version != 1 {
            return Err(ExtensionDiagnostic::new(
                "extension_install_metadata_schema_unsupported",
                format!(
                    "unsupported installation metadata schema {} in {}",
                    metadata.schema_version,
                    metadata_path.display()
                ),
            ));
        }
        if installation.file_name().and_then(|name| name.to_str()) != Some(metadata.name.as_str()) {
            return Err(ExtensionDiagnostic::new(
                "extension_install_directory_mismatch",
                format!(
                    "managed directory {} does not match package identity {}",
                    installation.display(),
                    metadata.name
                ),
            ));
        }
        let payload = installation.join(PAYLOAD_DIR);
        let payload_type = std::fs::symlink_metadata(&payload).map_err(|error| {
            ExtensionDiagnostic::new(
                "extension_managed_payload_unreadable",
                format!("failed to inspect {}: {error}", payload.display()),
            )
        })?;
        let link_expected = metadata.source_kind == ManagedSourceKind::Link;
        if payload_type.file_type().is_symlink() != link_expected {
            return Err(ExtensionDiagnostic::new(
                "extension_managed_payload_type_mismatch",
                format!(
                    "managed payload type does not match {:?} metadata: {}",
                    metadata.source_kind,
                    payload.display()
                ),
            ));
        }
        let resolved_payload = payload.canonicalize().map_err(|error| {
            ExtensionDiagnostic::new(
                "extension_managed_payload_unreadable",
                format!("failed to resolve {}: {error}", payload.display()),
            )
        })?;
        let source = match metadata.source_kind {
            ManagedSourceKind::PathCopy => ExtensionSourceKind::PathCopy,
            ManagedSourceKind::Link => ExtensionSourceKind::Link,
            ManagedSourceKind::GitHttps => ExtensionSourceKind::GitHttps,
        };
        let mut extension = self.load_extension(&resolved_payload, source)?;
        let digest = content_digest(&resolved_payload).map_err(|error| {
            ExtensionDiagnostic::new(error.code(), format!("{}: {error}", payload.display()))
        })?;
        if metadata.source_kind == ManagedSourceKind::Link {
            if extension.name != metadata.name {
                extension.health = ExtensionHealth::Broken;
                extension.diagnostics.push(ExtensionDiagnostic::new(
                    "extension_link_identity_changed",
                    "linked package identity no longer matches its installation record",
                ));
            } else if extension.capability_fingerprint != metadata.capability_fingerprint {
                extension.health = ExtensionHealth::Broken;
                extension.diagnostics.push(ExtensionDiagnostic::new(
                    "extension_link_consent_stale",
                    "linked package capability fingerprint changed; reinstall the link to review consent",
                ));
            } else if extension.version != metadata.version || digest != metadata.content_digest {
                extension.health = ExtensionHealth::Degraded;
                extension.diagnostics.push(ExtensionDiagnostic::new(
                    "extension_link_stale",
                    "linked package content changed and requires a safe-point reload",
                ));
            }
        } else if extension.name != metadata.name
            || extension.version != metadata.version
            || extension.capability_fingerprint != metadata.capability_fingerprint
            || digest != metadata.content_digest
        {
            extension.health = ExtensionHealth::Broken;
            extension.diagnostics.push(ExtensionDiagnostic::new(
                "extension_managed_payload_changed",
                "managed payload no longer matches its committed installation metadata",
            ));
        }
        extension.source_identity = metadata.source_identity.clone();
        extension.managed_install_metadata = Some(metadata);
        Ok(extension)
    }

    fn load_extension(
        &self,
        extension_root: &Path,
        source: ExtensionSourceKind,
    ) -> Result<Extension, ExtensionDiagnostic> {
        let manifest_path = extension_root.join(EXTENSION_CONFIG_FILENAME);
        let content = std::fs::read_to_string(&manifest_path).map_err(|error| {
            ExtensionDiagnostic::new(
                "extension_manifest_unreadable",
                format!("failed to read {}: {error}", manifest_path.display()),
            )
        })?;
        let parsed = parse_manifest(&content, extension_root).map_err(|error| {
            ExtensionDiagnostic::new(
                error.code(),
                format!("{}: {error}", manifest_path.display()),
            )
        })?;
        let mut config = parsed.config;
        let context = VariableContext {
            extension_path: extension_root,
            workspace_path: &self.workspace_dir,
        };
        hydrate_config(&mut config, &context);

        let metadata_path = extension_root.join(INSTALL_METADATA_FILENAME);
        let install_metadata = if metadata_path.exists() {
            let metadata = std::fs::read_to_string(&metadata_path).map_err(|error| {
                ExtensionDiagnostic::new(
                    "extension_install_metadata_unreadable",
                    format!("failed to read {}: {error}", metadata_path.display()),
                )
            })?;
            Some(
                serde_json::from_str::<InstallMetadata>(&metadata).map_err(|error| {
                    ExtensionDiagnostic::new(
                        "extension_install_metadata_invalid",
                        format!("failed to parse {}: {error}", metadata_path.display()),
                    )
                })?,
            )
        } else {
            None
        };
        let health = if parsed.diagnostics.is_empty() {
            ExtensionHealth::Healthy
        } else {
            ExtensionHealth::Degraded
        };
        Ok(Extension {
            name: config.name.clone(),
            version: config.version.clone(),
            path: extension_root.to_path_buf(),
            is_active: false,
            config,
            install_metadata,
            managed_install_metadata: None,
            schema_version: parsed.schema_version,
            source,
            source_identity: extension_root.to_string_lossy().into_owned(),
            available_sources: vec![source],
            available_source_identities: BTreeMap::new(),
            desired_state: DesiredState::Enabled,
            effective_state: EffectiveState::Disabled,
            activation: Activation::Immediate,
            health,
            capability_fingerprint: parsed.capability_fingerprint,
            capabilities: parsed.capabilities,
            diagnostics: parsed.diagnostics,
            settings: parsed.settings,
            contexts: parsed.contexts,
            mcp_servers: parsed.mcp_servers,
            agent_directories: parsed.agent_directories,
        })
    }
}

fn canonical_unique_roots(directories: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    directories
        .into_iter()
        .filter(|directory| {
            let identity = directory
                .canonicalize()
                .unwrap_or_else(|_| directory.clone());
            seen.insert(identity)
        })
        .collect()
}

fn insert_candidate(candidates: &mut BTreeMap<String, Vec<Extension>>, extension: Extension) {
    let group = candidates.entry(extension.name.clone()).or_default();
    if group.iter().any(|candidate| {
        candidate.source == extension.source
            && candidate.source_identity == extension.source_identity
    }) {
        return;
    }
    group.push(extension);
}

fn has_source(group: &[Extension], source: ExtensionSourceKind) -> bool {
    group.iter().any(|extension| extension.source == source)
}

fn is_user_source(source: ExtensionSourceKind) -> bool {
    matches!(
        source,
        ExtensionSourceKind::PathCopy
            | ExtensionSourceKind::Link
            | ExtensionSourceKind::GitHttps
            | ExtensionSourceKind::Legacy
    )
}

fn fail_closed_candidates(
    candidates: BTreeMap<String, Vec<Extension>>,
    diagnostic_code: &str,
) -> Vec<Extension> {
    candidates
        .into_values()
        .filter_map(|group| group.into_iter().next())
        .map(|mut extension| {
            extension.desired_state = DesiredState::Disabled;
            extension.effective_state = EffectiveState::Disabled;
            extension.is_active = false;
            extension.health = ExtensionHealth::Broken;
            extension.diagnostics.push(ExtensionDiagnostic::new(
                diagnostic_code,
                "extension activation failed closed because state migration could not persist",
            ));
            extension
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::installer::ExtensionInstaller;
    use std::fs;

    fn create_extension(directory: &Path, entry: &str, manifest: &str) -> PathBuf {
        let root = directory.join(entry);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(EXTENSION_CONFIG_FILENAME), manifest).unwrap();
        root
    }

    fn roots() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let temporary = tempfile::tempdir().unwrap();
        let user = temporary.path().join("user");
        let system = temporary.path().join("system");
        let states = temporary.path().join("states");
        fs::create_dir_all(&user).unwrap();
        fs::create_dir_all(&system).unwrap();
        (temporary, user, system, states)
    }

    #[test]
    fn loads_legacy_extension_and_preserves_runtime_projection() {
        let (_temporary, user, system, states) = roots();
        create_extension(
            &user,
            "my-ext",
            r#"{"name":"my-ext","version":"1.0.0","skills":[]}"#,
        );
        let mut manager = ExtensionManager::new_isolated_with_state(
            PathBuf::from("/workspace"),
            Some(user),
            Some(system),
            states,
        );
        manager.refresh();
        assert_eq!(manager.list().len(), 1);
        assert_eq!(manager.list()[0].name, "my-ext");
        assert!(manager.list()[0].is_active);
        assert_eq!(manager.list()[0].health, ExtensionHealth::Degraded);
    }

    #[test]
    fn discovers_distinct_extensions_from_all_system_roots() {
        let temporary = tempfile::tempdir().unwrap();
        let first_system = temporary.path().join("system-a");
        let second_system = temporary.path().join("system-b");
        let user = temporary.path().join("user");
        let states = temporary.path().join("states");
        fs::create_dir_all(&first_system).unwrap();
        fs::create_dir_all(&second_system).unwrap();
        fs::create_dir_all(&user).unwrap();
        state::save(&state::ExtensionState::default(), Some(&states)).unwrap();
        create_extension(
            &first_system,
            "zeta",
            r#"{"name":"zeta","version":"1.0.0","skills":[]}"#,
        );
        create_extension(
            &second_system,
            "alpha",
            r#"{
                "schemaVersion": 1,
                "name": "alpha",
                "version": "2.0.0",
                "compatibility": {"cosh": ">=0.12.0"},
                "hooks": {
                    "BeforeModel": [{
                        "hooks": [{
                            "type": "command",
                            "name": "raw-guard",
                            "command": "/usr/bin/true"
                        }]
                    }]
                }
            }"#,
        );

        let mut manager = ExtensionManager::new_isolated_with_system_dirs(
            PathBuf::from("/workspace"),
            Some(user),
            vec![first_system, second_system],
            states,
        );
        manager.refresh();

        assert_eq!(
            manager
                .list()
                .iter()
                .map(|extension| extension.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "zeta"]
        );
        assert!(manager.list().iter().all(|extension| {
            extension.source == ExtensionSourceKind::System && extension.is_active
        }));
        assert!(
            flatten_hook_groups(&manager.hook_definitions().before_model)
                .iter()
                .any(|hook| hook.name.as_deref() == Some("raw-guard"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn canonical_system_root_alias_is_scanned_once() {
        let temporary = tempfile::tempdir().unwrap();
        let system = temporary.path().join("system");
        let system_alias = temporary.path().join("system-alias");
        let user = temporary.path().join("user");
        let states = temporary.path().join("states");
        fs::create_dir_all(&system).unwrap();
        fs::create_dir_all(&user).unwrap();
        std::os::unix::fs::symlink(&system, &system_alias).unwrap();
        state::save(&state::ExtensionState::default(), Some(&states)).unwrap();
        create_extension(
            &system,
            "shared",
            r#"{"name":"shared","version":"1.0.0","skills":[]}"#,
        );

        let mut manager = ExtensionManager::new_isolated_with_system_dirs(
            PathBuf::from("/workspace"),
            Some(user),
            vec![system_alias, system],
            states,
        );
        manager.refresh();

        assert_eq!(manager.list().len(), 1);
        assert_eq!(manager.list()[0].source, ExtensionSourceKind::System);
        assert!(manager.list()[0].is_active);
    }

    #[cfg(unix)]
    #[test]
    fn canonical_system_installation_alias_is_discovered_once() {
        let temporary = tempfile::tempdir().unwrap();
        let first_system = temporary.path().join("system-a");
        let second_system = temporary.path().join("system-b");
        let payloads = temporary.path().join("payloads");
        let user = temporary.path().join("user");
        let states = temporary.path().join("states");
        fs::create_dir_all(&first_system).unwrap();
        fs::create_dir_all(&second_system).unwrap();
        fs::create_dir_all(&payloads).unwrap();
        fs::create_dir_all(&user).unwrap();
        state::save(&state::ExtensionState::default(), Some(&states)).unwrap();
        let shared = create_extension(
            &payloads,
            "shared",
            r#"{"name":"shared","version":"1.0.0","skills":[]}"#,
        );
        std::os::unix::fs::symlink(&shared, first_system.join("shared")).unwrap();
        std::os::unix::fs::symlink(&shared, second_system.join("shared")).unwrap();

        let mut manager = ExtensionManager::new_isolated_with_system_dirs(
            PathBuf::from("/workspace"),
            Some(user),
            vec![first_system, second_system],
            states,
        );
        manager.refresh();

        assert_eq!(manager.list().len(), 1);
        assert_eq!(manager.list()[0].source, ExtensionSourceKind::System);
        assert!(manager.list()[0].is_active);
    }

    #[test]
    fn duplicate_system_identity_fails_closed_without_exact_selection() {
        let temporary = tempfile::tempdir().unwrap();
        let first_system = temporary.path().join("system-a");
        let second_system = temporary.path().join("system-b");
        let user = temporary.path().join("user");
        let states = temporary.path().join("states");
        fs::create_dir_all(&first_system).unwrap();
        fs::create_dir_all(&second_system).unwrap();
        fs::create_dir_all(&user).unwrap();
        state::save(&state::ExtensionState::default(), Some(&states)).unwrap();
        let first_installation = create_extension(
            &first_system,
            "shared",
            r#"{"name":"shared","version":"1.0.0","skills":[]}"#,
        )
        .canonicalize()
        .unwrap();
        let second_installation = create_extension(
            &second_system,
            "shared",
            r#"{"name":"shared","version":"2.0.0","skills":[]}"#,
        )
        .canonicalize()
        .unwrap();

        let mut manager = ExtensionManager::new_isolated_with_system_dirs(
            PathBuf::from("/workspace"),
            Some(user),
            vec![first_system, second_system],
            states,
        );
        manager.refresh();

        let extension = &manager.list()[0];
        assert_eq!(extension.source, ExtensionSourceKind::Conflict);
        assert_eq!(extension.health, ExtensionHealth::Conflict);
        assert_eq!(extension.effective_state, EffectiveState::Disabled);
        assert!(!extension.is_active);
        assert_eq!(
            extension.available_sources,
            vec![ExtensionSourceKind::System]
        );
        assert!(!extension.available_source_identities.contains_key("system"));
        let diagnostic = extension
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "extension_system_source_conflict")
            .unwrap();
        assert!(diagnostic
            .message
            .contains(&first_installation.to_string_lossy().into_owned()));
        assert!(diagnostic
            .message
            .contains(&second_installation.to_string_lossy().into_owned()));
        assert!(diagnostic.message.contains("uninstall all but one"));
        assert!(manager
            .select_source("shared", SourceSelection::System)
            .unwrap_err()
            .contains("extension source not found"));
    }

    #[test]
    fn duplicate_system_identity_honors_exact_persisted_selection() {
        let temporary = tempfile::tempdir().unwrap();
        let first_system = temporary.path().join("system-a");
        let second_system = temporary.path().join("system-b");
        let user = temporary.path().join("user");
        let states = temporary.path().join("states");
        fs::create_dir_all(&first_system).unwrap();
        fs::create_dir_all(&second_system).unwrap();
        fs::create_dir_all(&user).unwrap();
        create_extension(
            &first_system,
            "shared",
            r#"{"name":"shared","version":"1.0.0","skills":[]}"#,
        );
        let selected_identity = create_extension(
            &second_system,
            "shared",
            r#"{"name":"shared","version":"2.0.0","skills":[]}"#,
        )
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
        let mut extension_state = state::ExtensionState::default();
        extension_state.source_selections.insert(
            "shared".to_string(),
            SourceSelectionRecord {
                source: SourceSelection::System,
                source_identity: selected_identity,
            },
        );
        state::save(&extension_state, Some(&states)).unwrap();

        let mut manager = ExtensionManager::new_isolated_with_system_dirs(
            PathBuf::from("/workspace"),
            Some(user),
            vec![first_system, second_system],
            states,
        );
        manager.refresh();

        let extension = &manager.list()[0];
        assert_eq!(extension.version, "2.0.0");
        assert_eq!(extension.source, ExtensionSourceKind::System);
        assert!(extension.is_active);
        assert_eq!(
            extension.available_source_identities.get("system"),
            Some(&extension.path.to_string_lossy().into_owned())
        );
        assert!(!extension
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "extension_system_source_conflict"));
    }

    #[test]
    fn duplicate_system_identity_preserves_exact_user_selection() {
        let temporary = tempfile::tempdir().unwrap();
        let first_system = temporary.path().join("system-a");
        let second_system = temporary.path().join("system-b");
        let user = temporary.path().join("user");
        let states = temporary.path().join("states");
        fs::create_dir_all(&first_system).unwrap();
        fs::create_dir_all(&second_system).unwrap();
        fs::create_dir_all(&user).unwrap();
        create_extension(
            &first_system,
            "shared",
            r#"{"name":"shared","version":"1.0.0","skills":[]}"#,
        );
        create_extension(
            &second_system,
            "shared",
            r#"{"name":"shared","version":"2.0.0","skills":[]}"#,
        );
        let selected_identity = create_extension(
            &user,
            "shared",
            r#"{"name":"shared","version":"3.0.0","skills":[]}"#,
        )
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
        let mut extension_state = state::ExtensionState::default();
        extension_state.source_selections.insert(
            "shared".to_string(),
            SourceSelectionRecord {
                source: SourceSelection::User,
                source_identity: selected_identity,
            },
        );
        state::save(&extension_state, Some(&states)).unwrap();

        let mut manager = ExtensionManager::new_isolated_with_system_dirs(
            PathBuf::from("/workspace"),
            Some(user),
            vec![first_system, second_system],
            states,
        );
        manager.refresh();

        let extension = &manager.list()[0];
        assert_eq!(extension.version, "3.0.0");
        assert_eq!(extension.source, ExtensionSourceKind::Legacy);
        assert!(extension.is_active);
        assert!(!extension.diagnostics.iter().any(|diagnostic| {
            matches!(
                diagnostic.code.as_str(),
                "extension_system_source_conflict" | "extension_source_selection_stale"
            )
        }));
    }

    #[test]
    fn migrates_existing_user_over_system_conflict_once() {
        let (_temporary, user, system, states) = roots();
        create_extension(
            &user,
            "shared",
            r#"{"name":"shared","version":"2.0.0","skills":[]}"#,
        );
        create_extension(
            &system,
            "shared",
            r#"{"name":"shared","version":"1.0.0","skills":[]}"#,
        );
        let expected_user_identity = user
            .join("shared")
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let mut manager = ExtensionManager::new_isolated_with_state(
            PathBuf::from("/workspace"),
            Some(user),
            Some(system),
            states.clone(),
        );
        manager.refresh();
        assert_eq!(manager.list()[0].version, "2.0.0");
        assert_eq!(manager.list()[0].source, ExtensionSourceKind::Legacy);
        assert!(manager
            .catalog_diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "legacy_source_selection_migrated"));
        let loaded = state::load(Some(&states)).unwrap();
        assert_eq!(loaded.origin, StateOrigin::Versioned);
        assert_eq!(
            loaded.state.source_selections.get("shared"),
            Some(&SourceSelectionRecord {
                source: SourceSelection::User,
                source_identity: expected_user_identity,
            })
        );
    }

    #[test]
    fn new_conflict_fails_closed_without_selection() {
        let (_temporary, user, system, states) = roots();
        state::save(&state::ExtensionState::default(), Some(&states)).unwrap();
        create_extension(
            &user,
            "shared",
            r#"{"name":"shared","version":"2.0.0","skills":[]}"#,
        );
        create_extension(
            &system,
            "shared",
            r#"{"name":"shared","version":"1.0.0","skills":[]}"#,
        );
        let mut manager = ExtensionManager::new_isolated_with_state(
            PathBuf::from("/workspace"),
            Some(user),
            Some(system),
            states,
        );
        manager.refresh();
        let extension = &manager.list()[0];
        assert_eq!(extension.health, ExtensionHealth::Conflict);
        assert_eq!(extension.effective_state, EffectiveState::Disabled);
        assert!(!extension.is_active);
    }

    #[test]
    fn corrupt_state_fails_closed() {
        let (_temporary, user, system, states) = roots();
        create_extension(
            &user,
            "example",
            r#"{"name":"example","version":"1.0.0","skills":[]}"#,
        );
        fs::create_dir_all(&states).unwrap();
        fs::write(states.join(crate::state::EXTENSIONS_STATE), "not json").unwrap();
        let mut manager = ExtensionManager::new_isolated_with_state(
            PathBuf::from("/workspace"),
            Some(user),
            Some(system),
            states,
        );
        manager.refresh();
        assert_eq!(manager.list()[0].health, ExtensionHealth::Broken);
        assert!(!manager.list()[0].is_active);
    }

    #[cfg(unix)]
    #[test]
    fn linked_content_change_is_stale_but_capability_change_requires_consent() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let user = temporary.path().join("extensions");
        let system = temporary.path().join("system");
        let states = temporary.path().join("states");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&system).unwrap();
        fs::write(
            source.join(EXTENSION_CONFIG_FILENAME),
            r#"{
                "schemaVersion": 1,
                "name": "example.dev",
                "version": "1.0.0",
                "compatibility": {"cosh": ">=0.12.0"}
            }"#,
        )
        .unwrap();
        fs::write(source.join("README.md"), "one").unwrap();
        let installer = ExtensionInstaller::new(user.clone(), states.clone());
        let preflight = installer.preflight_link(&source).unwrap();
        installer
            .commit(&preflight.operation_id, &preflight.capability_fingerprint)
            .unwrap();
        let mut manager = ExtensionManager::new_isolated_with_state(
            temporary.path().join("workspace"),
            Some(user),
            Some(system),
            states,
        );

        fs::write(source.join("README.md"), "two").unwrap();
        manager.refresh();
        let extension = &manager.list()[0];
        assert_eq!(extension.health, ExtensionHealth::Degraded);
        assert!(extension.is_active);
        assert!(extension
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "extension_link_stale"));

        fs::write(
            source.join(EXTENSION_CONFIG_FILENAME),
            r#"{
                "schemaVersion": 1,
                "name": "example.dev",
                "version": "1.0.0",
                "compatibility": {"cosh": ">=0.12.0"},
                "hooks": {
                    "BeforeModel": [{
                        "hooks": [{
                            "type": "command",
                            "name": "guard",
                            "command": "/usr/bin/true"
                        }]
                    }]
                }
            }"#,
        )
        .unwrap();
        manager.refresh();
        let extension = &manager.list()[0];
        assert_eq!(extension.health, ExtensionHealth::Broken);
        assert!(!extension.is_active);
        assert!(extension
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "extension_link_consent_stale"));

        fs::remove_dir_all(&source).unwrap();
        manager.refresh();
        let extension = &manager.list()[0];
        assert_eq!(extension.name, "example.dev");
        assert_eq!(extension.source, ExtensionSourceKind::Link);
        assert_eq!(extension.health, ExtensionHealth::Broken);
        assert!(!extension.is_active);
        assert!(extension
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "extension_managed_payload_unreadable"));
    }

    #[test]
    fn desired_state_round_trip_controls_effective_state() {
        let (_temporary, user, system, states) = roots();
        create_extension(
            &user,
            "example",
            r#"{"name":"example","version":"1.0.0","skills":[]}"#,
        );
        let mut manager = ExtensionManager::new_isolated_with_state(
            PathBuf::from("/workspace"),
            Some(user),
            Some(system),
            states,
        );
        manager.refresh();
        let disabled = manager.set_enabled("example", false).unwrap();
        assert_eq!(disabled.desired_state, DesiredState::Disabled);
        assert_eq!(disabled.effective_state, EffectiveState::Disabled);
        let enabled = manager.set_enabled("example", true).unwrap();
        assert_eq!(enabled.desired_state, DesiredState::Enabled);
        assert_eq!(enabled.effective_state, EffectiveState::Enabled);
    }

    #[test]
    fn v1_mcp_capability_is_healthy_before_runtime_validation() {
        let (_temporary, user, system, states) = roots();
        create_extension(
            &user,
            "example",
            r#"{
                "schemaVersion":1,
                "name":"example.ops",
                "version":"1.0.0",
                "compatibility":{"cosh":">=0.12.0"},
                "mcpServers":{"inventory":{"transport":"stdio","command":"inventory-mcp"}}
            }"#,
        );
        let mut manager = ExtensionManager::new_isolated_with_state(
            PathBuf::from("/workspace"),
            Some(user),
            Some(system),
            states,
        );
        manager.refresh();
        let extension = &manager.list()[0];
        assert_eq!(extension.health, ExtensionHealth::Healthy);
        assert!(extension.is_active);
        assert!(extension.diagnostics.is_empty());
        assert_eq!(extension.mcp_servers.len(), 1);
    }
}
