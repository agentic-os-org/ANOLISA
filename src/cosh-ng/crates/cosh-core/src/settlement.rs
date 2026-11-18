//! Turn settlement shared by every entry point.
//!
//! One finalizer owns the durable session record, the activity lease and the
//! turn/persist combination, so the legacy JSONL transport and the ACP adapter
//! only project a result they did not compute themselves.

use std::io;
use std::path::Path;

use crate::cli::CliArgs;
use crate::compaction::{ContextBudget, ModelCapability};
use crate::config::CoreConfig;
use crate::core::{AgentTurnOutcome, CoshCore};
use crate::protocol::OutputMessage;
use crate::session::{
    PersistedSession, ProviderSessionId, SessionError, SessionLock, SessionStore,
};

pub(crate) struct SessionRuntime {
    pub(crate) store: Option<SessionStore>,
    /// Held for the whole life of this process so a task-budget sweep from any
    /// other process treats the session as being interacted with right now.
    /// Dropped implicitly on exit, and released by the kernel if the process dies.
    pub(crate) live_lease: Option<SessionLock>,
    pub(crate) workspace_scope: String,
    pub(crate) record: PersistedSession,
    pub(crate) auto_persist: bool,
    pub(crate) resumed: bool,
    pub(crate) notice: PersistenceNotice,
}

/// Limits how often one refusal reason is logged, not how often it is retried.
///
/// Each user turn makes exactly one persistence attempt. Nothing about a refused
/// commit is permanent for the rest of the process: another process can reclaim
/// sessions, and the retention leg re-runs on every later turn, so the next turn
/// has to try again. What must not repeat is the log line.
#[derive(Default)]
pub(crate) struct PersistenceNotice {
    pub(crate) budget_refusal_logged: bool,
}

impl PersistenceNotice {
    pub(crate) fn record(&mut self, error: &SessionError) {
        if matches!(error, SessionError::BudgetExhausted { .. }) && !self.budget_refusal_logged {
            self.budget_refusal_logged = true;
            // The turn error tells the user; this is the durable evidence that
            // commits started being refused. Suppressing the *attempt* here as
            // well would report every later turn as persisted while its
            // transcript was silently dropped, and would never notice the budget
            // being freed.
            tracing::warn!("session persistence refused by the task budget: {error:?}");
        }
    }
}

impl Drop for SessionRuntime {
    fn drop(&mut self) {
        if !self.record.was_loaded {
            if let (Some(store), Some(lease)) = (self.store.as_ref(), self.live_lease.as_ref()) {
                if let Err(cleanup_error) =
                    store.cleanup_rejected_commit(&self.record.session_id, lease)
                {
                    tracing::warn!("uncommitted session exit cleanup failed: {cleanup_error}");
                }
            }
        }
    }
}

impl SessionRuntime {
    pub(crate) fn initialize(args: &CliArgs, config: &CoreConfig) -> Result<Self, SessionError> {
        Self::open(config, &args.workspace_root(), args.resume.as_deref(), None)
    }

    /// Opens the record for an entry point that already owns its session id.
    ///
    /// The ACP adapter negotiates the identity before the kernel exists, so the
    /// durable record has to adopt it instead of minting a second one.
    pub(crate) fn for_entry(
        config: &CoreConfig,
        workspace: &Path,
        session_id: ProviderSessionId,
    ) -> Result<Self, SessionError> {
        Self::open(config, workspace, None, Some(session_id))
    }

    /// Resumes a persisted session by its protocol id for the ACP session/load
    /// entry point, adopting the existing transcript instead of minting a new one.
    pub(crate) fn for_resume(
        config: &CoreConfig,
        workspace: &Path,
        session_id: &str,
    ) -> Result<Self, SessionError> {
        Self::open(config, workspace, Some(session_id), None)
    }

