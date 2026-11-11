fn prompt_snapshot(bytes: &[u8]) -> crate::raw_input::PromptSnapshot {
    let exchange = crate::raw_input::PromptEpochExchange::default();
    let epoch = exchange.open();
    exchange.publish(epoch, bytes);
    exchange.ready_snapshot(epoch).unwrap()
}

struct FailedWriter;
impl Write for FailedWriter {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("failed writer"))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

use super::*;

#[test]
fn initial_question_is_accepted_once_with_automatic_ai_disabled_even_when_output_fails() {
    let mut state = InlineState::default();
    state.personalization.ai_disabled = true;
    state.startup_auth.resolved = Some(false);
    state.startup_session.compact = true;
    state.startup_session.prompt_snapshot = Some(prompt_snapshot(b"native$ "));
    state.startup_session.initial_question = Some("  inspect my system  ".into());
    state.startup_session.failure_context = Some(crate::insight::guidance::BashEvent {
        expression: "cargo test --test 'my suite'".into(),
        status: 101,
        cwd: "/original project".into(),
        observed_at_ms: 1000,
        scope: crate::insight::guidance::ExecutionScope::ObservedCommandFragment,
        identity: crate::insight::guidance::CommandIdentity::ExternalUnshadowed,
    });
    state.shell_session_id = Some("initial-session".into());
    let mut ready = ShellEvent::user_input_intercepted("initial-session", "");
    ready.kind = ShellEventKind::ShellReady;
    ready.cwd = Some("/repo".into());
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let result = crate::runtime::startup::render_startup_banner(
        &[ready.clone()],
        &adapter,
        "bash",
        &mut state,
        &mut FailedWriter,
    );
    assert!(state.startup_session.initial_question.is_none());
    assert!(result.is_err());
    let active = state
        .agent_run
        .active
        .as_ref()
        .expect("accepted active run");
    assert_eq!(
        active.request.user_input.as_deref(),
        Some("  inspect my system  ")
    );
    let context = active
        .request
        .context_hints
        .iter()
        .find_map(|hint| hint.strip_prefix("external_bash_failure\n"))
        .expect("external facts");
    let context: serde_json::Value = serde_json::from_str(context).unwrap();
    assert_eq!(context["command_fragment"], "cargo test --test 'my suite'");
    assert_eq!(context["original_cwd"], "/original project");
    assert_eq!(context["output_captured"], false);
    assert!(state.startup_session.failure_context.is_none());
    let id = active.request.id.clone();
    crate::runtime::startup::render_startup_banner(
        &[ready],
        &adapter,
        "bash",
        &mut state,
        &mut Vec::new(),
    )
    .unwrap();
    assert_eq!(
        state
            .agent_run
            .active
            .as_ref()
            .map(|active| active.request.id.as_str()),
        Some(id.as_str())
    );
    assert!(state.agent_run.queued_requests.is_empty());
    assert!(state.personalization.ai_disabled);
}
#[test]
fn missing_credentials_do_not_bypass_the_core_trial_and_auth_owner() {
    let mut state = InlineState::default();
    state.personalization.ai_disabled = true;
    state.startup_session.compact = true;
    state.startup_session.initial_question = Some("original question".into());
    state.startup_session.prompt_snapshot = Some(prompt_snapshot(b"native$ "));
    state.startup_auth.resolved = Some(false);
    let mut ready = ShellEvent::user_input_intercepted("initial-session", "");
    ready.kind = ShellEventKind::ShellReady;
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();
    crate::runtime::startup::render_startup_banner(
        &[ready.clone()],
        &adapter,
        "bash",
        &mut state,
        &mut output,
    )
    .unwrap();
    let active_id = state.agent_run.active.as_ref().unwrap().request.id.clone();
    assert!(state.auth.state.is_none());
    assert!(state.startup_session.initial_question.is_none());
    assert!(state.personalization.ai_disabled);
    crate::runtime::startup::render_startup_banner(
        &[ready],
        &adapter,
        "bash",
        &mut state,
        &mut output,
    )
    .unwrap();
    assert_eq!(
        state.agent_run.active.as_ref().unwrap().request.id,
        active_id
    );
    assert!(state.agent_run.queued_requests.is_empty());
}

