// Hook-action disambiguation for shell/agent id collision (#1629).
//
// When `/hooks enable|disable <id>` collides between the shell layer and the
// agent layer, the user picks which layer(s) to act on via a question panel.
// This mirrors the `auth::delete_confirm` confirmation flow: `begin` enters
// the pending state, `render` draws the panel, `focus` moves the cursor,
// `submit` applies the choice, and `render_hook_action_card_actions` is the
// event-loop entry point wired into `dispatcher`.

use crate::hooks::state::{
    HookActionKind, PendingHookAction, HOOK_ACTION_AGENT, HOOK_ACTION_BOTH,
    HOOK_ACTION_OPTION_COUNT, HOOK_ACTION_SHELL,
};
use crate::i18n::MessageId;
use crate::runtime::prelude::*;
use crate::runtime::question_terminal::clear_active_question_panel;
use crate::slash::panel::render_notice_panel;
use crate::slash::prompt::write_shell_prompt;

/// Checks whether the agent layer (cosh-core registry) has a hook whose
/// `name` matches the given shell-layer hook id.
///
/// Note: the cosh-core hooks registry only exposes `list`, `enable`, and
/// `disable` actions, so an existence check has to go through the list
/// endpoint. Returns `false` when the adapter is not `CoshCore` or the
/// registry query fails — the caller treats that as "no agent hook".
pub(super) fn agent_list_contains_id(adapter: &AdapterInstance, id: &str) -> bool {
    let AdapterInstance::CoshCore(cosh_core) = adapter else {
        return false;
    };
    let Ok(data) = cosh_core.registry_query("hooks", "list", serde_json::Value::Null) else {
        return false;
    };
    data.as_array()
        .map(|arr| {
            arr.iter()
                .any(|h| h.get("name").and_then(|n| n.as_str()) == Some(id))
        })
        .unwrap_or(false)
}

/// Reports whether a hook-action disambiguation panel is currently active.
pub(crate) fn has_pending_hook_action(state: &InlineState) -> bool {
    state.hooks.pending_action.is_some()
}

/// Returns the `RawInputCapture` for the active hook-action panel so the
/// controller can route keystrokes to it.
pub(crate) fn pending_hook_action_capture(state: &InlineState) -> Option<RawInputCapture> {
    state
        .hooks
        .pending_action
        .as_ref()
        .map(|action| RawInputCapture::Question {
            id: action.panel_id.clone(),
            option_count: HOOK_ACTION_OPTION_COUNT,
            selected: action.selected_option,
            allow_free_text: false,
            multiple: false,
            secret: false,
        })
}

/// Enters the interactive disambiguation phase for a hook id that collides
/// between the shell and agent layers.
pub(super) fn begin_hook_action_confirmation(
    state: &mut InlineState,
    hook_id: &str,
    action: HookActionKind,
) {
    let panel_id = format!(
        "hook-action-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    state.hooks.pending_action = Some(PendingHookAction {
        panel_id,
        hook_id: hook_id.to_string(),
        action,
        selected_option: 0,
    });
}

