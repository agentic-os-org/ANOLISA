use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::activity::runtime::{RuntimeActivityRow, ToolInvocationRecord};
use crate::agent::run::{ActiveAgentRun, AgentRunOrigin, PendingAgentRequest};
use crate::agent::turn_extension::PendingTurnExtension;
use crate::diagnostics::health::HealthScanReport;
use crate::hooks::state::HookRuntimeState;
use crate::insight::correlation::InsightCorrelationState;
use crate::insight::model::{InsightBinding, InsightCandidate};
use crate::insight::policy::InterruptionBudget;
use crate::insight::shell_rewrite::ShellRewriteCatalogService;
use crate::question::runtime::RuntimeUserQuestion;
use crate::raw_input::PromptGhostRoute;
use crate::recommendation::personal_feedback::FrozenPromptBinding;
use crate::recommendation::personal_state::PersonalizationState;
use crate::runtime::approval_ledger::ApprovalLifecycleLedger;
use crate::runtime::approval_state::ApprovalState;
use crate::runtime::events::ShellEventCursor;
use crate::runtime::evidence_requests::EvidenceRequestState;
use crate::runtime::evidence_state::EvidenceState;
use crate::runtime::provider_cancellation_artifacts::ProviderCancellationArtifactState;
use crate::runtime::provider_tool_state::ProviderToolState;
use crate::runtime::shell_handoff_state::ShellHandoffState;
pub(crate) use crate::runtime::state_prelude::CoshApprovalMode;
use crate::runtime::state_prelude::{
    first_program_token, CommandBlock, GovernedEvent, I18n, Language,
};
use crate::runtime::trust_state::ApprovalTrustState;
use crate::slash::session::SessionControlState;
use crate::types::{AgentContextBinding, ShellEvent, ShellEventKind};

pub(crate) struct AnalysisThrottle {
    recent: HashMap<String, (Instant, usize)>,
    cooldown_secs: u64,
}

impl Default for AnalysisThrottle {
    fn default() -> Self {
        Self {
            recent: HashMap::new(),
            cooldown_secs: 30,
        }
    }
}

impl AnalysisThrottle {
    pub(crate) fn should_throttle(&mut self, command: &str) -> bool {
        self.should_throttle_at(command, Instant::now())
    }
    pub(crate) fn should_throttle_at(&mut self, command: &str, now: Instant) -> bool {
        let key = normalize_command(command);
        if let Some((window_started, count)) = self.recent.get_mut(&key) {
            if now.duration_since(*window_started).as_secs() < self.cooldown_secs {
                *count += 1;
                return true;
            }
        }
        self.recent.insert(key, (now, 1));
        false
    }
}

fn normalize_command(cmd: &str) -> String {
    first_program_token(cmd).to_string()
}

