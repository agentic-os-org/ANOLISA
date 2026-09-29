#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssessmentSource {
    ProviderShellTool,
    ProviderNativeNonShellTool,
    LocalAgentAction,
    HookSuggestedAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionDecision {
    AutoAllow,
    AskUser,
    Block,
    ForegroundHandoffRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskImpact {
    Low,
    Medium,
    High,
}

impl RiskImpact {
    pub fn legacy_risk(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssessmentConfidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionRequirement {
    None,
    TtyRequired,
    CredentialPromptLikely,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStability {
    StableSnapshot,
    PotentiallyLarge,
    Streaming,
    UnstableInteractive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputExposure {
    Normal,
    MayContainCommandLine,
    MayContainEnvironment,
    MayContainSecrets,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SideEffectClass {
    None,
    FilesystemWrite,
    FilesystemDelete,
    PermissionChange,
    ProcessControl,
    /// Whole-machine irrecoverable operations (reboot/shutdown/halt):
    /// stricter than ServiceControl because the blast radius is the
    /// entire host — SSH sessions drop and unsaved work is lost.
    SystemControl,
    ServiceControl,
    PackageInstall,
    NetworkRead,
    NetworkWrite,
    RemoteCodeExecution,
    CredentialAccess,
    SensitiveDataRead,
    PrivilegeEscalation,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoAllowEvidence {
    DirectReadonlyBroker,
    GuardedDiagnostic,
    ReadonlyPipelineExecutor,
    /// A readonly compound execution plan exists (issue #1882): every
    /// segment's tokens pass the direct readonly gate as-is, and the
    /// plan is executed by the dedicated argv executor — no shell
    /// parsing layer, no text rebuild, no handoff.
    CompoundReadonly,
    /// A single readonly argv step with stderr connected to the null device.
    StderrSuppressedReadonly,
}

impl AutoAllowEvidence {
    pub fn reason_code(self) -> &'static str {
        match self {
            Self::DirectReadonlyBroker => "bounded-readonly",
            Self::GuardedDiagnostic => "safe-diagnostic-family",
            Self::ReadonlyPipelineExecutor => "readonly-pipeline-executor",
            Self::CompoundReadonly => "compound-readonly",
            Self::StderrSuppressedReadonly => "stderr-suppressed-readonly",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadonlyEvidence {
    DirectReadonlyBroker,
}

impl ReadonlyEvidence {
    pub fn auto_allow(self) -> AutoAllowEvidence {
        match self {
            Self::DirectReadonlyBroker => AutoAllowEvidence::DirectReadonlyBroker,
        }
    }

    pub fn reason_code(self) -> &'static str {
        self.auto_allow().reason_code()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandShape {
    Empty,
    Simple,
    EnvSimple,
    Pipeline,
    AndOrList,
    Sequence,
    RedirectionRead,
    RedirectionWrite,
    CommandSubstitution,
    Complex,
    Unparseable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandAssessment {
    pub source: AssessmentSource,
    pub command: String,
    pub shape: CommandShape,
    pub execution: ExecutionDecision,
    pub impact: RiskImpact,
    pub confidence: AssessmentConfidence,
    pub interaction: InteractionRequirement,
    pub output_stability: OutputStability,
    pub output_exposure: OutputExposure,
    pub side_effects: Vec<SideEffectClass>,
    pub reasons: Vec<&'static str>,
    pub auto_allow: Option<AutoAllowEvidence>,
}

pub type RiskReason = &'static str;

/// Card-facing high-risk explanation whitelist (ARP SDD design.md §4).
/// Shared by the reason ordering rule in `assess_shell_command` and the
/// approval-card phrase policy in the UI layer so the two cannot drift.
pub const HIGH_RISK_EXPLANATION_REASONS: &[&str] = &[
    "privilege-escalation",
    "credential-access",
    "filesystem-delete",
    "filesystem-write",
    "permission-change",
    "process-control",
    "system-control",
    "service-control",
    "service-or-container-control",
    "package-manager-mutation",
    "interactive-editor",
    "remote-code-execution",
    "sensitive-path",
    "sensitive-search",
    "command-substitution",
    "redirection-write",
    "awk-shell-execution",
];

pub fn is_high_risk_explanation(code: &str) -> bool {
    HIGH_RISK_EXPLANATION_REASONS.contains(&code)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssessmentSummary {
    pub impact: RiskImpact,
    pub execution: ExecutionDecision,
    pub confidence: AssessmentConfidence,
    pub primary_reason: RiskReason,
    pub auto_allow: Option<AutoAllowEvidence>,
}

impl CommandAssessment {
    pub fn primary_reason(&self) -> &'static str {
        self.reasons.first().copied().unwrap_or("unknown-command")
    }

    pub fn summary(&self) -> AssessmentSummary {
        AssessmentSummary {
            impact: self.impact,
            execution: self.execution,
            confidence: self.confidence,
            primary_reason: self.primary_reason(),
            auto_allow: self.auto_allow,
        }
    }

    pub fn reason_trace(&self) -> String {
        self.reasons.join(",")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssessmentPolicy {
    pub source: AssessmentSource,
    pub auto_mode: bool,
    pub guarded_diagnostic_executor: bool,
    pub readonly_pipeline_executor: bool,
}

impl AssessmentPolicy {
    pub fn ask(source: AssessmentSource) -> Self {
        Self {
            source,
            auto_mode: false,
            guarded_diagnostic_executor: false,
            readonly_pipeline_executor: false,
        }
    }

    pub fn auto_with_guarded_diagnostics(source: AssessmentSource) -> Self {
        Self {
            source,
            auto_mode: true,
            guarded_diagnostic_executor: true,
            readonly_pipeline_executor: false,
        }
    }

    pub fn auto_direct_readonly(source: AssessmentSource) -> Self {
        Self {
            source,
            auto_mode: true,
            guarded_diagnostic_executor: false,
            readonly_pipeline_executor: false,
        }
    }

    pub fn auto_with_readonly_pipeline(source: AssessmentSource) -> Self {
        Self {
            source,
            auto_mode: true,
            guarded_diagnostic_executor: false,
            readonly_pipeline_executor: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoExecutionPolicy {
    pub guarded_diagnostic_executor: bool,
    pub readonly_pipeline_executor: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoExecutionRoute {
    AskUser,
    DirectReadonlyBroker,
    GuardedDiagnosticExecutor,
    ReadonlyPipelineExecutor,
    /// Auto-approves a fully readonly compound command and executes it
    /// through the dedicated readonly compound executor (issue #1882):
    /// parser tokens are spawned directly with `std::process::Command`,
    /// so no shell parsing layer ever touches the assessed text.
    CompoundReadonlyExecutor,
    Block,
}

impl AutoExecutionPolicy {
    pub fn current_runtime() -> Self {
        Self {
            guarded_diagnostic_executor: false,
            readonly_pipeline_executor: false,
        }
    }

    pub fn assessment_policy(self, source: AssessmentSource) -> AssessmentPolicy {
        AssessmentPolicy {
            source,
            auto_mode: true,
            guarded_diagnostic_executor: self.guarded_diagnostic_executor,
            readonly_pipeline_executor: self.readonly_pipeline_executor,
        }
    }

    pub fn route(self, assessment: &CommandAssessment) -> AutoExecutionRoute {
        if assessment.execution == ExecutionDecision::Block {
            return AutoExecutionRoute::Block;
        }
        match assessment.auto_allow {
            Some(AutoAllowEvidence::DirectReadonlyBroker) => {
                AutoExecutionRoute::DirectReadonlyBroker
            }
            // Always live, like DirectReadonlyBroker: the executor
            // reuses the readonly pipeline primitives, so there is no
            // separate mechanism to gate behind a runtime flag
            // (issue #1882).
            // The same argv executor also handles one stderr-suppressed step.
            Some(
                AutoAllowEvidence::CompoundReadonly | AutoAllowEvidence::StderrSuppressedReadonly,
            ) => AutoExecutionRoute::CompoundReadonlyExecutor,
            Some(AutoAllowEvidence::GuardedDiagnostic) if self.guarded_diagnostic_executor => {
                AutoExecutionRoute::GuardedDiagnosticExecutor
            }
            Some(AutoAllowEvidence::ReadonlyPipelineExecutor)
                if self.readonly_pipeline_executor =>
            {
                AutoExecutionRoute::ReadonlyPipelineExecutor
            }
            _ => AutoExecutionRoute::AskUser,
        }
    }
}
