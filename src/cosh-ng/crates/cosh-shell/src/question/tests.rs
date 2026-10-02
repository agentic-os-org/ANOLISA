use super::*;
use crate::question::answer::{resolve_pending_question_answer, QuestionAnswerResolution};
use crate::runtime::question_terminal::redraw_active_question_if_width_changed;

fn record_test_question(
    state: &mut InlineState,
    options: Vec<String>,
    allow_free_text: bool,
    selection_mode: QuestionSelectionMode,
) {
    let events = vec![GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event: AgentEvent::UserQuestion {
            run_id: "run-test".to_string(),
            provider_request_id: Some("provider-question".to_string()),
            question: "Choose".to_string(),
            options,
            allow_free_text,
            selection_mode,
        },
        reason: "display".to_string(),
        display_text: String::new(),
        auto_execute: false,
    }];
    record_user_questions(state, &events, AgentRunOrigin::Standard, Some("owner"));
}

fn card_answer(input: &str) -> ShellEvent {
    let mut event = ShellEvent::user_input_intercepted("session", input);
    event.component = Some("card".to_string());
    event.message = Some("answer".to_string());
    event.input = Some(input.to_string());
    event
}

#[test]
fn answer_resolution_distinguishes_empty_invalid_and_no_pending() {
    let mut state = InlineState::default();
    assert!(matches!(
        resolve_pending_question_answer(&card_answer(""), 1, &mut state),
        QuestionAnswerResolution::NoPending
    ));

    record_test_question(&mut state, Vec::new(), true, QuestionSelectionMode::Single);
    assert!(matches!(
        resolve_pending_question_answer(&card_answer(""), 2, &mut state),
        QuestionAnswerResolution::EmptyAnswer
    ));
    assert_eq!(state.questions.pending_id.as_deref(), Some("q-1"));

    state = InlineState::default();
    record_test_question(
        &mut state,
        vec!["One".to_string()],
        false,
        QuestionSelectionMode::Single,
    );
    assert!(matches!(
        resolve_pending_question_answer(&card_answer("2"), 3, &mut state),
        QuestionAnswerResolution::InvalidAnswer
    ));
    assert_eq!(state.questions.pending_id.as_deref(), Some("q-1"));
}

#[test]
fn stale_empty_submission_does_not_mutate_the_next_pending_question() {
    let mut state = InlineState::default();
    record_test_question(&mut state, Vec::new(), true, QuestionSelectionMode::Single);
    let mut event = ShellEvent::user_input_intercepted("session", "q-old");
    event.component = Some("card".to_string());
    event.message = Some("question_submit_empty".to_string());
    event.input = Some("q-old".to_string());

    assert!(matches!(
        resolve_pending_question_answer(&event, 1, &mut state),
        QuestionAnswerResolution::Ignored
    ));
    assert_eq!(state.questions.pending_id.as_deref(), Some("q-1"));
}

#[test]
fn answer_resolution_distinguishes_selection_and_request_build_failure() {
    let mut state = InlineState::default();
    record_test_question(
        &mut state,
        vec!["One".to_string()],
        true,
        QuestionSelectionMode::Multiple,
    );
    assert!(matches!(
        resolve_pending_question_answer(&card_answer(""), 1, &mut state),
        QuestionAnswerResolution::SelectionRequired
    ));
    assert_eq!(state.questions.pending_id.as_deref(), Some("q-1"));

    let mut event = card_answer("1");
    event.kind = ShellEventKind::CommandStarted;
    assert!(matches!(
        resolve_pending_question_answer(&event, 2, &mut state),
        QuestionAnswerResolution::RequestBuildFailed
    ));
    assert_eq!(state.questions.pending_id.as_deref(), Some("q-1"));
}

#[test]
fn multi_question_accepts_custom_only_answer() {
    let mut state = InlineState::default();
    record_test_question(
        &mut state,
        vec!["One".to_string()],
        true,
        QuestionSelectionMode::Multiple,
    );

    let QuestionAnswerResolution::Accepted(answer_run) =
        resolve_pending_question_answer(&card_answer("\neslint only"), 1, &mut state)
    else {
        panic!("custom-only answer should be accepted");
    };

    assert_eq!(answer_run.answer, "eslint only");
}

