use std::sync::{Arc, Mutex};

use crate::types::{AgentEvent, AgentRequest, CoshApprovalMode};

use super::cosh_core_process::run_sync_cosh_core_process;
use super::cosh_core_service::PersistentCoshCoreRuntime;
use super::prompt::provider_prompt_contract_for_request_with_evidence_access;
use super::{
    prompt_from_request_with_evidence_policy, start_threaded_adapter_run, AdapterError,
    AdapterInstance, AgentAdapter, AgentBackendCapabilities, AgentRunHandle, FreshSessionOutcome,
    PreparedInvocation,
};

pub(super) mod question_ingress;
mod recovery;
mod session;

pub(crate) use recovery::max_turn_limit;
pub(super) use recovery::{
    begin_session_attempt, commit_pending_session_for_scope, invalidate_resume_on_session_failure,
    mark_recovery_failure, retain_context_session, session_scope_from_request,
    terminal_events_for_session_commit, SessionResumeAttempt,
};
pub use recovery::{SessionRecovery, SessionRecoveryState, SessionRuntimeState};
pub use session::{
    SessionClearFailure, SessionClearInterruption, SessionClearPlan, SessionClearResult,
    SessionErrorInfo, SessionHealth, SessionList, SessionManagementClient, SessionSummary,
};

/// Provider name of the cosh-core driver. Verdict-channel behavior is keyed
/// on this single constant so a provider rename or alias cannot silently
/// disable the fail-closed guards (#2156).
pub(crate) const COSH_CORE_PROVIDER_NAME: &str = "cosh-core";

#[derive(Debug, Clone)]
/// Adapter that delegates Agent turns and session ownership to cosh-core.
pub struct CoshCoreAdapter {
    /// cosh-core executable path.
    pub program: String,
    /// Whether this adapter may start a real provider process.
    pub allow_model_call: bool,
    /// Atomically owned active session, workspace, generation, and recovery state.
    pub session: Arc<Mutex<SessionRuntimeState>>,
    pub(crate) runtime: Arc<PersistentCoshCoreRuntime>,
    /// Shell cwd tracked from the PTY host so that short-lived `--registry`
    /// processes resolve the same project root as the headless runtime.
    pub(crate) shell_cwd: Arc<Mutex<Option<String>>>,
}

impl Default for CoshCoreAdapter {
    fn default() -> Self {
        let program = std::env::var("COSH_CORE_PATH").unwrap_or_else(|_| {
            if let Ok(exe) = std::env::current_exe() {
                if let Some(dir) = exe.parent() {
                    let sibling = dir.join("cosh-core");
                    if sibling.is_file() {
                        return sibling.to_string_lossy().into_owned();
                    }
                }
            }
            "cosh-core".to_string()
        });
        Self {
            program,
            allow_model_call: false,
            session: Arc::new(Mutex::new(SessionRuntimeState::default())),
            runtime: Arc::new(PersistentCoshCoreRuntime::default()),
            shell_cwd: Arc::new(Mutex::new(None)),
        }
    }
}

impl CoshCoreAdapter {
    /// Creates an adapter for an explicit core executable.
    pub fn new(program: impl Into<String>, allow_model_call: bool) -> Self {
        Self {
            program: program.into(),
            allow_model_call,
            session: Arc::new(Mutex::new(SessionRuntimeState::default())),
            runtime: Arc::new(PersistentCoshCoreRuntime::default()),
            shell_cwd: Arc::new(Mutex::new(None)),
        }
    }

    /// Enables or disables real model process execution.
    pub fn with_model_call(mut self, allow: bool) -> Self {
        self.allow_model_call = allow;
        self
    }

    /// Update the shell cwd used to forward `--workspace` to short-lived
    /// `--registry` cosh-core processes. Without this, registry-mode
    /// invocations fall back to the cosh-shell process cwd and fail to
    /// discover project-level skills and hooks.
    ///
    /// `None` means the caller has no cwd to report (e.g. programmatic
    /// path without a PTY); the last known value is retained rather than
    /// cleared so subsequent registry queries keep using the real shell cwd
    /// instead of silently degrading to the session workspace scope.
    pub(crate) fn set_shell_cwd(&self, cwd: Option<&str>) {
        if let Some(cwd) = cwd {
            if let Ok(mut guard) = self.shell_cwd.lock() {
                *guard = Some(cwd.to_string());
            }
        }
    }

    /// Clears the cached shell cwd.
    ///
    /// The dispatch layer calls this when both the event and the
    /// dispatcher-tracked prompt cwd are unavailable. That combination
    /// means the previously reported cwd may be stale (e.g. after a `cd`
    /// whose OSC 1337 markers were lost), so continuing to forward it as
    /// `--workspace` would target the wrong project.
    pub(crate) fn clear_shell_cwd(&self) {
        if let Ok(mut guard) = self.shell_cwd.lock() {
            *guard = None;
        }
    }

