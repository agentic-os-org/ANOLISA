//! Trusted admission and launch policy for installed ACP Runtime adapters.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use cosh_gateway_contracts::{
    capability::{CapabilityRequest, CapabilityScope, OperationDescriptor},
    common::{
        ActorKind, ActorRef, AuthAssurance, BoundedName, Digest, RuntimeSelector, TargetRef,
        WorkspaceRef,
    },
    error::{ContractError, ErrorCategory},
    ids::{
        ActorId, AgentSessionId, InstallationId, RequestId, RuntimeBindingId, RuntimeInstanceId,
    },
};
use sha2::{Digest as _, Sha256};

use crate::daemon::ScheduledRun;

use super::{
    AcpAgentRuntime, AcpAgentRuntimeConfig, AcpAgentRuntimeIdentity, AcpPermissionContext,
    AcpPermissionNormalizer, AcpRuntimeProfileId, AcpRuntimeProfileRequest,
    AcpRuntimeProfileResolver, AcpSessionDriverConfig, AcpV1ClientConfig, AgentRuntimePortError,
    AgentRuntimePortFactory, PinnedDirectory, PinnedFileIdentity, ResolvedAcpRuntimeProfile,
    ScheduledRuntimePort,
};

const MAX_ACP_FRAME_BYTES: usize = 1024 * 1024;
const PERMISSION_LIFETIME_MS: u64 = 5 * 60 * 1_000;

/// Reconstructs the sole local OS actor accepted by a per-user Gateway.
#[derive(Clone)]
pub struct LocalOsActorResolver {
    installation_id: InstallationId,
    owner_uid: u32,
    actor: ActorRef,
}

impl LocalOsActorResolver {
    /// Builds the immutable local actor policy for one installation and owner.
    #[must_use]
    pub fn new(installation_id: InstallationId, owner_uid: u32) -> Self {
        let actor_id = local_actor_id(&installation_id, owner_uid);
        Self {
            installation_id,
            owner_uid,
            actor: ActorRef {
                actor_id,
                actor_kind: ActorKind::Human,
                issuer: static_name("local-os"),
                assurance: AuthAssurance::LocalOs,
            },
        }
    }

    /// Returns the trusted actor only when the complete supplied identity matches.
    ///
    /// # Errors
    ///
    /// Returns an unauthorized failure for actor ID, kind, issuer, or assurance
    /// substitution.
    pub fn resolve(&self, supplied: &ActorRef) -> Result<ActorRef, ContractError> {
        if supplied == &self.actor {
            Ok(self.actor.clone())
        } else {
            Err(contract_error(
                "runtime_actor_invalid",
                ErrorCategory::Unauthorized,
                false,
                "The Runtime actor does not match the authenticated local principal",
            ))
        }
    }

    /// Returns the complete local OS actor admitted by this resolver.
    #[must_use]
    pub fn actor_ref(&self) -> &ActorRef {
        &self.actor
    }

    pub(super) fn installation_id(&self) -> &InstallationId {
        &self.installation_id
    }
}

impl fmt::Debug for LocalOsActorResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalOsActorResolver")
            .field("installation_id", &self.installation_id)
            .field("owner_uid", &self.owner_uid)
            .finish_non_exhaustive()
    }
}

/// Canonical workspace admitted for one exact governed target.
#[derive(Clone)]
pub struct ResolvedWorkspace {
    directory: PinnedDirectory,
    reference: WorkspaceRef,
}

impl ResolvedWorkspace {
    /// Returns the canonical absolute directory used as the child working directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.directory.canonical_path()
    }

    /// Returns the immutable local filesystem identity selected at admission.
    #[must_use]
    pub fn identity(&self) -> PinnedFileIdentity {
        self.directory.identity()
    }

    pub(crate) fn pinned_directory(&self) -> &PinnedDirectory {
        &self.directory
    }

    /// Returns the public digest-only workspace projection.
    #[must_use]
    pub fn reference(&self) -> &WorkspaceRef {
        &self.reference
    }
}

