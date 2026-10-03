// Owner: runtime controller (input-wait timeout, issue #2161). Consumes
// the interactive sentinel's shared episode clock and interrupts the
// foreground handoff once an eligible input-wait stays unanswered past
// `shell.input_wait_timeout_secs`. Unlike the legacy wall-clock timeout
// in controller.rs this never fires on evidence-free long commands: the
// clock only runs while the foreground process group is blocked reading
// the session tty in line mode (alt-screen exempt, D10).
use std::io::Write;
use std::time::Duration;

use crate::runtime::prelude::*;
use crate::runtime::state::InlineState;

pub(super) fn input_wait_timeout_recovery_action<W: Write>(
    state: &mut InlineState,
    shell_busy: bool,
    output: &mut W,
) -> std::io::Result<Option<RawObserverAction>> {
    let waited = state.input_wait_status.waiting_for();
    input_wait_timeout_recovery_action_with(
        state,
        shell_busy,
        output,
        state.input_wait_timeout,
        waited,
    )
}

fn input_wait_timeout_recovery_action_with<W: Write>(
    state: &mut InlineState,
    shell_busy: bool,
    output: &mut W,
    timeout: Option<Duration>,
    waited: Option<Duration>,
) -> std::io::Result<Option<RawObserverAction>> {
    let shell_handoff_pending = state.control.shell_handoff().pending_front().is_some();
    if !shell_busy && !shell_handoff_pending {
        if let Some(timeout) = state.pending_input_wait_timeout_notice.take() {
            render_input_wait_timeout_notice(state, output, timeout)?;
        }
        // Facts for handoffs that never reached delivery (untracked/denied
        // closures) must not leak into a later command's result.
        state.input_wait_facts.clear();
        return Ok(None);
    }

    // Record detected-wait facts for the active handoff even when the
    // timeout is disabled: the provider learns the command was interactive
    // (detected-without-interrupt contract form).
    let front_approval_id = state
        .control
        .shell_handoff()
        .pending_front()
        .map(|handoff| handoff.request().approval_id.clone());
    if let (Some(approval_id), Some(waited)) = (&front_approval_id, waited) {
        let facts = state.input_wait_facts.entry(approval_id.clone()).or_insert(
            crate::adapter::HostExecutedInputWait {
                waited_secs: 0,
                interrupted: false,
            },
        );
        facts.waited_secs = facts.waited_secs.max(waited.as_secs());
    }

    let Some(timeout) = timeout else {
        return Ok(None);
    };
    let Some(waited) = waited else {
        return Ok(None);
    };
    if waited < timeout {
        return Ok(None);
    }
    if !state
        .control
        .shell_handoff_mut()
        .mark_input_wait_interrupt()
    {
        return Ok(None);
    }
    if let Some(approval_id) = front_approval_id {
        if let Some(facts) = state.input_wait_facts.get_mut(&approval_id) {
            facts.interrupted = true;
        }
    }
    // One interrupt per episode: restart the clock so a follow-up command
    // in the same handoff cannot double-fire before evidence returns.
    state.input_wait_status.clear();
    state.pending_input_wait_timeout_notice = Some(timeout);
    Ok(Some(RawObserverAction::InterruptForeground))
}

fn render_input_wait_timeout_notice<W: Write>(
    state: &InlineState,
    output: &mut W,
    timeout: Duration,
) -> std::io::Result<()> {
    let i18n = state.i18n();
    let timeout_secs = timeout.as_secs().to_string();
    RatatuiInlineRenderer::for_terminal()
        .with_language(state.language)
        .write_notice_panel(
            output,
            NoticePanelModel {
                title: i18n.t(MessageId::ApprovalShellHandoffInputWaitTimeoutTitle),
                body: vec![
                    i18n.format(
                        MessageId::ApprovalShellHandoffInputWaitTimeoutExceededBody,
                        &[("seconds", &timeout_secs)],
                    ),
                    i18n.t(MessageId::ApprovalShellHandoffInputWaitTimeoutInterruptBody)
                        .to_string(),
                ],
                footer: None,
            },
        )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ShellHandoffRequest;

    fn input_wait_test_state() -> InlineState {
        let mut state = InlineState::default();
        let request = ShellHandoffRequest::new(
            "bash repro.sh",
            "$ bash repro.sh",
            "approved_provider_shell_tool",
            "user",
            "req-input-wait",
            "run-input-wait",
            1,
        )
        .expect("handoff request");
        state
            .control
            .shell_handoff_mut()
            .enqueue_approved_request(request);
        state
            .control
            .shell_handoff_mut()
            .emit_next_approved(0)
            .expect("emit handoff");
        state
    }

    #[test]
    fn input_wait_timeout_interrupts_once_and_defers_notice() {
        let mut state = input_wait_test_state();
        let mut output = Vec::new();

        // Waited beyond the threshold => interrupt exactly once.
        let action = input_wait_timeout_recovery_action_with(
            &mut state,
            true,
            &mut output,
            Some(Duration::from_secs(1)),
            Some(Duration::from_secs(2)),
        )
        .expect("input-wait action");
        assert_eq!(action, Some(RawObserverAction::InterruptForeground));
        assert!(output.is_empty(), "{}", String::from_utf8_lossy(&output));

        // Same handoff cannot double-fire even if evidence re-appears.
        let action = input_wait_timeout_recovery_action_with(
            &mut state,
            true,
            &mut output,
            Some(Duration::from_secs(1)),
            Some(Duration::from_secs(5)),
        )
        .expect("second input-wait action");
        assert_eq!(action, None);

        // Notice renders only after the foreground is idle again.
        state
            .control
            .shell_handoff_mut()
            .pop_pending()
            .expect("handoff finished");
        let mut idle_output = Vec::new();
        let action = input_wait_timeout_recovery_action_with(
            &mut state,
            false,
            &mut idle_output,
            Some(Duration::from_secs(1)),
            None,
        )
        .expect("notice pass");
        let idle_text = String::from_utf8_lossy(&idle_output);
        assert_eq!(action, None);
        assert!(
            idle_text.contains("Foreground command waited for keyboard input over 1s"),
            "{idle_text}"
        );
        assert!(
            idle_text.contains("Interrupted the command (like Ctrl+C)"),
            "{idle_text}"
        );
    }

    #[test]
    fn input_wait_timeout_requires_threshold_and_configuration() {
        let mut state = input_wait_test_state();
        let mut output = Vec::new();

        // Below threshold: clock resets are the sentinel's job; no action.
        let action = input_wait_timeout_recovery_action_with(
            &mut state,
            true,
            &mut output,
            Some(Duration::from_secs(120)),
            Some(Duration::from_secs(119)),
        )
        .expect("below-threshold action");
        assert_eq!(action, None);

        // Disabled (None) never fires, regardless of waited time.
        let action = input_wait_timeout_recovery_action_with(
            &mut state,
            true,
            &mut output,
            None,
            Some(Duration::from_secs(3600)),
        )
        .expect("disabled action");
        assert_eq!(action, None);

        // Probe unavailable (waited=None) behaves like the status quo.
        let action = input_wait_timeout_recovery_action_with(
            &mut state,
            true,
            &mut output,
            Some(Duration::from_secs(1)),
            None,
        )
        .expect("no-evidence action");
        assert_eq!(action, None);
        assert!(output.is_empty(), "{}", String::from_utf8_lossy(&output));
    }
}
