//! Versioned Gateway capability profiles and their closed Runtime tool manifests.
//!
//! Profiles are fixed contracts selected by trusted Gateway configuration. Task
//! input and Runtime output may verify a profile, but cannot extend its tools.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::common::{BoundedName, BoundedOpaque, Digest, TargetRef};

/// Canonical wire and configuration name of the portable Task-only profile.
pub const TASK_ONLY_V1_PROFILE: &str = "task-only-v1";
/// Canonical wire and configuration name of the workspace-checkpoint profile.
pub const WORKSPACE_CHECKPOINT_V1_PROFILE: &str = "workspace-checkpoint-v1";
/// Canonical wire and configuration name of the workspace-write profile.
pub const WORKSPACE_WRITE_V1_PROFILE: &str = "workspace-write-v1";
/// Canonical wire and configuration name of the full ACP delegation profile.
pub const DELEGATED_ACP_V1_PROFILE: &str = "delegated-acp-v1";
/// Runtime tool resolved by the Gateway without a host side effect.
pub const ASK_USER_QUESTION_TOOL: &str = "ask_user_question";
/// Runtime tool whose side effect must cross a governed checkpoint target.
pub const WORKSPACE_CHECKPOINT_CREATE_TOOL: &str = "workspace_checkpoint_create";
/// Runtime tool that writes through the Core's pinned workspace boundary.
pub const WRITE_FILE_TOOL: &str = "write_file";
/// Canonical name of the only checkpoint provider admitted by this contract.
pub const WS_CKPT_PROVIDER: &str = "ws-ckpt";
/// Domain separator for the first capability-profile manifest format.
pub const CAPABILITY_PROFILE_MANIFEST_DOMAIN: &str = "cosh.gateway.capability-profile.v1";
/// Canonical manifest of the portable Task-only profile.
pub const TASK_ONLY_V1_CANONICAL_MANIFEST: &str = concat!(
    "cosh.gateway.capability-profile.v1\n",
    "profile:task-only-v1\n",
    "target:\n",
    "workspace/cosh/task-only-v1\n",
    "runtime-tools:\n",
    "ask_user_question\n",
);
/// Pinned SHA-256 digest of [`TASK_ONLY_V1_CANONICAL_MANIFEST`].
pub const TASK_ONLY_V1_MANIFEST_DIGEST: &str =
    "2b95e0f3e28df8eb2b7930f2dec3650ffe399f971671c971865e4663c382c94a";
/// Canonical manifest of the optional workspace-checkpoint profile.
///
/// Each profile manifest is an opaque pinned constant compared by digest, never
/// a parsed structure. A profile without providers therefore omits the trailing
/// `providers:` section entirely, which keeps
/// [`TASK_ONLY_V1_CANONICAL_MANIFEST`] byte-identical to its original revision.
pub const WORKSPACE_CHECKPOINT_V1_CANONICAL_MANIFEST: &str = concat!(
    "cosh.gateway.capability-profile.v1\n",
    "profile:workspace-checkpoint-v1\n",
    "target:\n",
    "workspace/cosh/workspace-checkpoint-v1\n",
    "runtime-tools:\n",
    "ask_user_question\n",
    "workspace_checkpoint_create\n",
    "providers:\n",
    "ws-ckpt\n",
);
/// Pinned SHA-256 digest of [`WORKSPACE_CHECKPOINT_V1_CANONICAL_MANIFEST`].
pub const WORKSPACE_CHECKPOINT_V1_MANIFEST_DIGEST: &str =
    "6b3e7093e7b8656d4a7cf21faa85b9eed761ef415d002623cfc442f3ef3c8ae1";
/// Canonical manifest of the workspace-scoped write profile.
pub const WORKSPACE_WRITE_V1_CANONICAL_MANIFEST: &str = concat!(
    "cosh.gateway.capability-profile.v1\n",
    "profile:workspace-write-v1\n",
    "target:\n",
    "workspace/cosh/workspace-write-v1\n",
    "runtime-tools:\n",
    "ask_user_question\n",
    "write_file\n",
);
/// Pinned SHA-256 digest of [`WORKSPACE_WRITE_V1_CANONICAL_MANIFEST`].
pub const WORKSPACE_WRITE_V1_MANIFEST_DIGEST: &str =
    "30574302eeba3adbb5ea143a8a869331d58a15bd24b9532d0f52613136bb2b2a";