impl fmt::Debug for ResolvedWorkspace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedWorkspace")
            .field("scope_digest", &self.reference.scope_digest)
            .finish_non_exhaustive()
    }
}

/// Maps a closed set of exact trusted targets to one canonical local workspace.
#[derive(Clone)]
pub struct TrustedWorkspaceResolver {
    targets: Vec<TargetRef>,
    path: PathBuf,
    reference: WorkspaceRef,
    directory: Arc<RwLock<Option<PinnedDirectory>>>,
}

impl TrustedWorkspaceResolver {
    /// Canonicalizes an absolute configured workspace without using Task display data.
    ///
    /// # Errors
    ///
    /// Rejects relative, unavailable, or non-directory workspace configuration.
    pub fn new(target: TargetRef, workspace: impl AsRef<Path>) -> Result<Self, ContractError> {
        Self::new_for_targets([target], workspace)
    }

    /// Canonicalizes one workspace for a closed set of exact target identities.
    ///
    /// # Errors
    ///
    /// Rejects an empty target set or relative, unavailable, or non-directory
    /// workspace configuration.
    pub fn new_for_targets(
        targets: impl IntoIterator<Item = TargetRef>,
        workspace: impl AsRef<Path>,
    ) -> Result<Self, ContractError> {
        let targets = targets.into_iter().collect::<Vec<_>>();
        if targets.is_empty() {
            return Err(workspace_error());
        }
        let configured = workspace.as_ref();
        if !configured.is_absolute() {
            return Err(workspace_error());
        }
        let directory = PinnedDirectory::pin(configured).map_err(|_| workspace_error())?;
        let identity = directory.identity();
        let device = identity.device().to_be_bytes();
        let inode = identity.inode().to_be_bytes();
        let reference = WorkspaceRef {
            scope_digest: sha256_parts(&[
                b"cosh.runtime.workspace.v2",
                path_bytes(directory.canonical_path()),
                &device,
                &inode,
            ]),
            display_name: None,
        };
        Ok(Self {
            targets,
            path: directory.canonical_path().to_path_buf(),
            reference,
            directory: Arc::new(RwLock::new(Some(directory))),
        })
    }

    /// Resolves only the complete target admitted by trusted daemon configuration.
    ///
    /// # Errors
    ///
    /// Rejects target kind, authority, or identifier substitution.
    pub fn resolve(&self, target: &TargetRef) -> Result<ResolvedWorkspace, ContractError> {
        if self.targets.iter().any(|candidate| candidate == target) {
            let directory = self
                .directory
                .read()
                .map_err(|_| workspace_error())?
                .clone()
                .ok_or_else(workspace_error)?;
            Ok(ResolvedWorkspace {
                directory,
                reference: self.reference.clone(),
            })
        } else {
            Err(contract_error(
                "runtime_target_invalid",
                ErrorCategory::Unauthorized,
                false,
                "The Runtime target is not mapped to a trusted workspace",
            ))
        }
    }

    /// Re-pins the admitted canonical path after a proven governed snapshot switch.
    ///
    /// Clones share the replacement. Existing launch handles retain their inode;
    /// the daemon must serialize switching with new Runtime admission. The public
    /// workspace reference stays stable for durable Tasks and checkpoint evidence.
    ///
    /// # Errors
    ///
    /// Leaves new Runtime admission disabled if the directory cannot be pinned or
    /// resolves to another path. Call only after trusted rollback success.
    pub fn refresh_after_snapshot_switch(&self) -> Result<(), ContractError> {
        let mut current = self.directory.write().map_err(|_| workspace_error())?;
        *current = None;
        let directory = PinnedDirectory::pin(&self.path).map_err(|_| workspace_error())?;
        if directory.canonical_path() != self.path {
            return Err(workspace_error());
        }
        *current = Some(directory);
        Ok(())
    }