/// Renders the question panel that lets the user choose which layer(s) to
/// enable/disable when a hook id collides.
pub(super) fn render_hook_action_confirmation<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    // Extract fields first so the immutable borrow on pending_action is
    // released before we mutate state.questions below.
    let (panel_id, hook_id, action_kind, selected_option) = match &state.hooks.pending_action {
        Some(a) => (
            a.panel_id.clone(),
            a.hook_id.clone(),
            a.action,
            a.selected_option,
        ),
        None => return Ok(()),
    };
    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
    let panel_width = renderer.panel_standard_width();
    let i18n = state.i18n();
    let verb = match action_kind {
        HookActionKind::Enable => i18n.t(MessageId::SlashHooksActionVerbEnable),
        HookActionKind::Disable => i18n.t(MessageId::SlashHooksActionVerbDisable),
    };
    let question = i18n.format(
        MessageId::SlashHooksActionQuestion,
        &[("id", &hook_id), ("verb", verb)],
    );
    let options = vec![
        i18n.t(MessageId::SlashHooksActionOptionShell).to_string(),
        i18n.t(MessageId::SlashHooksActionOptionAgent).to_string(),
        i18n.t(MessageId::SlashHooksActionOptionBoth).to_string(),
    ];
    let height = renderer.write_question_panel(
        output,
        QuestionPanelModel {
            id: &panel_id,
            question: &question,
            options: &options,
            selected_option,
            selected_options: &[],
            custom_answer: "",
            allow_free_text: false,
            selection_mode: QuestionSelectionMode::Single,
            input_feedback: QuestionInputFeedback::Disabled,
        },
    )?;
    // Track the panel geometry so clear_active_question_panel can erase
    // it before re-rendering on focus/submit/cancel.
    state.questions.active_panel_height = height;
    state.questions.active_panel_id = Some(panel_id);
    state.questions.active_panel_cursor_row = None;
    state.questions.active_panel_width = Some(panel_width);
    Ok(())
}

/// Moves the cursor to `selected` (clamped to the valid option range).
fn focus_hook_action_confirmation(state: &mut InlineState, selected: usize) {
    if let Some(action) = state.hooks.pending_action.as_mut() {
        action.selected_option = selected.min(HOOK_ACTION_OPTION_COUNT - 1);
    }
}

/// Applies the chosen enable/disable action to the selected layer(s), renders
/// a notice panel with the outcome, and clears the pending state. Resumes the
/// PTY prompt so the shell is ready for the next command.
fn submit_hook_action_confirmation<W: Write>(
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let pending = state.hooks.pending_action.take();
    let Some(action) = pending else {
        return Ok(());
    };
    let id = action.hook_id.as_str();
    let enable = action.action == HookActionKind::Enable;
    let i18n = state.i18n();
    let mut body = Vec::new();
    let do_shell =
        action.selected_option == HOOK_ACTION_SHELL || action.selected_option == HOOK_ACTION_BOTH;
    let do_agent =
        action.selected_option == HOOK_ACTION_AGENT || action.selected_option == HOOK_ACTION_BOTH;

    if do_shell {
        if enable {
            state.hooks.disabled.remove(id);
        } else {
            state.hooks.disabled.insert(id.to_string());
        }
        body.push(if enable {
            i18n.format(MessageId::SlashHooksEnabledBody, &[("id", id)])
        } else {
            i18n.format(MessageId::SlashHooksDisabledBody, &[("id", id)])
        });
    }

    if do_agent {
        if let AdapterInstance::CoshCore(cosh_core) = adapter {
            let params = serde_json::json!({ "name": id });
            let registry_action = if enable { "enable" } else { "disable" };
            match cosh_core.registry_query("hooks", registry_action, params) {
                Ok(_) => {
                    body.push(if enable {
                        i18n.format(MessageId::SlashHooksActionAgentEnabledBody, &[("id", id)])
                    } else {
                        i18n.format(MessageId::SlashHooksActionAgentDisabledBody, &[("id", id)])
                    });
                }
                Err(e) => {
                    body.push(i18n.format(
                        MessageId::SlashHooksActionAgentErrorBody,
                        &[("id", id), ("error", &e)],
                    ));
                }
            }
        }
    }

    // Erase the disambiguation panel before drawing the outcome notice.
    clear_active_question_panel(state, output)?;

    let title = if enable {
        i18n.t(MessageId::SlashHooksEnabledTitle)
    } else {
        i18n.t(MessageId::SlashHooksDisabledTitle)
    };
    render_notice_panel(output, title, body, None)?;

    // Resume the PTY prompt after the action is applied, mirroring the auth
    // confirmation outcome path.
    write_shell_prompt(state, output)?;
    output.flush()?;
    Ok(())
}