/// Canonical manifest of the profile that delegates one Task to an ACP Runtime.
///
/// The empty `runtime-tools` inventory means COSH hosts no individual tool for
/// this profile. Instead, the explicit delegation grants the pinned ACP Runtime
/// provider-native allow-once decisions for the lifetime of the Task Run.
pub const DELEGATED_ACP_V1_CANONICAL_MANIFEST: &str = concat!(
    "cosh.gateway.capability-profile.v1\n",
    "profile:delegated-acp-v1\n",
    "target:\n",
    "workspace/cosh/delegated-acp-v1\n",
    "runtime-tools:\n",
    "delegation:\n",
    "provider-native-allow-once\n",
);
/// Pinned SHA-256 digest of [`DELEGATED_ACP_V1_CANONICAL_MANIFEST`].
pub const DELEGATED_ACP_V1_MANIFEST_DIGEST: &str =
    "a6978e5eafa5befe62ba606e073fef71057278e75b6408f57318ddd94071a3f4";

const TASK_ONLY_V1_RUNTIME_TOOLS: &[&str] = &[ASK_USER_QUESTION_TOOL];
const WORKSPACE_CHECKPOINT_V1_RUNTIME_TOOLS: &[&str] =
    &[ASK_USER_QUESTION_TOOL, WORKSPACE_CHECKPOINT_CREATE_TOOL];
const WORKSPACE_WRITE_V1_RUNTIME_TOOLS: &[&str] = &[ASK_USER_QUESTION_TOOL, WRITE_FILE_TOOL];
const DELEGATED_ACP_V1_RUNTIME_TOOLS: &[&str] = &[];
const NO_PROVIDERS: &[CapabilityProviderId] = &[];
const WS_CKPT_PROVIDER_SET: &[CapabilityProviderId] = &[CapabilityProviderId::WsCkpt];

/// Failure returned when a profile name is not an admitted production profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error(
    "unsupported capability profile; expected `task-only-v1`, `workspace-checkpoint-v1`, `workspace-write-v1`, or `delegated-acp-v1`"
)]
pub struct CapabilityProfileParseError;

/// Failure returned when a provider name is not an admitted production provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("unsupported capability provider; expected `ws-ckpt`")]
pub struct CapabilityProviderParseError;

/// Versioned identity of a side-effect provider sealed into a capability profile.
///
/// Providers are selected only by the profile a trusted Gateway configuration
/// admits. Task input, Runtime tool names, and ACP payloads never widen this set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityProviderId {
    /// Workspace checkpoint provider backed by the local `ws-ckpt` daemon.
    WsCkpt,
}

impl CapabilityProviderId {
    /// Returns the canonical wire and configuration name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WsCkpt => WS_CKPT_PROVIDER,
        }
    }

    /// Parses an exact canonical production provider name.
    ///
    /// # Errors
    ///
    /// Returns [`CapabilityProviderParseError`] for every unknown name. Unknown
    /// names never fall back to an admitted provider.
    pub fn parse(value: &str) -> Result<Self, CapabilityProviderParseError> {
        match value {
            WS_CKPT_PROVIDER => Ok(Self::WsCkpt),
            _ => Err(CapabilityProviderParseError),
        }
    }
}

/// Failure returned when advertised profile state differs from its closed contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CapabilityProfileVerificationError {
    /// The profile name differs from the configured profile.
    #[error("capability profile identity does not match the configured profile")]
    ProfileMismatch,
    /// The profile manifest digest differs from the pinned contract digest.
    #[error("capability profile manifest digest does not match the configured profile")]
    ManifestDigestMismatch,
    /// The Runtime tool inventory differs from the profile's closed inventory.
    #[error("Runtime tool inventory does not match the configured capability profile")]
    RuntimeToolInventoryMismatch,
    /// The admitted provider set differs from the profile's sealed provider set.
    #[error("capability provider set does not match the configured capability profile")]
    ProviderSetMismatch,
}

/// Versioned identity of an admitted Gateway capability profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GatewayCapabilityProfileId {
    /// Portable profile whose only Runtime tool asks the user a question.
    TaskOnlyV1,
    /// Optional profile that additionally admits one governed checkpoint target.
    WorkspaceCheckpointV1,
    /// Profile that admits only an approval-gated workspace file write.
    WorkspaceWriteV1,
    /// Profile that grants a pinned ACP Runtime full provider-native Task authority.
    DelegatedAcpV1,
}