    /// Disables new Runtime admission after a snapshot switch with uncertain effects.
    ///
    /// # Errors
    ///
    /// Returns an error for a poisoned binding lock, which also blocks resolution.
    pub fn invalidate_after_snapshot_switch(&self) -> Result<(), ContractError> {
        *self.directory.write().map_err(|_| workspace_error())? = None;
        Ok(())
    }

    /// Returns the admitted digest-only workspace projection.
    #[must_use]
    pub fn workspace_ref(&self) -> &WorkspaceRef {
        &self.reference
    }
}

impl fmt::Debug for TrustedWorkspaceResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedWorkspaceResolver")
            .field("targets", &self.targets)
            .field("workspace", &self.reference)
            .finish()
    }
}

/// Production factory for explicitly configured, allowlisted ACP adapters.
pub struct InstalledAcpRuntimePortFactory {
    installation_id: InstallationId,
    actors: LocalOsActorResolver,
    workspaces: TrustedWorkspaceResolver,
    adapters: BTreeMap<AcpRuntimeProfileId, ResolvedAcpRuntimeProfile>,
}

impl InstalledAcpRuntimePortFactory {
    /// Validates all adapter entries and pins their canonical launch targets.
    ///
    /// The resulting profiles also retain an immutable filtered environment
    /// snapshot; later symlink retargeting cannot redirect Runtime launches.
    ///
    /// # Errors
    ///
    /// Rejects an installation mismatch or any adapter that is relative,
    /// profile-mismatched, unavailable, non-regular, or non-executable.
    pub fn new(
        installation_id: InstallationId,
        actors: LocalOsActorResolver,
        workspaces: TrustedWorkspaceResolver,
        adapters: BTreeMap<AcpRuntimeProfileId, PathBuf>,
        environment: BTreeMap<OsString, OsString>,
    ) -> Result<Self, ContractError> {
        if actors.installation_id() != &installation_id || adapters.is_empty() {
            return Err(profile_error());
        }
        let workspace = workspaces.path.clone();
        let mut resolved_adapters = BTreeMap::new();
        for (profile, executable) in adapters {
            let resolved = AcpRuntimeProfileResolver::resolve(AcpRuntimeProfileRequest {
                profile,
                executable: Some(executable),
                workspace: workspace.clone(),
                environment: environment.clone(),
            })
            .map_err(|_| profile_error())?;
            resolved_adapters.insert(profile, resolved);
        }
        Self::from_resolved_profiles(installation_id, actors, workspaces, resolved_adapters)
    }

    /// Builds a factory from profiles already resolved by trusted admission.
    ///
    /// This path preserves the canonical target selected during admission and
    /// never follows the configured adapter entry again.
    ///
    /// # Errors
    ///
    /// Rejects an installation mismatch, an empty profile set, a map key that
    /// differs from its resolved profile, or a profile resolved for another
    /// workspace.
    pub fn from_resolved_profiles(
        installation_id: InstallationId,
        actors: LocalOsActorResolver,
        workspaces: TrustedWorkspaceResolver,
        adapters: BTreeMap<AcpRuntimeProfileId, ResolvedAcpRuntimeProfile>,
    ) -> Result<Self, ContractError> {
        if actors.installation_id() != &installation_id || adapters.is_empty() {
            return Err(profile_error());
        }
        let workspace = &workspaces.path;
        let workspace_identity = workspaces
            .directory
            .read()
            .map_err(|_| workspace_error())?
            .as_ref()
            .ok_or_else(workspace_error)?
            .identity();
        if adapters.iter().any(|(profile, resolved)| {
            resolved.profile() != *profile
                || resolved.workspace() != workspace
                || resolved.pinned_workspace().identity() != workspace_identity
        }) {
            return Err(profile_error());
        }
        Ok(Self {
            installation_id,
            actors,
            workspaces,
            adapters,
        })
    }
}