/// Cancels the disambiguation without applying any action.
fn cancel_hook_action_confirmation<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    // Erase the panel before drawing the cancellation notice.
    clear_active_question_panel(state, output)?;
    state.hooks.pending_action = None;
    let i18n = state.i18n();
    render_notice_panel(
        output,
        i18n.t(MessageId::SlashHooksActionCancelledTitle),
        vec![i18n.t(MessageId::SlashHooksActionCancelledBody).to_string()],
        None,
    )?;
    write_shell_prompt(state, output)?;
    output.flush()?;
    Ok(())
}

/// Parses the selected option index from a `focus` event's input field.
/// The format is `{panel_id}:{index}`.
fn parse_action_selected(event: &ShellEvent, panel_id: &str) -> Option<usize> {
    let input = event.input.as_deref()?;
    let (id, val) = input.split_once(':')?;
    if id.trim() != panel_id {
        return None;
    }
    val.trim().parse::<usize>().ok()
}

/// Reports whether `event` carries the panel id the hook-action panel is
/// currently listening on.
fn event_targets_pending_hook_action(state: &InlineState, event: &ShellEvent) -> bool {
    let Some(target_id) = event.input.as_deref() else {
        return false;
    };
    let Some(target_id) = target_id.split(':').next() else {
        return false;
    };
    state
        .hooks
        .pending_action
        .as_ref()
        .is_some_and(|action| action.panel_id == target_id.trim())
}