#[derive(Default)]
pub(crate) struct InlineState {
    pub(crate) analyzed_blocks: HashSet<String>,
    pub(crate) queued_analysis_notices: HashSet<String>,
    pub(crate) canceled_blocks: HashSet<String>,
    pub(crate) evaluated_failed_command_insights: HashSet<String>,
    pub(crate) rendered_startup_banner: bool,
    pub(crate) handled_intercepts: HashSet<String>,
    pub(crate) hooks: HookRuntimeState,
    pub(crate) insight_correlation: InsightCorrelationState,
    pub(crate) insight_budget: InterruptionBudget,
    pub(crate) pending_command_insight: Option<InsightCandidate>,
    pub(crate) shell_rewrite: ShellRewriteCatalogService,
    pub(crate) handled_confirmations: HashSet<String>,
    pub(crate) handled_cancellations: HashSet<String>,
    pub(crate) handled_cancel_requests: HashSet<String>,
    pub(crate) handled_slash_commands: HashSet<String>,
    pub(crate) handled_details_actions: HashSet<String>,
    pub(crate) handled_selections: HashSet<String>,
    pub(crate) approvals: ApprovalState,
    pub(crate) auth: crate::auth::runtime::AuthState,
    pub(crate) task_form: crate::slash::TaskFormState,
    pub(crate) task_snapshot: crate::slash::TaskSnapshotState,
    pub(crate) questions: QuestionState,
    pub(crate) control: ControlState,
    pub(crate) activity: ActivityState,
    pub(crate) agent_run: AgentRunState,
    pub(crate) provider_cancellation_artifacts: ProviderCancellationArtifactState,
    pub(crate) evidence: EvidenceState,
    pub(crate) evidence_requests: EvidenceRequestState,
    pub(crate) shell_evidence: ShellEvidenceState,
    pub(crate) session_blocks: Vec<CommandBlock>,
    /// Whether any shell command activity (started/completed/failed
    /// marker events) was observed this session, including activity
    /// that produced ledger errors instead of command blocks (R9).
    pub(crate) shell_command_activity_observed: bool,
    /// The shell's own latest working-directory report from a prompt
    /// marker (a precmd with no command in flight): positive evidence
    /// of where the shell sits, refreshed at every command-less
    /// prompt and invalidated by any PTY input write (the input may
    /// submit a cwd-changing line through a binding the byte-stream
    /// heuristic cannot see, while its markers are lost). `None`
    /// until the marker channel has proven itself — a session without
    /// any marker traffic never gets a value here.
    pub(crate) shell_prompt_cwd: Option<String>,
    pub(crate) shell_session_id: Option<String>,
    pub(crate) shell_exited: bool,
    pub(crate) language: Language,
    pub(crate) approval_mode: CoshApprovalMode,
    pub(crate) analysis_mode: AnalysisMode,
    pub(crate) assistance_control: Option<crate::input::AssistanceControl>,
    pub(crate) debug: bool,
    pub(crate) analysis_throttle: AnalysisThrottle,
    pub(crate) trigger_pty_prompt: bool,
    pub(crate) pending_input_ghost: Option<String>,
    pub(crate) pending_input_ghost_route: PromptGhostRoute,
    pub(crate) pending_input_ghost_binding: Option<PendingInputGhostBinding>,
    pub(crate) pending_prompt_suggestion_bindings: HashMap<String, PendingInputGhostBinding>,
    pub(crate) shown_shell_rewrite_guidance: bool,
    pub(crate) shown_agent_prompt_guidance: bool,
    /// Multi-line prompt entry discoverability (#1721 tip, #1932 hint).
    pub(crate) prompt_entry_hints: crate::runtime::prompt_draft::PromptEntryHints,
    /// #1721 D13: active multi-line prompt draft card, if any.
    pub(crate) prompt_draft: Option<crate::runtime::prompt_draft::PromptDraftCardState>,
    pub(crate) prompt_draft_seq: u64,
    /// Submitted `/agent` card awaiting its paired intercept event.
    pub(crate) pending_agent_composer_submission:
        Option<crate::runtime::prompt_draft::PendingAgentComposerSubmission>,
    pub(crate) pending_shell_handoff_timeout_notice: Option<Duration>,
    /// #2161: shared clock written by the relay's interactive sentinel;
    /// read here to drive the input-wait interrupt.
    pub(crate) input_wait_status: crate::shell_host::InputWaitStatus,
    /// #2161: `shell.input_wait_timeout_secs` (None/0 = disabled).
    pub(crate) input_wait_timeout: Option<Duration>,
    /// #2161: waited duration captured at interrupt time, rendered as a
    /// notice panel once the foreground is idle again.
    pub(crate) pending_input_wait_timeout_notice: Option<Duration>,
    /// #2161: per-approval input-wait facts (max waited + interrupted),
    /// consumed into the host_executed_shell result on delivery.
    pub(crate) input_wait_facts: HashMap<String, crate::adapter::HostExecutedInputWait>,
    pub(crate) continuity: ContinuityState,
    pub(crate) startup_health: StartupHealthState,
    pub(crate) startup_auth: StartupAuthState,
    pub(crate) personalization: PersonalizationState,
    pub(crate) audit: Option<crate::journal::audit::ShellAuditRecorder>,
}

#[derive(Clone)]
pub(crate) enum PendingInputGhostBinding {
    Health(AgentContextBinding),
    Insight(Box<InsightBinding>),
    Personal(FrozenPromptBinding),
}

#[derive(Default)]
pub(crate) struct StartupHealthState {
    pub(crate) pending: Option<mpsc::Receiver<Option<HealthScanReport>>>,
    pub(crate) report: Option<HealthScanReport>,
    pub(crate) rendered: bool,
}

