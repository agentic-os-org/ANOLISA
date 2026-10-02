use std::collections::BTreeMap;

use crate::types::{
    CommandBlock, CommandOrigin, CommandStatus, OutputRefs, ShellEvent, ShellEventKind,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerOutput {
    pub blocks: Vec<CommandBlock>,
    pub errors: Vec<String>,
}

pub fn build_command_blocks(events: &[ShellEvent]) -> LedgerOutput {
    let mut starts = BTreeMap::new();
    // Starts closed by a correlated intercept (#2106). Ledger contract for a
    // started command: it yields a block (finish pairs a live or intercepted
    // start), or an explicit command_started_without_finish error (start
    // neither intercepted nor finished), or a by-design silent close (start
    // intercepted and never finished — the NL interception path from #1742,
    // which must stay free of block and error output). This map holds the
    // third state: excluded from the started-without-finish sweep, yet still
    // able to pair a later finish so an intercepted command that ran to
    // completion keeps its block instead of degrading into
    // command_finished_without_start.
    let mut intercepted = BTreeMap::new();
    let mut blocks = Vec::new();
    let mut errors = Vec::new();

    for event in events {
        match &event.kind {
            ShellEventKind::CommandStarted => {
                if let Some(command_id) = &event.command_id {
                    starts.insert(command_id.clone(), event.clone());
                } else {
                    errors.push("command_started_missing_id".to_string());
                }
            }
            ShellEventKind::CommandCompleted | ShellEventKind::CommandFailed => {
                let Some(command_id) = &event.command_id else {
                    errors.push("command_finished_missing_id".to_string());
                    continue;
                };

                let Some(start) = starts
                    .remove(command_id)
                    .or_else(|| intercepted.remove(command_id))
                else {
                    errors.push(format!("command_finished_without_start:{command_id}"));
                    continue;
                };

                let started_at_ms = start.started_at_ms.unwrap_or(0);
                let ended_at_ms = event.ended_at_ms.unwrap_or(started_at_ms);
                let duration_ms = event
                    .duration_ms
                    .unwrap_or_else(|| ended_at_ms.saturating_sub(started_at_ms));
                // Exit-code narrowing contract (Option<i32> -> i32):
                //   Some(c)                 -> c verbatim; the aggregator never
                //                              rewrites explicit exit codes.
                //   None + CommandFailed    -> -1 sentinel; a failed finish
                //                              without an exit code (replayed
                //                              journals from older or external
                //                              producers) must fall toward
                //                              failure, never toward success 0.
                //                              -1 sits outside the shell's
                //                              0..=255 exit domain and matches
                //                              the missing-exit-code sentinel
                //                              the agent host-executed chain
                //                              already uses (#2105).
                //   None + other kinds      -> 0; the finish is logically
                //                              successful and only the actual
                //                              exit code went unrecorded.
                // Invariant: missing data never fabricates success for a
                // failed command.
                let exit_code = event.exit_code.unwrap_or(match &event.kind {
                    ShellEventKind::CommandFailed => -1,
                    _ => 0,
                });
                let status =
                    if matches!(&event.kind, ShellEventKind::CommandFailed) || exit_code != 0 {
                        CommandStatus::Failed
                    } else {
                        CommandStatus::Completed
                    };

                blocks.push(CommandBlock {
                    id: command_id.clone(),
                    session_id: event.session_id.clone(),
                    command: start.command.unwrap_or_default(),
                    origin: start.command_origin.unwrap_or(CommandOrigin::Unknown),
                    cwd: start.cwd.clone().unwrap_or_default(),
                    end_cwd: event
                        .end_cwd
                        .clone()
                        .or_else(|| event.cwd.clone())
                        .or(start.cwd)
                        .unwrap_or_default(),
                    started_at_ms,
                    ended_at_ms,
                    duration_ms,
                    exit_code,
                    status,
                    output: OutputRefs {
                        terminal_output_ref: event.terminal_output_ref.clone(),
                        terminal_output_bytes: event.terminal_output_bytes.unwrap_or(0),
                    },
                    shell_environment_generation: start.shell_environment_generation,
                    audit_identity: event.audit_identity.clone().or(start.audit_identity),
                });
            }
            ShellEventKind::UserInputIntercepted => {
                if let Some(command_id) = &event.command_id {
                    if let Some(start) = starts.remove(command_id) {
                        intercepted.insert(command_id.clone(), start);
                    }
                }
            }
            _ => {}
        }
    }

    for command_id in starts.keys() {
        errors.push(format!("command_started_without_finish:{command_id}"));
    }

    LedgerOutput { blocks, errors }
}

#[cfg(test)]
mod tests {
    use super::build_command_blocks;
    use crate::types::{ShellEvent, ShellEventKind};

    #[test]
    fn command_block_copies_generation_only_from_start_event() {
        let mut start = ShellEvent::command_started("session", "command", "echo ok", "/tmp", 1);
        start.shell_environment_generation = Some(7);
        let mut finish = ShellEvent::command_finished(
            ShellEventKind::CommandCompleted,
            "session",
            "command",
            0,
            2,
            "/tmp/output",
        );
        finish.shell_environment_generation = Some(99);

        let output = build_command_blocks(&[start, finish]);

        assert!(output.errors.is_empty());
        assert_eq!(output.blocks[0].shell_environment_generation, Some(7));
    }

    #[test]
    fn command_failed_no_exit_code_does_not_default_to_zero() {
        let start = ShellEvent::command_started("session", "command", "sleep 60", "/tmp", 1);
        let mut finish = ShellEvent::command_finished(
            ShellEventKind::CommandFailed,
            "session",
            "command",
            0,
            2,
            "/tmp/output",
        );
        finish.exit_code = None;

        let output = build_command_blocks(&[start, finish]);

        assert!(output.errors.is_empty());
        assert_eq!(output.blocks.len(), 1);
        let block = &output.blocks[0];
        assert_eq!(block.status, crate::types::CommandStatus::Failed);
        assert_eq!(
            block.exit_code, -1,
            "CommandFailed without an exit code must surface the missing-exit sentinel, not success 0"
        );
    }

    #[test]
    fn command_completed_no_exit_code_keeps_success_zero() {
        let start = ShellEvent::command_started("session", "command", "echo ok", "/tmp", 1);
        let mut finish = ShellEvent::command_finished(
            ShellEventKind::CommandCompleted,
            "session",
            "command",
            0,
            2,
            "/tmp/output",
        );
        finish.exit_code = None;

        let output = build_command_blocks(&[start, finish]);

        assert!(output.errors.is_empty());
        assert_eq!(output.blocks.len(), 1);
        let block = &output.blocks[0];
        assert_eq!(block.status, crate::types::CommandStatus::Completed);
        assert_eq!(block.exit_code, 0);
    }

    #[test]
    fn explicit_exit_code_passes_through_verbatim() {
        let start = ShellEvent::command_started("session", "command", "grep x y", "/tmp", 1);
        let finish = ShellEvent::command_finished(
            ShellEventKind::CommandFailed,
            "session",
            "command",
            2,
            2,
            "/tmp/output",
        );

        let output = build_command_blocks(&[start, finish]);

        assert!(output.errors.is_empty());
        assert_eq!(output.blocks.len(), 1);
        let block = &output.blocks[0];
        assert_eq!(block.status, crate::types::CommandStatus::Failed);
        assert_eq!(
            block.exit_code, 2,
            "the aggregator must never rewrite an explicit exit code"
        );
    }

    #[test]
    fn correlated_intercept_closes_start_without_creating_command_block() {
        let start = ShellEvent::command_started("session", "command", "Who are you", "/tmp", 1);
        let mut intercept = ShellEvent::user_input_intercepted("session", "Who are you");
        intercept.command_id = Some("command".to_string());
        intercept.component = Some("natural_language".to_string());

        let output = build_command_blocks(&[start, intercept]);

        assert!(output.errors.is_empty());
        assert!(output.blocks.is_empty());
    }

    #[test]
    fn user_input_intercepted_does_not_drop_subsequent_finish() {
        let start = ShellEvent::command_started("session", "command", "echo ok", "/tmp", 1);
        let mut intercept = ShellEvent::user_input_intercepted("session", "echo ok");
        intercept.command_id = Some("command".to_string());
        intercept.component = Some("natural_language".to_string());
        let finish = ShellEvent::command_finished(
            ShellEventKind::CommandCompleted,
            "session",
            "command",
            0,
            2,
            "/tmp/output",
        );

        let output = build_command_blocks(&[start, intercept, finish]);

        assert_eq!(output.blocks.len(), 1, "errors: {:?}", output.errors);
        assert!(
            !output
                .errors
                .iter()
                .any(|error| error.starts_with("command_finished_without_start")),
            "errors: {:?}",
            output.errors
        );
    }

    #[test]
    fn user_input_intercepted_does_not_drop_subsequent_failed_finish() {
        let start = ShellEvent::command_started("session", "command", "echo ok", "/tmp", 1);
        let mut intercept = ShellEvent::user_input_intercepted("session", "echo ok");
        intercept.command_id = Some("command".to_string());
        intercept.component = Some("natural_language".to_string());
        let finish = ShellEvent::command_finished(
            ShellEventKind::CommandFailed,
            "session",
            "command",
            1,
            2,
            "/tmp/output",
        );

        let output = build_command_blocks(&[start, intercept, finish]);

        assert_eq!(output.blocks.len(), 1, "errors: {:?}", output.errors);
        assert!(output.errors.is_empty(), "errors: {:?}", output.errors);
    }
}
