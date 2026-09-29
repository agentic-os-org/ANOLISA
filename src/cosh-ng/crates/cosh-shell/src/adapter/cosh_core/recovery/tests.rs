use super::*;

const ACTIVE_ID: &str = "11111111-1111-4111-8111-111111111111";
const SELECTED_ID: &str = "00000000-0000-4000-8000-000000000000";
const NEW_ID: &str = "22222222-2222-4222-8222-222222222222";
const SCOPE: &str = "/tmp";

fn active_state() -> Arc<Mutex<SessionRuntimeState>> {
    Arc::new(Mutex::new(SessionRuntimeState::with_active(
        ACTIVE_ID, SCOPE,
    )))
}

fn selected_state() -> Arc<Mutex<SessionRuntimeState>> {
    let state = active_state();
    {
        let mut state = state.lock().expect("session state");
        state.recovery.state = SessionRecoveryState::Selected;
        state.recovery.selected_session_id = Some(SELECTED_ID.to_string());
        state.recovery.selected_workspace_scope = Some(SCOPE.to_string());
    }
    state
}

fn begin_selected(state: &Arc<Mutex<SessionRuntimeState>>) -> SessionResumeAttempt {
    let attempt = begin_session_attempt(state, Some(SELECTED_ID), SCOPE);
    assert!(matches!(attempt, SessionResumeAttempt::Selected { .. }));
    attempt
}

fn begin_active(state: &Arc<Mutex<SessionRuntimeState>>) -> SessionResumeAttempt {
    let attempt = begin_session_attempt(state, Some(ACTIVE_ID), SCOPE);
    assert!(matches!(attempt, SessionResumeAttempt::Active { .. }));
    attempt
}

#[test]
fn completed_non_resumable_restore_transitions_to_failed() {
    let state = selected_state();
    let attempt = begin_selected(&state);

    let outcome = commit_pending_session_for_scope(
        true,
        false,
        &state,
        &Arc::new(Mutex::new(None)),
        SCOPE,
        Some(false),
        &attempt,
    );

    assert!(matches!(outcome, SessionCommitOutcome::RestoreFailed(_)));
    let state = state.lock().expect("session state");
    assert_eq!(state.active_session_id(), Some(ACTIVE_ID));
    assert_eq!(state.active_workspace_scope(), Some(SCOPE));
    assert_eq!(state.recovery.state, SessionRecoveryState::Failed);
    assert_eq!(state.recovery.selected_session_id, None);
    assert_eq!(
        state
            .recovery
            .last_error
            .as_ref()
            .map(|error| error.message.as_str()),
        Some("provider session is not resumable")
    );
}

#[test]
fn completed_non_resumable_turn_discards_matching_active_session() {
    let state = active_state();
    let attempt = begin_active(&state);

    let outcome = discard_non_resumable_session(&attempt, &state);

    assert_eq!(outcome, SessionCommitOutcome::Continue);
    let state = state.lock().expect("session state");
    assert_eq!(state.active_session_id(), None);
    assert_eq!(state.active_workspace_scope(), None);
    assert_eq!(state.recovery.state, SessionRecoveryState::None);
}

#[test]
fn missing_pending_id_preserves_active_without_explicit_non_resumable_signal() {
    for resumable in [None, Some(true)] {
        let state = active_state();
        let attempt = begin_active(&state);

        let outcome = commit_pending_session_for_scope(
            true,
            false,
            &state,
            &Arc::new(Mutex::new(None)),
            SCOPE,
            resumable,
            &attempt,
        );

        assert_eq!(outcome, SessionCommitOutcome::Continue);
        assert_eq!(
            state.lock().expect("session state").active_session_id(),
            Some(ACTIVE_ID)
        );
    }
}

#[test]
fn missing_pending_id_fails_only_the_matching_restore_attempt() {
    let state = selected_state();
    let attempt = begin_selected(&state);

    let outcome = commit_pending_session_for_scope(
        true,
        false,
        &state,
        &Arc::new(Mutex::new(None)),
        SCOPE,
        Some(true),
        &attempt,
    );

    assert!(matches!(outcome, SessionCommitOutcome::RestoreFailed(_)));
    let state = state.lock().expect("session state");
    assert_eq!(state.active_session_id(), Some(ACTIVE_ID));
    assert_eq!(state.recovery.state, SessionRecoveryState::Failed);
    assert_eq!(state.recovery.selected_session_id, None);
}