#[test]
fn compact_startup_does_not_probe_credentials_before_the_explicit_question() {
    for ai in [false, true] {
        for banner in [false, true] {
            assert!(!StartupAuthState::should_probe(ai, true, banner));
            assert_eq!(
                StartupAuthState::should_probe(ai, false, banner),
                ai && banner
            );
        }
    }
}

#[test]
fn initial_continuation_never_replaces_an_existing_card() {
    let mut state = InlineState::default();
    state.startup_session.initial_question = Some("original question".into());
    state
        .control
        .set_pending_mode_panel(crate::runtime::state::RuntimeModePanelKind::Approval, 0);
    assert!(!offer_initial_question(&mut state, &mut Vec::new()).unwrap());
    assert!(state.startup_session.continuation_selected.is_none());
}

fn pending_state() -> InlineState {
    let mut state = InlineState {
        shell_session_id: Some("initial-session".into()),
        shell_prompt_cwd: Some("/repo".into()),
        ..Default::default()
    };
    state.startup_session.compact = true;
    state.startup_session.prompt_snapshot = Some(prompt_snapshot(b"native$ "));
    state.startup_session.initial_question = Some("original question".into());
    state
}

fn submitted(id: &str) -> ShellEvent {
    let mut event = ShellEvent::user_input_intercepted("initial-session", "");
    event.capture = Some(crate::types::ShellCaptureMetadata {
        kind: Some("question".into()),
        target_id: Some(id.into()),
        generation: 1,
        lifecycle: crate::types::ShellCaptureLifecycle::Submitted,
    });
    event
}

fn answer(value: &str) -> ShellEvent {
    let mut event = ShellEvent::user_input_intercepted("initial-session", value);
    event.component = Some("card".into());
    event.message = Some("answer".into());
    event
}

#[test]
fn initial_continuation_requires_its_current_capture_and_keeps_automatic_ai_disabled() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = pending_state();
    state.personalization.ai_disabled = true;
    offer_initial_question(&mut state, &mut Vec::new()).unwrap();
    let old_id = continuation_id(&state);
    offer_initial_question(&mut state, &mut Vec::new()).unwrap();
    let current_id = continuation_id(&state);
    handle_initial_question_actions(
        &[submitted(&old_id), answer("1")],
        &adapter,
        &mut state,
        &mut Vec::new(),
        0,
    )
    .unwrap();
    handle_initial_question_actions(
        &[ShellEvent::user_input_intercepted("initial-session", "1")],
        &adapter,
        &mut state,
        &mut Vec::new(),
        2,
    )
    .unwrap();
    assert!(state.agent_run.active.is_none());
    assert!(state.startup_session.initial_question.is_some());
    let events = vec![submitted(&current_id), answer("1")];
    handle_initial_question_actions(&events, &adapter, &mut state, &mut Vec::new(), 3).unwrap();
    assert!(state.startup_session.initial_question.is_none());
    assert!(state.agent_run.queued_requests.is_empty());
    let active_id = state
        .agent_run
        .active
        .as_ref()
        .map(|active| active.request.id.clone());
    handle_initial_question_actions(&events, &adapter, &mut state, &mut Vec::new(), 3).unwrap();
    assert_eq!(
        state
            .agent_run
            .active
            .as_ref()
            .map(|active| active.request.id.clone()),
        active_id
    );
    assert!(state.agent_run.queued_requests.is_empty());
    assert!(state.personalization.ai_disabled);
}

#[test]
fn another_capture_cannot_submit_the_pending_initial_question() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = pending_state();
    offer_initial_question(&mut state, &mut Vec::new()).unwrap();
    let current_id = continuation_id(&state);
    state
        .control
        .set_pending_mode_panel(crate::runtime::state::RuntimeModePanelKind::Approval, 0);
    handle_initial_question_actions(
        &[submitted(&current_id), answer("1")],
        &adapter,
        &mut state,
        &mut Vec::new(),
        0,
    )
    .unwrap();
    assert!(state.agent_run.active.is_none());
    assert!(state.startup_session.initial_question.is_some());
    state.control.clear_pending_mode_panel();
    handle_initial_question_actions(
        &[submitted("other-question"), answer("1")],
        &adapter,
        &mut state,
        &mut Vec::new(),
        2,
    )
    .unwrap();
    assert!(state.agent_run.active.is_none());
    assert!(state.startup_session.initial_question.is_some());
}