impl StartupHealthState {
    pub(crate) fn wait_ready(&mut self, timeout: Duration) {
        if self.report.is_some() || self.rendered {
            return;
        }
        let Some(receiver) = &self.pending else {
            return;
        };
        match receiver.recv_timeout(timeout) {
            Ok(report) => {
                self.report = report;
                self.pending = None;
                if self.report.is_none() {
                    self.rendered = true;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.pending = None;
                self.rendered = true;
            }
        }
    }

    pub(crate) fn poll_ready(&mut self) {
        if self.report.is_some() || self.rendered {
            return;
        }
        let Some(receiver) = &self.pending else {
            return;
        };
        match receiver.try_recv() {
            Ok(report) => {
                self.report = report;
                self.pending = None;
                if self.report.is_none() {
                    self.rendered = true;
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.pending = None;
                self.rendered = true;
            }
        }
    }
}

/// Background probe of the effective AI credential state, resolved via
/// cosh-core at bootstrap so the startup banner can hint `/auth` without
/// blocking first paint. Fail-quiet: an error, timeout, or missing probe
/// leaves `resolved` at `None` and nothing is shown.
///
/// Lifecycle: the `Default` state (no probe, no verdict) is the safe
/// state — every consumer treats it as "not unconfigured", so an
/// `InlineState` built without bootstrap wiring can never trigger the
/// hint. Bootstrap installs `pending` only for the CoshCore adapter
/// with AI enabled and the banner on; a successful `/auth` credential
/// change resets the state to `Default` so no stale verdict outlives
/// the credentials it described.
#[derive(Default)]
pub(crate) struct StartupAuthState {
    pub(crate) pending: Option<mpsc::Receiver<Option<bool>>>,
    pub(crate) resolved: Option<bool>,
}

impl StartupAuthState {
    pub(crate) fn wait_ready(&mut self, timeout: Duration) {
        if self.resolved.is_some() {
            return;
        }
        let Some(receiver) = &self.pending else {
            return;
        };
        match receiver.recv_timeout(timeout) {
            Ok(resolved) => {
                self.resolved = resolved;
                self.pending = None;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.pending = None;
            }
        }
    }

    pub(crate) fn poll_ready(&mut self) {
        if self.resolved.is_some() {
            return;
        }
        let Some(receiver) = &self.pending else {
            return;
        };
        match receiver.try_recv() {
            Ok(resolved) => {
                self.resolved = resolved;
                self.pending = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.pending = None;
            }
        }
    }

    /// True only when the authority explicitly reported "no usable
    /// credentials"; uncertainty never shows the hint.
    pub(crate) fn ai_unconfigured(&self) -> bool {
        self.resolved == Some(false)
    }
}

#[derive(Default)]
pub(crate) struct ShellEvidenceState {
    pub(crate) last_action: Option<ShellEvidenceActionRecord>,
    recent_shell_tool_outputs: VecDeque<RecentShellToolOutput>,
    recent_action_signatures: VecDeque<ShellEvidenceActionSignature>,
}

#[derive(Debug, Clone)]
pub(crate) struct ShellEvidenceActionRecord {
    pub(crate) mode: &'static str,
    pub(crate) request_id: String,
    pub(crate) tool_use_id: String,
    pub(crate) action: String,
    pub(crate) output_id: Option<String>,
    pub(crate) status: String,
    pub(crate) failure_reason: Option<String>,
}

pub(crate) const RECENT_SHELL_TOOL_OUTPUT_WINDOW: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecentShellToolOutput {
    pub(crate) output_id: String,
    pub(crate) run_id: Option<String>,
    pub(crate) coverage: EvidenceCoverage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShellEvidenceActionSignature {
    run_id: String,
    signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EvidenceCoverage {
    Summary { complete: bool },
    Excerpt { direction: String, lines: u16 },
}

impl ShellEvidenceState {
    pub(crate) fn clear_recent_shell_tool_outputs(&mut self) {
        self.recent_shell_tool_outputs.clear();
    }

    pub(crate) fn record_host_executed_shell_output(
        &mut self,
        output_id: String,
        run_id: Option<String>,
        summary_complete: bool,
    ) {
        self.push_recent_shell_tool_output(
            output_id,
            run_id,
            EvidenceCoverage::Summary {
                complete: summary_complete,
            },
        );
    }

    pub(crate) fn record_shell_evidence_read_output(
        &mut self,
        output_id: String,
        run_id: Option<String>,
        direction: String,
        lines: u16,
    ) {
        self.push_recent_shell_tool_output(
            output_id,
            run_id,
            EvidenceCoverage::Excerpt { direction, lines },
        );
    }

    pub(crate) fn read_output_recently_delivered(
        &self,
        output_id: &str,
        run_id: Option<&str>,
        direction: &str,
        lines: u16,
    ) -> bool {
        self.recent_shell_tool_outputs.iter().rev().any(|record| {
            record.output_id == output_id
                && record
                    .run_id
                    .as_deref()
                    .is_none_or(|record_run_id| Some(record_run_id) == run_id)
                && record.coverage.covers(direction, lines)
        })
    }

    pub(crate) fn read_output_excerpt_recently_delivered(
        &self,
        output_id: &str,
        run_id: Option<&str>,
        direction: &str,
        lines: u16,
    ) -> bool {
        self.recent_shell_tool_outputs.iter().rev().any(|record| {
            record.output_id == output_id
                && record
                    .run_id
                    .as_deref()
                    .is_none_or(|record_run_id| Some(record_run_id) == run_id)
                && matches!(record.coverage, EvidenceCoverage::Excerpt { .. })
                && record.coverage.covers(direction, lines)
        })
    }

    pub(crate) fn record_action_signature(&mut self, run_id: &str, signature: String) -> bool {
        let duplicate = self
            .recent_action_signatures
            .iter()
            .rev()
            .any(|record| record.run_id == run_id && record.signature == signature);
        self.recent_action_signatures
            .push_back(ShellEvidenceActionSignature {
                run_id: run_id.to_string(),
                signature,
            });
        while self.recent_action_signatures.len() > RECENT_SHELL_TOOL_OUTPUT_WINDOW * 2 {
            self.recent_action_signatures.pop_front();
        }
        duplicate
    }

    fn push_recent_shell_tool_output(
        &mut self,
        output_id: String,
        run_id: Option<String>,
        coverage: EvidenceCoverage,
    ) {
        self.recent_shell_tool_outputs
            .push_back(RecentShellToolOutput {
                output_id,
                run_id,
                coverage,
            });
        while self.recent_shell_tool_outputs.len() > RECENT_SHELL_TOOL_OUTPUT_WINDOW {
            self.recent_shell_tool_outputs.pop_front();
        }
    }
}

impl EvidenceCoverage {
    fn covers(&self, direction: &str, lines: u16) -> bool {
        match self {
            Self::Summary { complete } => *complete,
            Self::Excerpt {
                direction: delivered_direction,
                lines: delivered_lines,
            } => delivered_direction == direction && *delivered_lines >= lines,
        }
    }
}

#[cfg(test)]
#[path = "state_shell_evidence_tests.rs"]
mod shell_evidence_recent_tests;

#[derive(Default)]
pub(crate) struct AgentRunState {
    pub(crate) active: Option<ActiveAgentRun>,
    pub(crate) queued_requests: VecDeque<PendingAgentRequest>,
    pub(crate) pending_turn_extension: Option<PendingTurnExtension>,
    pub(crate) held_events: Vec<GovernedEvent>,
    pub(crate) needs_prompt_after_run: bool,
    pub(crate) native_prompt_after_run: bool,
    pub(crate) host_executed_shell_result_delivered: bool,
}

impl AgentRunState {
    pub(crate) fn queue_request(&mut self, pending: PendingAgentRequest) {
        if !pending.before_held_text {
            self.queued_requests.push_back(pending);
            return;
        }

        let insert_at = self
            .queued_requests
            .iter()
            .position(|queued| !queued.before_held_text)
            .unwrap_or(self.queued_requests.len());
        self.queued_requests.insert(insert_at, pending);
    }
}

#[derive(Default)]
pub(crate) struct ActivityState {
    pub(crate) rows: Vec<RuntimeActivityRow>,
    pub(crate) tool_invocations: Vec<ToolInvocationRecord>,
    pub(crate) output_dir: Option<PathBuf>,
}

#[derive(Default)]
pub(crate) struct QuestionState {
    pub(crate) items: Vec<RuntimeUserQuestion>,
    pub(crate) pending_id: Option<String>,
    pub(crate) active_panel_id: Option<String>,
    pub(crate) active_panel_height: usize,
    pub(crate) active_panel_cursor_row: Option<usize>,
    pub(crate) active_panel_width: Option<u16>,
    pub(crate) handled_focus: HashSet<String>,
    pub(crate) handled_answers: HashSet<String>,
    pub(crate) handled_cancellations: HashSet<String>,
    pub(crate) question_protocol_failure_reported: bool,
}

#[derive(Default)]
pub(crate) struct ControlState {
    pending_mode_panel: Option<RuntimeModePanel>,
    active_mode_panel_id: Option<String>,
    active_mode_panel_height: usize,
    handled_mode_actions: HashSet<String>,
    pending_config_panel: Option<RuntimeConfigPanel>,
    active_config_panel_id: Option<String>,
    active_config_panel_height: usize,
    pending_config_language_panel: Option<RuntimeConfigLanguagePanel>,
    active_config_language_panel_id: Option<String>,
    active_config_language_panel_height: usize,
    handled_config_actions: HashSet<String>,
    session: SessionControlState,
    provider_tool: ProviderToolState,
    approval_ledger: ApprovalLifecycleLedger,
    provider_shell_handoff_run_ids: HashSet<String>,
    interactive_shell_handoffs: Vec<PendingInteractiveShellHandoff>,
    shell_handoff: ShellHandoffState,
    selectable_commands: Vec<String>,
    selectable_after_event_index: Option<usize>,
    pub(crate) trust: ApprovalTrustState,
    event_cursor: ShellEventCursor,
    active_shell_command_ids: HashSet<String>,
    #[cfg(test)]
    ledger_rebuild_count: usize,
}

impl ControlState {
    pub(crate) fn set_pending_mode_panel(
        &mut self,
        kind: RuntimeModePanelKind,
        selected_option: usize,
    ) {
        self.pending_mode_panel = Some(RuntimeModePanel {
            id: format!(
                "{}-mode-{}",
                kind.id_prefix(),
                self.handled_mode_actions.len() + 1
            ),
            kind,
            selected_option,
        });
    }
    pub(crate) fn pending_mode_panel(&self) -> Option<&RuntimeModePanel> {
        self.pending_mode_panel.as_ref()
    }
    pub(crate) fn pending_mode_panel_mut(&mut self) -> Option<&mut RuntimeModePanel> {
        self.pending_mode_panel.as_mut()
    }
    pub(crate) fn clear_pending_mode_panel(&mut self) {
        self.pending_mode_panel = None;
    }
    pub(crate) fn claim_mode_action(&mut self, key: String) -> bool {
        self.handled_mode_actions.insert(key)
    }
    pub(crate) fn active_mode_panel_id(&self) -> Option<&str> {
        self.active_mode_panel_id.as_deref()
    }
    pub(crate) fn set_active_mode_panel(&mut self, id: String, height: usize) {
        self.active_mode_panel_id = Some(id);
        self.active_mode_panel_height = height;
    }
    pub(crate) fn active_mode_panel_height(&self) -> usize {
        self.active_mode_panel_height
    }
    pub(crate) fn clear_active_mode_panel(&mut self) {
        self.active_mode_panel_id = None;
        self.active_mode_panel_height = 0;
    }
    pub(crate) fn clear_active_mode_panel_id(&mut self) {
        self.active_mode_panel_id = None;
    }
    pub(crate) fn set_pending_config_panel(&mut self, panel: RuntimeConfigPanel) {
        self.pending_config_panel = Some(panel);
    }
    pub(crate) fn new_config_panel_id(&self) -> String {
        format!("config-{}", self.handled_config_actions.len() + 1)
    }
    pub(crate) fn pending_config_panel(&self) -> Option<&RuntimeConfigPanel> {
        self.pending_config_panel.as_ref()
    }
    pub(crate) fn pending_config_panel_mut(&mut self) -> Option<&mut RuntimeConfigPanel> {
        self.pending_config_panel.as_mut()
    }
    pub(crate) fn clear_pending_config_panel(&mut self) {
        self.pending_config_panel = None;
    }
    pub(crate) fn set_pending_config_language_panel(&mut self, selected_option: usize) {
        self.pending_config_language_panel = Some(RuntimeConfigLanguagePanel {
            id: format!("config-language-{}", self.handled_config_actions.len() + 1),
            selected_option,
        });
    }
    pub(crate) fn pending_config_language_panel(&self) -> Option<&RuntimeConfigLanguagePanel> {
        self.pending_config_language_panel.as_ref()
    }
    pub(crate) fn pending_config_language_panel_mut(
        &mut self,
    ) -> Option<&mut RuntimeConfigLanguagePanel> {
        self.pending_config_language_panel.as_mut()
    }
    pub(crate) fn clear_pending_config_language_panel(&mut self) {
        self.pending_config_language_panel = None;
    }
    pub(crate) fn claim_config_action(&mut self, key: String) -> bool {
        self.handled_config_actions.insert(key)
    }
    pub(crate) fn active_config_panel_id(&self) -> Option<&str> {
        self.active_config_panel_id.as_deref()
    }
    pub(crate) fn set_active_config_panel(&mut self, id: String, height: usize) {
        self.active_config_panel_id = Some(id);
        self.active_config_panel_height = height;
    }
    pub(crate) fn active_config_panel_height(&self) -> usize {
        self.active_config_panel_height
    }
    pub(crate) fn clear_active_config_panel(&mut self) {
        self.active_config_panel_id = None;
        self.active_config_panel_height = 0;
    }
    pub(crate) fn clear_active_config_panel_id(&mut self) {
        self.active_config_panel_id = None;
    }
    pub(crate) fn active_config_language_panel_id(&self) -> Option<&str> {
        self.active_config_language_panel_id.as_deref()
    }
    pub(crate) fn set_active_config_language_panel(&mut self, id: String, height: usize) {
        self.active_config_language_panel_id = Some(id);
        self.active_config_language_panel_height = height;
    }
    pub(crate) fn active_config_language_panel_height(&self) -> usize {
        self.active_config_language_panel_height
    }
    pub(crate) fn clear_active_config_language_panel(&mut self) {
        self.active_config_language_panel_id = None;
        self.active_config_language_panel_height = 0;
    }
    pub(crate) fn clear_active_config_language_panel_id(&mut self) {
        self.active_config_language_panel_id = None;
    }
    pub(crate) fn session(&self) -> &SessionControlState {
        &self.session
    }
    pub(crate) fn session_mut(&mut self) -> &mut SessionControlState {
        &mut self.session
    }
    pub(crate) fn provider_tool(&self) -> &ProviderToolState {
        &self.provider_tool
    }
    pub(crate) fn provider_tool_mut(&mut self) -> &mut ProviderToolState {
        &mut self.provider_tool
    }
    pub(crate) fn approval_ledger(&self) -> &ApprovalLifecycleLedger {
        &self.approval_ledger
    }
    pub(crate) fn approval_ledger_mut(&mut self) -> &mut ApprovalLifecycleLedger {
        &mut self.approval_ledger
    }
    pub(crate) fn provider_host_executed_shell_result_delivered(
        &self,
        run_id: &str,
        request_id: &str,
        tool_use_id: Option<&str>,
    ) -> bool {
        self.provider_tool
            .host_executed_shell_result_delivered(run_id, request_id, tool_use_id)
    }
    pub(crate) fn claim_provider_shell_transcript_command(
        &mut self,
        run_id: &str,
        tool_id: &str,
    ) -> bool {
        self.provider_tool
            .claim_shell_transcript_command(run_id, tool_id)
    }
    pub(crate) fn mark_provider_shell_transcript_output(&mut self, run_id: &str, tool_id: &str) {
        self.provider_tool
            .mark_shell_transcript_output(run_id, tool_id);
    }
    pub(crate) fn mark_provider_shell_transcript_seen(&mut self, run_id: &str, tool_id: &str) {
        self.provider_tool
            .mark_shell_transcript_seen(run_id, tool_id);
    }
    pub(crate) fn mark_provider_hook_blocked_result(&mut self, run_id: &str, tool_id: &str) {
        self.provider_tool.mark_hook_blocked_result(run_id, tool_id);
    }
    pub(crate) fn provider_hook_result_is_blocked(&self, run_id: &str, tool_id: &str) -> bool {
        self.provider_tool.hook_result_is_blocked(run_id, tool_id)
    }
    pub(crate) fn provider_shell_transcript_output_seen(
        &self,
        run_id: &str,
        tool_id: &str,
    ) -> bool {
        self.provider_tool
            .shell_transcript_output_seen(run_id, tool_id)
    }
    pub(crate) fn provider_shell_transcript_seen(&self, run_id: &str, tool_id: &str) -> bool {
        self.provider_tool.shell_transcript_seen(run_id, tool_id)
    }
    pub(crate) fn mark_provider_foreground_shell_command(&mut self, command: &str) -> bool {
        self.provider_tool.mark_foreground_shell_command(command)
    }
    pub(crate) fn provider_foreground_shell_command_seen(&self, command: &str) -> bool {
        self.provider_tool.foreground_shell_command_seen(command)
    }
    pub(crate) fn provider_tool_is_shell(&self, run_id: &str, tool_id: &str) -> bool {
        self.provider_tool.is_shell_tool(run_id, tool_id)
    }
    pub(crate) fn provider_tool_is_control_permission_shell(
        &self,
        run_id: &str,
        tool_id: &str,
    ) -> bool {
        self.provider_tool
            .is_control_permission_shell_tool(run_id, tool_id)
    }
    pub(crate) fn mark_provider_shell_handoff_run(&mut self, run_id: &str) {
        self.provider_shell_handoff_run_ids
            .insert(run_id.to_string());
    }
    pub(crate) fn provider_shell_handoff_run_seen(&self, run_id: &str) -> bool {
        self.provider_shell_handoff_run_ids.contains(run_id)
    }
    pub(crate) fn record_provider_tool_command_from_input(
        &mut self,
        run_id: &str,
        tool_id: &str,
        tool_input: &serde_json::Value,
    ) -> bool {
        self.provider_tool
            .record_command_from_input(run_id, tool_id, tool_input)
    }
    pub(crate) fn mark_provider_control_permission_shell_tool(
        &mut self,
        run_id: &str,
        tool_id: &str,
    ) {
        self.provider_tool
            .mark_control_permission_shell_tool(run_id, tool_id);
    }
    pub(crate) fn record_provider_shell_command_from_tool_call(
        &mut self,
        run_id: &str,
        tool_id: &str,
        input: &str,
    ) -> bool {
        self.provider_tool
            .record_shell_command_from_tool_call(run_id, tool_id, input)
    }
    pub(crate) fn record_pending_provider_shell_command(
        &mut self,
        run_id: &str,
        command: &str,
    ) -> bool {
        self.provider_tool
            .record_pending_shell_command(run_id, command)
    }
    pub(crate) fn record_provider_tool_output_delta(
        &mut self,
        run_id: &str,
        tool_id: &str,
        stream: &str,
        text: &str,
    ) {
        self.provider_tool
            .record_output_delta(run_id, tool_id, stream, text);
    }
    pub(crate) fn shell_handoff(&self) -> &ShellHandoffState {
        &self.shell_handoff
    }
    pub(crate) fn shell_handoff_mut(&mut self) -> &mut ShellHandoffState {
        &mut self.shell_handoff
    }
    pub(crate) fn find_interactive_shell_handoff(
        &self,
        handoff_id: &str,
    ) -> Option<PendingInteractiveShellHandoff> {
        self.interactive_shell_handoffs
            .iter()
            .find(|handoff| handoff.id == handoff_id)
            .cloned()
    }
    pub(crate) fn queue_interactive_shell_handoff_for_tool_failure(
        &mut self,
        run_id: &str,
        tool_id: &str,
        status: &str,
        origin: AgentRunOrigin,
    ) -> Option<PendingInteractiveShellHandoff> {
        let command = self
            .provider_tool
            .interactive_failure_command(run_id, tool_id, status)?;
        if let Some(handoff) = self
            .interactive_shell_handoffs
            .iter()
            .find(|handoff| handoff.run_id == run_id && handoff.tool_id == tool_id)
            .cloned()
        {
            return Some(handoff);
        }

        let handoff = PendingInteractiveShellHandoff {
            id: format!("handoff-{}", self.interactive_shell_handoffs.len() + 1),
            run_id: command.run_id.clone(),
            tool_id: command.tool_id.clone(),
            command: command.command.clone(),
            exact_preview: format!("$ {}", command.command),
            origin,
        };
        self.interactive_shell_handoffs.push(handoff.clone());
        Some(handoff)
    }
    pub(crate) fn interactive_shell_handoff_ids(&self) -> impl Iterator<Item = &str> {
        self.interactive_shell_handoffs
            .iter()
            .map(|handoff| handoff.id.as_str())
    }
    pub(crate) fn remember_selectable_commands(
        &mut self,
        commands: Vec<String>,
        after_event_index: Option<usize>,
    ) {
        self.selectable_commands = commands;
        self.selectable_after_event_index = after_event_index;
    }
    pub(crate) fn selectable_command(&self, index: usize) -> Option<&str> {
        self.selectable_commands.get(index).map(String::as_str)
    }
    pub(crate) fn selectable_command_count(&self) -> usize {
        self.selectable_commands.len()
    }
    pub(crate) fn selectable_commands_available_after(&self) -> Option<usize> {
        self.selectable_after_event_index
    }
    pub(crate) fn has_selectable_commands(&self) -> bool {
        !self.selectable_commands.is_empty()
    }
    pub(crate) fn event_cursor(&self) -> ShellEventCursor {
        self.event_cursor
    }
    pub(crate) fn set_event_cursor(&mut self, cursor: ShellEventCursor) {
        self.event_cursor = cursor;
    }
    pub(crate) fn observe_shell_command_activity(&mut self, events: &[ShellEvent]) {
        for event in events {
            let Some(command_id) = event.command_id.as_ref() else {
                continue;
            };
            match event.kind {
                ShellEventKind::CommandStarted => {
                    self.active_shell_command_ids.insert(command_id.clone());
                }
                ShellEventKind::CommandCompleted
                | ShellEventKind::CommandFailed
                | ShellEventKind::UserInputIntercepted => {
                    self.active_shell_command_ids.remove(command_id);
                }
                _ => {}
            }
        }
    }
    pub(crate) fn shell_busy(&self) -> bool {
        !self.active_shell_command_ids.is_empty()
    }
    #[cfg(test)]
    pub(crate) fn record_ledger_rebuild(&mut self) {
        self.ledger_rebuild_count += 1;
    }
    #[cfg(test)]
    pub(crate) fn ledger_rebuild_count(&self) -> usize {
        self.ledger_rebuild_count
    }
}

#[derive(Default)]
pub(crate) struct ContinuityState {
    pub(crate) facts: ContinuityFacts,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingInteractiveShellHandoff {
    pub(crate) id: String,
    pub(crate) run_id: String,
    pub(crate) tool_id: String,
    pub(crate) command: String,
    pub(crate) exact_preview: String,
    pub(crate) origin: AgentRunOrigin,
}

#[derive(Debug, Clone)]
pub(crate) struct ContinuityFact {
    pub(crate) kind: ContinuityFactKind,
    pub(crate) text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContinuityFactKind {
    UserIntent,
    AgentResult,
}

#[derive(Debug, Clone)]
pub(crate) struct ContinuityFacts {
    pub(crate) items: VecDeque<ContinuityFact>,
    max_items: usize,
}

impl Default for ContinuityFacts {
    fn default() -> Self {
        Self {
            items: VecDeque::new(),
            max_items: 12,
        }
    }
}

impl ContinuityFacts {
    pub(crate) fn push(&mut self, kind: ContinuityFactKind, text: impl Into<String>) {
        let text = text.into();
        if text.trim().is_empty() {
            return;
        }
        self.items.push_back(ContinuityFact { kind, text });
        while self.items.len() > self.max_items {
            self.items.pop_front();
        }
    }
}

impl InlineState {
    pub(crate) fn with_raw_session_dir(path: &Path) -> Self {
        Self {
            activity: ActivityState {
                output_dir: Some(path.join("agent-output-refs")),
                ..ActivityState::default()
            },
            ..Self::default()
        }
    }
    pub(crate) fn i18n(&self) -> I18n {
        I18n::new(self.language)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum AnalysisMode {
    #[default]
    Smart,
    Auto,
    Manual,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeModePanel {
    pub(crate) id: String,
    pub(crate) kind: RuntimeModePanelKind,
    pub(crate) selected_option: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeModePanelKind {
    Approval,
    Analysis,
}

impl RuntimeModePanelKind {
    fn id_prefix(self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::Analysis => "analysis",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeConfigPanel {
    pub(crate) id: String,
    pub(crate) setting: String,
    pub(crate) before_value: String,
    pub(crate) pending_value: String,
    pub(crate) config_path: PathBuf,
    pub(crate) selected_option: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeConfigLanguagePanel {
    pub(crate) id: String,
    pub(crate) selected_option: usize,
}

impl AnalysisMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Smart => "smart",
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }
}
