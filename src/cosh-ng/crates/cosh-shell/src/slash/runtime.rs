use crate::runtime::mode::render_mode_card_actions;
use crate::runtime::prelude::*;
use crate::slash::capture_notice::render_capture_input_rejected;
use crate::slash::commands::render_slash_command;
use crate::slash::config::render_config_card_actions;
use crate::slash::notices::render_slash_parse_error;
use crate::slash::parser::{slash_input, SlashCommand};
use crate::slash::prompt::{clear_shell_prompt_line, write_shell_prompt};
use crate::slash::session::{render_session_card_actions, render_session_launch};

pub(crate) fn render_slash_actions<W: Write>(
    events: &[ShellEvent],
    blocks: &[CommandBlock],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    render_session_launch(events, blocks, adapter, state, output)?;
    render_session_card_actions(events, adapter, state, output, event_index_base)?;
    render_mode_card_actions(events, state, output, event_index_base)?;
    render_config_card_actions(events, state, output, event_index_base)?;
    render_capture_input_rejected(events, state, output)?;

    for (idx, event) in events.iter().enumerate() {
        let event_index = event_index_base + idx;
        let Some(input) = slash_input(event) else {
            continue;
        };
        let parsed = match SlashCommand::parse(input) {
            Ok(Some(command)) => Ok(command),
            Ok(None) => continue,
            Err(error) => Err(error),
        };

        let key = stable_event_key("slash", event_index, event);
        if !state.handled_slash_commands.insert(key) {
            continue;
        }

        clear_shell_prompt_line(output)?;
        let restore_prompt = match parsed {
            Ok(command) => render_slash_command(
                command,
                event,
                blocks,
                adapter,
                state,
                event.cwd.as_deref(),
                output,
            )?,
            Err(error) => {
                render_slash_parse_error(error, state, output)?;
                true
            }
        };
        if restore_prompt {
            write_shell_prompt(state, output)?;
        }
        output.flush()?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::hooks::state::{
        HookFeedback, RuntimeHookDisplay, RuntimeHookDisplayAction, RuntimeHookDisplayEvent,
        RuntimeHookFinding,
    };
    use crate::runtime::evidence_state::RuntimeShellCommandCompleted;
    use crate::runtime::state::ContinuityFactKind;
    use crate::slash::debug::render_debug_command;
    use crate::slash::hooks::render_hooks_command;
    use crate::slash::hooks_tests::TestHookFixture;
    use crate::types::{FindingSeverity, HookFinding};
    use std::sync::{Arc, Mutex};

    use super::*;

    struct EnvLock {
        path: std::path::PathBuf,
    }

    impl Drop for EnvLock {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn env_lock() -> EnvLock {
        let path =
            std::env::temp_dir().join(format!("cosh-shell-test-env-{}.lock", std::process::id()));
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return EnvLock { path },
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(err) => panic!("create env test lock failed: {err}"),
            }
        }
    }

    fn hook_finding() -> HookFinding {
        HookFinding {
            hook_id: "memory-pressure".to_string(),
            severity: FindingSeverity::Critical,
            title: "Available memory is low".to_string(),
            description: "description".to_string(),
            suggestion: "suggestion".to_string(),
            skill: Some("memory-analysis".to_string()),
            cli_hint: Some("free -m".to_string()),
            context_refs: Vec::new(),
        }
    }

    fn hook_hint(display: RuntimeHookDisplay, reason: &str) -> RuntimeHookFinding {
        RuntimeHookFinding {
            id: "hook-cmd-1-memory-pressure".to_string(),
            command_block_id: "cmd-1".to_string(),
            command: "free -m".to_string(),
            output_ref: Some("/tmp/out".to_string()),
            ended_at_ms: 200,
            prompt_hint: "hook_finding=memory-pressure".to_string(),
            finding_markdown: None,
            hook_finding: Some(hook_finding()),
            recommended_skill: Some("memory-analysis".to_string()),
            display,
            display_reason: reason.to_string(),
            related_hook_ids: Vec::new(),
            topic: "memory".to_string(),
            entity_key: "system-memory".to_string(),
            effective_severity: FindingSeverity::Critical,
            confidence: "high".to_string(),
            suppression_key: "memory:memory-pressure:free".to_string(),
        }
    }

    fn render_hooks_test_command<W: Write>(
        sub: Option<&str>,
        arg: Option<&str>,
        extra: Option<&str>,
        blocks: &[CommandBlock],
        state: &mut InlineState,
        output: &mut W,
    ) -> std::io::Result<()> {
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        render_hooks_command(sub, arg, extra, blocks, &adapter, state, output)
    }

    #[test]
    fn debug_session_renders_provider_session_and_local_facts() {
        let adapter = AdapterInstance::QwenCli(QwenCliAdapter {
            program: "co".to_string(),
            allow_model_call: false,
            session_id: Arc::new(Mutex::new(Some("sess-123".to_string()))),
        });
        let mut state = InlineState::default();
        state
            .continuity
            .facts
            .push(ContinuityFactKind::UserIntent, "帮我更新 git");
        let mut output = Vec::new();

        render_debug_command(Some("session"), &adapter, &state, &mut output)
            .expect("render debug session");

        let rendered = String::from_utf8(output).expect("utf8 output");
        assert!(rendered.contains("provider invocation: co"));
        assert!(rendered.contains("provider committed session: sess-123"));
        assert!(rendered.contains("provider pending session: <none>"));
        assert!(rendered.contains("provider initialize seen: <none>"));
        assert!(rendered.contains("host-executed shell result: <none>"));
        assert!(rendered.contains("selected shell execution path: <none>"));
        assert!(rendered.contains("latest provider request: <none>"));
        assert!(rendered.contains("latest tool use id: <none>"));
        assert!(rendered.contains("latest recovery status: <none>"));
        assert!(rendered.contains("latest recovery reason: <none>"));
        assert!(rendered.contains("fact 1 [user]"));
    }

    #[test]
    fn debug_session_renders_latest_recovery_reason() {
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let mut state = InlineState::default();
        let evidence = RuntimeShellCommandCompleted {
            approval_id: Some("req-1".to_string()),
            origin: AgentRunOrigin::Standard,
            provider_request_id: Some("ctrl-1".to_string()),
            tool_use_id: Some("toolu-1".to_string()),
            shell_session_id: "raw-test".to_string(),
            command_block_id: "cmd-1".to_string(),
            command: "df -h".to_string(),
            provider_command: "df -h".to_string(),
            cwd: "/tmp".to_string(),
            end_cwd: "/tmp".to_string(),
            status: "completed",
            exit_code: 0,
            duration_ms: 10,
            terminal_output_ref: None,
            redaction_status: "ref_only",
            provider_result_delivered: false,
            provider_result_delivery_status: "provider_run_not_active",
            recovery_reason: Some(
                "provider run was not active when shell completed; shell evidence continuation required",
            ),
            continuation_state:
                crate::runtime::evidence_state::ShellEvidenceContinuationState::PendingRecovery,
        };
        state.evidence.record_shell_command_completed(evidence);
        let mut output = Vec::new();

        render_debug_command(Some("session"), &adapter, &state, &mut output)
            .expect("render debug session");

        let rendered = String::from_utf8(output).expect("utf8 output");
        assert!(rendered.contains("latest provider request: ctrl-1"));
        assert!(rendered.contains("latest tool use id: toolu-1"));
        assert!(rendered.contains("latest recovery status: provider_run_not_active"));
        assert!(
            rendered.contains(
                "latest recovery reason: provider run was not active when shell completed"
            ),
            "{rendered}"
        );
    }

    #[test]
    fn hooks_history_renders_recent_finding_display_decision() {
        let mut state = InlineState::default();
        state
            .hooks
            .findings
            .push(hook_hint(RuntimeHookDisplay::Hint, "interruption-budget"));
        let mut output = Vec::new();

        render_hooks_test_command(Some("history"), None, None, &[], &mut state, &mut output)
            .expect("render hook history");

        let rendered = String::from_utf8(output.clone()).expect("utf8");
        assert!(rendered.contains("Hook history"), "{rendered}");
        assert!(
            rendered.contains("id=hook-cmd-1-memory-pressure"),
            "{rendered}"
        );
        assert!(
            rendered.contains("memory-pressure [critical]"),
            "{rendered}"
        );
        assert!(rendered.contains("display=hint"), "{rendered}");
        assert!(
            rendered.contains("reason=interruption-budget"),
            "{rendered}"
        );
        assert!(rendered.contains("entity=system-memory"), "{rendered}");
        assert!(rendered.contains("command=free -m"), "{rendered}");
    }

    #[test]
    fn hooks_history_empty_session_is_explicit() {
        let mut state = InlineState::default();
        let mut output = Vec::new();

        render_hooks_test_command(Some("history"), None, None, &[], &mut state, &mut output)
            .expect("render empty hook history");

        let rendered = String::from_utf8(output.clone()).expect("utf8");
        assert!(
            rendered.contains("No hook findings recorded in this session."),
            "{rendered}"
        );
    }

    #[test]
    fn hooks_events_renders_recent_display_events() {
        let mut state = InlineState::default();
        state.hooks.display_events.push(RuntimeHookDisplayEvent {
            action: RuntimeHookDisplayAction::Shown,
            finding_id: "hook-cmd-1-memory-pressure".to_string(),
            command_block_id: "cmd-1".to_string(),
            hook_id: "memory-pressure".to_string(),
            topic: "memory".to_string(),
            entity_key: "system-memory".to_string(),
            suppression_key: "memory:memory-pressure:free".to_string(),
            display: RuntimeHookDisplay::Consultation,
            display_reason: "allowed".to_string(),
            confidence: "high".to_string(),
            ended_at_ms: 200,
        });
        let mut output = Vec::new();

        render_hooks_test_command(Some("events"), None, None, &[], &mut state, &mut output)
            .expect("render hook events");

        let rendered = String::from_utf8(output).expect("utf8");
        assert!(rendered.contains("Hook display events"), "{rendered}");
        assert!(rendered.contains("action=shown"), "{rendered}");
        assert!(rendered.contains("hook=memory-pressure"), "{rendered}");
        assert!(rendered.contains("display=consultation"), "{rendered}");
        assert!(
            rendered.contains("suppression_key=memory:memory-pressure:free"),
            "{rendered}"
        );
    }

    #[test]
    fn hooks_mute_and_unmute_update_session_targets() {
        let mut state = InlineState::default();
        let mut output = Vec::new();

        render_hooks_test_command(
            Some("mute"),
            Some("memory"),
            None,
            &[],
            &mut state,
            &mut output,
        )
        .expect("mute hook target");
        assert!(state.hooks.muted_targets.contains("memory"));

        render_hooks_test_command(None, None, None, &[], &mut state, &mut output)
            .expect("show hook status");
        let rendered = String::from_utf8(output.clone()).expect("utf8");
        assert!(rendered.contains("Muted targets: memory."), "{rendered}");

        render_hooks_test_command(
            Some("unmute"),
            Some("memory"),
            None,
            &[],
            &mut state,
            &mut output,
        )
        .expect("unmute hook target");
        assert!(!state.hooks.muted_targets.contains("memory"));
        let rendered = String::from_utf8(output).expect("utf8");
        assert!(
            rendered.contains("Unmuted hook target 'memory'."),
            "{rendered}"
        );
    }

    #[test]
    fn hooks_trust_project_and_untrust_project_update_session_state() {
        let _env_lock = env_lock();
        let project_hook = TestHookFixture::new("hooks-trust-project");
        let store = std::env::temp_dir().join(format!(
            "cosh-shell-slash-trust-store-{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&store);
        std::env::set_var("COSH_SHELL_PROJECT_TRUST_STORE", &store);
        let mut state = InlineState::default();
        state.hooks.engine.register_external(ExternalHookConfig {
            path: project_hook.path.clone(),
            matcher: HookMatcher {
                id: "project-hook".to_string(),
                commands: vec!["echo".to_string()],
                command_patterns: Vec::new(),
                command_regex: None,
                min_output_bytes: None,
                exit_codes: None,
                trigger: HookTrigger::OnComplete,
            },
            timeout_ms: 1000,
            source: ExternalHookSource::Project,
            project_root: Some(project_hook.root.clone()),
            trusted: false,
        });
        let mut output = Vec::new();

        render_hooks_test_command(
            Some("trust-project"),
            None,
            None,
            &[],
            &mut state,
            &mut output,
        )
        .expect("trust project hooks");
        assert!(state.hooks.engine.external_hooks()[0].trusted);
        let persisted = std::fs::read_to_string(&store).expect("read trust store");
        assert!(
            persisted.contains(project_hook.root.to_string_lossy().as_ref()),
            "{persisted}"
        );

        render_hooks_test_command(None, None, None, &[], &mut state, &mut output)
            .expect("show hook status");
        let rendered = String::from_utf8(output.clone()).expect("utf8");
        assert!(
            rendered.contains("Project trust: trusted=1; untrusted=0."),
            "{rendered}"
        );

        render_hooks_test_command(
            Some("untrust-project"),
            None,
            None,
            &[],
            &mut state,
            &mut output,
        )
        .expect("untrust project hooks");
        assert!(!state.hooks.engine.external_hooks()[0].trusted);
        let persisted = std::fs::read_to_string(&store).expect("read trust store");
        assert!(
            !persisted.contains(project_hook.root.to_string_lossy().as_ref()),
            "{persisted}"
        );
        let rendered = String::from_utf8(output.clone()).expect("utf8");
        assert!(
            rendered.contains("1 project hook(s) marked untrusted."),
            "{rendered}"
        );

        render_hooks_test_command(
            Some("trust-project"),
            None,
            None,
            &[],
            &mut state,
            &mut output,
        )
        .expect("trust project hooks again");
        assert!(state.hooks.engine.external_hooks()[0].trusted);
        let persisted = std::fs::read_to_string(&store).expect("read trust store");
        assert!(
            persisted.contains(project_hook.root.to_string_lossy().as_ref()),
            "{persisted}"
        );

        render_hooks_test_command(
            Some("clear-project-trust"),
            None,
            None,
            &[],
            &mut state,
            &mut output,
        )
        .expect("clear project trust store");
        assert!(!state.hooks.engine.external_hooks()[0].trusted);
        let persisted = std::fs::read_to_string(&store).expect("read trust store");
        assert!(
            !persisted.contains(project_hook.root.to_string_lossy().as_ref()),
            "{persisted}"
        );
        assert!(
            persisted.contains("cosh-shell trusted project hook roots"),
            "{persisted}"
        );
        let rendered = String::from_utf8(output).expect("utf8");
        assert!(
            rendered.contains("Project hook trust cleared"),
            "{rendered}"
        );
        std::env::remove_var("COSH_SHELL_PROJECT_TRUST_STORE");
        let _ = std::fs::remove_file(&store);
    }

    #[test]
    fn hooks_feedback_noisy_records_policy_feedback() {
        let _env_lock = env_lock();
        let store = std::env::temp_dir().join(format!(
            "cosh-shell-slash-feedback-store-{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&store);
        std::env::set_var("COSH_SHELL_HOOK_FEEDBACK_STORE", &store);
        let mut state = InlineState::default();
        state
            .hooks
            .findings
            .push(hook_hint(RuntimeHookDisplay::Hint, "allowed"));
        let mut output = Vec::new();

        render_hooks_test_command(
            Some("feedback"),
            Some("noisy"),
            Some("hook-cmd-1-memory-pressure"),
            &[],
            &mut state,
            &mut output,
        )
        .expect("record noisy feedback");

        assert_eq!(
            state
                .hooks
                .feedback
                .get("memory:memory-pressure:free")
                .copied(),
            Some(HookFeedback::Noisy)
        );
        let persisted = std::fs::read_to_string(&store).expect("read feedback store");
        assert!(
            persisted.contains("noisy\tmemory:memory-pressure:free"),
            "{persisted}"
        );
        assert!(persisted.contains("topic=memory"), "{persisted}");
        assert!(persisted.contains("entity=system-memory"), "{persisted}");
        assert!(persisted.contains("severity=critical"), "{persisted}");
        assert!(persisted.contains("intent=free"), "{persisted}");
        assert!(persisted.contains("action=noisy"), "{persisted}");
        assert!(persisted.contains("recorded_at_ms="), "{persisted}");
        assert!(persisted.contains("window_ms=600000"), "{persisted}");
        let rendered = String::from_utf8(output).expect("utf8");
        assert!(rendered.contains("Feedback 'noisy' recorded"), "{rendered}");
        assert!(rendered.contains("Feedback persisted"), "{rendered}");
        std::env::remove_var("COSH_SHELL_HOOK_FEEDBACK_STORE");
        let _ = std::fs::remove_file(&store);
    }

    #[test]
    fn hooks_feedback_useful_clears_ignored_same_finding() {
        let _env_lock = env_lock();
        let store = std::env::temp_dir().join(format!(
            "cosh-shell-slash-feedback-useful-{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&store);
        std::fs::write(&store, "noisy\tmemory:memory-pressure:free\n")
            .expect("seed feedback store");
        std::env::set_var("COSH_SHELL_HOOK_FEEDBACK_STORE", &store);
        let mut state = InlineState::default();
        state
            .hooks
            .findings
            .push(hook_hint(RuntimeHookDisplay::Hint, "allowed"));
        state
            .hooks
            .ignored_cards
            .insert("memory:memory-pressure:free".to_string());
        let mut output = Vec::new();

        render_hooks_test_command(
            Some("feedback"),
            Some("useful"),
            Some("hook-cmd-1-memory-pressure"),
            &[],
            &mut state,
            &mut output,
        )
        .expect("record useful feedback");

        assert_eq!(
            state
                .hooks
                .feedback
                .get("memory:memory-pressure:free")
                .copied(),
            Some(HookFeedback::Useful)
        );
        assert!(!state
            .hooks
            .ignored_cards
            .contains("memory:memory-pressure:free"));
        let persisted = std::fs::read_to_string(&store).expect("read feedback store");
        assert!(
            persisted.contains("useful\tmemory:memory-pressure:free"),
            "{persisted}"
        );
        assert!(persisted.contains("action=useful"), "{persisted}");
        assert!(
            !persisted.contains("noisy\tmemory:memory-pressure:free"),
            "{persisted}"
        );
        std::env::remove_var("COSH_SHELL_HOOK_FEEDBACK_STORE");
        let _ = std::fs::remove_file(&store);
    }

    #[test]
    fn hooks_clear_feedback_clears_session_and_store() {
        let _env_lock = env_lock();
        let store = std::env::temp_dir().join(format!(
            "cosh-shell-slash-feedback-clear-{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&store);
        std::fs::write(&store, "noisy\tmemory:memory-pressure:free\n")
            .expect("seed feedback store");
        std::env::set_var("COSH_SHELL_HOOK_FEEDBACK_STORE", &store);
        let mut state = InlineState::default();
        state.hooks.feedback.insert(
            "memory:memory-pressure:free".to_string(),
            HookFeedback::Noisy,
        );
        state
            .hooks
            .ignored_cards
            .insert("memory:memory-pressure:free".to_string());
        let mut output = Vec::new();

        render_hooks_test_command(
            Some("clear-feedback"),
            None,
            None,
            &[],
            &mut state,
            &mut output,
        )
        .expect("clear feedback");

        assert!(state.hooks.feedback.is_empty());
        assert!(state.hooks.ignored_cards.is_empty());
        let persisted = std::fs::read_to_string(&store).expect("read feedback store");
        assert!(
            !persisted.contains("memory:memory-pressure:free"),
            "{persisted}"
        );
        let rendered = String::from_utf8(output).expect("utf8");
        assert!(rendered.contains("Hook feedback cleared"), "{rendered}");
        std::env::remove_var("COSH_SHELL_HOOK_FEEDBACK_STORE");
        let _ = std::fs::remove_file(&store);
    }
}
