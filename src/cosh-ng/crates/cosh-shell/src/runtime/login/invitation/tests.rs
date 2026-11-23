use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;

fn fixture() -> (tempfile::TempDir, GuideStore, RuntimeState) {
    #[cfg(target_os = "linux")]
    let dir = tempfile::tempdir_in("/dev/shm").unwrap();
    #[cfg(not(target_os = "linux"))]
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let base = dir.path().canonicalize().unwrap();
    let store = GuideStore::open(base.join("guide")).unwrap();
    let runtime = RuntimeState::open(&base).unwrap();
    (dir, store, runtime)
}
fn ready_state() -> InlineState {
    let exchange = crate::raw_input::PromptEpochExchange::default();
    let epoch = exchange.open();
    exchange.publish(epoch, b"actual-shell$ ");
    let mut state = InlineState {
        shell_at_prompt: true,
        shell_session_id: Some("invitation-session".into()),
        ..Default::default()
    };
    state.startup_session.login_invitation.pending = true;
    state
        .startup_session
        .login_invitation
        .observe_prompt(exchange.ready_snapshot(epoch).as_ref());
    state
}

#[test]
fn idle_boundary_renders_claimed_invitation_once_and_requests_the_real_prompt() {
    let (_dir, store, runtime) = fixture();
    let mut state = ready_state();
    let mut output = Vec::new();
    render_with(&mut state, &mut output, |language, session| {
        claim_offer(store, runtime, language, "host-a", session, 10)
    })
    .unwrap();
    assert!(String::from_utf8_lossy(&output).contains("cosh login set"));
    assert!(state.startup_session.login_invitation.shown);
    assert!(state.trigger_pty_prompt);
    assert!(!String::from_utf8_lossy(&output).contains("actual-shell$"));
    render_with(&mut state, &mut output, |_, _| {
        panic!("session must not retry")
    })
    .unwrap();
}

#[test]
fn pending_invitation_never_crosses_input_busy_or_compaction_boundaries() {
    let mut state = ready_state();
    let input = ShellEvent::user_input_intercepted("session", "new question");
    observe_user_activity(&mut state, &[input]);
    render_with(&mut state, &mut Vec::new(), |_, _| {
        panic!("new input cancels the old candidate")
    })
    .unwrap();
    let mut state = ready_state();
    state.personalization.shell_input_active = true;
    render_with(&mut state, &mut Vec::new(), |_, _| {
        panic!("draft belongs to user")
    })
    .unwrap();
    assert!(!state.startup_session.login_invitation.pending);
    let mut state = ready_state();
    state
        .control
        .session_mut()
        .compaction_mut()
        .note_recommendation("01234567-89ab-4def-8123-0123456789ab:1:2:100:200");
    render_with(&mut state, &mut Vec::new(), |_, _| {
        panic!("compaction owns the idle boundary")
    })
    .unwrap();
    assert!(!state.startup_session.login_invitation.pending);
}

#[test]
fn guide_disabled_or_failed_claim_never_gets_a_replacement_invitation() {
    let (_dir, store, runtime) = fixture();
    store.set_guide(Some(&runtime), false).unwrap();
    assert!(claim_offer(store, runtime, Language::EnUs, "host-a", "session", 10).is_none());
    let mut state = ready_state();
    render_with(&mut state, &mut Vec::new(), |_, _| None).unwrap();
    assert!(!state.startup_session.login_invitation.shown);
    render_with(&mut state, &mut Vec::new(), |_, _| {
        panic!("failed claim must not retry")
    })
    .unwrap();
}

