use super::dispatcher::*;
use crate::runtime::events::ShellEventSnapshot;
use crate::runtime::prelude::{AdapterInstance, FakeAgentAdapter, InlineState, ShellEvent};
use crate::types::ShellEventKind;

#[test]
fn dispatcher_advances_cursor_to_snapshot_end() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = InlineState::default();
    let mut output = Vec::new();
    let events = [
        ShellEvent::user_input_intercepted("s", "/help"),
        ShellEvent::user_input_intercepted("s", "/help"),
    ];
    let snapshot = ShellEventSnapshot::new(&events);

    let actions = RuntimeDispatcher::dispatch_inline_batch(
        &snapshot,
        &adapter,
        "bash",
        &mut state,
        &mut output,
    )
    .expect("dispatch should render");
    RuntimeDispatcher::apply_actions(actions, &mut state);

    assert_eq!(
        state.control.event_cursor().position(),
        snapshot.cursor().position()
    );
}

#[test]
fn command_activity_evidence_holds_across_cumulative_snapshots() {
    // The activity flag keys the executor's cwd fallback off positive
    // evidence of zero command activity, so it must hold once a
    // command marker was observed. Production dispatch always scans
    // the session's cumulative event stream (the raw relay passes the
    // parser's append-only event vec), so a later snapshot is a
    // superset of an earlier one and the recomputed flag can never
    // regress to `false`.
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = InlineState::default();
    let mut output = Vec::new();

    let marker = ShellEvent::command_started("s", "cmd-1", "cd /tmp", "/", 1);
    let with_activity = ShellEventSnapshot::new(std::slice::from_ref(&marker));
    let actions = RuntimeDispatcher::dispatch_inline_batch(
        &with_activity,
        &adapter,
        "bash",
        &mut state,
        &mut output,
    )
    .expect("dispatch should render");
    RuntimeDispatcher::apply_actions(actions, &mut state);
    assert!(state.shell_command_activity_observed);

    let cumulative = [marker, ShellEvent::user_input_intercepted("s", "/help")];
    let later_snapshot = ShellEventSnapshot::new(&cumulative);
    let actions = RuntimeDispatcher::dispatch_inline_batch(
        &later_snapshot,
        &adapter,
        "bash",
        &mut state,
        &mut output,
    )
    .expect("dispatch should render");
    RuntimeDispatcher::apply_actions(actions, &mut state);
    assert!(
        state.shell_command_activity_observed,
        "the cumulative snapshot keeps the marker, so the flag must hold"
    );
}

/// A prompt marker with no command in flight: the shell reports its
/// `$PWD` at every command-less prompt.
fn shell_ready_event(cwd: &str) -> ShellEvent {
    let mut event = ShellEvent::user_input_intercepted("s", "");
    event.kind = ShellEventKind::ShellReady;
    event.input = None;
    event.cwd = Some(cwd.to_string());
    event
}

/// A PTY input write barrier observed by the raw input relay.
fn pty_input_event() -> ShellEvent {
    let mut event = ShellEvent::user_input_intercepted("s", "");
    event.input = None;
    event.component = Some("shell_pty_input".to_string());
    event.message = Some("write".to_string());
    event
}

#[test]
fn dispatcher_records_the_latest_shell_prompt_cwd_report() {
    // The executor's cwd fallback consumes the shell's own latest
    // prompt-time report: the most recent `ShellReady` cwd wins, and
    // later dispatches over the cumulative stream keep it even when
    // no new report arrived.
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = InlineState::default();
    let mut output = Vec::new();

    let reports = [shell_ready_event("/first"), shell_ready_event("/second")];
    let snapshot = ShellEventSnapshot::new(&reports);
    let actions = RuntimeDispatcher::dispatch_inline_batch(
        &snapshot,
        &adapter,
        "bash",
        &mut state,
        &mut output,
    )
    .expect("dispatch should render");
    RuntimeDispatcher::apply_actions(actions, &mut state);
    assert_eq!(state.shell_prompt_cwd.as_deref(), Some("/second"));

    let cumulative = [
        reports[0].clone(),
        reports[1].clone(),
        ShellEvent::user_input_intercepted("s", "/help"),
    ];
    let later_snapshot = ShellEventSnapshot::new(&cumulative);
    let actions = RuntimeDispatcher::dispatch_inline_batch(
        &later_snapshot,
        &adapter,
        "bash",
        &mut state,
        &mut output,
    )
    .expect("dispatch should render");
    RuntimeDispatcher::apply_actions(actions, &mut state);
    assert_eq!(
        state.shell_prompt_cwd.as_deref(),
        Some("/second"),
        "the last known report must survive report-free dispatches"
    );
}