impl fmt::Debug for InstalledAcpRuntimePortFactory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let environment_names = self
            .adapters
            .values()
            .flat_map(ResolvedAcpRuntimeProfile::environment_names)
            .collect::<Vec<_>>();
        formatter
            .debug_struct("InstalledAcpRuntimePortFactory")
            .field("installation_id", &self.installation_id)
            .field("actors", &self.actors)
            .field("workspaces", &self.workspaces)
            .field("profiles", &self.adapters.keys())
            .field("environment_names", &environment_names)
            .finish()
    }
}

impl AgentRuntimePortFactory for InstalledAcpRuntimePortFactory {
    fn create(&mut self, run: &ScheduledRun) -> Result<ScheduledRuntimePort, ContractError> {
        let profile = selected_profile(&run.runtime)?;
        let resolved = self.adapters.get(&profile).ok_or_else(profile_error)?;
        let actor = self.actors.resolve(&run.actor)?;
        let workspace = self.workspaces.resolve(&run.target)?;
        if workspace.reference() != &run.workspace {
            return Err(contract_error(
                "runtime_workspace_mismatch",
                ErrorCategory::Unauthorized,
                false,
                "The admitted Runtime workspace no longer matches trusted configuration",
            ));
        }
        if resolved.workspace() != workspace.path() {
            return Err(workspace_error());
        }

        let identity = AcpAgentRuntimeIdentity {
            installation_id: self.installation_id.clone(),
            actor,
            task_id: run.task_id.clone(),
            run_id: run.run_id.clone(),
            agent_session_id: AgentSessionId::new(),
            binding_id: RuntimeBindingId::new(),
            runtime_instance_id: RuntimeInstanceId::new(),
            runtime_generation: run.lease_generation,
            adapter_authority: static_name(profile.profile().executable_name()),
            connection_scope_digest: connection_scope_digest(
                &self.installation_id,
                profile,
                resolved.executable(),
                resolved.workspace(),
            ),
        };
        let config = AcpAgentRuntimeConfig {
            session: AcpSessionDriverConfig::new(
                resolved.launch_spec_in(workspace.pinned_directory().clone()),
                resolved.bind_client_config(AcpV1ClientConfig::new(
                    "cosh-gateway",
                    env!("CARGO_PKG_VERSION"),
                    MAX_ACP_FRAME_BYTES,
                )),
                resolved.workspace(),
            ),
            workspace: workspace.reference().clone(),
            identity,
        };
        let normalizer = GenericProviderNativeNormalizer {
            target: run.target.clone(),
        };
        let port = AcpAgentRuntime::launch(config, Box::new(normalizer)).map_err(|_| {
            contract_error(
                "runtime_launch_failed",
                ErrorCategory::RuntimeUnavailable,
                true,
                "The installed ACP Runtime could not be launched",
            )
        })?;
        Ok(ScheduledRuntimePort::new(
            Box::new(port),
            workspace.reference().clone(),
        ))
    }
}

struct GenericProviderNativeNormalizer {
    target: TargetRef,
}

impl AcpPermissionNormalizer for GenericProviderNativeNormalizer {
    fn normalize(
        &mut self,
        request: &super::AcpV1PermissionRequest,
        context: &AcpPermissionContext,
    ) -> Result<CapabilityRequest, AgentRuntimePortError> {
        let input =
            serde_json::to_vec(&request.tool_call).map_err(|_| AgentRuntimePortError::Protocol)?;
        let input_digest = sha256(&input);
        let target =
            serde_json::to_vec(&self.target).map_err(|_| AgentRuntimePortError::Protocol)?;
        let operation_digest = sha256_parts(&[
            b"cosh.provider-native.invoke.v1",
            input_digest.as_str().as_bytes(),
            &target,
        ]);
        let now_ms: u64 = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| AgentRuntimePortError::Protocol)?
            .as_millis()
            .try_into()
            .map_err(|_| AgentRuntimePortError::Protocol)?;
        Ok(CapabilityRequest {
            request_id: RequestId::new(),
            task_id: context.task_id.clone(),
            run_id: context.run_id.clone(),
            actor: context.actor.clone(),
            target: self.target.clone(),
            operation: OperationDescriptor {
                namespace: static_name("provider-native"),
                name: static_name("invoke"),
                arguments_digest: input_digest.clone(),
            },
            operation_digest,
            requested_scope: CapabilityScope {
                resource: static_name("provider-tool"),
                access: static_name("execute"),
            },
            input_digest,
            expires_at_ms: now_ms.saturating_add(PERMISSION_LIFETIME_MS),
        })
    }
}

