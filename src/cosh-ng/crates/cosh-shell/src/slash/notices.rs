use crate::runtime::prelude::*;
use crate::slash::panel::render_notice_panel;
use crate::slash::parser::{
    slash_command_hints, RemovedCommand, SlashInfoCommand, SlashParseError,
};

pub(super) fn render_slash_parse_error<W: Write>(
    error: SlashParseError,
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let i18n = state.i18n();
    match error {
        SlashParseError::QuotedArgumentsUnsupported => render_notice_panel(
            output,
            i18n.t(MessageId::SlashInvalidArgumentsTitle),
            vec![i18n
                .t(MessageId::SlashQuotedArgumentsUnsupported)
                .to_string()],
            None,
        ),
    }
}

pub(super) fn render_removed_command<W: Write>(
    command: RemovedCommand<'_>,
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    match command {
        RemovedCommand::ApprovalMode(value) => {
            let replacement = value
                .filter(|value| matches!(*value, "recommend" | "auto" | "trust"))
                .map(|value| {
                    state
                        .i18n()
                        .format(MessageId::ModeRemovedFooter, &[("mode", value)])
                })
                .unwrap_or_else(|| {
                    state
                        .i18n()
                        .t(MessageId::ApprovalModeRemovedFooter)
                        .to_string()
                });
            render_notice_panel(
                output,
                state.i18n().t(MessageId::CommandRemovedTitle),
                vec![state
                    .i18n()
                    .t(MessageId::ApprovalModeRemovedBody)
                    .to_string()],
                Some(&replacement),
            )
        }
        RemovedCommand::ApprovalDecision(command) => render_notice_panel(
            output,
            state.i18n().t(MessageId::CommandRemovedTitle),
            vec![state.i18n().format(
                MessageId::RemovedDecisionCommandBody,
                &[("command", command)],
            )],
            Some(state.i18n().t(MessageId::RemovedApprovalDecisionFooter)),
        ),
        RemovedCommand::QuestionAnswer => render_notice_panel(
            output,
            state.i18n().t(MessageId::CommandRemovedTitle),
            vec![state.i18n().format(
                MessageId::RemovedDecisionCommandBody,
                &[("command", "/answer")],
            )],
            Some(state.i18n().t(MessageId::RemovedQuestionAnswerFooter)),
        ),
    }
}

pub(super) fn render_help<W: Write>(state: &InlineState, output: &mut W) -> std::io::Result<()> {
    let i18n = state.i18n();
    let mut groups = Vec::new();
    for (group, label_id) in [
        ("Prompt", MessageId::HelpGroupPrompt),
        ("Status", MessageId::HelpGroupStatus),
        ("Config", MessageId::HelpGroupConfig),
        ("Health", MessageId::HelpGroupHealth),
        ("Sessions", MessageId::HelpGroupSessions),
        ("Modes", MessageId::HelpGroupModes),
        ("Hooks", MessageId::HelpGroupHooks),
        ("Registry", MessageId::HelpGroupRegistry),
    ] {
        let entries = visible_slash_commands()
            .filter(|hint| hint.group == Some(group))
            .map(|hint| HelpPanelEntry {
                usage: hint.usage,
                summary: i18n.t(hint.summary_id),
                scope: hint.scope,
            })
            .collect::<Vec<_>>();
        if entries.is_empty() {
            continue;
        }
        groups.push(HelpPanelGroup {
            label: i18n.t(label_id),
            entries,
        });
    }

    RatatuiInlineRenderer::for_terminal().write_help_panel(
        output,
        HelpPanelModel {
            title: i18n.t(MessageId::HelpTitle),
            groups,
            footer: i18n.format(
                MessageId::HelpFooter,
                &[
                    ("mode", state.approval_mode.label()),
                    ("strategy", state.analysis_mode.label()),
                ],
            ),
        },
    )
}

pub(super) fn render_hint<W: Write>(
    prefix: &str,
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let mut body = vec![
        state
            .i18n()
            .format(MessageId::SlashHintPrefix, &[("prefix", prefix)]),
        state.i18n().format(
            MessageId::SlashHintCurrentMode,
            &[("mode", state.approval_mode.label())],
        ),
    ];
    body.extend(
        slash_command_hints(prefix)
            .into_iter()
            .map(|hint| format!("{} - {}", hint.usage, state.i18n().t(hint.summary_id))),
    );

    render_notice_panel(
        output,
        state.i18n().t(MessageId::SlashHintTitle),
        body,
        Some(state.i18n().t(MessageId::SlashHintFooter)),
    )
}