#[test]
fn mismatched_pending_id_fails_restore_without_committing_provider_identity() {
    let state = selected_state();
    let attempt = begin_selected(&state);

    let outcome = commit_pending_session_for_scope(
        true,
        false,
        &state,
        &Arc::new(Mutex::new(Some(NEW_ID.to_string()))),
        SCOPE,
        Some(true),
        &attempt,
    );

    let SessionCommitOutcome::RestoreFailed(error) = outcome else {
        panic!("identity mismatch must fail");
    };
    assert!(error.message.contains("identity mismatch"));
    let state = state.lock().expect("session state");
    assert_eq!(state.active_session_id(), Some(ACTIVE_ID));
    assert_eq!(state.recovery.state, SessionRecoveryState::Failed);
}

#[test]
fn successful_selected_restore_commits_atomically_and_clears_selection() {
    let state = selected_state();
    let attempt = begin_selected(&state);

    let outcome = commit_pending_session_for_scope(
        true,
        false,
        &state,
        &Arc::new(Mutex::new(Some(SELECTED_ID.to_string()))),
        SCOPE,
        Some(true),
        &attempt,
    );

    assert_eq!(outcome, SessionCommitOutcome::Continue);
    let state = state.lock().expect("session state");
    assert_eq!(state.active_session_id(), Some(SELECTED_ID));
    assert_eq!(state.active_workspace_scope(), Some(SCOPE));
    assert_eq!(state.recovery.state, SessionRecoveryState::Active);
    assert_eq!(state.recovery.selected_session_id, None);
}

#[test]
fn context_limit_failure_retains_the_persisted_session_for_compaction() {
    let events = vec![AgentEvent::AgentFailed {
        run_id: "run-1".to_string(),
        error: "context_limit: effective context exceeds the emergency threshold".to_string(),
        error_code: None,
        max_turns: None,
    }];

    assert!(retain_context_session(&events, None, Some(true)));
}

#[test]
fn max_turns_failure_retains_the_persisted_session_for_continuation() {
    let events = vec![AgentEvent::AgentFailed {
        run_id: "run-1".to_string(),
        error: "Agent exceeded max turns (50)".to_string(),
        error_code: Some("max_turns".to_string()),
        max_turns: Some(50),
    }];

    assert!(retain_context_session(&events, None, Some(true)));
    assert!(!retain_context_session(
        &events,
        Some("persist"),
        Some(true)
    ));
    assert!(!retain_context_session(&events, None, None));
    assert_eq!(max_turn_limit(&events), Some(50));
}

#[test]
fn max_turn_limit_rejects_incomplete_or_invalid_metadata() {
    for (error_code, max_turns) in [
        (Some("max_turns"), Some(0)),
        (Some("max_turns"), None),
        (Some("provider"), Some(50)),
        (None, Some(50)),
    ] {
        let events = vec![AgentEvent::AgentFailed {
            run_id: "run-1".to_string(),
            error: "arbitrary display text".to_string(),
            error_code: error_code.map(str::to_string),
            max_turns,
        }];
        assert_eq!(max_turn_limit(&events), None);
    }
}

#[test]
fn ordinary_failure_does_not_retain_a_provider_session() {
    let events = vec![AgentEvent::AgentFailed {
        run_id: "run-1".to_string(),
        error: "API error 500".to_string(),
        error_code: None,
        max_turns: None,
    }];

    assert!(!retain_context_session(&events, None, Some(true)));
}

#[test]
fn persist_failure_does_not_retain_a_context_limited_session() {
    let events = vec![AgentEvent::AgentFailed {
        run_id: "run-1".to_string(),
        error: "context_limit: effective context exceeds the emergency threshold".to_string(),
        error_code: None,
        max_turns: None,
    }];
    let state = Arc::new(Mutex::new(SessionRuntimeState::default()));
    let attempt = begin_session_attempt(&state, None, SCOPE);
    let retain = retain_context_session(&events, Some("persist"), Some(true));

    assert!(!retain);
    assert_eq!(
        commit_pending_session_for_scope(
            retain,
            !retain,
            &state,
            &Arc::new(Mutex::new(Some(NEW_ID.to_string()))),
            SCOPE,
            Some(true),
            &attempt,
        ),
        SessionCommitOutcome::Continue
    );
    assert_eq!(
        state.lock().expect("session state").active_session_id(),
        None
    );
}