#[test]
fn idle_dispatch_reuses_ledger_until_new_events_arrive() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = InlineState::default();
    let mut output = Vec::new();
    let mut events = vec![
        ShellEvent::command_started("s", "cmd-1", "echo one", "/tmp", 1),
        ShellEvent::command_finished(
            ShellEventKind::CommandCompleted,
            "s",
            "cmd-1",
            0,
            2,
            "/tmp/cmd-1",
        ),
    ];

    dispatch_and_apply(&events, &adapter, &mut state, &mut output);
    assert_eq!(state.session_blocks.len(), 1);
    assert_eq!(state.control.ledger_rebuild_count(), 1);

    dispatch_and_apply(&events, &adapter, &mut state, &mut output);
    assert_eq!(state.session_blocks.len(), 1);
    assert_eq!(
        state.control.ledger_rebuild_count(),
        1,
        "an end cursor must bypass the cumulative ledger"
    );

    events.extend([
        ShellEvent::command_started("s", "cmd-2", "echo two", "/tmp", 3),
        ShellEvent::command_finished(
            ShellEventKind::CommandCompleted,
            "s",
            "cmd-2",
            0,
            4,
            "/tmp/cmd-2",
        ),
    ]);
    dispatch_and_apply(&events, &adapter, &mut state, &mut output);

    assert_eq!(state.session_blocks.len(), 2);
    assert_eq!(state.control.ledger_rebuild_count(), 2);
}

fn dispatch_and_apply(
    events: &[ShellEvent],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut Vec<u8>,
) {
    let snapshot = ShellEventSnapshot::new(events);
    let actions =
        RuntimeDispatcher::dispatch_inline_batch(&snapshot, adapter, "bash", state, output)
            .expect("dispatch should render");
    RuntimeDispatcher::apply_actions(actions, state);
}

#[test]
fn pty_input_invalidates_the_prompt_cwd_report() {
    // Any PTY input may submit a `cd` through a binding the
    // byte-stream heuristic cannot see, and its markers may be lost
    // entirely (no CommandStarted/Completed/Failed and no fresh
    // ShellReady): the pre-input report no longer proves where the
    // shell sits, so it must be dropped until a newer cwd-bearing
    // marker arrives — and a fresh ShellReady after the input
    // restores the evidence with the shell's current directory.
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = InlineState::default();
    let mut output = Vec::new();

    let stale = [shell_ready_event("/repo-a"), pty_input_event()];
    let snapshot = ShellEventSnapshot::new(&stale);
    let actions = RuntimeDispatcher::dispatch_inline_batch(
        &snapshot,
        &adapter,
        "bash",
        &mut state,
        &mut output,
    )
    .expect("dispatch should render");
    RuntimeDispatcher::apply_actions(actions, &mut state);
    assert_eq!(
        state.shell_prompt_cwd, None,
        "a PTY input write must invalidate the earlier report"
    );

    let refreshed = [
        shell_ready_event("/repo-a"),
        pty_input_event(),
        shell_ready_event("/repo-b"),
    ];
    let snapshot = ShellEventSnapshot::new(&refreshed);
    let actions = RuntimeDispatcher::dispatch_inline_batch(
        &snapshot,
        &adapter,
        "bash",
        &mut state,
        &mut output,
    )
    .expect("dispatch should render");
    RuntimeDispatcher::apply_actions(actions, &mut state);
    assert_eq!(
        state.shell_prompt_cwd.as_deref(),
        Some("/repo-b"),
        "a fresh report after the input restores the evidence"
    );
}

#[test]
fn stable_event_key_uses_marker_timestamp_when_available() {
    let mut event = ShellEvent::user_input_intercepted("s", "/help");
    assert_eq!(stable_event_key("slash", 7, &event), "slash:7");

    event.started_at_ms = Some(123);
    assert_eq!(stable_event_key("slash", 7, &event), "slash:123::/help");
}

#[test]
fn stable_event_key_does_not_retain_secret_card_input() {
    let mut event = ShellEvent::user_input_intercepted("s", "auth-1:secret-value");
    event.started_at_ms = Some(123);
    event.component = Some("card_secret".to_string());

    let key = stable_event_key("auth", 7, &event);

    assert_eq!(key, "auth:123:card_secret:7");
    assert!(!key.contains("secret-value"));
}

#[test]
fn personal_idle_tracks_whether_the_shell_input_line_is_empty() {
    let mut state = InlineState::default();
    let mut editing = ShellEvent::user_input_intercepted("s", "");
    editing.component = Some("shell_input".to_string());
    editing.message = Some("input editing".to_string());
    update_personal_shell_input_state(&[editing], &mut state);
    assert!(state.personalization.shell_input_active);

    let mut empty = ShellEvent::user_input_intercepted("s", "");
    empty.component = Some("shell_input".to_string());
    empty.message = Some("input empty".to_string());
    update_personal_shell_input_state(&[empty], &mut state);
    assert!(!state.personalization.shell_input_active);
}

#[test]
fn busy_shell_updates_the_analyzer_foreground_gate() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let cancellation =
        crate::recommendation::personal_analysis_runtime::AnalyzerCancellation::new();
    let mut state = InlineState {
        personalization: crate::recommendation::personal_state::PersonalizationState {
            analyzer_cancellation: Some(cancellation.clone()),
            ..Default::default()
        },
        ..InlineState::default()
    };
    let mut output = Vec::new();
    let events = [ShellEvent::command_started(
        "session", "command", "sleep 1", "/tmp", 1,
    )];
    let snapshot = ShellEventSnapshot::new(&events);

    RuntimeDispatcher::dispatch_inline_batch(&snapshot, &adapter, "bash", &mut state, &mut output)
        .expect("dispatch should render");

    assert!(!cancellation.foreground_idle());
}