    fn open(
        config: &CoreConfig,
        workspace: &Path,
        resume: Option<&str>,
        session_id: Option<ProviderSessionId>,
    ) -> Result<Self, SessionError> {
        #[cfg(not(test))]
        let store_result = SessionStore::for_workspace(&config.session.persist_dir, workspace);
        #[cfg(test)]
        let store_result = SessionStore::for_test_workspace(&config.session.persist_dir, workspace);
        let store = match store_result {
            Ok(store) => Some(store),
            // Resume cannot proceed without the store, but a fresh turn can:
            // degrade to a non-resumable session instead of refusing to run.
            Err(error) => {
                if resume.is_some() {
                    return Err(error);
                }
                tracing::warn!("session persistence disabled: {error}");
                None
            }
        };
        let workspace_scope = store
            .as_ref()
            .map(|store| store.workspace_scope().to_string())
            .unwrap_or_else(|| workspace.to_string_lossy().into_owned());
        let mut live_lease = None;
        let record = match (resume, store.as_ref()) {
            (Some(value), Some(store)) => {
                let session_id = ProviderSessionId::parse(value)?;
                live_lease = store.acquire_resume_live_lease(&session_id)?;
                let mut resumed = match store.load(&session_id) {
                    Ok(resumed) => resumed,
                    Err(error) => {
                        if let Some(lease) = live_lease.as_ref() {
                            if let Err(cleanup_error) =
                                store.cleanup_failed_resume(&session_id, lease)
                            {
                                tracing::warn!(
                                    "failed resume activity cleanup failed: {cleanup_error}"
                                );
                            }
                        }
                        return Err(error);
                    }
                };
                // A resumed session is being interacted with right now, but its
                // envelope still carries the clock of the last turn before the
                // idle gap. Without a re-commit here, the task budget's
                // least-recently-interacted ordering sees the oldest session of
                // this user, so a sweep from any other process can reclaim it
                // mid-turn; this turn's own commit then fails with Conflict and
                // the work is lost.
                //
                // Best effort on purpose: refusing the resume when the stamp
                // cannot be written would block the current interaction, which
                // the budget contract forbids. The turn's own commit retries it.
                if config.session.auto_persist {
                    if let Err(error) = store.persist(&mut resumed) {
                        tracing::warn!("resumed session clock not refreshed: {error}");
                    }
                }
                resumed
            }
            _ => PersistedSession::new(
                session_id.unwrap_or_default(),
                workspace_scope.clone(),
                String::new(),
                Vec::new(),
            ),
        };
        Ok(Self {
            store,
            live_lease,
            workspace_scope,
            record,
            auto_persist: config.session.auto_persist,
            resumed: resume.is_some(),
            notice: PersistenceNotice::default(),
        })
    }

    pub(crate) fn workspace_scope(&self) -> &str {
        &self.workspace_scope
    }

    pub(crate) fn finalize_model(&mut self, resolved_model: &str, explicit_override: bool) {
        if !self.resumed || explicit_override || self.record.model.is_empty() {
            self.record.model = resolved_model.to_string();
        }
    }

    pub(crate) fn refresh_background_compaction(&mut self, engine: &mut CoshCore) {
        if !self.auto_persist
            || !self.record.was_loaded
            || self.record.session_id.as_str() != engine.session_id
        {
            return;
        }
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let saved = match store.load(&self.record.session_id) {
            Ok(saved) => saved,
            Err(error) => {
                tracing::warn!(error = ?error, "background compaction refresh failed");
                return;
            }
        };
        if saved.generation <= self.record.generation
            || saved.compaction_revision <= engine.compaction.revision()
        {
            return;
        }
        // Compare the durable redaction without changing live provider context.
        let mut persisted_messages = engine.messages.clone();
        crate::redaction::redact_messages(&mut persisted_messages);
        if crate::compaction::source_digest(&persisted_messages)
            != crate::compaction::source_digest(&saved.messages)
        {
            tracing::warn!("background compaction transcript differs from live session");
            return;
        }
        engine
            .compaction
            .load_state(saved.compaction.clone(), saved.compaction_revision);
        self.record = saved;
    }

    pub(crate) fn persist(&mut self, engine: &CoshCore) -> Result<(), SessionError> {
        if !self.auto_persist || self.store.is_none() {
            return Ok(());
        }
        self.record.messages = engine.messages.clone();
        self.record.model = engine.model.clone();
        // Emergency in-run compaction updates the projection in memory; it
        // commits together with the transcript it belongs to.
        self.record.compaction = engine.compaction.state().cloned();
        // The revision clock is persisted even when no projection survives, so
        // the next commit cannot reuse an already published revision.
        self.record.compaction_revision = engine.compaction.revision();
        self.commit()
    }

    /// Commits the current record once. Split out of [`Self::persist`] so the
    /// retry contract is testable without building a provider-backed engine.
    pub(crate) fn commit(&mut self) -> Result<(), SessionError> {
        if !self.auto_persist {
            return Ok(());
        }
        let Some(store) = self.store.as_ref() else {
            return Ok(());
        };
        let result = (|| {
            if self.live_lease.is_none() {
                self.live_lease = store.acquire_live_lease(&self.record.session_id)?;
            }
            store.persist(&mut self.record)
        })();
        if let Err(error) = &result {
            self.notice.record(error);
            if let Some(lease) = self.live_lease.as_ref() {
                match store.cleanup_rejected_commit(&self.record.session_id, lease) {
                    Ok(true) => {
                        self.live_lease.take();
                    }
                    Ok(false) => {}
                    Err(cleanup_error) => tracing::warn!(
                        "uncommitted session activity cleanup failed: {cleanup_error}"
                    ),
                }
            }
        }
        result
    }

    pub(crate) fn resumable(&self) -> bool {
        self.auto_persist && self.store.is_some()
    }