#[test]
fn record_user_questions_localizes_empty_question_fallback() {
    let mut state = InlineState {
        language: Language::ZhCn,
        ..InlineState::default()
    };
    let events = vec![GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event: AgentEvent::UserQuestion {
            run_id: "run-1".to_string(),
            provider_request_id: None,
            question: String::new(),
            options: Vec::new(),
            allow_free_text: true,
            selection_mode: QuestionSelectionMode::Single,
        },
        reason: "display".to_string(),
        display_text: String::new(),
        auto_execute: false,
    }];

    let (ids, rejection) =
        record_user_questions(&mut state, &events, AgentRunOrigin::InsightPrompt, None);

    assert_eq!(ids, vec!["q-1".to_string()]);
    assert_eq!(rejection, None);
    assert_eq!(state.questions.items[0].question, "Agent 需要你的输入");
    assert_eq!(
        state.questions.items[0].origin,
        AgentRunOrigin::InsightPrompt
    );
    let mut output = Vec::new();
    render_user_questions(&mut state, &ids, &mut output).expect("render question");
    let text = String::from_utf8(output).expect("utf8 question");
    assert!(text.contains("Agent 需要你的输入"), "{text}");
    assert!(!text.contains("Agent needs your input"), "{text}");
}

#[test]
fn pending_question_answer_keeps_the_origin_without_an_active_run() {
    let mut state = InlineState::default();
    let events = vec![GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event: AgentEvent::UserQuestion {
            run_id: "run-1".to_string(),
            provider_request_id: Some("provider-question-1".to_string()),
            question: "Choose a scope".to_string(),
            options: vec!["Current project".to_string()],
            allow_free_text: false,
            selection_mode: QuestionSelectionMode::Single,
        },
        reason: "display".to_string(),
        display_text: String::new(),
        auto_execute: false,
    }];
    record_user_questions(
        &mut state,
        &events,
        AgentRunOrigin::AutoFailure,
        Some("owner-request"),
    );
    let mut answer = ShellEvent::user_input_intercepted("session-1", "1");
    answer.component = Some("card".to_string());
    answer.message = Some("answer".to_string());
    answer.input = Some("1".to_string());

    let answer_run = agent_request_from_pending_question_answer(&answer, 2, &mut state)
        .expect("question answer run");

    assert_eq!(answer_run.origin, AgentRunOrigin::AutoFailure);
    assert_eq!(
        answer_run.provider_owner_request_id.as_deref(),
        Some("owner-request")
    );
    assert_eq!(
        respond_question_answer_to_provider(&state, &answer_run),
        ProviderQuestionResponse::OwnerUnavailable
    );
}

#[test]
fn provider_question_does_not_respond_through_an_unrelated_active_run() {
    let mut state = InlineState::default();
    let mut unrelated_event = ShellEvent::user_input_intercepted("session-1", "other task");
    unrelated_event.cwd = Some("/repo".to_string());
    let mut unrelated_request =
        agent_request_from_intercepted_input(&unrelated_event, 1, true).expect("unrelated request");
    unrelated_request.id = "different-owner".to_string();
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    start_agent_run(
        &unrelated_request,
        crate::agent::run::AgentStartIntent::UserInitiated,
        &adapter,
        &mut state,
        &mut Vec::new(),
        None,
    )
    .expect("start unrelated run");
    let events = vec![GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event: AgentEvent::UserQuestion {
            run_id: "run-1".to_string(),
            provider_request_id: Some("provider-question-1".to_string()),
            question: "Choose a scope".to_string(),
            options: vec!["Current project".to_string()],
            allow_free_text: false,
            selection_mode: QuestionSelectionMode::Single,
        },
        reason: "display".to_string(),
        display_text: String::new(),
        auto_execute: false,
    }];
    record_user_questions(
        &mut state,
        &events,
        AgentRunOrigin::InsightPrompt,
        Some("question-owner"),
    );
    let mut answer = ShellEvent::user_input_intercepted("session-1", "1");
    answer.component = Some("card".to_string());
    answer.message = Some("answer".to_string());
    answer.input = Some("1".to_string());
    let answer_run = agent_request_from_pending_question_answer(&answer, 2, &mut state)
        .expect("question answer run");

    assert_eq!(
        respond_question_answer_to_provider(&state, &answer_run),
        ProviderQuestionResponse::OwnerUnavailable
    );
    assert_eq!(
        state
            .agent_run
            .active
            .as_ref()
            .map(|run| run.request.id.as_str()),
        Some("different-owner")
    );
}