#[test]
fn cancelling_initial_continuation_prevents_later_auth_from_replaying_it() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = pending_state();
    offer_initial_question(&mut state, &mut Vec::new()).unwrap();
    let id = continuation_id(&state);
    handle_initial_question_actions(
        &[submitted(&id), answer("2")],
        &adapter,
        &mut state,
        &mut Vec::new(),
        0,
    )
    .unwrap();
    assert!(state.startup_session.initial_question.is_none());
    assert!(!offer_initial_question(&mut state, &mut Vec::new()).unwrap());
    assert!(state.agent_run.active.is_none());
}

#[test]
fn queue_full_keeps_initial_question_without_adding_a_second_queue_entry() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = pending_state();
    let (active, _receiver) = crate::agent::run::test_support::test_active_run_with_id("existing");
    let request = active.request.clone();
    state.agent_run.active = Some(active);
    for _ in 0..crate::agent::queue::MAX_QUEUED_AGENT_REQUESTS {
        state
            .agent_run
            .queue_request(crate::agent::run::PendingAgentRequest {
                request: request.clone(),
                origin: AgentRunOrigin::Standard,
                intent: AgentStartIntent::UserInitiated,
                class: crate::agent::run::PendingRequestClass::Normal,
                selectable_after_event_index: None,
                before_held_text: false,
            });
    }
    submit_initial_question(&adapter, &mut state, &mut Vec::new(), false).unwrap();
    assert!(state.startup_session.initial_question.is_some());
    assert_eq!(
        state.agent_run.queued_requests.len(),
        crate::agent::queue::MAX_QUEUED_AGENT_REQUESTS
    );
    assert_eq!(
        state.agent_run.active.as_ref().unwrap().request.id,
        "existing"
    );
}

#[test]
fn later_flush_failure_never_restores_accepted_initial_question() {
    struct FailSecondFlush(usize);
    impl Write for FailSecondFlush {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0 += 1;
            if self.0 >= 2 {
                Err(std::io::Error::other("late flush failed"))
            } else {
                Ok(())
            }
        }
    }
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = pending_state();
    assert!(submit_initial_question(&adapter, &mut state, &mut FailSecondFlush(0), true).is_err());
    assert!(state.startup_session.initial_question.is_none());
    submit_initial_question(&adapter, &mut state, &mut Vec::new(), true).unwrap();
    assert!(state.agent_run.queued_requests.is_empty());
}

#[test]
fn expired_capture_or_ordinary_input_cannot_authorize_a_later_initial_answer() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    for expired in [false, true] {
        let mut state = pending_state();
        offer_initial_question(&mut state, &mut Vec::new()).unwrap();
        let id = continuation_id(&state);
        let boundary = if expired {
            let mut event = submitted(&id);
            event.capture.as_mut().unwrap().lifecycle =
                crate::types::ShellCaptureLifecycle::Expired;
            event
        } else {
            ShellEvent::user_input_intercepted("initial-session", "ordinary input")
        };
        handle_initial_question_actions(
            &[submitted(&id), boundary, answer("1")],
            &adapter,
            &mut state,
            &mut Vec::new(),
            0,
        )
        .unwrap();
        assert!(state.agent_run.active.is_none(), "expired={expired}");
        assert!(state.startup_session.initial_question.is_some());
    }
}

#[test]
fn queued_initial_question_is_not_requeued_after_output_failure() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = pending_state();
    let (active, _receiver) = crate::agent::run::test_support::test_active_run_with_id("existing");
    state.agent_run.active = Some(active);
    assert!(submit_initial_question(&adapter, &mut state, &mut FailedWriter, true).is_err());
    assert!(state.startup_session.initial_question.is_none());
    assert_eq!(state.agent_run.queued_requests.len(), 1);
    assert_eq!(
        state
            .agent_run
            .queued_requests
            .front()
            .unwrap()
            .request
            .user_input
            .as_deref(),
        Some("original question")
    );
    submit_initial_question(&adapter, &mut state, &mut Vec::new(), true).unwrap();
    assert_eq!(state.agent_run.queued_requests.len(), 1);
}

