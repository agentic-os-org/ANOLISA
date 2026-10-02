use serde::{Deserialize, Serialize};

pub(crate) const RECOMMENDATION_SCHEMA_VERSION: u16 = 1;
pub(crate) const DISCLOSURE_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecommendationState {
    pub(crate) schema_version: u16,
    pub(crate) store_epoch: String,
    pub(crate) generation: u64,
    pub(crate) updated_hour_bucket: u64,
    pub(crate) preferences: RecommendationPreferences,
    pub(crate) journal: ActivityJournal,
    pub(crate) profile: UserWorkProfile,
    pub(crate) cache: RecommendationCache,
    pub(crate) feedback: Vec<RecommendationFeedbackState>,
    pub(crate) scheduler: AnalyzerSchedulerState,
}

impl RecommendationState {
    pub(crate) fn empty(store_epoch: String, updated_hour_bucket: u64) -> Self {
        Self {
            schema_version: RECOMMENDATION_SCHEMA_VERSION,
            store_epoch,
            generation: 0,
            updated_hour_bucket,
            preferences: RecommendationPreferences::default(),
            journal: ActivityJournal {
                records: Vec::new(),
                history_cursor: None,
                history_baseline_pending: true,
            },
            profile: UserWorkProfile::default(),
            cache: RecommendationCache::default(),
            feedback: Vec::new(),
            scheduler: AnalyzerSchedulerState::default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecommendationPreferences {
    pub(crate) user_enabled: Option<bool>,
    pub(crate) notice_version_seen: u16,
}

pub(crate) fn resolve_recommendations_enabled(
    environment: Option<bool>,
    user_enabled: Option<bool>,
    configured_default: bool,
) -> bool {
    if environment == Some(false) {
        return false;
    }
    user_enabled.unwrap_or_else(|| environment.unwrap_or(configured_default))
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AnalyzerSchedulerState {
    pub(crate) attempts: Vec<AnalyzerAttempt>,
    pub(crate) last_attempt_unix_secs: Option<u64>,
    pub(crate) lease: Option<AnalyzerLease>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AnalyzerAttempt {
    pub(crate) attempt_id: String,
    pub(crate) reserved_unix_secs: u64,
    pub(crate) phase: AttemptPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AttemptPhase {
    Reserved,
    BodyWriteStarted,
    BodySent,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AnalyzerLease {
    pub(crate) owner_session_id: String,
    pub(crate) lease_nonce: String,
    pub(crate) owner_pid: u32,
    pub(crate) owner_start_identity: String,
    pub(crate) core_leader_pid: Option<u32>,
    pub(crate) core_leader_start_identity: Option<String>,
    pub(crate) core_process_group_id: Option<u32>,
    pub(crate) base_epoch: String,
    pub(crate) base_generation: u64,
    pub(crate) expires_unix_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecommendationFeedbackState {
    pub(crate) task_ref: String,
    pub(crate) last_impression_hour_bucket: Option<u64>,
    pub(crate) last_submitted_hour_bucket: Option<u64>,
    pub(crate) consecutive_explicit_dismissals: u8,
    pub(crate) last_explicit_dismissal_hour_bucket: Option<u64>,
    pub(crate) consecutive_overrides: u8,
    pub(crate) last_override_hour_bucket: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivityJournal {
    pub(crate) records: Vec<ActivityRecord>,
    pub(crate) history_cursor: Option<HistoryCursor>,
    pub(crate) history_baseline_pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HistoryCursor {
    pub(crate) file_identity_hmac: String,
    pub(crate) last_entry_hmac: String,
    pub(crate) size_mtime_hmac: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivityRecord {
    pub(crate) activity_id: String,
    pub(crate) session_scope_id: Option<String>,
    pub(crate) source_fingerprint: String,
    pub(crate) observed_hour_bucket: u64,
    pub(crate) source: ActivitySource,
    pub(crate) context: ActivityContext,
    pub(crate) payload: ActivityPayload,
    pub(crate) redaction: RedactionReport,
    pub(crate) summarized_generation: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActivitySource {
    ShellCommand,
    AgentRequest,
    AgentRun,
    RecommendationFeedback,
    BashHistory,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivityContext {
    pub(crate) host_id: Option<String>,
    pub(crate) repo_id: Option<String>,
    pub(crate) repo_name: Option<String>,
    pub(crate) cwd_relative: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ActivityPayload {
    ShellCommand {
        command: String,
        origin: ShellActivityOrigin,
        parent_request_activity_id: Option<String>,
        outcome: ActivityOutcome,
    },
    AgentRequest {
        text: String,
        binding: AgentRequestBindingKind,
        context_command_activity_id: Option<String>,
        intent_lifecycle_id: String,
        system_recommended_skill: Option<String>,
    },
    AgentRun {
        request_activity_id: String,
        tool_categories: Vec<ToolCategory>,
        outcome: ActivityOutcome,
    },
    RecommendationFeedback {
        candidate_id: String,
        candidate_source: CandidateSource,
        task_ref: String,
        profile_generation: u64,
        intent_lifecycle_id: String,
        action: FeedbackAction,
        edit_bucket: Option<EditBucket>,
    },
    BashHistoryCommand {
        command: String,
        origin_unverified: bool,
        execution_hour_bucket: Option<u64>,
        time_unverified: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ShellActivityOrigin {
    #[serde(rename = "user_interactive")]
    Interactive,
    #[serde(rename = "user_send_to_shell")]
    SendToShell,
    #[serde(rename = "user_analysis_action")]
    AnalysisAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActivityOutcome {
    Success,
    Failure,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentRequestBindingKind {
    FreeForm,
    StartupHealthFollowUp,
    HookConsultation,
    FailedCommand,
    SelectedCommand,
    PromptRecommendation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolCategory {
    Shell,
    FilesystemRead,
    FilesystemWrite,
    Skill,
    ExternalService,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FeedbackAction {
    Impression,
    TabAccepted,
    Submitted,
    ExplicitDismissed,
    Overridden,
    Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EditBucket {
    None,
    Small,
    Large,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RedactionReport {
    pub(crate) replacements: Vec<RedactionKind>,
    pub(crate) truncated: bool,
    pub(crate) sanitizer_version: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RedactionKind {
    Authorization,
    Secret,
    Credential,
    PrivateKey,
    HomePath,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UserWorkProfile {
    pub(crate) summary_generation: u64,
    pub(crate) updated_hour_bucket: u64,
    pub(crate) evidence_snapshots: Vec<EvidenceSnapshot>,
    pub(crate) recent_tasks: Vec<RecentTask>,
    pub(crate) frequent_patterns: Vec<FrequentPattern>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CandidateSource {
    Health,
    RecentTask,
    FrequentPattern,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContextAffinity {
    pub(crate) scope_kind: ScopeKind,
    pub(crate) repo_id: Option<String>,
    pub(crate) host_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ScopeKind {
    #[default]
    HostFallback,
    Repo,
    HostWide,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskEntity {
    pub(crate) kind: EntityKind,
    pub(crate) value: String,
    pub(crate) volatility: EntityVolatility,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EntityKind {
    Namespace,
    Workload,
    Service,
    Repo,
    Branch,
    RelativePath,
    TestTarget,
    Process,
    Package,
    Host,
    Url,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EntityVolatility {
    Stable,
    Ephemeral,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EvidenceSnapshot {
    pub(crate) snapshot_id: String,
    pub(crate) source_kinds: Vec<ActivitySource>,
    pub(crate) first_seen_hour_bucket: u64,
    pub(crate) last_seen_hour_bucket: u64,
    pub(crate) active_day_buckets: Vec<u32>,
    pub(crate) context_affinity: ContextAffinity,
    pub(crate) entities: Vec<TaskEntity>,
    pub(crate) agent_request_count: u16,
    pub(crate) compatible_shell_count: u16,
    pub(crate) submitted_feedback_count: u16,
    pub(crate) intent_occurrence_count: u16,
    pub(crate) last_action_failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecentTask {
    pub(crate) task_id: String,
    pub(crate) summary: String,
    pub(crate) entities: Vec<TaskEntity>,
    pub(crate) context_affinity: ContextAffinity,
    pub(crate) last_seen_hour_bucket: u64,
    pub(crate) evidence_snapshot_ids: Vec<String>,
    pub(crate) prompt_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrequentPattern {
    pub(crate) pattern_id: String,
    pub(crate) summary: String,
    pub(crate) stable_entities: Vec<TaskEntity>,
    pub(crate) active_day_buckets: Vec<u32>,
    pub(crate) context_affinity: ContextAffinity,
    pub(crate) evidence_snapshot_ids: Vec<String>,
    pub(crate) prompt_text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecommendationCache {
    pub(crate) profile_generation: u64,
    pub(crate) generated_hour_bucket: u64,
    pub(crate) candidates: Vec<CachedPromptCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CachedPromptCandidate {
    pub(crate) candidate_id: String,
    pub(crate) source: CandidateSource,
    pub(crate) task_ref: String,
    pub(crate) prompt_text: String,
    pub(crate) context_affinity: ContextAffinity,
    pub(crate) last_seen_hour_bucket: u64,
    pub(crate) last_action_failed: bool,
    pub(crate) evidence: CandidateEvidenceSummary,
    pub(crate) entities: Vec<EntityEvidenceRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CandidateEvidenceSummary {
    pub(crate) snapshot_ids: Vec<String>,
    pub(crate) agent_request_count: u16,
    pub(crate) compatible_shell_count: u16,
    pub(crate) submitted_feedback_count: u16,
    pub(crate) intent_occurrence_count: u16,
    pub(crate) active_day_buckets: Vec<u32>,
    pub(crate) continuation_evidence: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EntityEvidenceRef {
    pub(crate) entity: TaskEntity,
    pub(crate) snapshot_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProfileAnalyzerResult {
    pub(crate) discarded_activities: Vec<DiscardedActivity>,
    pub(crate) recent_tasks: Vec<AnalyzedRecentTask>,
    pub(crate) frequent_patterns: Vec<AnalyzedFrequentPattern>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiscardedActivity {
    pub(crate) activity_id: String,
    pub(crate) reason: DiscardReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DiscardReason {
    NoRecommendationValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AnalyzedRecentTask {
    pub(crate) prior_task_id: Option<String>,
    pub(crate) summary: String,
    pub(crate) entities: Vec<TaskEntity>,
    pub(crate) evidence_activity_ids: Vec<String>,
    pub(crate) prior_snapshot_ids: Vec<String>,
    pub(crate) prompt_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AnalyzedFrequentPattern {
    pub(crate) prior_pattern_id: Option<String>,
    pub(crate) summary: String,
    pub(crate) stable_entities: Vec<TaskEntity>,
    pub(crate) evidence_activity_ids: Vec<String>,
    pub(crate) prior_snapshot_ids: Vec<String>,
    pub(crate) prompt_text: String,
}
