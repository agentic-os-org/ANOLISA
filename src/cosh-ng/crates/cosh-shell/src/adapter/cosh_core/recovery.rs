//! Provider-session recovery state and workspace-scoped commit helpers.

use std::sync::{Arc, Mutex};

use crate::types::{AgentEvent, AgentRequest};

use super::SessionErrorInfo;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
/// Interactive provider-session recovery lifecycle.
pub enum SessionRecoveryState {
    /// No recovery selection or active recovered session exists.
    #[default]
    None,
    /// A validated historical session is selected for the next request.
    Selected,
    /// The selected session is being loaded by cosh-core.
    Restoring,
    /// A recovered provider session completed a turn successfully.
    Active,
    /// Recovery failed without terminating the interactive shell.
    Failed,
}

impl SessionRecoveryState {
    /// Returns the user-facing stable lifecycle label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Selected => "selected",
            Self::Restoring => "restoring",
            Self::Active => "active",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Default)]
/// Shared recovery selection and its latest typed failure.
pub struct SessionRecovery {
    /// Current lifecycle state.
    pub state: SessionRecoveryState,
    /// Validated provider session selected for the next request.
    pub selected_session_id: Option<String>,
    /// Canonical workspace paired with the selected ID.
    pub selected_workspace_scope: Option<String>,
    /// Latest recoverable management or restore failure.
    pub last_error: Option<SessionErrorInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActiveSession {
    session_id: String,
    workspace_scope: String,
    generation: u64,
}

#[derive(Debug, Clone, Default)]
/// Atomically owned active-session and recovery-selection state.
pub struct SessionRuntimeState {
    active: Option<ActiveSession>,
    /// Interactive recovery selection and lifecycle.
    pub recovery: SessionRecovery,
    selected_attempt_generation: Option<u64>,
    latest_attempt_generation: Option<u64>,
    next_generation: u64,
    management_gate: Arc<Mutex<()>>,
}

impl SessionRuntimeState {
    /// Returns the shared gate that serializes mutating management calls.
    ///
    /// Callers lock the gate instead of this state across subprocess I/O so
    /// snapshot readers and turn commits never block on a management call.
    pub(super) fn management_gate(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.management_gate)
    }

    /// Builds state with an already committed active provider session.
    pub fn with_active(session_id: impl Into<String>, workspace_scope: impl Into<String>) -> Self {
        Self {
            active: Some(ActiveSession {
                session_id: session_id.into(),
                workspace_scope: workspace_scope.into(),
                generation: 0,
            }),
            recovery: SessionRecovery {
                state: SessionRecoveryState::Active,
                ..SessionRecovery::default()
            },
            ..Self::default()
        }
    }

    /// Returns the committed provider session ID.
    pub fn active_session_id(&self) -> Option<&str> {
        self.active
            .as_ref()
            .map(|active| active.session_id.as_str())
    }

    /// Returns the canonical workspace paired with the active session.
    pub fn active_workspace_scope(&self) -> Option<&str> {
        self.active
            .as_ref()
            .map(|active| active.workspace_scope.as_str())
    }

    fn allocate_generation(&mut self) -> u64 {
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        self.next_generation
    }

    fn supersede_current_attempt(&mut self) {
        let generation = self.allocate_generation();
        self.latest_attempt_generation = Some(generation);
        self.selected_attempt_generation = None;
    }

    /// Detaches every active and selected provider-session binding without
    /// touching persisted sessions, so the next Agent request starts a fresh
    /// conversation.
    ///
    /// Returns the provider session we detached from — the committed active id
    /// when present, otherwise the pending selection — or `None` when nothing
    /// was bound (the operation is idempotent). Supersedes every in-flight
    /// attempt so a late commit from the old conversation cannot re-bind this
    /// runtime to a stale provider session.
    pub(super) fn start_fresh_session(&mut self) -> Option<String> {
        let previous = self
            .active
            .as_ref()
            .map(|active| active.session_id.clone())
            .or_else(|| self.recovery.selected_session_id.clone());
        self.active = None;
        self.recovery.state = SessionRecoveryState::None;
        self.recovery.selected_session_id = None;
        self.recovery.selected_workspace_scope = None;
        self.recovery.last_error = None;
        self.supersede_current_attempt();
        previous
    }