#[test]
fn continuation_resize_keeps_selection_and_updates_submit_clear_geometry() {
    let mut state = pending_state();
    state.startup_session.initial_question =
        Some("a long original question that wraps on a narrow terminal ".repeat(3));
    offer_initial_question(&mut state, &mut Vec::new()).unwrap();
    let id = continuation_id(&state);
    let mut focus = ShellEvent::user_input_intercepted("initial-session", format!("{id}:1"));
    focus.component = Some("card".into());
    focus.message = Some("focus".into());
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    handle_initial_question_actions(&[focus], &adapter, &mut state, &mut Vec::new(), 0).unwrap();
    let mut resized = Vec::new();
    assert!(
        crate::runtime::question_terminal::redraw_active_question_if_width_changed(
            &mut state,
            &mut resized,
            RatatuiInlineRenderer::with_width(32),
        )
        .unwrap()
    );
    assert_eq!(state.startup_session.continuation_selected, Some(1));
    assert_eq!(
        state.questions.active_panel_width,
        Some(RatatuiInlineRenderer::with_width(32).panel_standard_width())
    );
    let height = state.questions.active_panel_height;
    assert!(height > 0);
    let mut submitted_output = Vec::new();
    handle_initial_question_actions(
        &[submitted(&id), answer("2")],
        &adapter,
        &mut state,
        &mut submitted_output,
        1,
    )
    .unwrap();
    assert!(String::from_utf8_lossy(&submitted_output).contains(&format!("\x1b[{height}A")));
    assert!(state.questions.active_panel_id.is_none());
    assert!(state.questions.active_panel_width.is_none());
    assert!(state.startup_session.initial_question.is_none());
}

#[test]
fn initial_question_waits_until_the_native_shell_prompt_is_observed() {
    let mut state = InlineState::default();
    state.startup_session.compact = true;
    state.startup_session.initial_question = Some("original question".into());
    let mut ready = ShellEvent::user_input_intercepted("initial-session", "");
    ready.kind = ShellEventKind::ShellReady;
    let mut output = Vec::new();
    render_initial_question(
        &[ready],
        &AdapterInstance::Fake(FakeAgentAdapter),
        &mut state,
        &mut output,
    )
    .unwrap();
    assert!(state.startup_session.initial_question.is_some());
    assert!(state.agent_run.active.is_none());
    assert!(output.is_empty());
}