impl GatewayCapabilityProfileId {
    /// Returns the canonical wire and configuration name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TaskOnlyV1 => TASK_ONLY_V1_PROFILE,
            Self::WorkspaceCheckpointV1 => WORKSPACE_CHECKPOINT_V1_PROFILE,
            Self::WorkspaceWriteV1 => WORKSPACE_WRITE_V1_PROFILE,
            Self::DelegatedAcpV1 => DELEGATED_ACP_V1_PROFILE,
        }
    }

    /// Parses an exact canonical production profile name.
    ///
    /// # Errors
    ///
    /// Returns [`CapabilityProfileParseError`] for every unknown name. Unknown
    /// names never fall back to the Task-only profile.
    pub fn parse(value: &str) -> Result<Self, CapabilityProfileParseError> {
        match value {
            TASK_ONLY_V1_PROFILE => Ok(Self::TaskOnlyV1),
            WORKSPACE_CHECKPOINT_V1_PROFILE => Ok(Self::WorkspaceCheckpointV1),
            WORKSPACE_WRITE_V1_PROFILE => Ok(Self::WorkspaceWriteV1),
            DELEGATED_ACP_V1_PROFILE => Ok(Self::DelegatedAcpV1),
            _ => Err(CapabilityProfileParseError),
        }
    }

    /// Returns the complete closed profile for this identity.
    #[must_use]
    pub const fn profile(self) -> GatewayCapabilityProfile {
        match self {
            Self::TaskOnlyV1 => GatewayCapabilityProfile::task_only_v1(),
            Self::WorkspaceCheckpointV1 => GatewayCapabilityProfile::workspace_checkpoint_v1(),
            Self::WorkspaceWriteV1 => GatewayCapabilityProfile::workspace_write_v1(),
            Self::DelegatedAcpV1 => GatewayCapabilityProfile::delegated_acp_v1(),
        }
    }
}

/// Durable identity that binds a profile name to its exact manifest revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayCapabilityProfileIdentity {
    /// Versioned profile name.
    pub profile_id: GatewayCapabilityProfileId,
    /// Digest of the complete canonical profile manifest.
    pub manifest_digest: Digest,
}

/// Closed capability profile admitted by a production Gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatewayCapabilityProfile {
    id: GatewayCapabilityProfileId,
}

impl GatewayCapabilityProfile {
    /// Returns the portable profile that admits no side-effect provider.
    #[must_use]
    pub const fn task_only_v1() -> Self {
        Self {
            id: GatewayCapabilityProfileId::TaskOnlyV1,
        }
    }

    /// Returns the optional profile that admits exactly one checkpoint provider.
    ///
    /// Selecting this profile does not make a checkpoint reachable on its own. A
    /// trusted Gateway configuration must additionally admit the sealed provider
    /// set as a real, identity-bound execution target.
    #[must_use]
    pub const fn workspace_checkpoint_v1() -> Self {
        Self {
            id: GatewayCapabilityProfileId::WorkspaceCheckpointV1,
        }
    }

    /// Returns the profile that admits one approval-gated workspace write tool.
    #[must_use]
    pub const fn workspace_write_v1() -> Self {
        Self {
            id: GatewayCapabilityProfileId::WorkspaceWriteV1,
        }
    }

    /// Returns the profile that delegates one complete Run to a pinned ACP Runtime.
    #[must_use]
    pub const fn delegated_acp_v1() -> Self {
        Self {
            id: GatewayCapabilityProfileId::DelegatedAcpV1,
        }
    }

    /// Returns the versioned profile identity.
    #[must_use]
    pub const fn id(self) -> GatewayCapabilityProfileId {
        self.id
    }