#[test]
fn question_cancel_without_matching_owner_keeps_unrelated_active_run() {
    for owner_request_id in [None, Some("question-owner")] {
        let mut state = InlineState::default();
        let mut unrelated_event = ShellEvent::user_input_intercepted("session-1", "other task");
        unrelated_event.cwd = Some("/repo".to_string());
        let mut unrelated_request = agent_request_from_intercepted_input(&unrelated_event, 1, true)
            .expect("unrelated request");
        unrelated_request.id = "different-owner".to_string();
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        start_agent_run(
            &unrelated_request,
            crate::agent::run::AgentStartIntent::UserInitiated,
            &adapter,
            &mut state,
            &mut Vec::new(),
            None,
        )
        .expect("start unrelated run");
        let events = vec![GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::DisplayOnly,
            event: AgentEvent::UserQuestion {
                run_id: "run-1".to_string(),
                provider_request_id: Some("provider-question-1".to_string()),
                question: "Choose a scope".to_string(),
                options: vec!["Current project".to_string()],
                allow_free_text: false,
                selection_mode: QuestionSelectionMode::Single,
            },
            reason: "display".to_string(),
            display_text: String::new(),
            auto_execute: false,
        }];
        record_user_questions(
            &mut state,
            &events,
            AgentRunOrigin::InsightPrompt,
            owner_request_id,
        );
        let mut cancel = ShellEvent::user_input_intercepted("session-1", "q-1");
        cancel.component = Some("card".to_string());
        cancel.message = Some("question_cancel".to_string());

        render_question_cancel_actions(&[cancel], &mut state, &mut Vec::new(), 2)
            .expect("cancel question");

        assert_eq!(state.questions.pending_id, None);
        assert_eq!(state.questions.items[0].answer.as_deref(), Some(""));
        assert_eq!(
            state
                .agent_run
                .active
                .as_ref()
                .map(|run| run.request.id.as_str()),
            Some("different-owner"),
            "owner={owner_request_id:?}"
        );
    }
}

