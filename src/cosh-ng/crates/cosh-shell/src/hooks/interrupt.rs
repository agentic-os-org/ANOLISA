use std::fs;

use super::prelude::{
    classify_command_interaction, interactive_cancel_output, CommandBlock, PtyRequirement,
    ShellEvent, ShellEventKind,
};

pub(crate) fn command_should_skip_failure_analysis(
    events: &[ShellEvent],
    block: &CommandBlock,
) -> bool {
    command_has_user_interrupt_event(events, block) || command_looks_like_interactive_cancel(block)
}

fn command_has_user_interrupt_event(events: &[ShellEvent], block: &CommandBlock) -> bool {
    events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.component.as_deref() == Some("control")
            && event.input.as_deref() == Some("ctrl_c")
            && event.started_at_ms.is_some_and(|timestamp| {
                timestamp >= block.started_at_ms && timestamp <= block.ended_at_ms
            })
    })
}

fn command_looks_like_interactive_cancel(block: &CommandBlock) -> bool {
    if block.exit_code != 1 {
        return false;
    }

    let output = output_preview(block);
    if output.is_empty() {
        return false;
    }
    if interactive_cancel_output(&output) {
        return true;
    }

    classify_command_interaction(&block.command).pty_requirement == PtyRequirement::Required
        && prompt_like_output(&output)
}

fn output_preview(block: &CommandBlock) -> String {
    let Some(path) = block.output.terminal_output_ref.as_deref() else {
        return String::new();
    };
    let Ok(content) = fs::read_to_string(path) else {
        return String::new();
    };
    content.chars().take(4096).collect()
}

fn prompt_like_output(output: &str) -> bool {
    let trimmed = output.trim_end();
    let normalized = trimmed.to_ascii_lowercase();
    trimmed.ends_with(':')
        && [
            "password",
            "passphrase",
            "access key",
            "secret",
            "token",
            "region",
            "profile",
            "please input",
            "please enter",
            "enter ",
        ]
        .iter()
        .any(|needle| normalized.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::prelude::{CommandStatus, OutputRefs};

    fn block() -> CommandBlock {
        CommandBlock {
            id: "cmd-1".to_string(),
            session_id: "session".to_string(),
            command: "sudo df -h".to_string(),
            origin: Default::default(),
            cwd: "/tmp".to_string(),
            end_cwd: "/tmp".to_string(),
            started_at_ms: 100,
            ended_at_ms: 200,
            duration_ms: 100,
            exit_code: 1,
            status: CommandStatus::Failed,
            output: OutputRefs {
                terminal_output_ref: None,
                terminal_output_bytes: 0,
            },
            shell_environment_generation: None,
            audit_identity: None,
        }
    }

    fn ctrl_c_at(timestamp: u64) -> ShellEvent {
        let mut event = ShellEvent::user_input_intercepted("session", "ctrl_c");
        event.component = Some("control".to_string());
        event.started_at_ms = Some(timestamp);
        event
    }

    #[test]
    fn detects_ctrl_c_during_command_window() {
        assert!(command_should_skip_failure_analysis(
            &[ctrl_c_at(150)],
            &block()
        ));
    }

    #[test]
    fn ignores_ctrl_c_outside_command_window() {
        assert!(!command_should_skip_failure_analysis(
            &[ctrl_c_at(99)],
            &block()
        ));
        assert!(!command_should_skip_failure_analysis(
            &[ctrl_c_at(201)],
            &block()
        ));
    }

    #[test]
    fn detects_sudo_password_required_output_without_ctrl_c_event() {
        let mut block = block();
        block.output.terminal_output_ref = Some(write_output("sudo: a password is required\n"));

        assert!(command_should_skip_failure_analysis(&[], &block));
    }

    #[test]
    fn detects_aliyun_configure_prompt_without_ctrl_c_event() {
        let mut block = block();
        block.command = "aliyun configure".to_string();
        block.output.terminal_output_ref = Some(write_output("Access Key Id []:"));

        assert!(command_should_skip_failure_analysis(&[], &block));
    }

    #[test]
    fn detects_bare_interrupted_line_as_cancel() {
        let mut block = block();
        block.command = "pip install requests".to_string();
        block.output.terminal_output_ref = Some(write_output("Collecting requests\nInterrupted\n"));

        assert!(command_should_skip_failure_analysis(&[], &block));
    }

    #[test]
    fn detects_ansi_colored_interrupted_line_as_cancel() {
        let mut block = block();
        block.command = "pip install requests".to_string();
        block.output.terminal_output_ref = Some(write_output(
            "Collecting requests\n\x1b[31mInterrupted\x1b[0m\n",
        ));

        assert!(command_should_skip_failure_analysis(&[], &block));
    }

    #[test]
    fn detects_ansi_colored_credential_prompt_as_cancel() {
        let mut block = block();
        block.command = "aliyun configure".to_string();
        block.output.terminal_output_ref = Some(write_output("\x1b[36mAccess Key Id []:\x1b[0m"));

        assert!(command_should_skip_failure_analysis(&[], &block));
    }

    #[test]
    fn keeps_credential_error_header_analyzable() {
        let mut block = block();
        block.command = "ossutil ls oss://bucket".to_string();
        block.output.terminal_output_ref = Some(write_output(
            "Error: failed to validate access key id:\n  the key contains invalid characters\n",
        ));

        assert!(!command_should_skip_failure_analysis(&[], &block));
    }

    #[test]
    fn keeps_failures_with_interrupted_substring_analyzable() {
        let mut block = block();
        block.command = "curl https://example.com/api".to_string();
        block.output.terminal_output_ref = Some(write_output(
            "curl: (18) transfer was interrupted by peer\n",
        ));

        assert!(!command_should_skip_failure_analysis(&[], &block));
    }

    #[test]
    fn keeps_failures_mentioning_access_key_analyzable() {
        let mut block = block();
        block.command = "ossutil cp local oss://bucket/key".to_string();
        block.output.terminal_output_ref = Some(write_output(
            "Error: InvalidAccessKeyId, the access key id you provided does not exist\n",
        ));

        assert!(!command_should_skip_failure_analysis(&[], &block));
    }

    #[test]
    fn keeps_plain_exit_one_failures_analyzable() {
        let mut block = block();
        block.command = "false".to_string();
        block.output.terminal_output_ref = Some(write_output(""));

        assert!(!command_should_skip_failure_analysis(&[], &block));
    }

    fn write_output(content: &str) -> String {
        let path = std::env::temp_dir().join(format!(
            "cosh-shell-interrupt-test-{}-{}.txt",
            std::process::id(),
            content.len()
        ));
        std::fs::write(&path, content).expect("write output");
        path.to_string_lossy().to_string()
    }
}