#[test]
fn initial_question_uses_native_prompt_bytes_and_sanitizes_only_the_display_copy() {
    for (language, capabilities) in [
        (Language::EnUs, "Run commands or ask AI questions."),
        (Language::ZhCn, "可执行命令，也可直接提问。"),
    ] {
        let mut state = pending_state();
        state.language = language;
        let question = "question\x1b[2J\r\x07 original";
        state.startup_session.initial_question = Some(question.into());
        state.startup_session.prompt_snapshot =
            Some(prompt_snapshot(b"\x1b[32mcustom/path $ \x1b[0m"));
        let mut output = Vec::new();
        write_initial_question(&state, question, &mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        let prompt = "\x1b[32mcustom/path $ \x1b[0mquestion original\n";
        assert!(text.contains(prompt));
        assert!(
            text.find(capabilities)
                .expect("AI Shell input capabilities")
                < text.find(prompt).unwrap()
        );
        assert!(!text.contains("◆"));
        assert!(!text.contains("\x07"));
        assert_eq!(
            state.startup_session.initial_question.as_deref(),
            Some(question)
        );
    }
}

#[test]
fn initial_question_limits_ai_permission_copy_to_this_ask_when_ai_is_disabled() {
    for (language, notice, ordinary_capabilities) in [
        (
            Language::EnUs,
            "Only this --ask request uses AI; AI remains off for subsequent input.",
            "Run commands or ask AI questions.",
        ),
        (
            Language::ZhCn,
            "仅本次 --ask 使用 AI；后续输入仍保持 AI 关闭。",
            "可执行命令，也可直接提问。",
        ),
    ] {
        let mut state = pending_state();
        state.language = language;
        state.personalization.ai_disabled = true;
        let mut output = Vec::new();
        write_initial_question(&state, "original question", &mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains(notice), "{text}");
        assert!(!text.contains(ordinary_capabilities), "{text}");
        assert!(text.find(notice).unwrap() < text.find("native$ original question").unwrap());
        assert!(state.personalization.ai_disabled);
    }
}

#[test]
fn missing_native_prompt_times_out_without_submitting_or_blocking_shell() {
    let mut state = pending_state();
    state.startup_session.prompt_snapshot = None;
    state.startup_session.prompt_wait_started = Some(Instant::now() - Duration::from_secs(6));
    let mut output = Vec::new();
    render_initial_question(
        &[],
        &AdapterInstance::Fake(FakeAgentAdapter),
        &mut state,
        &mut output,
    )
    .unwrap();
    assert!(String::from_utf8_lossy(&output).contains("initial question was not submitted"));
    assert!(state.startup_session.initial_question.is_none());
    assert!(state.agent_run.active.is_none());
    assert!(crate::runtime::controller::pending_card_capture(&state).is_none());
    let mut repeated = Vec::new();
    render_initial_question(
        &[],
        &AdapterInstance::Fake(FakeAgentAdapter),
        &mut state,
        &mut repeated,
    )
    .unwrap();
    assert!(repeated.is_empty());
}

#[test]
fn user_input_during_prompt_wait_cancels_the_original_automatic_submission() {
    let mut state = pending_state();
    state.startup_session.prompt_snapshot = None;
    let mut ready = ShellEvent::user_input_intercepted("initial-session", "");
    ready.kind = ShellEventKind::ShellReady;
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    render_initial_question(&[ready.clone()], &adapter, &mut state, &mut Vec::new()).unwrap();
    let mut user_input = ShellEvent::user_input_intercepted("initial-session", "/auth");
    user_input.component = Some("slash".into());
    state.startup_session.prompt_snapshot = Some(prompt_snapshot(b"native$ "));
    render_initial_question(
        &[ready.clone(), user_input],
        &adapter,
        &mut state,
        &mut Vec::new(),
    )
    .unwrap();
    assert!(state.startup_session.initial_question.is_none());
    assert!(state.agent_run.active.is_none());
    assert!(state.agent_run.queued_requests.is_empty());
    state.shell_at_prompt = true;
    let mut notice = Vec::new();
    render_initial_question(&[ready], &adapter, &mut state, &mut notice).unwrap();
    assert!(String::from_utf8_lossy(&notice).contains("initial question was not submitted"));
}

#[test]
fn initial_question_rejects_snapshot_after_already_owned_user_write() {
    let input_generation = crate::raw_input::UserPtyInputGeneration::default();
    let exchange = input_generation.prompt_epoch_exchange();
    let epoch = exchange.open();
    exchange.publish(epoch, b"native$ ");
    let input_mode = std::sync::Arc::new(std::sync::Mutex::new(
        crate::raw_input::RawInputMode::Passthrough,
    ));
    let (copied_tx, copied_rx) = mpsc::sync_channel(0);
    let (resume_tx, resume_rx) = mpsc::sync_channel(0);
    let (auth_tx, auth_rx) = mpsc::sync_channel(1);
    let path = std::env::temp_dir().join(format!("cosh-initial-stale-{}", std::process::id()));
    let mut master = std::fs::File::create(&path).unwrap();
    let writer = std::thread::spawn(move || {
        let mode = input_mode.lock().unwrap().clone();
        assert!(matches!(mode, crate::raw_input::RawInputMode::Passthrough));
        copied_tx.send(()).unwrap();
        resume_rx.recv().unwrap();
        crate::raw_input::test_write_user_bytes_to_pty(
            &mut master,
            &input_generation,
            b"echo keyboard\r",
        )
        .unwrap();
        auth_tx.send(Some(true)).unwrap();
    });
    copied_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let mut state = crate::runtime::state::InlineState::default();
    state.startup_session.compact = true;
    state.startup_session.initial_question = Some("initial question".into());
    state.startup_session.prompt_snapshot = Some(exchange.ready_snapshot(epoch).unwrap());
    state.startup_auth.pending = Some(auth_rx);
    resume_tx.send(()).unwrap();
    writer.join().unwrap();
    let mut ready = crate::types::ShellEvent::user_input_intercepted("session", "");
    ready.kind = crate::types::ShellEventKind::ShellReady;
    let mut output = Vec::new();
    crate::runtime::startup::render_startup_banner(
        &[ready],
        &crate::adapter::AdapterInstance::Fake(crate::adapter::FakeAgentAdapter),
        "bash",
        &mut state,
        &mut output,
    )
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"echo keyboard\r");
    std::fs::remove_file(path).unwrap();
    assert!(
        state.agent_run.active.is_none(),
        "stale snapshot accepted initial question"
    );
    assert!(state.agent_run.queued_requests.is_empty());
    assert!(!String::from_utf8_lossy(&output).contains("native$ initial question"));
}

