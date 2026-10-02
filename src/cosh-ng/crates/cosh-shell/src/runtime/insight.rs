use std::io::{IsTerminal, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::insight::model::{EntityKey, PromptSuggestion, SuppressionTopic};
use crate::raw_input::PromptGhostRoute;
use crate::runtime::state::{InlineState, PendingInputGhostBinding};
use crate::{I18n, MessageId};

pub(crate) fn render_pending_command_insight<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let Some(mut candidate) = state.pending_command_insight.take() else {
        return Ok(());
    };
    if state.agent_run.active.is_some() || state.pending_input_ghost.is_some() {
        return Ok(());
    }
    let Some(suggestion) = candidate.suggestion.take() else {
        return Ok(());
    };
    let now_ms = match &suggestion {
        PromptSuggestion::AgentPrompt { binding, .. } => binding.target.created_at_ms,
        PromptSuggestion::ShellRewrite { .. } => SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64,
    };
    if state
        .insight_budget
        .should_suppress(candidate.suppression_key, candidate.severity, now_ms)
    {
        return Ok(());
    }

    match suggestion {
        PromptSuggestion::AgentPrompt { binding } => {
            let summary =
                localized_agent_summary(state.i18n(), &candidate.topic, &candidate.entity);
            let show_guidance = !state.shown_agent_prompt_guidance;
            write_insight(
                output,
                state.i18n(),
                &summary,
                show_guidance.then_some(MessageId::InsightAgentPromptFirstUseHint),
            )?;
            state.pending_input_ghost = Some(localized_agent_prompt(
                state.i18n(),
                &candidate.topic,
                &candidate.entity,
            ));
            state.pending_input_ghost_route = PromptGhostRoute::AgentIntercept {
                suggestion_id: Some(binding.suggestion_id.clone()),
            };
            state.pending_input_ghost_binding = Some(PendingInputGhostBinding::Insight(binding));
            state.trigger_pty_prompt = true;
            output.flush()?;
            state.shown_agent_prompt_guidance |= show_guidance;
        }
        PromptSuggestion::ShellRewrite { text } => {
            let summary = localized_summary(state.i18n(), &candidate.topic, &candidate.entity);
            let show_guidance = !state.shown_shell_rewrite_guidance;
            write_insight(
                output,
                state.i18n(),
                &summary,
                show_guidance.then_some(MessageId::InsightShellRewriteFirstUseHint),
            )?;
            state.pending_input_ghost = Some(text);
            state.pending_input_ghost_route = PromptGhostRoute::NativeShell;
            state.pending_input_ghost_binding = None;
            state.trigger_pty_prompt = true;
            output.flush()?;
            state.shown_shell_rewrite_guidance |= show_guidance;
        }
    }
    // #1932 F5: a multi-line paste just line-executed in bash and produced
    // this failure insight; nudge the terminal-agnostic entry, capped at
    // two per session. Best-effort: the flag clears with this insight.
    if matches!(candidate.topic, SuppressionTopic::CommandNotFound)
        && state.prompt_entry_hints.multiline_paste_observed
        && state.prompt_entry_hints.multiline_entry_hint_count < 2
    {
        let hint = state.i18n().t(MessageId::PromptMultilineEntryHint);
        write!(output, "\x1b[2m{hint}\x1b[0m\r\n")?;
        output.flush()?;
        state.prompt_entry_hints.multiline_entry_hint_count += 1;
    }
    state.prompt_entry_hints.multiline_paste_observed = false;
    Ok(())
}

fn localized_agent_summary(i18n: I18n, topic: &SuppressionTopic, entity: &EntityKey) -> String {
    if *topic == SuppressionTopic::CommandNotFound {
        return i18n.t(MessageId::InsightCommandNotFoundSummary).to_string();
    }
    localized_summary(i18n, topic, entity)
}