    /// Replaces the selection while making every in-flight attempt stale.
    pub(super) fn select_session(&mut self, session_id: String, workspace_scope: String) {
        self.supersede_current_attempt();
        self.recovery.state = SessionRecoveryState::Selected;
        self.recovery.selected_session_id = Some(session_id);
        self.recovery.selected_workspace_scope = Some(workspace_scope);
        self.recovery.last_error = None;
    }

    /// Records validation failure while making every in-flight attempt stale.
    pub(super) fn fail_selection(&mut self, error: SessionErrorInfo) {
        self.supersede_current_attempt();
        self.recovery.state = SessionRecoveryState::Failed;
        self.recovery.selected_session_id = None;
        self.recovery.selected_workspace_scope = None;
        self.recovery.last_error = Some(error);
    }

    fn owns_attempt(&self, attempt: &SessionResumeAttempt) -> bool {
        self.latest_attempt_generation == Some(attempt.generation())
    }

    fn owns_selected_attempt(&self, attempt: &SessionResumeAttempt) -> bool {
        matches!(attempt, SessionResumeAttempt::Selected { .. })
            && self.owns_attempt(attempt)
            && self.selected_attempt_generation == Some(attempt.generation())
    }

    fn owns_active_attempt(&self, attempt: &SessionResumeAttempt) -> bool {
        let SessionResumeAttempt::Active {
            session_id,
            generation,
        } = attempt
        else {
            return false;
        };
        self.owns_attempt(attempt)
            && self.active.as_ref().is_some_and(|active| {
                active.session_id == *session_id && active.generation == *generation
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::adapter) enum SessionResumeAttempt {
    Fresh { generation: u64 },
    Selected { session_id: String, generation: u64 },
    Active { session_id: String, generation: u64 },
}

impl SessionResumeAttempt {
    pub(in crate::adapter) fn session_id(&self) -> Option<&str> {
        match self {
            Self::Fresh { .. } => None,
            Self::Selected { session_id, .. } | Self::Active { session_id, .. } => Some(session_id),
        }
    }

    fn generation(&self) -> u64 {
        match self {
            Self::Fresh { generation }
            | Self::Selected { generation, .. }
            | Self::Active { generation, .. } => *generation,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub(in crate::adapter) enum SessionCommitOutcome {
    Continue,
    RestoreFailed(SessionErrorInfo),
    StaleAttempt,
}

pub(in crate::adapter) fn terminal_events_for_session_commit(
    run_id: &str,
    terminal_events: Vec<AgentEvent>,
    outcome: SessionCommitOutcome,
) -> Vec<AgentEvent> {
    match outcome {
        SessionCommitOutcome::Continue => terminal_events,
        SessionCommitOutcome::StaleAttempt => Vec::new(),
        SessionCommitOutcome::RestoreFailed(error) => {
            let mut terminal_events = terminal_events
                .into_iter()
                .filter(|event| !matches!(event, AgentEvent::AgentCompleted { .. }))
                .collect::<Vec<_>>();
            if !terminal_events
                .iter()
                .any(|event| matches!(event, AgentEvent::AgentFailed { .. }))
            {
                terminal_events.push(AgentEvent::AgentFailed {
                    run_id: run_id.to_string(),
                    error: format!("[{}] {}", error.code, error.message),
                    error_code: None,
                    max_turns: None,
                });
            }
            terminal_events
        }
    }
}

pub(in crate::adapter) fn session_scope_from_request(request: &AgentRequest) -> String {
    let request_scope = if request.command_block.end_cwd.is_empty() {
        &request.command_block.cwd
    } else {
        &request.command_block.end_cwd
    };
    let scope = if request_scope.is_empty() || request_scope == "<unknown>" {
        std::env::current_dir()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|_| request_scope.clone())
    } else {
        request_scope.clone()
    };
    std::fs::canonicalize(&scope)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or(scope)
}

pub(in crate::adapter) fn begin_session_attempt(
    state: &Arc<Mutex<SessionRuntimeState>>,
    resume_id: Option<&str>,
    session_scope: &str,
) -> SessionResumeAttempt {
    let mut state = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let generation = state.allocate_generation();
    state.latest_attempt_generation = Some(generation);

    if let Some(resume_id) = resume_id {
        let selected_matches = matches!(
            state.recovery.state,
            SessionRecoveryState::Selected | SessionRecoveryState::Restoring
        ) && state.recovery.selected_session_id.as_deref()
            == Some(resume_id)
            && state.recovery.selected_workspace_scope.as_deref() == Some(session_scope);
        if selected_matches {
            state.recovery.state = SessionRecoveryState::Restoring;
            state.recovery.last_error = None;
            state.selected_attempt_generation = Some(generation);
            return SessionResumeAttempt::Selected {
                session_id: resume_id.to_string(),
                generation,
            };
        }

        let active_matches = state.active.as_ref().is_some_and(|active| {
            active.session_id == resume_id && active.workspace_scope == session_scope
        });
        if active_matches {
            if let Some(active) = state.active.as_mut() {
                active.generation = generation;
            }
            return SessionResumeAttempt::Active {
                session_id: resume_id.to_string(),
                generation,
            };
        }
    }

    if state.recovery.state == SessionRecoveryState::Restoring {
        state.recovery.state = SessionRecoveryState::Selected;
        state.selected_attempt_generation = None;
    }
    SessionResumeAttempt::Fresh { generation }
}

pub(in crate::adapter) fn commit_pending_session_for_scope(
    completed: bool,
    failed: bool,
    state: &Arc<Mutex<SessionRuntimeState>>,
    pending: &Arc<Mutex<Option<String>>>,
    session_scope: &str,
    session_resumable: Option<bool>,
    resume_attempt: &SessionResumeAttempt,
) -> SessionCommitOutcome {
    if session_resumable == Some(false) {
        return discard_non_resumable_session(resume_attempt, state);
    }
    if !completed || failed {
        return recovery_failure_outcome(
            state,
            resume_attempt,
            "provider session did not complete",
        );
    }
    let pending_id = pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let Some(pending_id) = pending_id else {
        return recovery_failure_outcome(
            state,
            resume_attempt,
            "provider session completed without a resumable session ID",
        );
    };

    let mut state = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !state.owns_attempt(resume_attempt) {
        return SessionCommitOutcome::StaleAttempt;
    }
    if let Some(expected_id) = resume_attempt
        .session_id()
        .filter(|expected_id| *expected_id != pending_id)
    {
        return reject_resume_identity_mismatch(
            &mut state,
            resume_attempt,
            expected_id,
            &pending_id,
        );
    }

    state.active = Some(ActiveSession {
        session_id: pending_id,
        workspace_scope: session_scope.to_string(),
        generation: resume_attempt.generation(),
    });
    match resume_attempt {
        SessionResumeAttempt::Selected { .. } => {
            state.recovery.state = SessionRecoveryState::Active;
            state.recovery.selected_session_id = None;
            state.recovery.selected_workspace_scope = None;
            state.selected_attempt_generation = None;
        }
        SessionResumeAttempt::Active { .. } | SessionResumeAttempt::Fresh { .. }
            if !matches!(
                state.recovery.state,
                SessionRecoveryState::Selected | SessionRecoveryState::Restoring
            ) =>
        {
            state.recovery.state = SessionRecoveryState::Active;
            state.recovery.selected_session_id = None;
            state.recovery.selected_workspace_scope = None;
            state.selected_attempt_generation = None;
        }
        SessionResumeAttempt::Active { .. } | SessionResumeAttempt::Fresh { .. } => {}
    }
    state.recovery.last_error = None;
    SessionCommitOutcome::Continue
}

/// Returns whether a failed turn still leaves a safely persisted session that
/// should remain active for continuation or manual compaction.
///
/// Context-limit and per-request turn-budget failures stop the current run only
/// after cosh-core has already written the transcript, so the provider
/// conversation stays usable: the next user message resumes it with a fresh turn
/// budget. A persistence-phase failure proves the opposite, so it never
/// qualifies.
pub(in crate::adapter) fn retain_context_session(
    terminal_events: &[AgentEvent],
    session_error_phase: Option<&str>,
    session_resumable: Option<bool>,
) -> bool {
    session_resumable == Some(true)
        && session_error_phase != Some("persist")
        && terminal_events.iter().any(|event| {
            matches!(
                event,
                AgentEvent::AgentFailed { error, error_code, .. }
                    if error.starts_with("context_limit:")
                        || error_code.as_deref() == Some("max_turns")
            )
        })
}

/// Returns the effective configured turn limit from structured failure metadata.
pub(crate) fn max_turn_limit(terminal_events: &[AgentEvent]) -> Option<u32> {
    terminal_events.iter().find_map(|event| {
        let AgentEvent::AgentFailed {
            error_code,
            max_turns,
            ..
        } = event
        else {
            return None;
        };
        (error_code.as_deref() == Some("max_turns"))
            .then_some(*max_turns)
            .flatten()
            .filter(|limit| *limit > 0)
    })
}

pub(in crate::adapter) fn invalidate_resume_on_session_failure(
    attempt: &SessionResumeAttempt,
    session_error_code: Option<&str>,
    session_error_phase: Option<&str>,
    terminal_events: &[AgentEvent],
    state: &Arc<Mutex<SessionRuntimeState>>,
) {
    let Some(code) = session_error_code else {
        return;
    };
    let is_load_failure = matches!(session_error_phase, None | Some("load"))
        && matches!(
            code,
            "not_found" | "corrupt" | "incompatible_version" | "scope_mismatch"
        );
    let is_persistence_failure = session_error_phase == Some("persist");
    if !is_load_failure && !is_persistence_failure {
        return;
    }
    let message = terminal_events
        .iter()
        .find_map(|event| match event {
            AgentEvent::AgentFailed { error, .. } => Some(error.clone()),
            _ => None,
        })
        .unwrap_or_else(|| {
            if is_persistence_failure {
                format!("provider session persistence failed [{code}]")
            } else {
                format!("provider session load failed [{code}]")
            }
        });
    let error = SessionErrorInfo {
        code: code.to_string(),
        message,
        recoverable: true,
        hint: Some(if is_persistence_failure {
            "Resolve the persistence failure before resuming this session again.".to_string()
        } else {
            "Refresh the session list or start a new session.".to_string()
        }),
    };
    let mut state = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match attempt {
        SessionResumeAttempt::Active { .. } if state.owns_active_attempt(attempt) => {
            state.active = None;
            if state.recovery.selected_session_id.is_none() {
                state.recovery.state = SessionRecoveryState::Failed;
                state.recovery.selected_workspace_scope = None;
            }
        }
        SessionResumeAttempt::Selected { .. } if state.owns_selected_attempt(attempt) => {
            state.recovery.state = SessionRecoveryState::Failed;
            state.recovery.selected_session_id = None;
            state.recovery.selected_workspace_scope = None;
            state.selected_attempt_generation = None;
        }
        SessionResumeAttempt::Fresh { .. }
        | SessionResumeAttempt::Selected { .. }
        | SessionResumeAttempt::Active { .. } => return,
    }
    state.recovery.last_error = Some(error);
}

fn reject_resume_identity_mismatch(
    state: &mut SessionRuntimeState,
    attempt: &SessionResumeAttempt,
    expected_id: &str,
    pending_id: &str,
) -> SessionCommitOutcome {
    let error = SessionErrorInfo {
        code: "restore_failed".to_string(),
        message: format!(
            "provider session identity mismatch: resumed {expected_id}, provider returned {pending_id}"
        ),
        recoverable: true,
        hint: Some("Refresh the session list and retry.".to_string()),
    };
    match attempt {
        SessionResumeAttempt::Selected { .. } if state.owns_selected_attempt(attempt) => {
            state.recovery.state = SessionRecoveryState::Failed;
            state.recovery.selected_session_id = None;
            state.recovery.selected_workspace_scope = None;
            state.selected_attempt_generation = None;
        }
        SessionResumeAttempt::Active { .. } if state.owns_active_attempt(attempt) => {
            state.active = None;
            if state.recovery.selected_session_id.is_none() {
                state.recovery.state = SessionRecoveryState::Failed;
                state.recovery.selected_workspace_scope = None;
            }
        }
        SessionResumeAttempt::Fresh { .. } => {}
        SessionResumeAttempt::Selected { .. } | SessionResumeAttempt::Active { .. } => {
            return SessionCommitOutcome::StaleAttempt;
        }
    }
    state.recovery.last_error = Some(error.clone());
    SessionCommitOutcome::RestoreFailed(error)
}

pub(in crate::adapter) fn mark_recovery_failure(
    state: &Arc<Mutex<SessionRuntimeState>>,
    attempt: &SessionResumeAttempt,
    message: &str,
) -> Option<SessionErrorInfo> {
    let mut state = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !state.owns_selected_attempt(attempt)
        || state.recovery.state != SessionRecoveryState::Restoring
    {
        return None;
    }
    let error = SessionErrorInfo {
        code: "restore_failed".to_string(),
        message: message.to_string(),
        recoverable: true,
        hint: Some("Refresh the session list and retry.".to_string()),
    };
    state.recovery.state = SessionRecoveryState::Failed;
    state.recovery.selected_session_id = None;
    state.recovery.selected_workspace_scope = None;
    state.recovery.last_error = Some(error.clone());
    state.selected_attempt_generation = None;
    // Session lifecycle event: recovery state transitions are key diagnostic
    // nodes (the /health live probe reports them from recovery_snapshot).
    tracing::warn!(
        code = %error.code,
        error = %error.message,
        "session recovery failed"
    );
    Some(error)
}

fn recovery_failure_outcome(
    state: &Arc<Mutex<SessionRuntimeState>>,
    attempt: &SessionResumeAttempt,
    message: &str,
) -> SessionCommitOutcome {
    let owns_attempt = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .owns_attempt(attempt);
    if !owns_attempt {
        return SessionCommitOutcome::StaleAttempt;
    }
    mark_recovery_failure(state, attempt, message).map_or(
        SessionCommitOutcome::Continue,
        SessionCommitOutcome::RestoreFailed,
    )
}

fn discard_non_resumable_session(
    attempt: &SessionResumeAttempt,
    state: &Arc<Mutex<SessionRuntimeState>>,
) -> SessionCommitOutcome {
    let mut state = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !state.owns_attempt(attempt) {
        return SessionCommitOutcome::StaleAttempt;
    }
    match attempt {
        SessionResumeAttempt::Selected { .. } if state.owns_selected_attempt(attempt) => {
            let error = SessionErrorInfo {
                code: "restore_failed".to_string(),
                message: "provider session is not resumable".to_string(),
                recoverable: true,
                hint: Some("Start a new session or enable session persistence.".to_string()),
            };
            state.recovery.state = SessionRecoveryState::Failed;
            state.recovery.selected_session_id = None;
            state.recovery.selected_workspace_scope = None;
            state.recovery.last_error = Some(error.clone());
            state.selected_attempt_generation = None;
            SessionCommitOutcome::RestoreFailed(error)
        }
        SessionResumeAttempt::Active { .. } if state.owns_active_attempt(attempt) => {
            state.active = None;
            if state.recovery.selected_session_id.is_none() {
                state.recovery.state = SessionRecoveryState::None;
                state.recovery.last_error = None;
            }
            SessionCommitOutcome::Continue
        }
        SessionResumeAttempt::Fresh { .. }
        | SessionResumeAttempt::Selected { .. }
        | SessionResumeAttempt::Active { .. } => SessionCommitOutcome::Continue,
    }
}

#[cfg(test)]
mod tests;