#[test]
fn fresh_non_resumable_turn_preserves_unattempted_active_and_selection() {
    let state = selected_state();
    let attempt = begin_session_attempt(&state, None, SCOPE);

    let outcome = commit_pending_session_for_scope(
        true,
        false,
        &state,
        &Arc::new(Mutex::new(None)),
        SCOPE,
        Some(false),
        &attempt,
    );

    assert_eq!(outcome, SessionCommitOutcome::Continue);
    let state = state.lock().expect("session state");
    assert_eq!(state.active_session_id(), Some(ACTIVE_ID));
    assert_eq!(state.recovery.state, SessionRecoveryState::Selected);
    assert_eq!(
        state.recovery.selected_session_id.as_deref(),
        Some(SELECTED_ID)
    );
}

#[test]
fn active_load_failure_preserves_an_unrelated_selection() {
    let state = active_state();
    let attempt = begin_active(&state);
    {
        let mut state = state.lock().expect("session state");
        state.recovery.state = SessionRecoveryState::Selected;
        state.recovery.selected_session_id = Some(SELECTED_ID.to_string());
        state.recovery.selected_workspace_scope = Some("/other".to_string());
    }
    let events = vec![AgentEvent::AgentFailed {
        run_id: "run".to_string(),
        error: "session load failed".to_string(),
        error_code: None,
        max_turns: None,
    }];

    invalidate_resume_on_session_failure(
        &attempt,
        Some("not_found"),
        Some("load"),
        &events,
        &state,
    );

    let state = state.lock().expect("session state");
    assert_eq!(state.active_session_id(), None);
    assert_eq!(state.recovery.state, SessionRecoveryState::Selected);
    assert_eq!(
        state.recovery.selected_session_id.as_deref(),
        Some(SELECTED_ID)
    );
    assert_eq!(
        state
            .recovery
            .last_error
            .as_ref()
            .map(|error| error.code.as_str()),
        Some("not_found")
    );
}

#[test]
fn provider_error_text_cannot_impersonate_a_session_load_failure() {
    let state = active_state();
    let attempt = begin_active(&state);
    let events = vec![AgentEvent::AgentFailed {
        run_id: "run".to_string(),
        error: "provider returned a user string containing [not_found]".to_string(),
        error_code: None,
        max_turns: None,
    }];

    invalidate_resume_on_session_failure(&attempt, None, None, &events, &state);

    assert_eq!(
        state.lock().expect("session state").active_session_id(),
        Some(ACTIVE_ID)
    );
}

#[test]
fn selected_structured_failures_preserve_typed_recovery_metadata() {
    for (code, phase, message, hint_fragment) in [
        (
            "not_found",
            "load",
            "session recovery failed [not_found]: selected session disappeared",
            "Refresh the session list",
        ),
        (
            "conflict",
            "persist",
            "session persistence failed [conflict]: selected session changed",
            "Resolve the persistence failure",
        ),
    ] {
        let state = selected_state();
        let attempt = begin_selected(&state);
        let terminal_events = vec![AgentEvent::AgentFailed {
            run_id: "run".to_string(),
            error: message.to_string(),
            error_code: None,
            max_turns: None,
        }];

        invalidate_resume_on_session_failure(
            &attempt,
            Some(code),
            Some(phase),
            &terminal_events,
            &state,
        );
        let outcome = commit_pending_session_for_scope(
            false,
            true,
            &state,
            &Arc::new(Mutex::new(None)),
            SCOPE,
            Some(true),
            &attempt,
        );

        assert_eq!(outcome, SessionCommitOutcome::Continue);
        let state = state.lock().expect("session state");
        assert_eq!(state.active_session_id(), Some(ACTIVE_ID));
        assert_eq!(state.recovery.state, SessionRecoveryState::Failed);
        assert_eq!(state.recovery.selected_session_id, None);
        let error = state.recovery.last_error.as_ref().expect("typed failure");
        assert_eq!(error.code, code);
        assert_eq!(error.message, message);
        assert!(error
            .hint
            .as_deref()
            .is_some_and(|hint| hint.contains(hint_fragment)));
    }
}