fn selected_profile(runtime: &RuntimeSelector) -> Result<AcpRuntimeProfileId, ContractError> {
    if runtime.runtime.as_str() != "acp" {
        return Err(profile_error());
    }
    match runtime.profile.as_ref().map(BoundedName::as_str) {
        Some("codex") => Ok(AcpRuntimeProfileId::Codex),
        Some("claude-code") => Ok(AcpRuntimeProfileId::ClaudeCode),
        _ => Err(profile_error()),
    }
}

fn local_actor_id(installation_id: &InstallationId, uid: u32) -> ActorId {
    let mut bytes = Sha256::digest(
        [
            b"cosh.gateway.local.actor.v1".as_slice(),
            installation_id.as_str().as_bytes(),
            &uid.to_be_bytes(),
        ]
        .concat(),
    );
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let uuid = format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    );
    ActorId::parse(format!("act_{uuid}"))
        .unwrap_or_else(|_| unreachable!("derived local actor ID must remain canonical"))
}

fn connection_scope_digest(
    installation_id: &InstallationId,
    profile: AcpRuntimeProfileId,
    executable: &Path,
    workspace: &Path,
) -> Digest {
    let profile = match profile {
        AcpRuntimeProfileId::Codex => b"codex".as_slice(),
        AcpRuntimeProfileId::ClaudeCode => b"claude-code".as_slice(),
    };
    sha256_parts(&[
        b"cosh.acp.connection-scope.v1",
        installation_id.as_str().as_bytes(),
        profile,
        path_bytes(executable),
        path_bytes(workspace),
    ])
}

fn sha256(bytes: &[u8]) -> Digest {
    Digest::parse(format!("{:x}", Sha256::digest(bytes)))
        .unwrap_or_else(|_| unreachable!("SHA-256 output must remain canonical"))
}

fn sha256_parts(parts: &[&[u8]]) -> Digest {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part);
    }
    Digest::parse(format!("{:x}", digest.finalize()))
        .unwrap_or_else(|_| unreachable!("SHA-256 output must remain canonical"))
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> &[u8] {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes()
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> &[u8] {
    path.as_os_str().to_str().unwrap_or_default().as_bytes()
}

fn static_name(value: &'static str) -> BoundedName {
    BoundedName::new(value).unwrap_or_else(|_| unreachable!("static Runtime name must be bounded"))
}

fn workspace_error() -> ContractError {
    contract_error(
        "runtime_workspace_invalid",
        ErrorCategory::InvalidRequest,
        false,
        "The configured Runtime workspace is invalid",
    )
}

fn profile_error() -> ContractError {
    contract_error(
        "runtime_profile_invalid",
        ErrorCategory::InvalidRequest,
        false,
        "The selected installed Runtime profile is invalid",
    )
}

fn contract_error(
    code: &'static str,
    category: ErrorCategory,
    retryable: bool,
    message: &'static str,
) -> ContractError {
    ContractError::new(code, category, retryable, message)
        .unwrap_or_else(|_| unreachable!("static Runtime error must remain bounded"))
}

#[cfg(test)]
#[path = "installed_acp_factory/tests.rs"]
mod tests;