#[test]
fn full_control_queue_keeps_question_pending_and_retryable() {
    use crate::agent::queue::MAX_TOTAL_QUEUED_AGENT_REQUESTS;
    use crate::agent::run::{AgentStartIntent, PendingAgentRequest, PendingRequestClass};

    let mut state = InlineState::default();
    let events = vec![GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event: AgentEvent::UserQuestion {
            run_id: "run-1".to_string(),
            provider_request_id: None,
            question: "Choose a scope".to_string(),
            options: vec!["Current project".to_string()],
            allow_free_text: false,
            selection_mode: QuestionSelectionMode::Single,
        },
        reason: "display".to_string(),
        display_text: String::new(),
        auto_execute: false,
    }];
    record_user_questions(&mut state, &events, AgentRunOrigin::Standard, None);
    assert!(state.questions.pending_id.is_some());

    // Force queueing (a compaction is recommended) and exhaust the total
    // hard cap so the control reserve is gone too.
    crate::slash::session::note_compaction_recommendation(
        &mut state,
        "00000000-0000-4000-8000-000000000000:1:0:200000:100000",
    );
    for index in 0..MAX_TOTAL_QUEUED_AGENT_REQUESTS {
        let mut filler_event =
            ShellEvent::user_input_intercepted("session-1", format!("filler {index}"));
        filler_event.cwd = Some("/repo".to_string());
        let request = agent_request_from_intercepted_input(&filler_event, index + 10, true)
            .expect("filler request");
        state
            .agent_run
            .queued_requests
            .push_back(PendingAgentRequest {
                request,
                origin: AgentRunOrigin::Standard,
                intent: AgentStartIntent::UserInitiated,
                class: PendingRequestClass::ControlResponse,
                selectable_after_event_index: None,
                before_held_text: false,
            });
    }

    let mut answer = ShellEvent::user_input_intercepted("session-1", "1");
    answer.component = Some("card".to_string());
    answer.message = Some("answer".to_string());
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();
    render_question_answer_actions(&[answer], &adapter, &mut state, &mut output, 100)
        .expect("answer action");

    // The question state was NOT consumed: it is still pending and the user
    // can retry once the queue drains; a visible notice explains why.
    assert!(state.questions.pending_id.is_some());
    assert!(state.questions.items[0].answer.is_none());
    assert!(state.agent_run.active.is_none());
    assert_eq!(
        state.agent_run.queued_requests.len(),
        MAX_TOTAL_QUEUED_AGENT_REQUESTS
    );
    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(rendered.contains("still pending"), "{rendered}");
}

/// Fills the pending queue to the total hard cap with control-class fillers.
fn fill_queue_to_hard_cap(state: &mut InlineState) {
    use crate::agent::queue::MAX_TOTAL_QUEUED_AGENT_REQUESTS;
    use crate::agent::run::{AgentStartIntent, PendingAgentRequest, PendingRequestClass};

    for index in 0..MAX_TOTAL_QUEUED_AGENT_REQUESTS {
        let mut filler_event =
            ShellEvent::user_input_intercepted("session-1", format!("filler {index}"));
        filler_event.cwd = Some("/repo".to_string());
        let request = agent_request_from_intercepted_input(&filler_event, index + 500, true)
            .expect("filler request");
        state
            .agent_run
            .queued_requests
            .push_back(PendingAgentRequest {
                request,
                origin: AgentRunOrigin::Standard,
                intent: AgentStartIntent::UserInitiated,
                class: PendingRequestClass::ControlResponse,
                selectable_after_event_index: None,
                before_held_text: false,
            });
    }
}

fn provider_question_events(owner_hint: &str) -> Vec<GovernedEvent> {
    vec![GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event: AgentEvent::UserQuestion {
            run_id: format!("run-{owner_hint}"),
            provider_request_id: Some("provider-question-1".to_string()),
            question: "Choose a scope".to_string(),
            options: vec!["Current project".to_string()],
            allow_free_text: false,
            selection_mode: QuestionSelectionMode::Single,
        },
        reason: "display".to_string(),
        display_text: String::new(),
        auto_execute: false,
    }]
}

#[test]
fn full_queue_does_not_block_answer_delivery_to_the_owner_run() {
    use crate::agent::queue::MAX_TOTAL_QUEUED_AGENT_REQUESTS;

    // The active provider run owns the pending question: it is waiting for
    // exactly this answer, which is delivered through its own handle (or, on
    // a delivery failure, restarts after the run is stopped). Neither path
    // consumes a queue slot, so a full queue must never reject the answer —
    // that would deadlock provider and queue against each other.
    let mut state = InlineState::default();
    let mut owner_event = ShellEvent::user_input_intercepted("session-1", "owner task");
    owner_event.cwd = Some("/repo".to_string());
    let mut owner_request =
        agent_request_from_intercepted_input(&owner_event, 1, true).expect("owner request");
    owner_request.id = "owner-run".to_string();
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    start_agent_run(
        &owner_request,
        crate::agent::run::AgentStartIntent::UserInitiated,
        &adapter,
        &mut state,
        &mut Vec::new(),
        None,
    )
    .expect("start owner run");
    record_user_questions(
        &mut state,
        &provider_question_events("owner"),
        AgentRunOrigin::Standard,
        Some("owner-run"),
    );
    fill_queue_to_hard_cap(&mut state);

    let mut answer = ShellEvent::user_input_intercepted("session-1", "1");
    answer.component = Some("card".to_string());
    answer.message = Some("answer".to_string());
    let mut output = Vec::new();
    render_question_answer_actions(&[answer], &adapter, &mut state, &mut output, 100)
        .expect("answer action");

    // The answer was consumed and handled; no queue-full rejection, and the
    // queue did not grow past the cap.
    assert!(state.questions.items[0].answer.is_some());
    assert!(state.questions.pending_id.is_none());
    assert_eq!(
        state.agent_run.queued_requests.len(),
        MAX_TOTAL_QUEUED_AGENT_REQUESTS
    );
    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(!rendered.contains("still pending"), "{rendered}");
}