/// Event-loop entry point for the hook-action disambiguation panel.
///
/// Wired into `dispatcher::render_inline_guidance_from_batch`, this handles:
/// - `focus` messages whose input carries the pending action's panel id
///   (format: `{panel_id}:{selected}`),
/// - `answer` messages while a pending hook-action is active (answer events
///   do not carry a panel id, so they are routed solely by the
///   `pending_action` guard),
/// - `question_cancel` / `cancel` / `question_abort` messages whose input
///   carries the panel id.
///
/// This mirrors `auth::runtime::render_auth_card_actions`, which applies the
/// same split: answer events are unscoped, while focus/cancel events are
/// scoped to the auth capture id.
pub(crate) fn render_hook_action_card_actions<W: Write>(
    events: &[ShellEvent],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    if state.hooks.pending_action.is_none() {
        return Ok(());
    }
    for (idx, event) in events.iter().enumerate() {
        let event_index = event_index_base + idx;
        if event.kind != ShellEventKind::UserInputIntercepted {
            continue;
        }
        if !matches!(
            event.component.as_deref(),
            Some("card") | Some("card_secret")
        ) {
            continue;
        }
        let panel_id = state
            .hooks
            .pending_action
            .as_ref()
            .map(|a| a.panel_id.clone());
        let Some(panel_id) = panel_id else {
            continue;
        };
        match event.message.as_deref() {
            Some("focus") => {
                // Focus events carry the panel id in the input field
                // ({panel_id}:{selected}); verify it matches before
                // updating the selection.
                if let Some(selected) = parse_action_selected(event, &panel_id) {
                    focus_hook_action_confirmation(state, selected);
                    clear_active_question_panel(state, output)?;
                    render_hook_action_confirmation(state, output)?;
                }
            }
            Some("answer") => {
                // Answer events carry only the answer value (no panel id),
                // so they are routed by the pending_action guard at the top
                // of this function, not by the input field.
                let key = stable_event_key("hook-action-answer", event_index, event);
                if state.questions.handled_answers.contains(&key) {
                    continue;
                }
                state.questions.handled_answers.insert(key);
                submit_hook_action_confirmation(adapter, state, output)?;
            }
            Some("question_cancel") | Some("cancel") | Some("question_abort")
                if event_targets_pending_hook_action(state, event) =>
            {
                cancel_hook_action_confirmation(state, output)?;
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::AdapterInstance;

    /// Creates a state with a pending enable action for `shared-hook`.
    fn state_with_pending(action: HookActionKind) -> InlineState {
        let mut state = InlineState::default();
        begin_hook_action_confirmation(&mut state, "shared-hook", action);
        state
    }

    /// Builds a `UserInputIntercepted` card event with the given message and input.
    fn card_event(message: &str, input: &str) -> ShellEvent {
        let mut event = ShellEvent::user_input_intercepted("sess", input);
        event.component = Some("card".to_string());
        event.message = Some(message.to_string());
        event
    }

    // ---- agent_list_contains_id ----

    #[test]
    fn agent_list_contains_id_returns_false_for_fake_adapter() {
        // A Fake adapter never has agent hooks; the function must safely
        // return false instead of panicking.
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        assert!(!agent_list_contains_id(&adapter, "shared-hook"));
    }

    // ---- has_pending_hook_action / pending_hook_action_capture ----

    #[test]
    fn has_pending_hook_action_reflects_state() {
        let state = InlineState::default();
        assert!(!has_pending_hook_action(&state));

        let state = state_with_pending(HookActionKind::Enable);
        assert!(has_pending_hook_action(&state));
    }

    #[test]
    fn pending_hook_action_capture_returns_question_capture() {
        let state = state_with_pending(HookActionKind::Disable);
        let capture = pending_hook_action_capture(&state).expect("capture");
        let RawInputCapture::Question {
            id,
            option_count,
            selected,
            allow_free_text,
            multiple,
            secret,
        } = capture
        else {
            panic!("expected Question capture, got {capture:?}");
        };
        assert!(id.starts_with("hook-action-"));
        assert_eq!(option_count, HOOK_ACTION_OPTION_COUNT);
        assert_eq!(selected, 0);
        assert!(!allow_free_text);
        assert!(!multiple);
        assert!(!secret);
    }

    // ---- begin_hook_action_confirmation ----

    #[test]
    fn begin_sets_pending_action_with_panel_id() {
        let mut state = InlineState::default();
        begin_hook_action_confirmation(&mut state, "my-hook", HookActionKind::Enable);
        let action = state.hooks.pending_action.expect("pending action");
        assert!(action.panel_id.starts_with("hook-action-"));
        assert_eq!(action.hook_id, "my-hook");
        assert_eq!(action.action, HookActionKind::Enable);
        assert_eq!(action.selected_option, 0);
    }

    // ---- focus_hook_action_confirmation ----

    #[test]
    fn focus_updates_selected_option() {
        let mut state = state_with_pending(HookActionKind::Enable);
        focus_hook_action_confirmation(&mut state, 2);
        assert_eq!(
            state.hooks.pending_action.as_ref().unwrap().selected_option,
            2
        );
    }

    #[test]
    fn focus_clamps_selected_to_max_option() {
        let mut state = state_with_pending(HookActionKind::Disable);
        // Index beyond the last valid option (HOOK_ACTION_OPTION_COUNT - 1)
        // must be clamped, not stored verbatim.
        focus_hook_action_confirmation(&mut state, 99);
        assert_eq!(
            state.hooks.pending_action.as_ref().unwrap().selected_option,
            HOOK_ACTION_OPTION_COUNT - 1
        );
    }

    #[test]
    fn focus_is_noop_without_pending_action() {
        let mut state = InlineState::default();
        focus_hook_action_confirmation(&mut state, 1);
        assert!(state.hooks.pending_action.is_none());
    }

    // ---- parse_action_selected ----

    #[test]
    fn parse_action_selected_extracts_index() {
        let event = card_event("focus", "hook-action-42:1");
        assert_eq!(parse_action_selected(&event, "hook-action-42"), Some(1));
    }

    #[test]
    fn parse_action_selected_rejects_wrong_panel_id() {
        let event = card_event("focus", "hook-action-42:1");
        assert_eq!(parse_action_selected(&event, "hook-action-99"), None);
    }

    #[test]
    fn parse_action_selected_rejects_missing_colon() {
        let event = card_event("focus", "no-colon-here");
        assert_eq!(parse_action_selected(&event, "hook-action-42"), None);
    }

    #[test]
    fn parse_action_selected_rejects_non_numeric_index() {
        let event = card_event("focus", "hook-action-42:abc");
        assert_eq!(parse_action_selected(&event, "hook-action-42"), None);
    }

    // ---- event_targets_pending_hook_action ----

    #[test]
    fn event_targets_matching_panel_id() {
        let state = state_with_pending(HookActionKind::Enable);
        let panel_id = state
            .hooks
            .pending_action
            .as_ref()
            .unwrap()
            .panel_id
            .clone();
        // Cancel events carry the panel_id as the input (possibly with a
        // trailing `:suffix` from the raw_relay layer).
        let event = card_event("cancel", &panel_id);
        assert!(event_targets_pending_hook_action(&state, &event));
    }

    #[test]
    fn event_targets_rejects_mismatched_panel_id() {
        let state = state_with_pending(HookActionKind::Enable);
        let event = card_event("cancel", "hook-action-wrong:0");
        assert!(!event_targets_pending_hook_action(&state, &event));
    }

    // ---- submit_hook_action_confirmation (shell-only path) ----

    #[test]
    fn submit_shell_option_enables_hook() {
        let mut state = state_with_pending(HookActionKind::Enable);
        // Pre-disable so we can verify the enable takes effect.
        state.hooks.disabled.insert("shared-hook".to_string());
        // Select the Shell option.
        state.hooks.pending_action.as_mut().unwrap().selected_option = HOOK_ACTION_SHELL;

        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let mut output = Vec::new();
        submit_hook_action_confirmation(&adapter, &mut state, &mut output).expect("submit");

        // pending_action must be consumed.
        assert!(state.hooks.pending_action.is_none());
        // The hook must be removed from the disabled set.
        assert!(!state.hooks.disabled.contains("shared-hook"));
        // Output must contain the enable notice.
        let out = String::from_utf8(output).expect("utf8");
        assert!(out.contains("shared-hook"), "{out}");
    }

    #[test]
    fn submit_shell_option_disables_hook() {
        let mut state = state_with_pending(HookActionKind::Disable);
        state.hooks.pending_action.as_mut().unwrap().selected_option = HOOK_ACTION_SHELL;

        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let mut output = Vec::new();
        submit_hook_action_confirmation(&adapter, &mut state, &mut output).expect("submit");

        assert!(state.hooks.pending_action.is_none());
        assert!(state.hooks.disabled.contains("shared-hook"));
    }

    #[test]
    fn submit_both_option_enables_shell_layer_with_fake_adapter() {
        // With a Fake adapter the agent layer is silently skipped (the
        // `if let AdapterInstance::CoshCore` guard does not match), but the
        // shell layer must still be applied.
        let mut state = state_with_pending(HookActionKind::Enable);
        state.hooks.disabled.insert("shared-hook".to_string());
        state.hooks.pending_action.as_mut().unwrap().selected_option = HOOK_ACTION_BOTH;

        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let mut output = Vec::new();
        submit_hook_action_confirmation(&adapter, &mut state, &mut output).expect("submit");

        assert!(state.hooks.pending_action.is_none());
        assert!(!state.hooks.disabled.contains("shared-hook"));
    }

    // ---- cancel_hook_action_confirmation ----

    #[test]
    fn cancel_clears_pending_action() {
        let mut state = state_with_pending(HookActionKind::Enable);
        let mut output = Vec::new();
        cancel_hook_action_confirmation(&mut state, &mut output).expect("cancel");

        assert!(state.hooks.pending_action.is_none());
        let out = String::from_utf8(output).expect("utf8");
        assert!(out.contains("cancelled"), "{out}");
    }

    // ---- render_hook_action_card_actions (event routing) ----

    #[test]
    fn card_actions_skips_when_no_pending_action() {
        // Without a pending action the function must return immediately
        // without processing any events.
        let mut state = InlineState::default();
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let events = [card_event("answer", "0")];
        let mut output = Vec::new();
        render_hook_action_card_actions(&events, &adapter, &mut state, &mut output, 0)
            .expect("no-op");
        assert!(state.hooks.pending_action.is_none());
    }

    #[test]
    fn card_actions_ignores_non_card_events() {
        let mut state = state_with_pending(HookActionKind::Enable);
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        // A non-card component must be ignored.
        let mut event = ShellEvent::user_input_intercepted("sess", "0");
        event.component = Some("shell_pty_input".to_string());
        event.message = Some("answer".to_string());
        let mut output = Vec::new();
        render_hook_action_card_actions(&[event], &adapter, &mut state, &mut output, 0)
            .expect("ok");
        // pending_action must still be set (answer was not processed).
        assert!(state.hooks.pending_action.is_some());
    }

    #[test]
    fn card_actions_routes_answer_to_submit() {
        let mut state = state_with_pending(HookActionKind::Enable);
        state.hooks.disabled.insert("shared-hook".to_string());
        state.hooks.pending_action.as_mut().unwrap().selected_option = HOOK_ACTION_SHELL;

        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let events = [card_event("answer", "0")];
        let mut output = Vec::new();
        render_hook_action_card_actions(&events, &adapter, &mut state, &mut output, 0)
            .expect("submit");

        // submit_hook_action_confirmation must have run: pending is gone,
        // hook is enabled.
        assert!(state.hooks.pending_action.is_none());
        assert!(!state.hooks.disabled.contains("shared-hook"));
    }

    #[test]
    fn card_actions_deduplicates_answer_events() {
        let mut state = state_with_pending(HookActionKind::Disable);
        state.hooks.pending_action.as_mut().unwrap().selected_option = HOOK_ACTION_SHELL;

        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        // Two identical answer events in the same batch: only the first must
        // be processed (the second is filtered by handled_answers).
        let events = [card_event("answer", "0"), card_event("answer", "0")];
        let mut output = Vec::new();
        render_hook_action_card_actions(&events, &adapter, &mut state, &mut output, 0).expect("ok");

        // The hook must be disabled exactly once (single insert).
        assert!(state.hooks.disabled.contains("shared-hook"));
    }

    #[test]
    fn card_actions_routes_cancel_event() {
        let mut state = state_with_pending(HookActionKind::Enable);
        let panel_id = state
            .hooks
            .pending_action
            .as_ref()
            .unwrap()
            .panel_id
            .clone();

        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let events = [card_event("cancel", &panel_id)];
        let mut output = Vec::new();
        render_hook_action_card_actions(&events, &adapter, &mut state, &mut output, 0)
            .expect("cancel");

        assert!(state.hooks.pending_action.is_none());
    }

    #[test]
    fn card_actions_ignores_cancel_with_wrong_panel_id() {
        let mut state = state_with_pending(HookActionKind::Enable);
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        // A cancel event targeting a different panel id must not cancel.
        let events = [card_event("cancel", "hook-action-wrong")];
        let mut output = Vec::new();
        render_hook_action_card_actions(&events, &adapter, &mut state, &mut output, 0).expect("ok");
        assert!(state.hooks.pending_action.is_some());
    }

    #[test]
    fn card_actions_routes_focus_to_selection_update() {
        let mut state = state_with_pending(HookActionKind::Enable);
        let panel_id = state
            .hooks
            .pending_action
            .as_ref()
            .unwrap()
            .panel_id
            .clone();

        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let events = [card_event("focus", &format!("{panel_id}:2"))];
        let mut output = Vec::new();
        render_hook_action_card_actions(&events, &adapter, &mut state, &mut output, 0)
            .expect("focus");

        assert_eq!(
            state.hooks.pending_action.as_ref().unwrap().selected_option,
            2
        );
    }
}