    /// Inspects a persisted session summary without selecting it.
    ///
    /// # Errors
    ///
    /// Returns a recoverable management protocol error.
    pub fn inspect_session(
        &self,
        workspace_scope: &str,
        session_id: &str,
    ) -> Result<SessionSummary, SessionErrorInfo> {
        SessionManagementClient::new(self.program.clone()).inspect(workspace_scope, session_id)
    }

    /// Validates and selects a persisted session for the next Agent request.
    ///
    /// # Errors
    ///
    /// Returns a recoverable validation or transport error.
    pub fn select_session(
        &self,
        workspace_scope: &str,
        session_id: &str,
    ) -> Result<SessionSummary, SessionErrorInfo> {
        // Serialize with other mutating management calls through the gate;
        // the state lock itself is never held across subprocess I/O, so
        // snapshot readers and turn commits cannot block on validation.
        let gate = self.management_gate();
        let _management = gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let summary = SessionManagementClient::new(self.program.clone())
            .validate(workspace_scope, session_id);
        let mut session = self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match summary {
            Ok(summary) => {
                session.select_session(summary.session_id.clone(), summary.workspace_scope.clone());
                Ok(summary)
            }
            Err(error) => {
                session.fail_selection(error.clone());
                Err(error)
            }
        }
    }

    /// Clears explicit persisted sessions with active and selected IDs protected.
    ///
    /// # Errors
    ///
    /// Returns a recoverable request-level management error.
    pub fn clear_sessions(
        &self,
        workspace_scope: &str,
        session_ids: &[String],
    ) -> Result<SessionClearResult, SessionErrorInfo> {
        // Hold the gate, not the state lock, across the clear subprocess so a
        // concurrent selection cannot validate an ID that is being deleted.
        let gate = self.management_gate();
        let _management = gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let protected = self.protected_session_ids();
        SessionManagementClient::new(self.program.clone()).clear(
            workspace_scope,
            session_ids,
            &protected,
        )
    }

    /// Prepares exact clear-all candidates without eagerly loading all summaries.
    ///
    /// # Errors
    ///
    /// Returns a recoverable request-level management error.
    pub fn prepare_clear_all(
        &self,
        workspace_scope: &str,
    ) -> Result<SessionClearPlan, SessionErrorInfo> {
        let gate = self.management_gate();
        let _management = gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let protected = self.protected_session_ids();
        SessionManagementClient::new(self.program.clone())
            .prepare_clear_all(workspace_scope, &protected)
    }

    /// Returns the gate that serializes mutating session-management calls.
    fn management_gate(&self) -> std::sync::Arc<Mutex<()>> {
        self.session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .management_gate()
    }

    /// Detaches the active and selected provider-session bindings so the next
    /// Agent request starts a fresh conversation, without deleting or rewriting
    /// any persisted session.
    ///
    /// Serializes with management mutations through the management gate.
    /// The recovery generation separately prevents a late turn commit from
    /// re-binding the detached session.
    pub(super) fn start_fresh_session(&self) -> FreshSessionOutcome {
        let gate = self.management_gate();
        let _management = gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_session_id = self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .start_fresh_session();
        FreshSessionOutcome::Detached {
            previous_session_id,
        }
    }

    /// Returns a consistent snapshot of interactive recovery state.
    pub fn recovery_snapshot(&self) -> SessionRecovery {
        self.session
            .lock()
            .map(|session| session.recovery.clone())
            .unwrap_or_default()
    }

    /// Returns the provider conversation committed after a successful turn.
    pub fn committed_session_id(&self) -> Option<String> {
        self.session
            .lock()
            .ok()
            .and_then(|session| session.active_session_id().map(str::to_string))
    }

    /// Returns active and selected provider IDs that clear must protect.
    pub fn protected_session_ids(&self) -> Vec<String> {
        let session = self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        protected_session_ids_from_state(&session)
    }

    fn begin_resume_attempt(
        &self,
        prepared: &mut PreparedInvocation,
        session_scope: &str,
    ) -> SessionResumeAttempt {
        let resume_id = prepared
            .args
            .windows(2)
            .find(|arguments| arguments[0] == "--resume")
            .map(|arguments| arguments[1].clone());
        let attempt = begin_session_attempt(&self.session, resume_id.as_deref(), session_scope);
        if resume_id.is_some() && matches!(attempt, SessionResumeAttempt::Fresh { .. }) {
            if let Some(index) = prepared
                .args
                .iter()
                .position(|argument| argument == "--resume")
            {
                prepared.args.drain(index..=(index + 1));
            }
        }
        attempt
    }

