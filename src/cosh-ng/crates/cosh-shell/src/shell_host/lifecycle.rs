use std::io;

use crate::journal::{redacted_shell_events, write_shell_events};
use crate::types::{ShellEvent, ShellEventKind};

use super::model::{ShellHostConfig, ShellHostOutput};
use super::osc::{now_ms, OscParser};

pub(super) fn push_shell_started_event(parser: &mut OscParser, config: &ShellHostConfig) {
    parser.events.push(ShellEvent {
        kind: ShellEventKind::ShellStarted,
        session_id: config.session_id.clone(),
        command_id: None,
        command: None,
        cwd: std::env::current_dir()
            .ok()
            .map(|path| path.display().to_string()),
        end_cwd: None,
        exit_code: None,
        started_at_ms: Some(now_ms()),
        ended_at_ms: None,
        duration_ms: None,
        terminal_output_ref: None,
        terminal_output_bytes: None,
        input: None,
        component: None,
        message: None,
        command_origin: None,
        shell_environment_generation: None,
        audit_identity: None,
        routing: None,
        capture: None,
    });
}

pub(super) fn push_shell_exited_event(
    parser: &mut OscParser,
    config: &ShellHostConfig,
    exit_status: Option<i32>,
) -> io::Result<()> {
    // A shell exit without a waitable status leaves the in-flight command's
    // outcome unknown; surface the -1 missing-exit sentinel instead of
    // fabricating success (#2413), matching the ledger contract from #2105.
    parser.finish_current_on_exit(exit_status.unwrap_or(-1))?;
    parser.events.push(ShellEvent {
        kind: ShellEventKind::ShellExited,
        session_id: config.session_id.clone(),
        command_id: None,
        command: None,
        cwd: None,
        end_cwd: None,
        exit_code: exit_status,
        started_at_ms: None,
        ended_at_ms: Some(now_ms()),
        duration_ms: None,
        terminal_output_ref: None,
        terminal_output_bytes: None,
        input: None,
        component: None,
        message: None,
        command_origin: None,
        shell_environment_generation: None,
        audit_identity: None,
        routing: None,
        capture: None,
    });
    Ok(())
}

pub(super) fn finish_shell_host_output(
    config: &ShellHostConfig,
    mut parser: OscParser,
    exit_status: Option<i32>,
) -> io::Result<ShellHostOutput> {
    push_shell_exited_event(&mut parser, config, exit_status)?;
    build_shell_host_output(config, parser, exit_status)
}

pub(super) fn build_shell_host_output(
    config: &ShellHostConfig,
    mut parser: OscParser,
    exit_status: Option<i32>,
) -> io::Result<ShellHostOutput> {
    let incremental_journal = parser.uses_incremental_event_journal();
    let journal_path = parser
        .incremental_event_journal_path()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| config.work_dir.join("events.jsonl"));
    let events = redacted_shell_events(&parser.take_output_events()?);
    if !incremental_journal {
        write_shell_events(&journal_path, &events)?;
    }
    let terminal_output = parser.clean.into_output_bytes();

    Ok(ShellHostOutput {
        events,
        terminal_output,
        work_dir: config.work_dir.clone(),
        journal_path,
        exit_status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ShellEventKind;

    // #2413: a shell exit without a waitable status (killed by a signal on
    // the non-EOF shutdown path) leaves the in-flight command's outcome
    // unknown; defaulting the missing status to success would fabricate a
    // completed block for it. Fall toward the -1 sentinel instead, aligned
    // with the ledger missing-exit contract (#2105/PR #2412).
    #[test]
    fn missing_shell_exit_status_fails_in_flight_command_with_sentinel() {
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-shell-lifecycle-missing-status-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&work_dir).expect("work dir");
        let output_ref_dir = work_dir.join("output-refs");
        std::fs::create_dir_all(&output_ref_dir).expect("output ref dir");
        let config = ShellHostConfig::new("lifecycle-missing-status", &work_dir);
        let mut parser = OscParser::new(
            "lifecycle-missing-status".to_string(),
            output_ref_dir,
            "test-marker-token".to_string(),
        );
        parser
            .feed(b"\x1b]1337;COSH;{\"event\":\"preexec\",\"token\":\"test-marker-token\",\"command\":\"sleep 60\",\"cwd\":\"/tmp\"}\x07")
            .expect("feed preexec");

        push_shell_exited_event(&mut parser, &config, None).expect("shell exited event");

        let failed = parser
            .events
            .iter()
            .find(|event| event.kind == ShellEventKind::CommandFailed)
            .expect("in-flight command fails on unknown shell exit");
        assert_eq!(
            failed.exit_code,
            Some(-1),
            "missing shell exit status must surface the sentinel, not success 0"
        );
        assert_eq!(failed.command.as_deref(), Some("sleep 60"));

        // The shell's own exit code stays unknown; only the in-flight command
        // falls toward failure.
        let exited = parser
            .events
            .iter()
            .find(|event| event.kind == ShellEventKind::ShellExited)
            .expect("shell exited event");
        assert_eq!(exited.exit_code, None);

        let _ = std::fs::remove_dir_all(&work_dir);
    }

    // qoderai #2709 P1: pin the exit-status mapping at both boundaries of
    // `push_shell_exited_event` — an explicit status flows through verbatim
    // to both events, while a missing status surfaces the -1 sentinel for
    // the in-flight command and keeps `ShellExited` itself unknown.
    #[test]
    fn shell_exit_status_mapping_pins_explicit_and_missing_boundaries() {
        for (exit_status, command_kind, command_exit, shell_exit) in [
            (Some(0), ShellEventKind::CommandCompleted, Some(0), Some(0)),
            (None, ShellEventKind::CommandFailed, Some(-1), None),
        ] {
            let case = exit_status.map_or(
                "none",
                |status| {
                    if status == 0 {
                        "zero"
                    } else {
                        "nonzero"
                    }
                },
            );
            let work_dir = std::env::temp_dir().join(format!(
                "cosh-shell-lifecycle-boundary-{case}-{}",
                std::process::id()
            ));
            std::fs::create_dir_all(&work_dir).expect("work dir");
            let output_ref_dir = work_dir.join("output-refs");
            std::fs::create_dir_all(&output_ref_dir).expect("output ref dir");
            let config = ShellHostConfig::new("lifecycle-boundary", &work_dir);
            let mut parser = OscParser::new(
                "lifecycle-boundary".to_string(),
                output_ref_dir,
                "test-marker-token".to_string(),
            );
            parser
                .feed(b"\x1b]1337;COSH;{\"event\":\"preexec\",\"token\":\"test-marker-token\",\"command\":\"sleep 60\",\"cwd\":\"/tmp\"}\x07")
                .expect("feed preexec");

            push_shell_exited_event(&mut parser, &config, exit_status).expect("shell exited event");

            let command = parser
                .events
                .iter()
                .find(|event| event.kind == command_kind)
                .unwrap_or_else(|| panic!("command finish event for {exit_status:?}"));
            assert_eq!(command.exit_code, command_exit, "{exit_status:?}");
            let exited = parser
                .events
                .iter()
                .find(|event| event.kind == ShellEventKind::ShellExited)
                .expect("shell exited event");
            assert_eq!(exited.exit_code, shell_exit, "{exit_status:?}");

            let _ = std::fs::remove_dir_all(&work_dir);
        }
    }
}