pub(super) fn render_info<W: Write>(
    command: SlashInfoCommand,
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let i18n = state.i18n();
    let (title, body, footer) = match command {
        SlashInfoCommand::Audit => (
            i18n.t(MessageId::SlashInfoAuditTitle).to_string(),
            vec![
                i18n.t(MessageId::SlashInfoAuditApprovalsBody).to_string(),
                i18n.t(MessageId::SlashInfoAuditActivityBody).to_string(),
            ],
            i18n.t(MessageId::SlashInfoAuditFooter).to_string(),
        ),
        SlashInfoCommand::Config => (
            i18n.t(MessageId::SlashInfoConfigTitle).to_string(),
            render_config_body(state),
            i18n.t(MessageId::SlashInfoConfigFooter).to_string(),
        ),
    };

    render_notice_panel(output, &title, body, Some(&footer))
}

fn render_config_body(state: &InlineState) -> Vec<String> {
    let i18n = state.i18n();
    let language = language_config_status();
    let effective = language.effective.as_config_value();
    let mut body = Vec::new();
    if language.setting == effective {
        body.push(i18n.format(
            MessageId::SlashInfoConfigLanguageLine,
            &[("effective", effective), ("source", language.source)],
        ));
    } else {
        body.push(i18n.format(
            MessageId::SlashInfoConfigLanguageEffectiveLine,
            &[
                ("effective", effective),
                ("setting", &language.setting),
                ("source", language.source),
            ],
        ));
    }
    if let Some(path) = language.config_path {
        body.push(i18n.format(
            MessageId::SlashInfoConfigPathLine,
            &[("path", &path.display().to_string())],
        ));
    }
    body.push(i18n.format(
        MessageId::SlashInfoConfigDebugActivityLine,
        &[("state", if state.debug { "on" } else { "off" })],
    ));
    body.push(
        i18n.t(MessageId::SlashInfoConfigAnalysisStrategyLine)
            .to_string(),
    );
    body.push(
        i18n.t(MessageId::SlashInfoConfigRenderFallbackLine)
            .to_string(),
    );
    body
}

pub(super) fn render_unknown<W: Write>(
    command: &str,
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let i18n = state.i18n();
    let mut body = vec![i18n.format(MessageId::SlashUnknownBody, &[("command", command)])];
    if let Some(suggestion) = nearest_canonical_slash_command(command) {
        body.push(i18n.format(
            MessageId::SlashUnknownSuggestionBody,
            &[("command", suggestion)],
        ));
    }
    render_notice_panel(
        output,
        i18n.t(MessageId::SlashUnknownTitle),
        body,
        Some(i18n.t(MessageId::SlashUnknownFooter)),
    )
}

fn nearest_canonical_slash_command(command: &str) -> Option<&'static str> {
    active_slash_commands()
        .filter(|candidate| edit_distance_at_most(command, candidate, 2))
        .min_by_key(|candidate| edit_distance(command, candidate))
}

fn edit_distance_at_most(left: &str, right: &str, max: usize) -> bool {
    left.len().abs_diff(right.len()) <= max && edit_distance(left, right) <= max
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right_chars = right.chars().collect::<Vec<_>>();
    let mut prev = (0..=right_chars.len()).collect::<Vec<_>>();
    let mut curr = vec![0; right_chars.len() + 1];

    for (left_idx, left_ch) in left.chars().enumerate() {
        curr[0] = left_idx + 1;
        for (right_idx, right_ch) in right_chars.iter().enumerate() {
            let cost = usize::from(left_ch != *right_ch);
            curr[right_idx + 1] = (prev[right_idx + 1] + 1)
                .min(curr[right_idx] + 1)
                .min(prev[right_idx] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[right_chars.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zh_state() -> InlineState {
        InlineState {
            language: Language::ZhCn,
            ..InlineState::default()
        }
    }

    #[test]
    fn slash_info_config_uses_zh_catalog_text() {
        let state = zh_state();
        let mut output = Vec::new();

        render_info(SlashInfoCommand::Config, &state, &mut output).expect("render config info");

        let output = String::from_utf8(output).expect("utf8 output");
        assert!(output.contains("配置"), "{output}");
        assert!(output.contains("语言:"), "{output}");
        assert!(output.contains("调试活动:"), "{output}");
        assert!(
            output.contains("分析策略: /mode analysis smart|auto|manual"),
            "{output}"
        );
        assert!(output.contains("渲染降级:"), "{output}");
        assert!(
            output.contains("使用 /config language [auto|en-US|zh-CN]。"),
            "{output}"
        );
        assert!(!output.contains("debug activity:"), "{output}");
        assert!(!output.contains("render fallback:"), "{output}");
    }

    #[test]
    fn slash_info_audit_uses_zh_catalog_text() {
        let state = zh_state();
        let mut output = Vec::new();

        render_info(SlashInfoCommand::Audit, &state, &mut output).expect("render audit info");

        let output = String::from_utf8(output).expect("utf8 output");
        assert!(output.contains("审计"), "{output}");
        assert!(
            output.contains("审批决策可通过 Details 操作查看。"),
            "{output}"
        );
        assert!(
            output.contains("审计视图是只读的；不会运行 shell 命令。"),
            "{output}"
        );
        assert!(!output.contains("Audit views are read-only"), "{output}");
    }
}