#[test]
fn default_login_has_an_independent_seven_day_budget() {
    let (_dir, store, runtime) = fixture();
    store.initialize_runtime(&runtime).unwrap();
    let first = claim_offer(store, runtime, Language::EnUs, "host-a", "session-a", 10).unwrap();
    let snapshot = first.store.snapshot(Some(&first.runtime)).unwrap();
    assert!(matches!(
        first.store.claim(
            Some(&first.runtime),
            &snapshot,
            ClaimRequest {
                kind: InviteKind::LoginHealth,
                host: "host-a",
                problem: "health",
                session: "session-b",
            },
            11
        ),
        Ok(ClaimDecision::Claimed(_))
    ));
    assert!(claim_offer(
        first.store,
        first.runtime,
        Language::EnUs,
        "host-a",
        "session-c",
        6 * 86_400_000
    )
    .is_none());
    let base = _dir.path().canonicalize().unwrap();
    let store = GuideStore::open(base.join("guide")).unwrap();
    let runtime = RuntimeState::open(&base).unwrap();
    store
        .initialize_runtime(&runtime)
        .expect("reopen existing runtime generation");
    let snapshot = store
        .snapshot(Some(&runtime))
        .expect("read quota generation");
    let result = store.claim(
        Some(&runtime),
        &snapshot,
        ClaimRequest {
            kind: InviteKind::DefaultLogin,
            host: "host-a",
            problem: "default-login",
            session: "session-d",
        },
        7 * 86_400_000 + 11,
    );
    assert!(
        matches!(result, Ok(ClaimDecision::Claimed(_))),
        "expired independent quota: {result:?}"
    );
}

struct FailedWriter;
impl Write for FailedWriter {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("fixture output failure"))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn failed_output_never_marks_shown_or_retries_the_claim() {
    let (_dir, store, runtime) = fixture();
    let mut state = ready_state();
    assert!(
        render_with(&mut state, &mut FailedWriter, |language, session| {
            claim_offer(store, runtime, language, "host-a", session, 10)
        })
        .is_err()
    );
    assert!(!state.startup_session.login_invitation.shown);
    render_with(&mut state, &mut Vec::new(), |_, _| {
        panic!("failed output does not refund the claim")
    })
    .unwrap();
}

#[test]
fn claimed_or_superseded_prompt_never_draws_the_invitation() {
    for claim in [true, false] {
        let exchange = crate::raw_input::PromptEpochExchange::default();
        let epoch = exchange.open();
        exchange.publish(epoch, b"old$ ");
        let mut state = ready_state();
        state.startup_session.login_invitation.prompt = exchange.ready_snapshot(epoch);
        let (_dir, store, runtime) = fixture();
        let mut output = Vec::new();
        render_with(&mut state, &mut output, |language, session| {
            let offer = claim_offer(store, runtime, language, "host-a", session, 10);
            if claim {
                exchange.claim_before_user_write();
            } else {
                exchange.open();
            }
            offer
        })
        .unwrap();
        assert!(output.is_empty());
        assert!(!state.startup_session.login_invitation.shown);
    }
}

#[test]
fn default_cosh_and_external_third_shells_are_not_invitation_targets() {
    for shell in [super::super::COSH_PATH, "/bin/zsh"] {
        let status = super::super::LoginStatus {
            username: "alice".into(),
            uid: 1001,
            current_shell: shell.into(),
            original_bash: None,
            transaction: "unmanaged".into(),
            managed: false,
            conflict: false,
        };
        assert!(!eligible_account(&status));
    }
}

#[test]
fn active_authorization_and_real_shell_commands_take_priority_without_a_late_invitation() {
    let mut state = ready_state();
    let events = govern_agent_events_with_language(
        &[AgentEvent::AuthRequired {
            run_id: "run".into(),
            request_id: "auth".into(),
            reason: "credentials".into(),
            error_message: None,
            providers: Vec::new(),
        }],
        &Policy::default(),
        Language::EnUs,
    )
    .events;
    crate::auth::runtime::record_auth_required(&mut state, &events);
    assert!(crate::auth::runtime::has_auth_operation(&state));
    render_with(&mut state, &mut Vec::new(), |_, _| {
        panic!("authorization owns the interaction")
    })
    .unwrap();
    state.auth.state = None;
    render_with(&mut state, &mut Vec::new(), |_, _| {
        panic!("no promotion after authorization")
    })
    .unwrap();
    let mut state = ready_state();
    let command = ShellEvent::command_started("session", "cmd", "sleep 1", "/", 1);
    state.control.observe_shell_command_activity(&[command]);
    assert!(state.control.shell_busy());
    render_with(&mut state, &mut Vec::new(), |_, _| {
        panic!("command owns the terminal")
    })
    .unwrap();
    assert!(state.startup_session.login_invitation.attempted);
}

