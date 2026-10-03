/// Configuration for one per-user local Gateway daemon.
#[derive(Debug, Clone)]
pub struct GatewayDaemonConfig {
    /// Absolute Unix socket path inside a private directory.
    pub socket_path: PathBuf,
    /// Absolute SQLite state path.
    pub database_path: PathBuf,
    /// Durable identity shared by events in this database.
    pub installation_id: Option<InstallationId>,
    /// Sealed per-Task launch choices and their operator-configured readiness.
    pub launch_catalog: TaskLaunchCatalog,
}

/// Safe readiness exposed to local launch surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LaunchReadiness {
    /// The configured adapter is ready for new Tasks.
    Ready,
    /// The adapter is unavailable for a bounded, presentation-safe reason.
    Unavailable {
        /// Safe reason suitable for a local launch form.
        reason: cosh_gateway_contracts::common::BoundedText,
    },
}

impl LaunchReadiness {
    /// Constructs a ready capability.
    #[must_use]
    pub const fn ready() -> Self {
        Self::Ready
    }

    /// Constructs an unavailable capability with a presentation-safe reason.
    #[must_use]
    pub fn unavailable(reason: cosh_gateway_contracts::common::BoundedText) -> Self {
        Self::Unavailable { reason }
    }

    /// Returns the safe unavailability reason, when present.
    #[must_use]
    pub const fn reason(&self) -> Option<&cosh_gateway_contracts::common::BoundedText> {
        match self {
            Self::Ready => None,
            Self::Unavailable { reason } => Some(reason),
        }
    }

    fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// Security posture attached to one Runtime catalog entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSecurityPosture {
    /// Runtime-native effects execute with the local user's authority.
    pub delegated_local_authority: bool,
    /// COSH-owned capability effects require the Gateway broker.
    pub gateway_brokered_effects: bool,
    /// A pre-Runtime checkpoint is only a recovery baseline.
    pub checkpoint_is_baseline_only: bool,
}

/// Safe capability data for one provider-neutral Runtime choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeLaunchCapability {
    /// Runtime choice accepted in [`TaskLaunchSpecV1`].
    pub runtime: TaskRuntime,
    /// Current operator-configured readiness.
    pub readiness: LaunchReadiness,
    /// Honest authority and governance posture.
    pub security: RuntimeSecurityPosture,
}

/// Safe launch capabilities returned by the local daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayCapabilities {
    /// Task launch contract version accepted by this daemon.
    pub launch_schema_version: u16,
    /// Only canonical workspace admitted by this catalog.
    pub default_workspace: WorkspaceRef,
    /// Closed Runtime choices in stable presentation order.
    pub runtimes: Vec<RuntimeLaunchCapability>,
    /// Current pre-Runtime checkpoint provider readiness.
    pub checkpoint: LaunchReadiness,
    /// Default approval policy for new launch surfaces.
    pub default_approval: ApprovalPolicy,
}

/// Sealed Runtime and checkpoint choices admitted by one Gateway instance.
///
/// Callers configure readiness only. Runtime selectors, capability profiles,
/// targets, and security posture remain fixed by this crate.
#[derive(Debug, Clone)]
pub struct TaskLaunchCatalog {
    default_workspace: WorkspaceRef,
    core: LaunchReadiness,
    codex: LaunchReadiness,
    checkpoint: LaunchReadiness,
    legacy_profiles: Vec<GatewayCapabilityProfile>,
}

impl TaskLaunchCatalog {
    /// Creates the closed built-in Core/Codex launch catalog.
    #[must_use]
    pub fn new(
        default_workspace: WorkspaceRef,
        core: LaunchReadiness,
        codex: LaunchReadiness,
        checkpoint: LaunchReadiness,
    ) -> Self {
        Self {
            default_workspace,
            core,
            codex,
            checkpoint,
            legacy_profiles: Vec::new(),
        }
    }

    fn for_legacy_profile(
        default_workspace: WorkspaceRef,
        profile: GatewayCapabilityProfile,
    ) -> Self {
        let unavailable = || {
            LaunchReadiness::Unavailable {
                reason: cosh_gateway_contracts::common::BoundedText::new(
                    "not configured by this legacy Gateway instance",
                )
                .unwrap_or_else(|_| unreachable!()),
            }
        };
        let mut catalog = Self::new(
            default_workspace,
            unavailable(),
            unavailable(),
            unavailable(),
        );
        catalog.legacy_profiles.push(profile);
        catalog
    }

    /// Returns a safe client-facing capability projection.
    #[must_use]
    pub fn capabilities(&self) -> GatewayCapabilities {
        GatewayCapabilities {
            launch_schema_version: TASK_LAUNCH_SPEC_V1,
            default_workspace: self.default_workspace.clone(),
            runtimes: vec![
                RuntimeLaunchCapability {
                    runtime: TaskRuntime::Core,
                    readiness: self.core.clone(),
                    security: RuntimeSecurityPosture {
                        delegated_local_authority: true,
                        gateway_brokered_effects: false,
                        checkpoint_is_baseline_only: false,
                    },
                },
                RuntimeLaunchCapability {
                    runtime: TaskRuntime::Codex,
                    readiness: self.codex.clone(),
                    security: RuntimeSecurityPosture {
                        delegated_local_authority: true,
                        gateway_brokered_effects: false,
                        checkpoint_is_baseline_only: false,
                    },
                },
            ],
            checkpoint: self.checkpoint.clone(),
            default_approval: ApprovalPolicy::AllowAll,
        }
    }