fn localized_summary(i18n: I18n, topic: &SuppressionTopic, entity: &EntityKey) -> String {
    let id = match topic {
        SuppressionTopic::CommandNotFound => MessageId::InsightCommandTypoSummary,
        SuppressionTopic::PermissionDenied => MessageId::InsightPermissionDeniedSummary,
        SuppressionTopic::BuildOrTestFailure => MessageId::InsightBuildOrTestFailureSummary,
        SuppressionTopic::RuntimeException => MessageId::InsightRuntimeExceptionSummary,
        SuppressionTopic::AbnormalSignal => MessageId::InsightAbnormalSignalSummary,
        SuppressionTopic::MemoryPressure => MessageId::InsightMemoryPressureSummary,
        SuppressionTopic::HighMemoryProcess if matches!(entity, EntityKey::Process(_)) => {
            MessageId::InsightHighMemoryProcessSummary
        }
        SuppressionTopic::HighMemoryProcess => MessageId::InsightHighMemoryProcessGenericSummary,
        SuppressionTopic::MemoryRootCause if matches!(entity, EntityKey::Process(_)) => {
            MessageId::InsightMemoryRootCauseSummary
        }
        SuppressionTopic::MemoryRootCause => MessageId::InsightMemoryRootCauseGenericSummary,
    };
    localize_with_entity(i18n, id, entity)
}

fn localized_agent_prompt(i18n: I18n, topic: &SuppressionTopic, entity: &EntityKey) -> String {
    let id = match topic {
        SuppressionTopic::CommandNotFound => MessageId::InsightCommandNotFoundPrompt,
        SuppressionTopic::PermissionDenied => MessageId::InsightPermissionDeniedPrompt,
        SuppressionTopic::BuildOrTestFailure => MessageId::InsightBuildOrTestFailurePrompt,
        SuppressionTopic::RuntimeException => MessageId::InsightRuntimeExceptionPrompt,
        SuppressionTopic::AbnormalSignal => MessageId::InsightAbnormalSignalPrompt,
        SuppressionTopic::MemoryPressure => MessageId::InsightMemoryPressurePrompt,
        SuppressionTopic::HighMemoryProcess if matches!(entity, EntityKey::Process(_)) => {
            MessageId::InsightHighMemoryProcessPrompt
        }
        SuppressionTopic::HighMemoryProcess => MessageId::InsightHighMemoryProcessGenericPrompt,
        SuppressionTopic::MemoryRootCause if matches!(entity, EntityKey::Process(_)) => {
            MessageId::InsightMemoryRootCausePrompt
        }
        SuppressionTopic::MemoryRootCause => MessageId::InsightMemoryRootCauseGenericPrompt,
    };
    localize_with_entity(i18n, id, entity)
}

fn localize_with_entity(i18n: I18n, id: MessageId, entity: &EntityKey) -> String {
    match entity {
        EntityKey::Process(process) | EntityKey::Program(process) => {
            i18n.format(id, &[("process", process)])
        }
        EntityKey::SystemMemory | EntityKey::Unknown => i18n.t(id).to_string(),
    }
}

fn write_insight<W: Write>(
    output: &mut W,
    i18n: I18n,
    summary: &str,
    hint: Option<MessageId>,
) -> std::io::Result<()> {
    write_insight_with_style(output, i18n, summary, hint, insight_styles_enabled())
}

fn write_insight_with_style<W: Write>(
    output: &mut W,
    i18n: I18n,
    summary: &str,
    hint: Option<MessageId>,
    styled: bool,
) -> std::io::Result<()> {
    if styled {
        write!(
            output,
            "\x1b[36m{}\x1b[0m{}",
            i18n.t(MessageId::InsightLabel),
            summary
        )?;
        if let Some(hint) = hint {
            write!(output, "  \x1b[2m{}\x1b[0m", i18n.t(hint))?;
        }
        writeln!(output)?;
    } else {
        write!(output, "{}{}", i18n.t(MessageId::InsightLabel), summary)?;
        if let Some(hint) = hint {
            write!(output, "  {}", i18n.t(hint))?;
        }
        writeln!(output)?;
    }
    Ok(())
}