#[test]
fn stale_active_failure_cannot_clear_a_newer_committed_session() {
    let state = active_state();
    let old_attempt = begin_active(&state);
    let new_attempt = begin_session_attempt(&state, None, SCOPE);
    let new_outcome = commit_pending_session_for_scope(
        true,
        false,
        &state,
        &Arc::new(Mutex::new(Some(NEW_ID.to_string()))),
        SCOPE,
        Some(true),
        &new_attempt,
    );
    assert_eq!(new_outcome, SessionCommitOutcome::Continue);

    invalidate_resume_on_session_failure(
        &old_attempt,
        Some("not_found"),
        Some("load"),
        &[],
        &state,
    );
    let stale_outcome = commit_pending_session_for_scope(
        false,
        true,
        &state,
        &Arc::new(Mutex::new(None)),
        SCOPE,
        Some(false),
        &old_attempt,
    );

    assert_eq!(stale_outcome, SessionCommitOutcome::StaleAttempt);
    assert_eq!(
        state.lock().expect("session state").active_session_id(),
        Some(NEW_ID)
    );
}

#[test]
fn stale_active_failure_cannot_clear_a_new_generation_with_the_same_id() {
    let state = active_state();
    let old_attempt = begin_active(&state);
    let new_attempt = begin_session_attempt(&state, None, SCOPE);
    assert_eq!(
        commit_pending_session_for_scope(
            true,
            false,
            &state,
            &Arc::new(Mutex::new(Some(ACTIVE_ID.to_string()))),
            SCOPE,
            Some(true),
            &new_attempt,
        ),
        SessionCommitOutcome::Continue
    );

    invalidate_resume_on_session_failure(
        &old_attempt,
        Some("not_found"),
        Some("load"),
        &[],
        &state,
    );

    assert_eq!(
        state.lock().expect("session state").active_session_id(),
        Some(ACTIVE_ID)
    );
}

#[test]
fn stale_failure_cannot_clear_a_new_attempt_for_the_same_selected_id() {
    let state = selected_state();
    let old_attempt = begin_selected(&state);
    let new_attempt = begin_selected(&state);

    assert_eq!(
        mark_recovery_failure(&state, &old_attempt, "old runner failed"),
        None
    );
    {
        let state = state.lock().expect("session state");
        assert_eq!(state.recovery.state, SessionRecoveryState::Restoring);
        assert_eq!(
            state.recovery.selected_session_id.as_deref(),
            Some(SELECTED_ID)
        );
    }

    assert!(mark_recovery_failure(&state, &new_attempt, "new runner failed").is_some());
    assert_eq!(
        state.lock().expect("session state").recovery.state,
        SessionRecoveryState::Failed
    );
}

#[test]
fn stale_success_is_suppressed_from_the_terminal_stream() {
    let state = selected_state();
    let old_attempt = begin_selected(&state);
    let _new_attempt = begin_selected(&state);
    let outcome = commit_pending_session_for_scope(
        true,
        false,
        &state,
        &Arc::new(Mutex::new(Some(SELECTED_ID.to_string()))),
        SCOPE,
        Some(true),
        &old_attempt,
    );

    assert_eq!(outcome, SessionCommitOutcome::StaleAttempt);
    let events = terminal_events_for_session_commit(
        "old-run",
        vec![AgentEvent::AgentCompleted {
            run_id: "old-run".to_string(),
            summary: "stale".to_string(),
        }],
        outcome,
    );
    assert!(events.is_empty());
}

#[test]
fn replacing_selection_invalidates_every_terminal_from_the_old_attempt() {
    let state = selected_state();
    let old_attempt = begin_selected(&state);
    {
        let mut state = state.lock().expect("session state");
        state.select_session(NEW_ID.to_string(), SCOPE.to_string());
    }

    invalidate_resume_on_session_failure(
        &old_attempt,
        Some("not_found"),
        Some("load"),
        &[AgentEvent::AgentFailed {
            run_id: "old-run".to_string(),
            error: "old selected session disappeared".to_string(),
            error_code: None,
            max_turns: None,
        }],
        &state,
    );
    assert_eq!(
        discard_non_resumable_session(&old_attempt, &state),
        SessionCommitOutcome::StaleAttempt
    );
    assert_eq!(
        commit_pending_session_for_scope(
            true,
            false,
            &state,
            &Arc::new(Mutex::new(Some(SELECTED_ID.to_string()))),
            SCOPE,
            Some(true),
            &old_attempt,
        ),
        SessionCommitOutcome::StaleAttempt
    );
    let state = state.lock().expect("session state");
    assert_eq!(state.active_session_id(), Some(ACTIVE_ID));
    assert_eq!(state.recovery.state, SessionRecoveryState::Selected);
    assert_eq!(state.recovery.selected_session_id.as_deref(), Some(NEW_ID));
    assert_eq!(state.selected_attempt_generation, None);
}