    fn admission(&self, launch: &TaskLaunchSpecV1) -> Result<TaskLaunchAdmission, GatewayDaemonError> {
        if launch.workspace != self.default_workspace {
            return Err(GatewayDaemonError::Protocol(
                "Task workspace is not admitted by this Gateway catalog".to_owned(),
            ));
        }
        let readiness = match launch.runtime {
            TaskRuntime::Core => &self.core,
            TaskRuntime::Codex => &self.codex,
        };
        if !readiness.is_ready() {
            return Err(GatewayDaemonError::Protocol(
                "selected Task Runtime is not ready".to_owned(),
            ));
        }
        if launch.checkpoint == CheckpointPolicy::On && !self.checkpoint.is_ready() {
            return Err(GatewayDaemonError::Protocol(
                "checkpoint policy On requires a ready checkpoint provider".to_owned(),
            ));
        }
        Ok(TaskLaunchAdmission::built_in(launch.runtime))
    }

    fn admits_identity(
        &self,
        runtime: &RuntimeSelector,
        target: &TargetRef,
        profile: &cosh_gateway_contracts::profile::GatewayCapabilityProfileIdentity,
    ) -> bool {
        [(TaskRuntime::Core, &self.core), (TaskRuntime::Codex, &self.codex)]
            .into_iter()
            .filter(|(_, readiness)| readiness.is_ready())
            .map(|(runtime, _)| TaskLaunchAdmission::built_in(runtime))
            .any(|entry| {
                entry.runtime == *runtime
                    && entry.target == *target
                    && entry.profile.identity() == *profile
            })
            || self.legacy_profiles.iter().any(|candidate| {
                candidate.identity() == *profile
                    && candidate.governed_target() == *target
                    && runtime_matches_capability_profile(*candidate, runtime)
            })
            // Recovery accepts only the repository's sealed historical profiles.
            // This does not widen ingress: legacy `Submit` uses `legacy_admission`,
            // which remains constrained by configured readiness.
            || [
                GatewayCapabilityProfile::task_only_v1(),
                GatewayCapabilityProfile::workspace_checkpoint_v1(),
                GatewayCapabilityProfile::workspace_write_v1(),
                GatewayCapabilityProfile::delegated_acp_v1(),
            ]
            .into_iter()
            .any(|candidate| {
                candidate.identity() == *profile
                    && candidate.governed_target() == *target
                    && runtime_matches_capability_profile(candidate, runtime)
            })
    }

    fn legacy_admission(
        &self,
        target: &TargetRef,
        runtime: &RuntimeSelector,
    ) -> Option<GatewayCapabilityProfile> {
        [
            (GatewayCapabilityProfile::task_only_v1(), &self.core),
            (GatewayCapabilityProfile::delegated_acp_v1(), &self.codex),
        ]
        .into_iter()
        .filter(|(_, readiness)| readiness.is_ready())
        .map(|(profile, _)| profile)
        .chain(self.legacy_profiles.iter().copied())
        .find(|profile| {
            profile.governed_target() == *target
                && runtime_matches_capability_profile(*profile, runtime)
        })
    }

    /// Returns the only canonical workspace admitted by this catalog.
    #[must_use]
    pub const fn default_workspace(&self) -> &WorkspaceRef {
        &self.default_workspace
    }
}

fn runtime_matches_capability_profile(
    profile: GatewayCapabilityProfile,
    runtime: &RuntimeSelector,
) -> bool {
    match profile.id() {
        cosh_gateway_contracts::profile::GatewayCapabilityProfileId::TaskOnlyV1 => {
            runtime.runtime.as_str() == "core"
                && runtime.profile.as_ref().map(BoundedName::as_str)
                    == Some("gateway-brokered-v1")
        }
        cosh_gateway_contracts::profile::GatewayCapabilityProfileId::WorkspaceCheckpointV1 => {
            runtime.runtime.as_str() == "core"
                && runtime.profile.as_ref().map(BoundedName::as_str)
                    == Some("gateway-checkpoint-v1")
        }
        cosh_gateway_contracts::profile::GatewayCapabilityProfileId::WorkspaceWriteV1 => {
            runtime.runtime.as_str() == "core"
                && runtime.profile.as_ref().map(BoundedName::as_str)
                    == Some("gateway-workspace-write-v1")
        }
        cosh_gateway_contracts::profile::GatewayCapabilityProfileId::DelegatedAcpV1 => {
            runtime.runtime.as_str() == "acp"
                && matches!(
                    runtime.profile.as_ref().map(BoundedName::as_str),
                    Some("codex" | "claude-code")
                )
        }
    }
}

#[derive(Debug, Clone)]
struct TaskLaunchAdmission {
    runtime: RuntimeSelector,
    target: TargetRef,
    profile: GatewayCapabilityProfile,
}

impl TaskLaunchAdmission {
    fn built_in(runtime: TaskRuntime) -> Self {
        let (runtime_name, runtime_profile, profile) = match runtime {
            TaskRuntime::Core => (
                "core",
                "gateway-workspace-write-v1",
                GatewayCapabilityProfile::workspace_write_v1(),
            ),
            TaskRuntime::Codex => (
                "acp",
                "codex",
                GatewayCapabilityProfile::delegated_acp_v1(),
            ),
        };
        Self {
            runtime: RuntimeSelector {
                runtime: BoundedName::new(runtime_name).unwrap_or_else(|_| unreachable!()),
                profile: Some(
                    BoundedName::new(runtime_profile).unwrap_or_else(|_| unreachable!()),
                ),
            },
            target: profile.governed_target(),
            profile,
        }
    }
}