fn insight_styles_enabled() -> bool {
    if !std::io::stdout().is_terminal() || std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if std::env::var("TERM")
        .map(|term| term.eq_ignore_ascii_case("dumb"))
        .unwrap_or(false)
    {
        return false;
    }
    if std::env::var("COSH_SHELL_RENDER")
        .map(|mode| mode.eq_ignore_ascii_case("plain") || mode.eq_ignore_ascii_case("text"))
        .unwrap_or(false)
    {
        return false;
    }
    ratatui::crossterm::terminal::size()
        .map(|(columns, _)| columns >= 40)
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::insight::model::{
        CommandIntent, EntityKey, ExecutionScope, InsightBinding, InsightCandidate,
        InsightConfidence, InsightSeverity, InsightSource, InsightTarget, OutputExcerptStatus,
        SuppressionKey, SuppressionTopic,
    };
    use crate::Language;
    use std::io;

    fn candidate(suggestion: PromptSuggestion) -> InsightCandidate {
        let scope = ExecutionScope::local("session-1");
        InsightCandidate {
            source: InsightSource::FailedCommand,
            topic: SuppressionTopic::BuildOrTestFailure,
            entity: EntityKey::Program("cargo".to_string()),
            severity: InsightSeverity::Warning,
            confidence: InsightConfidence::High,
            evidence: Vec::new(),
            suggestion: Some(suggestion),
            scope: scope.clone(),
            suppression_key: SuppressionKey {
                version: 1,
                topic: SuppressionTopic::BuildOrTestFailure,
                entity: EntityKey::Program("cargo".to_string()),
                scope,
                intent: CommandIntent::AnalyzeFailure,
            },
        }
    }

    fn binding() -> InsightBinding {
        InsightBinding {
            suggestion_id: "suggestion-1".to_string(),
            target: InsightTarget {
                insight_id: "insight-1".to_string(),
                source_session_id: "session-1".to_string(),
                source_command_block_id: "cmd-1".to_string(),
                scope: ExecutionScope::local("session-1"),
                evidence_handle: None,
                evidence_status: OutputExcerptStatus::Available,
                severity: InsightSeverity::Warning,
                confidence: InsightConfidence::High,
                evidence: Vec::new(),
                created_at_ms: 1,
            },
        }
    }

    #[test]
    fn shell_rewrite_arms_native_shell_prompt_ghost() {
        let mut state = InlineState {
            pending_command_insight: Some(candidate(PromptSuggestion::ShellRewrite {
                text: "grep file".to_string(),
            })),
            ..Default::default()
        };
        let mut output = Vec::new();

        render_pending_command_insight(&mut state, &mut output).expect("render insight");

        assert_eq!(state.pending_input_ghost.as_deref(), Some("grep file"));
        assert_eq!(
            state.pending_input_ghost_route,
            PromptGhostRoute::NativeShell
        );
        assert!(state.pending_input_ghost_binding.is_none());
        assert!(state.trigger_pty_prompt);
    }

    #[test]
    fn agent_prompt_arms_bound_agent_intercept_route() {
        let binding = binding();
        let mut state = InlineState {
            pending_command_insight: Some(candidate(PromptSuggestion::AgentPrompt {
                binding: Box::new(binding.clone()),
            })),
            ..Default::default()
        };
        let mut output = Vec::new();

        render_pending_command_insight(&mut state, &mut output).expect("render insight");

        assert_eq!(
            state.pending_input_ghost.as_deref(),
            Some("Analyze the build or test failure and identify the first actionable error")
        );
        assert_eq!(
            state.pending_input_ghost_route,
            PromptGhostRoute::AgentIntercept {
                suggestion_id: Some("suggestion-1".to_string())
            }
        );
        assert!(matches!(
            state.pending_input_ghost_binding,
            Some(PendingInputGhostBinding::Insight(ref actual)) if actual.as_ref() == &binding
        ));
        assert!(state.trigger_pty_prompt);
    }

    #[test]
    fn command_not_found_uses_generic_agent_fallback_copy() {
        let entity = EntityKey::Program("terraform".to_string());
        assert_eq!(
            localized_agent_summary(
                I18n::new(Language::EnUs),
                &SuppressionTopic::CommandNotFound,
                &entity,
            ),
            "The previous input did not run successfully"
        );
        assert_eq!(
            localized_agent_prompt(
                I18n::new(Language::ZhCn),
                &SuppressionTopic::CommandNotFound,
                &entity,
            ),
            "分析并处理刚才未成功执行的输入"
        );
    }

    #[test]
    fn visible_ghost_is_not_replaced_by_command_insight() {
        let mut state = InlineState {
            pending_command_insight: Some(candidate(PromptSuggestion::ShellRewrite {
                text: "grep file".to_string(),
            })),
            pending_input_ghost: Some("existing health prompt".to_string()),
            ..Default::default()
        };

        render_pending_command_insight(&mut state, &mut Vec::new()).expect("expire insight");

        assert_eq!(
            state.pending_input_ghost.as_deref(),
            Some("existing health prompt")
        );
        assert!(state.pending_command_insight.is_none());
        assert!(!state.shown_shell_rewrite_guidance);
        assert!(!state.shown_agent_prompt_guidance);
    }

    #[test]
    fn zh_agent_insight_localizes_visible_copy_and_prompt() {
        let mut state = InlineState {
            language: Language::ZhCn,
            pending_command_insight: Some(candidate(PromptSuggestion::AgentPrompt {
                binding: Box::new(binding()),
            })),
            ..Default::default()
        };
        let mut output = Vec::new();

        render_pending_command_insight(&mut state, &mut output).expect("render insight");

        let output = String::from_utf8(output).expect("utf8 output");
        assert!(output.contains("洞察：构建或测试失败"), "{output}");
        assert_eq!(
            state.pending_input_ghost.as_deref(),
            Some("分析这次构建或测试失败，定位首个可行动错误")
        );
    }

    #[test]
    fn shell_rewrite_guidance_is_shown_only_on_first_successful_render() {
        let mut state = InlineState {
            pending_command_insight: Some(candidate(PromptSuggestion::ShellRewrite {
                text: "grep file".to_string(),
            })),
            ..Default::default()
        };
        let mut first = Vec::new();

        render_pending_command_insight(&mut state, &mut first).expect("render first insight");

        state.pending_input_ghost = None;
        state.pending_input_ghost_route = PromptGhostRoute::NativeShell;
        state.insight_budget = Default::default();
        state.pending_command_insight = Some(candidate(PromptSuggestion::ShellRewrite {
            text: "grep other-file".to_string(),
        }));
        let mut second = Vec::new();
        render_pending_command_insight(&mut state, &mut second).expect("render second insight");

        let first = String::from_utf8(first).expect("utf8 output");
        let second = String::from_utf8(second).expect("utf8 output");
        let hint = "Press Tab to fill, then Enter to run; keep typing to ignore";
        assert!(first.contains(hint), "{first}");
        assert!(!second.contains(hint), "{second}");
    }

    #[test]
    fn agent_prompt_has_independent_first_use_guidance() {
        let mut state = InlineState {
            pending_command_insight: Some(candidate(PromptSuggestion::ShellRewrite {
                text: "grep file".to_string(),
            })),
            ..Default::default()
        };
        render_pending_command_insight(&mut state, &mut Vec::new()).expect("render rewrite");

        state.pending_input_ghost = None;
        state.insight_budget = Default::default();
        state.pending_command_insight = Some(candidate(PromptSuggestion::AgentPrompt {
            binding: Box::new(binding()),
        }));
        let mut output = Vec::new();
        render_pending_command_insight(&mut state, &mut output).expect("render agent prompt");

        let output = String::from_utf8(output).expect("utf8 output");
        assert!(
            output.contains("Press Tab to fill, then Enter to submit; keep typing to ignore"),
            "{output}"
        );
    }

    #[test]
    fn all_insight_topics_have_english_and_chinese_presentation() {
        let cases = [
            (
                SuppressionTopic::CommandNotFound,
                EntityKey::Program("grep".to_string()),
            ),
            (
                SuppressionTopic::PermissionDenied,
                EntityKey::Program("cat".to_string()),
            ),
            (
                SuppressionTopic::BuildOrTestFailure,
                EntityKey::Program("cargo".to_string()),
            ),
            (
                SuppressionTopic::RuntimeException,
                EntityKey::Program("python".to_string()),
            ),
            (
                SuppressionTopic::AbnormalSignal,
                EntityKey::Program("demo".to_string()),
            ),
            (SuppressionTopic::MemoryPressure, EntityKey::SystemMemory),
            (
                SuppressionTopic::HighMemoryProcess,
                EntityKey::Process("java".to_string()),
            ),
            (
                SuppressionTopic::MemoryRootCause,
                EntityKey::Process("java".to_string()),
            ),
        ];

        for (topic, entity) in cases {
            let en_summary = localized_summary(I18n::new(Language::EnUs), &topic, &entity);
            let zh_summary = localized_summary(I18n::new(Language::ZhCn), &topic, &entity);
            assert!(!en_summary.is_empty());
            assert!(!zh_summary.is_empty());
            assert_ne!(en_summary, zh_summary);
            if topic != SuppressionTopic::CommandNotFound {
                let en_prompt = localized_agent_prompt(I18n::new(Language::EnUs), &topic, &entity);
                let zh_prompt = localized_agent_prompt(I18n::new(Language::ZhCn), &topic, &entity);
                assert!(!en_prompt.is_empty());
                assert!(!zh_prompt.is_empty());
                assert_ne!(en_prompt, zh_prompt);
            }
        }
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("write failed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn failed_render_does_not_consume_first_use_guidance() {
        let mut state = InlineState {
            pending_command_insight: Some(candidate(PromptSuggestion::ShellRewrite {
                text: "grep file".to_string(),
            })),
            ..Default::default()
        };

        assert!(render_pending_command_insight(&mut state, &mut FailingWriter).is_err());

        assert!(!state.shown_shell_rewrite_guidance);
        assert!(!state.shown_agent_prompt_guidance);
    }

    #[test]
    fn unknown_process_uses_complete_generic_copy() {
        for topic in [
            SuppressionTopic::HighMemoryProcess,
            SuppressionTopic::MemoryRootCause,
        ] {
            for language in [Language::EnUs, Language::ZhCn] {
                let i18n = I18n::new(language);
                let summary = localized_summary(i18n, &topic, &EntityKey::Unknown);
                let prompt = localized_agent_prompt(i18n, &topic, &EntityKey::Unknown);
                assert!(!summary.contains("{process}"), "{summary}");
                assert!(!prompt.contains("{process}"), "{prompt}");
            }
        }
    }

    #[test]
    fn styled_and_plain_insight_have_identical_single_line_text() {
        let i18n = I18n::new(Language::EnUs);
        let mut plain = Vec::new();
        let mut styled = Vec::new();

        write_insight_with_style(
            &mut plain,
            i18n,
            "The build or test command failed",
            Some(MessageId::InsightAgentPromptFirstUseHint),
            false,
        )
        .expect("plain insight");
        write_insight_with_style(
            &mut styled,
            i18n,
            "The build or test command failed",
            Some(MessageId::InsightAgentPromptFirstUseHint),
            true,
        )
        .expect("styled insight");

        let plain = String::from_utf8(plain).expect("plain utf8");
        let styled = String::from_utf8(styled).expect("styled utf8");
        let stripped = styled
            .replace("\x1b[36m", "")
            .replace("\x1b[2m", "")
            .replace("\x1b[0m", "");
        assert_eq!(stripped, plain);
        assert_eq!(plain.lines().count(), 1, "{plain}");
        assert!(
            styled.starts_with("\x1b[36mInsight: \x1b[0mThe"),
            "{styled}"
        );
        assert!(styled.contains("\x1b[2mPress Tab to fill"), "{styled}");
    }
}