#[test]
fn initial_question_commit_serializes_shell_bytes_even_when_the_first_display_write_fails() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    struct PausingWriter {
        first_write: Option<mpsc::SyncSender<()>>,
        attempting: mpsc::Receiver<()>,
        displayed: Arc<AtomicBool>,
        fail: bool,
        first_flush: bool,
        snapshot: crate::raw_input::PromptSnapshot,
    }
    impl Write for PausingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if let Some(first) = self.first_write.take() {
                first.send(()).unwrap();
                self.attempting
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
                if self.fail {
                    return Err(std::io::Error::other("first display write failed"));
                }
            }
            if bytes == b"original question" {
                self.displayed.store(true, Ordering::SeqCst);
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            if self.first_flush {
                assert!(
                    self.snapshot.commit_is_locked(),
                    "initial display flushed outside claim guard"
                );
                self.first_flush = false;
            }
            Ok(())
        }
    }
    for fail in [false, true] {
        let generation = crate::raw_input::UserPtyInputGeneration::default();
        let exchange = generation.prompt_epoch_exchange();
        let epoch = exchange.open();
        exchange.publish(epoch, b"native$ ");
        let snapshot = exchange.ready_snapshot(epoch).unwrap();
        let mut state = pending_state();
        state.startup_session.prompt_snapshot = Some(snapshot.clone());
        let (first_tx, first_rx) = mpsc::sync_channel(0);
        let (attempt_tx, attempt_rx) = mpsc::sync_channel(0);
        let displayed = Arc::new(AtomicBool::new(false));
        let display_check = displayed.clone();
        let path =
            std::env::temp_dir().join(format!("cosh-initial-serial-{}-{fail}", std::process::id()));
        let mut master = std::fs::File::create(&path).unwrap();
        let input_mode = Arc::new(std::sync::Mutex::new(
            crate::raw_input::RawInputMode::Passthrough,
        ));
        let (copied_tx, copied_rx) = mpsc::sync_channel(0);
        let keyboard = std::thread::spawn(move || {
            let mode = input_mode.lock().unwrap().clone();
            assert!(matches!(mode, crate::raw_input::RawInputMode::Passthrough));
            copied_tx.send(()).unwrap();
            first_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let held = snapshot.commit_is_locked();
            attempt_tx.send(()).unwrap();
            crate::raw_input::test_write_user_bytes_to_pty(
                &mut master,
                &generation,
                b"echo keyboard\r",
            )
            .unwrap();
            assert!(held, "display escaped the user-write claim boundary");
            if !fail {
                assert!(
                    display_check.load(Ordering::SeqCst),
                    "PTY write overtook initial display"
                );
            }
        });
        copied_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut output = PausingWriter {
            first_write: Some(first_tx),
            attempting: attempt_rx,
            displayed,
            fail,
            first_flush: true,
            snapshot: state.startup_session.prompt_snapshot.clone().unwrap(),
        };
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let result = submit_initial_question(&adapter, &mut state, &mut output, true);
        keyboard.join().unwrap();
        assert_eq!(result.is_err(), fail);
        assert_eq!(std::fs::read(&path).unwrap(), b"echo keyboard\r");
        std::fs::remove_file(path).unwrap();
        assert!(state.startup_session.initial_question.is_none());
        submit_initial_question(&adapter, &mut state, &mut Vec::new(), true).unwrap();
        assert!(state.agent_run.queued_requests.is_empty());
    }
}

#[test]
fn health_state_keeps_the_worker_report_and_candidate_pool_unchanged() {
    use crate::diagnostics::health::{HealthMessageId, HealthTryItem, HealthTryKind};
    let mut report = HealthScanReport::new("worker-candidate-pool", 1000);
    report.try_items.push(HealthTryItem {
        id: "worker-candidate".to_string(),
        label_id: HealthMessageId::HealthTryReviewUnavailableChecks,
        label_args: Default::default(),
        prompt_id: Some(HealthMessageId::HealthPromptReviewUnavailableChecks),
        prompt_args: Default::default(),
        kind: HealthTryKind::AskAgent,
        command: None,
        reason_id: HealthMessageId::HealthTryReasonMissingCoreCheck,
        reason_args: Default::default(),
        score: 100,
        finding_id: String::new(),
    });
    let original = report.clone();
    let mut health = StartupHealthState::default();
    health.store_report(report);
    assert_eq!(health.report.as_ref(), Some(&original));
}