    /// Returns the canonical manifest covered by [`Self::manifest_digest`].
    #[must_use]
    pub const fn canonical_manifest(self) -> &'static str {
        match self.id {
            GatewayCapabilityProfileId::TaskOnlyV1 => TASK_ONLY_V1_CANONICAL_MANIFEST,
            GatewayCapabilityProfileId::WorkspaceCheckpointV1 => {
                WORKSPACE_CHECKPOINT_V1_CANONICAL_MANIFEST
            }
            GatewayCapabilityProfileId::WorkspaceWriteV1 => WORKSPACE_WRITE_V1_CANONICAL_MANIFEST,
            GatewayCapabilityProfileId::DelegatedAcpV1 => DELEGATED_ACP_V1_CANONICAL_MANIFEST,
        }
    }

    /// Returns the pinned digest of the complete canonical manifest.
    #[must_use]
    pub fn manifest_digest(self) -> Digest {
        let digest = match self.id {
            GatewayCapabilityProfileId::TaskOnlyV1 => TASK_ONLY_V1_MANIFEST_DIGEST,
            GatewayCapabilityProfileId::WorkspaceCheckpointV1 => {
                WORKSPACE_CHECKPOINT_V1_MANIFEST_DIGEST
            }
            GatewayCapabilityProfileId::WorkspaceWriteV1 => WORKSPACE_WRITE_V1_MANIFEST_DIGEST,
            GatewayCapabilityProfileId::DelegatedAcpV1 => DELEGATED_ACP_V1_MANIFEST_DIGEST,
        };
        Digest::parse(digest)
            .unwrap_or_else(|_| unreachable!("reviewed static profile digests are canonical"))
    }

    /// Returns the durable identity for this exact manifest revision.
    #[must_use]
    pub fn identity(self) -> GatewayCapabilityProfileIdentity {
        GatewayCapabilityProfileIdentity {
            profile_id: self.id,
            manifest_digest: self.manifest_digest(),
        }
    }

    /// Returns the single governed target bound into the profile manifest.
    #[must_use]
    pub fn governed_target(self) -> TargetRef {
        TargetRef {
            kind: BoundedName::new("workspace")
                .unwrap_or_else(|_| unreachable!("static profile target names are bounded")),
            authority: BoundedName::new("cosh")
                .unwrap_or_else(|_| unreachable!("static profile target names are bounded")),
            identifier: BoundedOpaque::new(self.id.as_str())
                .unwrap_or_else(|_| unreachable!("static profile target IDs are bounded")),
        }
    }

    /// Returns the exact ordered Runtime tool inventory admitted by the profile.
    #[must_use]
    pub const fn runtime_tools(self) -> &'static [&'static str] {
        match self.id {
            GatewayCapabilityProfileId::TaskOnlyV1 => TASK_ONLY_V1_RUNTIME_TOOLS,
            GatewayCapabilityProfileId::WorkspaceCheckpointV1 => {
                WORKSPACE_CHECKPOINT_V1_RUNTIME_TOOLS
            }
            GatewayCapabilityProfileId::WorkspaceWriteV1 => WORKSPACE_WRITE_V1_RUNTIME_TOOLS,
            GatewayCapabilityProfileId::DelegatedAcpV1 => DELEGATED_ACP_V1_RUNTIME_TOOLS,
        }
    }

    /// Returns the exact ordered side-effect provider set sealed into the profile.
    ///
    /// The Task-only profile returns an empty set, so a Task-only instance can
    /// never reach a side-effect provider even when one is installed on the host.
    #[must_use]
    pub const fn providers(self) -> &'static [CapabilityProviderId] {
        match self.id {
            GatewayCapabilityProfileId::TaskOnlyV1 => NO_PROVIDERS,
            GatewayCapabilityProfileId::WorkspaceCheckpointV1 => WS_CKPT_PROVIDER_SET,
            GatewayCapabilityProfileId::WorkspaceWriteV1 => NO_PROVIDERS,
            GatewayCapabilityProfileId::DelegatedAcpV1 => NO_PROVIDERS,
        }
    }

    /// Returns whether Task submission grants provider-native allow-once delegation.
    #[must_use]
    pub const fn delegates_provider_native(self) -> bool {
        matches!(self.id, GatewayCapabilityProfileId::DelegatedAcpV1)
    }

    /// Verifies a durable or advertised identity against the configured profile.
    ///
    /// # Errors
    ///
    /// Returns a mismatch when either the versioned name or manifest digest
    /// differs. Callers must reject the binding instead of selecting a fallback.
    pub fn verify_identity(
        self,
        actual: &GatewayCapabilityProfileIdentity,
    ) -> Result<(), CapabilityProfileVerificationError> {
        if actual.profile_id != self.id {
            return Err(CapabilityProfileVerificationError::ProfileMismatch);
        }
        if actual.manifest_digest != self.manifest_digest() {
            return Err(CapabilityProfileVerificationError::ManifestDigestMismatch);
        }
        Ok(())
    }

    /// Verifies that a Runtime advertises exactly the closed profile inventory.
    ///
    /// # Errors
    ///
    /// Returns a mismatch for missing, additional, reordered, or renamed tools.
    pub fn verify_runtime_tools(
        self,
        actual: &[&str],
    ) -> Result<(), CapabilityProfileVerificationError> {
        if actual == self.runtime_tools() {
            Ok(())
        } else {
            Err(CapabilityProfileVerificationError::RuntimeToolInventoryMismatch)
        }
    }

    /// Verifies that trusted configuration admitted exactly the sealed provider set.
    ///
    /// # Errors
    ///
    /// Returns a mismatch for missing, additional, reordered, or substituted
    /// providers. A Task-only instance therefore rejects any admitted provider.
    pub fn verify_providers(
        self,
        actual: &[CapabilityProviderId],
    ) -> Result<(), CapabilityProfileVerificationError> {
        if actual == self.providers() {
            Ok(())
        } else {
            Err(CapabilityProfileVerificationError::ProviderSetMismatch)
        }
    }
}

#[cfg(test)]
mod tests;