    /// Builds a workspace-scoped headless cosh-core invocation.
    pub fn prepare_invocation(
        &self,
        request: &AgentRequest,
        mode: CoshApprovalMode,
    ) -> PreparedInvocation {
        let disable_resume = request
            .context_hints
            .iter()
            .any(|hint| hint.contains("disable provider resume"));
        let session_scope = session_scope_from_request(request);
        let resume_session = if disable_resume {
            None
        } else {
            let selected = self.session.lock().ok().and_then(|session| {
                let recovery = &session.recovery;
                (matches!(
                    recovery.state,
                    SessionRecoveryState::Selected | SessionRecoveryState::Restoring
                ) && recovery.selected_workspace_scope.as_deref() == Some(session_scope.as_str()))
                .then(|| recovery.selected_session_id.clone())
                .flatten()
            });
            selected.or_else(|| {
                self.session.lock().ok().and_then(|session| {
                    (session.active_workspace_scope() == Some(session_scope.as_str()))
                        .then(|| session.active_session_id().map(str::to_string))
                        .flatten()
                })
            })
        };

        let mut args = vec![
            "--headless".to_string(),
            "--cosh-shell-transport".to_string(),
            "--enable-shell-evidence-tool".to_string(),
            "--approval-mode".to_string(),
            mode.label().to_string(),
            "--workspace".to_string(),
            session_scope,
        ];

        if let Some(session_id) = resume_session {
            args.extend(["--resume".to_string(), session_id]);
        }

        PreparedInvocation {
            program: self.program.clone(),
            args,
            prompt: cosh_core_prompt_from_request(request, mode),
        }
    }

    /// Starts a cancellable cosh-core turn and updates recovery state.
    pub fn start_cancellable(
        &self,
        request: AgentRequest,
        mode: CoshApprovalMode,
    ) -> AgentRunHandle {
        let session_scope = session_scope_from_request(&request);
        let mut prepared = self.prepare_invocation(&request, mode);
        if !self.allow_model_call {
            let adapter = AdapterInstance::CoshCore(self.clone());
            return start_threaded_adapter_run(adapter, request);
        }

        let resume_attempt = self.begin_resume_attempt(&mut prepared, &session_scope);
        let raw_user_input = request.user_input.clone();
        self.runtime.start_run(
            request.id,
            prepared,
            raw_user_input,
            mode,
            Arc::clone(&self.session),
            session_scope,
            resume_attempt,
        )
    }
}

impl AgentAdapter for CoshCoreAdapter {
    fn name(&self) -> &'static str {
        COSH_CORE_PROVIDER_NAME
    }

    fn capabilities(&self) -> AgentBackendCapabilities {
        AgentBackendCapabilities {
            text_stream: true,
            thinking_stream: false,
            session_resume: true,
            tool_intent: true,
            user_question: true,
            cancellable: true,
            control_protocol: true,
        }
    }

    fn run(&self, request: &AgentRequest) -> Result<Vec<AgentEvent>, AdapterError> {
        let mut events = Vec::new();
        self.run_stream(request, &mut |event| {
            events.push(event);
            Ok(())
        })?;
        Ok(events)
    }

    fn run_stream(
        &self,
        request: &AgentRequest,
        sink: &mut dyn FnMut(AgentEvent) -> Result<(), AdapterError>,
    ) -> Result<(), AdapterError> {
        let mut prepared = self.prepare_invocation(request, CoshApprovalMode::Recommend);
        if !self.allow_model_call {
            for event in cosh_core_dry_run_events(request, &prepared) {
                sink(event)?;
            }
            return Ok(());
        }
        let session_scope = session_scope_from_request(request);
        let resume_attempt = self.begin_resume_attempt(&mut prepared, &session_scope);
        run_sync_cosh_core_process(
            request,
            &prepared,
            &self.session,
            &session_scope,
            &resume_attempt,
            sink,
        )
    }
}

fn protected_session_ids_from_state(session: &SessionRuntimeState) -> Vec<String> {
    let mut protected = session
        .active_session_id()
        .map(str::to_string)
        .into_iter()
        .collect::<Vec<_>>();
    if let Some(selected) = matches!(
        session.recovery.state,
        SessionRecoveryState::Selected | SessionRecoveryState::Restoring
    )
    .then(|| session.recovery.selected_session_id.clone())
    .flatten()
    {
        if !protected.contains(&selected) {
            protected.push(selected);
        }
    }
    protected
}

fn cosh_core_prompt_from_request(request: &AgentRequest, mode: CoshApprovalMode) -> String {
    let access = crate::evidence::ShellEvidenceAccess::ControlProtocolTool;
    let request_prompt = prompt_from_request_with_evidence_policy(
        request,
        access,
        mode != CoshApprovalMode::Recommend,
    );
    format!(
        "{}{}",
        request_prompt,
        provider_prompt_contract_for_request_with_evidence_access(request, mode, "shell", access)
    )
}

fn cosh_core_dry_run_events(
    request: &AgentRequest,
    prepared: &PreparedInvocation,
) -> Vec<AgentEvent> {
    vec![
        AgentEvent::StatusChanged {
            run_id: request.id.clone(),
            phase: "prepared".to_string(),
            message: format!(
                "cosh-core invocation prepared: {} {}",
                prepared.program,
                prepared.args.join(" ")
            ),
        },
        AgentEvent::Recommendation {
            run_id: request.id.clone(),
            summary:
                "cosh-core adapter is configured but model calls are disabled in dry-run mode."
                    .to_string(),
            commands: vec![format!("{} {}", prepared.program, prepared.args.join(" "))],
            auto_execute: false,
        },
    ]
}