fn capture_logs<T>(test: impl FnOnce(Arc<Mutex<Vec<u8>>>) -> T) -> T {
    #[derive(Clone)]
    struct TestWriter(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for TestWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let buf = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer({
            let buf = Arc::clone(&buf);
            move || TestWriter(Arc::clone(&buf))
        })
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_level(false)
        .with_target(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    test(buf)
}

#[test]
fn fail_selection_logs_recovery_failed() {
    let state = selected_state();
    capture_logs(|buf| {
        state
            .lock()
            .expect("session state")
            .fail_selection(SessionErrorInfo {
                code: "not_found".to_string(),
                message: "selected session disappeared".to_string(),
                recoverable: true,
                hint: None,
            });
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(output.contains("session recovery failed"));
        assert!(output.contains("not_found"));
        assert!(output.contains("selection validation failed"));
    });
    assert_eq!(
        state.lock().expect("session state").recovery.state,
        SessionRecoveryState::Failed
    );
}

#[test]
fn selected_load_failure_logs_recovery_failed() {
    let state = selected_state();
    let attempt = begin_selected(&state);
    let events = vec![AgentEvent::AgentFailed {
        run_id: "run".to_string(),
        error: "selected session disappeared".to_string(),
        error_code: None,
        max_turns: None,
    }];
    capture_logs(|buf| {
        invalidate_resume_on_session_failure(
            &attempt,
            Some("not_found"),
            Some("load"),
            &events,
            &state,
        );
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(output.contains("session recovery failed"));
        assert!(output.contains("session load failed"));
    });
    assert_eq!(
        state.lock().expect("session state").recovery.state,
        SessionRecoveryState::Failed
    );
}

#[test]
fn identity_mismatch_logs_recovery_failed() {
    let state = selected_state();
    let attempt = begin_selected(&state);
    let pending = Arc::new(Mutex::new(Some(NEW_ID.to_string())));
    let outcome = capture_logs(|buf| {
        let outcome = commit_pending_session_for_scope(
            true,
            false,
            &state,
            &pending,
            SCOPE,
            Some(true),
            &attempt,
        );
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(output.contains("session recovery failed"));
        assert!(output.contains("identity mismatch"));
        outcome
    });
    assert!(matches!(outcome, SessionCommitOutcome::RestoreFailed(_)));
    assert_eq!(
        state.lock().expect("session state").recovery.state,
        SessionRecoveryState::Failed
    );
}

#[test]
fn fresh_identity_mismatch_preserves_last_error() {
    let state = Arc::new(Mutex::new(SessionRuntimeState::default()));
    let attempt = begin_session_attempt(&state, None, SCOPE);
    assert!(matches!(attempt, SessionResumeAttempt::Fresh { .. }));

    let outcome = {
        let mut state = state.lock().expect("session state");
        reject_resume_identity_mismatch(&mut state, &attempt, ACTIVE_ID, NEW_ID)
    };

    assert!(matches!(outcome, SessionCommitOutcome::RestoreFailed(_)));
    let state = state.lock().expect("session state");
    assert_eq!(state.recovery.state, SessionRecoveryState::None);
    assert!(state
        .recovery
        .last_error
        .as_ref()
        .unwrap()
        .message
        .contains("identity mismatch"));
}

#[test]
fn non_resumable_session_logs_recovery_failed() {
    let state = selected_state();
    let attempt = begin_selected(&state);
    let pending = Arc::new(Mutex::new(Some(SELECTED_ID.to_string())));
    let outcome = capture_logs(|buf| {
        let outcome = commit_pending_session_for_scope(
            true,
            false,
            &state,
            &pending,
            SCOPE,
            Some(false),
            &attempt,
        );
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(output.contains("session recovery failed"));
        assert!(output.contains("session not resumable"));
        outcome
    });
    assert!(matches!(outcome, SessionCommitOutcome::RestoreFailed(_)));
    assert_eq!(
        state.lock().expect("session state").recovery.state,
        SessionRecoveryState::Failed
    );
}

#[test]
fn failed_selection_invalidates_the_attempt_it_superseded() {
    let state = selected_state();
    let old_attempt = begin_selected(&state);
    {
        let mut state = state.lock().expect("session state");
        state.fail_selection(SessionErrorInfo {
            code: "not_found".to_string(),
            message: "selected session disappeared".to_string(),
            recoverable: true,
            hint: None,
        });
    }

    assert_eq!(
        commit_pending_session_for_scope(
            true,
            false,
            &state,
            &Arc::new(Mutex::new(Some(SELECTED_ID.to_string()))),
            SCOPE,
            Some(true),
            &old_attempt,
        ),
        SessionCommitOutcome::StaleAttempt
    );
    let state = state.lock().expect("session state");
    assert_eq!(state.active_session_id(), Some(ACTIVE_ID));
    assert_eq!(state.recovery.state, SessionRecoveryState::Failed);
    assert_eq!(state.recovery.selected_session_id, None);
    assert_eq!(
        state
            .recovery
            .last_error
            .as_ref()
            .map(|error| error.code.as_str()),
        Some("not_found")
    );
}

#[test]
fn start_fresh_session_detaches_bindings_and_invalidates_in_flight_attempts() {
    let state = selected_state();
    // An attempt is already resuming the selected session when the user
    // detaches, mirroring a late-arriving commit from the old conversation.
    let old_attempt = begin_selected(&state);

    let previous = state.lock().expect("session state").start_fresh_session();
    // The active committed id is preferred over the pending selection.
    assert_eq!(previous.as_deref(), Some(ACTIVE_ID));
    {
        let state = state.lock().expect("session state");
        assert_eq!(state.active_session_id(), None);
        assert_eq!(state.recovery.state, SessionRecoveryState::None);
        assert_eq!(state.recovery.selected_session_id, None);
        assert_eq!(state.recovery.selected_workspace_scope, None);
        assert!(state.recovery.last_error.is_none());
    }

    // The superseded attempt must commit as stale, never re-binding a session.
    let outcome = commit_pending_session_for_scope(
        true,
        false,
        &state,
        &Arc::new(Mutex::new(Some(SELECTED_ID.to_string()))),
        SCOPE,
        Some(true),
        &old_attempt,
    );
    assert_eq!(outcome, SessionCommitOutcome::StaleAttempt);
    assert_eq!(
        state.lock().expect("session state").active_session_id(),
        None
    );
}

#[test]
fn start_fresh_session_reports_selection_when_no_session_is_active() {
    let state = Arc::new(Mutex::new(SessionRuntimeState::default()));
    {
        let mut state = state.lock().expect("session state");
        state.select_session(SELECTED_ID.to_string(), SCOPE.to_string());
    }

    let previous = state.lock().expect("session state").start_fresh_session();
    assert_eq!(previous.as_deref(), Some(SELECTED_ID));
    let state = state.lock().expect("session state");
    assert_eq!(state.recovery.state, SessionRecoveryState::None);
    assert_eq!(state.recovery.selected_session_id, None);
}

#[test]
fn start_fresh_session_is_idempotent_without_any_binding() {
    let state = Arc::new(Mutex::new(SessionRuntimeState::default()));
    let previous = state.lock().expect("session state").start_fresh_session();
    assert_eq!(previous, None);
    assert_eq!(
        state.lock().expect("session state").recovery.state,
        SessionRecoveryState::None
    );
}

#[test]
fn fresh_attempt_returns_a_superseded_restore_to_selected() {
    let state = selected_state();
    let old_attempt = begin_selected(&state);

    let fresh_attempt = begin_session_attempt(&state, None, SCOPE);

    assert!(matches!(fresh_attempt, SessionResumeAttempt::Fresh { .. }));
    assert_eq!(
        mark_recovery_failure(&state, &old_attempt, "stale restore failed"),
        None
    );
    let state = state.lock().expect("session state");
    assert_eq!(state.recovery.state, SessionRecoveryState::Selected);
    assert_eq!(
        state.recovery.selected_session_id.as_deref(),
        Some(SELECTED_ID)
    );
    assert_eq!(state.selected_attempt_generation, None);
}
