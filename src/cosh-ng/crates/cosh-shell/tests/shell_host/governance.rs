use super::*;

#[test]
fn raw_relay_host_intercepts_natural_language_via_bash_hook() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }
    if !bash_supports_command_not_found_handler() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-raw-hook-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("raw-hook-test", &work_dir);
    config
        .env_overrides
        .push(("LANG".to_string(), "C.UTF-8".to_string()));
    config
        .env_overrides
        .push(("LC_ALL".to_string(), "C.UTF-8".to_string()));
    let input = std::io::Cursor::new(
        "please analyze last failure\n\
         \u{8bf7}\u{5e2e}\u{6211}\u{5206}\u{6790}\n\
         missing-cosh-test-command\n\
         exit\n",
    );
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash(&config, input, &mut rendered).expect("raw relay host");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(!rendered_text.contains("intercepted  natural_language"));
    assert!(!rendered
        .windows(b"\x1b]1337;COSH;".len())
        .any(|window| window == b"\x1b]1337;COSH;"));

    let replayed_events = read_shell_events(&output.journal_path).expect("journal events");
    assert!(replayed_events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some("please analyze last failure")
            && event.component.as_deref() == Some("natural_language")
    }));
    assert!(replayed_events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.input.as_deref() == Some("\u{8bf7}\u{5e2e}\u{6211}\u{5206}\u{6790}")
            && event.component.as_deref() == Some("natural_language")
    }));

    let ledger = build_command_blocks(&replayed_events);
    assert!(ledger.errors.is_empty(), "{:?}", ledger.errors);
    assert!(!ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("please analyze last failure")));
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("missing-cosh-test-command") && block.exit_code != 0));
}

#[test]
fn raw_relay_host_can_render_inline_guidance_before_shell_exit() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-raw-inline-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("raw-inline-test", &work_dir);
    let input = std::io::Cursor::new(
        "/explain last error\n\
         ls /path/that/does/not/exist\n\
         echo after-inline\n\
         exit\n",
    );
    let mut rendered = Vec::new();
    let mut handled_blocks = HashSet::new();
    let output =
        run_raw_relay_bash_with_observer(&config, input, &mut rendered, |events, output| {
            let ledger = build_command_blocks(events);
            for block in ledger.blocks.iter().filter(|block| block.exit_code != 0) {
                if handled_blocks.insert(block.id.clone()) {
                    writeln!(output, "\n[inline] failed: {}", block.command)?;
                }
            }
            Ok(())
        })
        .expect("raw relay host");

    let rendered_text = String::from_utf8_lossy(&rendered);
    let inline_pos = rendered_text
        .find("[inline] failed")
        .expect("inline guidance");
    let after_pos = rendered_text
        .rfind("after-inline")
        .expect("continued shell command");
    assert!(inline_pos < after_pos, "{rendered_text}");

    let replayed_events = read_shell_events(&output.journal_path).expect("journal events");
    let ledger = build_command_blocks(&replayed_events);
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("echo after-inline") && block.exit_code == 0));
}

#[test]
fn raw_relay_inline_governance_blocks_agent_execution_events() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-raw-governance-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let side_effect_path = work_dir.join("agent-should-not-create-this");
    let config = ShellHostConfig::new("raw-governance-test", &work_dir);
    let input = std::io::Cursor::new(
        "/explain last error\n\
         ls /path/that/does/not/exist\n\
         echo after-governance\n\
         exit\n",
    );
    let mut rendered = Vec::new();
    let mut handled_blocks = HashSet::new();
    let side_effect_command = format!("touch {}", side_effect_path.display());
    let output =
        run_raw_relay_bash_with_observer(&config, input, &mut rendered, |events, output| {
            let ledger = build_command_blocks(events);
            for block in ledger.blocks.iter().filter(|block| block.exit_code != 0) {
                if !handled_blocks.insert(block.id.clone()) {
                    continue;
                }

                let agent_events = vec![
                    AgentEvent::ToolCall {
                        run_id: "run-1".to_string(),
                        tool_id: None,
                        name: "shell".to_string(),
                        input: side_effect_command.clone(),
                    },
                    AgentEvent::Action {
                        run_id: "run-1".to_string(),
                        command: side_effect_command.clone(),
                    },
                    AgentEvent::Recommendation {
                        run_id: "run-1".to_string(),
                        summary: "Try an unsafe auto fix".to_string(),
                        commands: vec![side_effect_command.clone()],
                        auto_execute: true,
                    },
                ];
                let governed = govern_agent_events(&agent_events, &Policy::default());
                assert!(governed.events.iter().all(|event| !event.auto_execute));
                assert!(governed
                    .events
                    .iter()
                    .any(|event| event.decision == GovernanceDecision::Rejected));
                assert!(governed
                    .events
                    .iter()
                    .any(|event| event.decision == GovernanceDecision::Degraded));

                writeln!(output)?;
                for event in governed.events {
                    writeln!(output, "{}", event.display_text)?;
                }
            }
            Ok(())
        })
        .expect("raw relay host");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(rendered_text.contains("Approval required: Bash command"));
    assert!(rendered_text.contains("Approval required: Shell command"));
    assert!(rendered_text.contains("Blocked: user approval required"));
    assert!(!rendered_text.contains("Decision: blocked by recommend-only governance"));
    assert!(rendered_text.contains("Try an unsafe auto fix"));
    assert!(rendered_text.contains("after-governance"));
    assert!(!side_effect_path.exists());

    let replayed_events = read_shell_events(&output.journal_path).expect("journal events");
    let ledger = build_command_blocks(&replayed_events);
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("echo after-governance") && block.exit_code == 0));
}