#[test]
fn slow_invitation_preparation_stays_pending_without_blocking_the_idle_frame() {
    let mut command = std::process::Command::new("/bin/sh");
    command.args(["-c", "sleep 30"]);
    let mut state = ready_state();
    state.startup_session.login_invitation.preparation =
        Some(prepare::Preparation::fixture(command));
    let input_mode = std::sync::Mutex::new(());
    let guard = input_mode.lock().unwrap();
    render_at_idle(&mut state, &mut Vec::new()).unwrap();
    assert!(
        state.startup_session.login_invitation.pending,
        "slow preparation must not be synchronously consumed or skipped"
    );
    assert!(state.startup_session.login_invitation.preparation.is_some());
    drop(guard);
    assert!(input_mode.try_lock().is_ok());
}

#[test]
fn generation_changes_during_preparation_suppress_the_finished_offer() {
    let (_dir, store, runtime) = fixture();
    let mut state = ready_state();
    let mut output = Vec::new();
    render_with(&mut state, &mut output, |language, session| {
        let prepared = claim_offer(store, runtime, language, "host-a", session, 10)?;
        prepared
            .store
            .set_guide(Some(&prepared.runtime), false)
            .unwrap();
        Some(prepared)
    })
    .unwrap();
    assert!(output.is_empty());
    assert!(!state.startup_session.login_invitation.shown);
}

#[test]
fn completed_answer_waits_for_a_fresh_real_prompt_before_starting_preparation() {
    let exchange = crate::raw_input::PromptEpochExchange::default();
    let epoch = exchange.open();
    exchange.publish(epoch, b"old$ ");
    let mut state = ready_state();
    state.startup_session.login_invitation.prompt = exchange.ready_snapshot(epoch);
    exchange.claim_before_user_write();
    render_at_idle(&mut state, &mut Vec::new()).unwrap();
    assert!(state.startup_session.login_invitation.pending);
    assert!(!state.startup_session.login_invitation.attempted);
    assert!(state.startup_session.login_invitation.preparation.is_none());
}

#[test]
fn user_input_cancels_an_inflight_preparation_and_never_resumes_it() {
    let mut command = std::process::Command::new("/bin/sh");
    command.args(["-c", "sleep 30"]);
    let mut state = ready_state();
    state.startup_session.login_invitation.preparation =
        Some(prepare::Preparation::fixture(command));
    observe_user_activity(
        &mut state,
        &[ShellEvent::user_input_intercepted(
            "session",
            "a new question",
        )],
    );
    assert!(state.startup_session.login_invitation.preparation.is_none());
    assert!(state.startup_session.login_invitation.attempted);
    render_at_idle(&mut state, &mut Vec::new()).unwrap();
    assert!(state.startup_session.login_invitation.preparation.is_none());
}

#[test]
fn configuration_or_session_changes_cancel_an_inflight_offer() {
    for change_language in [false, true] {
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "sleep 30"]);
        let mut state = ready_state();
        state.language = Language::EnUs;
        state.startup_session.login_invitation.preparation =
            Some(prepare::Preparation::fixture(command));
        state.startup_session.login_invitation.preparation_context =
            Some((state.language, state.shell_session_id.clone().unwrap()));
        if change_language {
            state.language = Language::ZhCn;
        } else {
            state.shell_session_id = Some("replacement-shell-session".into());
        }
        render_at_idle(&mut state, &mut Vec::new()).unwrap();
        assert!(state.startup_session.login_invitation.preparation.is_none());
        assert!(state.startup_session.login_invitation.attempted);
        assert!(!state.startup_session.login_invitation.shown);
    }
}