#[test]
fn full_queue_blocks_answer_that_must_queue_behind_a_foreign_run() {
    use crate::agent::queue::MAX_TOTAL_QUEUED_AGENT_REQUESTS;

    // The active run does NOT own the question (owner mismatch): the answer
    // continuation would have to queue behind it, so a full queue must keep
    // the card pending and retryable instead of consuming it.
    let mut state = InlineState::default();
    let mut unrelated_event = ShellEvent::user_input_intercepted("session-1", "other task");
    unrelated_event.cwd = Some("/repo".to_string());
    let mut unrelated_request =
        agent_request_from_intercepted_input(&unrelated_event, 1, true).expect("unrelated request");
    unrelated_request.id = "different-owner".to_string();
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    start_agent_run(
        &unrelated_request,
        crate::agent::run::AgentStartIntent::UserInitiated,
        &adapter,
        &mut state,
        &mut Vec::new(),
        None,
    )
    .expect("start unrelated run");
    record_user_questions(
        &mut state,
        &provider_question_events("foreign"),
        AgentRunOrigin::Standard,
        Some("somebody-else"),
    );
    fill_queue_to_hard_cap(&mut state);

    let mut answer = ShellEvent::user_input_intercepted("session-1", "1");
    answer.component = Some("card".to_string());
    answer.message = Some("answer".to_string());
    let mut output = Vec::new();
    render_question_answer_actions(&[answer], &adapter, &mut state, &mut output, 100)
        .expect("answer action");

    assert!(state.questions.pending_id.is_some());
    assert!(state.questions.items[0].answer.is_none());
    assert_eq!(
        state.agent_run.queued_requests.len(),
        MAX_TOTAL_QUEUED_AGENT_REQUESTS
    );
    // The unrelated run keeps running; only the answer was deferred.
    assert!(state.agent_run.active.is_some());
    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(rendered.contains("still pending"), "{rendered}");
}

#[test]
fn terminal_resize_reflows_and_reanchors_the_pending_question() {
    let mut state = InlineState::default();
    record_test_question(&mut state, Vec::new(), true, QuestionSelectionMode::Single);
    state.questions.items[0].question =
        "Choose a sufficiently long answer so the narrow card must wrap onto additional rows"
            .to_string();
    state.questions.active_panel_id = Some("q-1".to_string());
    state.questions.active_panel_height = 4;
    state.questions.active_panel_cursor_row = Some(2);
    state.questions.active_panel_width = Some(100);
    let previous_height = state.questions.active_panel_height;
    let mut output = Vec::new();

    assert!(redraw_active_question_if_width_changed(
        &mut state,
        &mut output,
        RatatuiInlineRenderer::with_width(40),
    )
    .expect("resize redraw"));

    assert_eq!(state.questions.active_panel_width, Some(40));
    assert!(state.questions.active_panel_height >= previous_height);
    assert_eq!(state.questions.active_panel_id.as_deref(), Some("q-1"));
    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(rendered.starts_with("\u{1b}[2B\r\u{1b}[4A"), "{rendered:?}");
    assert!(rendered.contains("\u{1b}[2K"), "{rendered:?}");
    assert!(rendered.contains("Type your answer"), "{rendered:?}");
}