    /// Runs idle-boundary automatic compaction when the soft threshold is
    /// crossed, with per-context-revision failure suppression.
    /// Emits a background-compaction recommendation when the soft threshold
    /// is crossed at an idle boundary.
    ///
    /// The recommendation is a cheap synchronous status line; the shell owns
    /// the actual compactor process (`cosh-core --compact`) so this process
    /// can exit and the user gets the normal shell prompt back immediately.
    /// The payload carries the context revision (generation + projection
    /// revision) so the shell can suppress retrigger loops per revision.
    pub(crate) fn recommend_auto_compaction<W: io::Write>(
        &self,
        engine: &mut CoshCore,
        writer: &mut W,
    ) {
        let policy = &engine.config.session.compaction;
        if engine.trial_access.is_some()
            || crate::auth::trial_policy::decide(
                &engine.config,
                &crate::auth::trial_policy::ProviderIntent::from_env(),
                true,
                true,
                true,
            ) == crate::auth::trial_policy::TrialDecision::EcsIdentity
        {
            return;
        }
        if !policy.enabled || !policy.auto || !self.resumable() {
            return;
        }
        let capability = ModelCapability::resolve(
            policy,
            engine.config.agent.session_token_limit,
            &engine.model,
        );
        let prefix_tokens = engine.estimate_prefix_tokens();
        let budget = ContextBudget::compute(capability, prefix_tokens, policy);
        let history_tokens = engine.effective_history_tokens(prefix_tokens);
        if !budget.over_trigger(history_tokens) {
            return;
        }
        let compacted_through = engine
            .compaction
            .state()
            .map(|state| state.compacted_through)
            .unwrap_or(0);
        match crate::compaction::has_new_compactable_prefix(
            &engine.messages,
            policy.preserve_recent_runs,
            compacted_through,
        ) {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                tracing::warn!(
                    session_id = %self.record.session_id,
                    "automatic compaction preflight rejected the transcript: {error}"
                );
                return;
            }
        }
        // Bind to the durable revision clock, not the live projection: this
        // recommendation is emitted right after a persist, and a projection
        // that sanitization later rejects must not make it look stale.
        let projection_revision = engine.compaction.revision();
        // Versioned protocol: the shell must be able to bind the recommendation
        // to the exact session and context revision it was emitted for, and
        // reject anything malformed. Field order is fixed:
        //   compaction_recommended_v1:<session-id>:<generation>:<revision>:<history>:<usable>
        engine.emit(
            writer,
            &OutputMessage::system_status(&format!(
                "compaction_recommended_v1:{}:{}:{}:{}:{}",
                self.record.session_id,
                self.record.generation,
                projection_revision,
                history_tokens,
                budget.usable_history
            )),
        );
    }
}

pub(crate) struct TurnFailure {
    pub(crate) message: String,
    pub(crate) error_code: Option<&'static str>,
    pub(crate) max_turns: Option<u32>,
    pub(crate) session_error_code: Option<&'static str>,
}

impl TurnFailure {
    pub(crate) fn output_message(&self, session_id: &str) -> OutputMessage {
        match self.session_error_code {
            Some(code) => {
                OutputMessage::session_result_error(session_id, &self.message, code, "persist")
            }
            None if self.error_code == Some("max_turns") && self.max_turns.is_some() => {
                OutputMessage::max_turns_result_error(
                    session_id,
                    &self.message,
                    self.max_turns.unwrap_or_default(),
                )
            }
            None => {
                OutputMessage::result_error_with_code(session_id, &self.message, self.error_code)
            }
        }
    }
}

pub(crate) fn combine_turn_and_persist(
    turn: Result<AgentTurnOutcome, String>,
    persist: Result<(), SessionError>,
) -> Result<AgentTurnOutcome, TurnFailure> {
    match (turn, persist) {
        (Ok(AgentTurnOutcome::MaxTurns { limit }), Ok(())) => Err(TurnFailure {
            message: crate::core::max_turns_error(limit),
            error_code: Some("max_turns"),
            max_turns: Some(limit),
            session_error_code: None,
        }),
        (Ok(outcome), Ok(())) => Ok(outcome),
        (Err(turn_error), Ok(())) => Err(TurnFailure {
            error_code: None,
            max_turns: None,
            message: turn_error,
            session_error_code: None,
        }),
        (Ok(AgentTurnOutcome::MaxTurns { limit }), Err(persist_error)) => Err(TurnFailure {
            message: format!(
                "{}; session persistence failed [{}]: {persist_error}",
                crate::core::max_turns_error(limit),
                persist_error.code()
            ),
            error_code: None,
            max_turns: None,
            session_error_code: Some(persist_error.code()),
        }),
        (Ok(_), Err(persist_error)) => Err(TurnFailure {
            message: format!(
                "session persistence failed [{}]: {persist_error}",
                persist_error.code()
            ),
            error_code: None,
            max_turns: None,
            session_error_code: Some(persist_error.code()),
        }),
        (Err(turn_error), Err(persist_error)) => Err(TurnFailure {
            message: format!(
                "{turn_error}; session persistence failed [{}]: {persist_error}",
                persist_error.code()
            ),
            error_code: None,
            max_turns: None,
            session_error_code: Some(persist_error.code()),
        }),
    }
}
