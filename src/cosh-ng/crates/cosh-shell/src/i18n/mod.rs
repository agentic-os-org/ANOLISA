mod en;
mod message_id;
mod zh;

use crate::config::Language;

pub use message_id::MessageId;

#[derive(Debug, Clone, Copy)]
pub struct I18n {
    language: Language,
}

impl I18n {
    pub fn new(language: Language) -> Self {
        Self { language }
    }

    pub fn t(&self, id: MessageId) -> &'static str {
        message(self.language, id)
    }

    pub fn format(&self, id: MessageId, args: &[(&str, &str)]) -> String {
        let mut text = self.t(id).to_string();
        for (key, value) in args {
            text = text.replace(&format!("{{{key}}}"), value);
        }
        text
    }

    pub fn language(&self) -> Language {
        self.language
    }
}

fn message(language: Language, id: MessageId) -> &'static str {
    match language {
        Language::EnUs => en::message(id),
        Language::ZhCn => zh::message(id),
    }
}

#[cfg(test)]
mod tests {
    use super::{I18n, MessageId};
    use crate::config::Language;
    use std::fs;
    use std::path::Path;

    const EXPECTED_CATALOG_DOMAINS: &[&str] = &[
        "activity",
        "agent",
        "approval",
        "auth",
        "config",
        "debug",
        "health",
        "help",
        "hook_action",
        "hook_details",
        "hooks",
        "insight",
        "modes",
        "question",
        "recommendation",
        "session",
        "startup",
    ];

    fn catalog_modules(directory: &str) -> Vec<String> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/i18n")
            .join(directory);
        let mut modules = fs::read_dir(path)
            .expect("read i18n catalog directory")
            .map(|entry| entry.expect("read i18n catalog entry").path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
            .map(|path| {
                path.file_stem()
                    .expect("i18n catalog module stem")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        modules.sort();
        modules
    }

    #[test]
    fn language_catalog_modules_match_message_id_domains() {
        let domains = EXPECTED_CATALOG_DOMAINS
            .iter()
            .map(|domain| (*domain).to_owned())
            .collect::<Vec<_>>();

        assert_eq!(catalog_modules("message_id"), domains);
        assert_eq!(catalog_modules("en"), domains);
        assert_eq!(catalog_modules("zh"), domains);
    }

    #[test]
    fn all_messages_have_en_and_zh_values() {
        for id in MessageId::ALL {
            assert!(!I18n::new(Language::EnUs).t(*id).trim().is_empty());
            assert!(!I18n::new(Language::ZhCn).t(*id).trim().is_empty());
        }
    }

    #[test]
    fn message_id_keeps_fieldless_enum_compatibility() {
        for (ordinal, id) in MessageId::ALL.iter().copied().enumerate() {
            assert_eq!(id as usize, ordinal);
        }
        assert_eq!(MessageId::AgentControlQueueFullBody as usize, 791);
        assert_eq!(MessageId::SlashInvalidArgumentsTitle as usize, 792);
        assert_eq!(MessageId::SlashQuotedArgumentsUnsupported as usize, 793);
        assert_eq!(
            MessageId::AgentQuestionUnavailableTitle as usize,
            MessageId::SlashQuotedArgumentsUnsupported as usize + 1
        );
        assert_eq!(
            MessageId::ApprovalTitle as usize,
            MessageId::QuestionNoPendingBody as usize + 1
        );
        // The tool-argument status pair is a registered stable runtime
        // interface: pin the discriminants with fixed values so a segment
        // inserted ahead of them can never shift the tail unnoticed
        // (new segments must append after mcp_registry_ids).
        assert_eq!(MessageId::AgentStatusToolArguments as usize, 870);
        assert_eq!(MessageId::AgentStatusGeneratingToolArguments as usize, 871);
        assert_eq!(MessageId::HelpGroupPrompt as usize, 872);
        // The #1747 trailing segment must stay appended after every earlier
        // segment so pre-existing discriminants never shift.
        assert_eq!(MessageId::HelpSummaryMcp as usize, 875);
        assert_eq!(MessageId::SlashMcpTitle as usize, 876);
        // The #1988 segment remains pinned ahead of the appended #2029
        // turn-extension messages.
        assert_eq!(
            MessageId::ApprovalReceiptForegroundInteractiveHint as usize,
            877
        );
        // The #2029 turn-extension segment keeps its pinned discriminants.
        assert_eq!(MessageId::ApprovalTurnExtensionSubject as usize, 878);
        assert_eq!(
            MessageId::ApprovalTurnExtensionUnavailableBody as usize,
            887
        );
        // The #1913 capture-notice segment remains pinned ahead of the
        // appended shell-recovery, auth, and hook-notification messages.
        assert_eq!(MessageId::CaptureInputRejectedTitle as usize, 888);
        assert_eq!(MessageId::CaptureInputRejectedBody as usize, 889);
        assert_eq!(
            MessageId::AgentRecoveryTriggerLine as usize,
            MessageId::CaptureInputRejectedBody as usize + 1
        );
        // The #2031 recovery-retry segment remains pinned after auth.
        assert_eq!(
            MessageId::AuthSelectProviderQuestion as usize,
            MessageId::AgentRecoveryTriggerLine as usize + 1
        );
        // The #2064 system-control segment is the current tail; the tail
        // ownership assertions move with each appended segment.
        assert_eq!(
            MessageId::AgentRecoverySameSessionRetryLine as usize,
            MessageId::AuthSelectProviderQuestion as usize + 1
        );
        // Hook-notification messages are the current tail; tail ownership
        // assertions move with each appended segment.
        assert_eq!(
            MessageId::AgentGovernanceHookNotification as usize,
            MessageId::AgentRecoverySameSessionRetryLine as usize + 1
        );
        // The #2064 system-control segment follows the hook-notification
        // segment and keeps the tail; tail ownership assertions move with
        // each appended segment.
        assert_eq!(
            MessageId::ApprovalRiskPhraseSystemControl as usize,
            MessageId::AgentGovernanceHookDecisionUnspecified as usize + 1
        );
        // The #2025/#2161 input-wait segment follows the system-control
        // segment; tail ownership assertions move with each appended segment.
        assert_eq!(
            MessageId::ApprovalShellHandoffInputWaitTimeoutTitle as usize,
            MessageId::ApprovalIrrecoverableWarningLine as usize + 1
        );
        assert_eq!(
            MessageId::ApprovalShellHandoffInputWaitTimeoutTitle as usize,
            MessageId::ALL.len() - 46
        );
        assert_eq!(
            MessageId::ShellInputWaitHintTimeoutForecastBody as usize,
            MessageId::ALL.len() - 37
        );
        // The #2068 startup auth-hint segment remains ahead of the appended
        // session-picker footer, Agent Composer, Trust-catalog, and hook-action
        // segments.
        assert_eq!(
            MessageId::StartupAuthHintLine as usize,
            MessageId::ALL.len() - 36
        );
        assert_eq!(
            MessageId::SessionPickerMarkedFooter as usize,
            MessageId::ALL.len() - 35
        );
        assert_eq!(
            MessageId::AgentComposerTitle as usize,
            MessageId::SessionPickerMarkedFooter as usize + 1
        );
        assert_eq!(
            MessageId::AgentComposerFooterEditing as usize,
            MessageId::ALL.len() - 32
        );
        assert_eq!(
            MessageId::AgentComposerRejectedTitle as usize,
            MessageId::AgentComposerFooterEditing as usize + 1
        );
        assert_eq!(
            MessageId::AgentComposerRejectedFooter as usize,
            MessageId::ApprovalTrustUnknownToolReason as usize - 1
        );
        assert_eq!(
            MessageId::ApprovalTrustUnknownToolReason as usize,
            MessageId::ALL.len() - 24
        );
        // The hook-action segment follows the Trust-catalog segment and remains
        // ahead of the appended Enhanced-routing segment.
        assert_eq!(
            MessageId::SlashHooksActionCancelledTitle as usize,
            MessageId::ALL.len() - 23
        );
        assert_eq!(
            MessageId::SlashHooksActionCancelledBody as usize,
            MessageId::ALL.len() - 22
        );
        assert_eq!(
            MessageId::SlashHooksActionVerbEnable as usize,
            MessageId::ALL.len() - 21
        );
        assert_eq!(
            MessageId::SlashHooksActionVerbDisable as usize,
            MessageId::ALL.len() - 20
        );
        assert_eq!(
            MessageId::SlashHooksActionQuestion as usize,
            MessageId::ALL.len() - 19
        );
        assert_eq!(
            MessageId::SlashHooksActionOptionShell as usize,
            MessageId::ALL.len() - 18
        );
        assert_eq!(
            MessageId::SlashHooksActionOptionAgent as usize,
            MessageId::ALL.len() - 17
        );
        assert_eq!(
            MessageId::SlashHooksActionOptionBoth as usize,
            MessageId::ALL.len() - 16
        );
        assert_eq!(
            MessageId::SlashHooksActionAgentEnabledBody as usize,
            MessageId::ALL.len() - 15
        );
        assert_eq!(
            MessageId::SlashHooksActionAgentDisabledBody as usize,
            MessageId::ALL.len() - 14
        );
        assert_eq!(
            MessageId::SlashHooksActionAgentErrorBody as usize,
            MessageId::ALL.len() - 13
        );
        assert_eq!(
            MessageId::HelpSummaryModeRouting as usize,
            MessageId::SlashHooksActionAgentErrorBody as usize + 1
        );
        assert_eq!(
            MessageId::RoutingModeShellOnlyFooter as usize,
            MessageId::ALL.len() - 3
        );
        assert_eq!(
            MessageId::HelpSummaryTask as usize,
            MessageId::RoutingModeShellOnlyFooter as usize + 1
        );
        assert_eq!(
            MessageId::HelpSummaryTask as usize,
            MessageId::ALL.len() - 2
        );
        assert_eq!(
            MessageId::PromptDraftFooterControlSubmitted as usize,
            MessageId::HelpSummaryTask as usize + 1
        );
        assert_eq!(
            MessageId::PromptDraftFooterControlSubmitted as usize,
            MessageId::ALL.len() - 1
        );
    }

    #[test]
    fn question_interaction_messages_match_the_approved_contract() {
        let en = I18n::new(Language::EnUs);
        let zh = I18n::new(Language::ZhCn);
        assert_eq!(
            en.t(MessageId::QuestionRequiredGhost),
            "Please enter an answer"
        );
        assert_eq!(
            en.t(MessageId::QuestionInvalidGhost),
            "Choose a valid answer"
        );
        assert_eq!(
            en.t(MessageId::QuestionAnswerNotSentTitle),
            "Answer not sent"
        );
        assert_eq!(
            en.t(MessageId::QuestionAnswerNotSentBody),
            "The question is still pending. Retry or press Ctrl+C to cancel."
        );
        assert_eq!(zh.t(MessageId::QuestionRequiredGhost), "请先输入回答");
        assert_eq!(zh.t(MessageId::QuestionInvalidGhost), "请选择有效回答");
        assert_eq!(zh.t(MessageId::QuestionAnswerNotSentTitle), "回答未发送");
        assert_eq!(
            zh.t(MessageId::QuestionAnswerNotSentBody),
            "问题仍在等待回答，请重试或按 Ctrl+C 取消。"
        );
    }

    #[test]
    fn format_replaces_known_args_and_keeps_missing_args() {
        let i18n = I18n::new(Language::EnUs);
        let text = i18n.format(
            MessageId::StartupAdapterLine,
            &[("adapter", "qwen"), ("shell", "bash"), ("approval", "auto")],
        );

        assert!(text.contains("qwen"));
        assert!(text.contains("bash"));
        assert!(text.contains("{analysis}"));
    }

    #[test]
    fn zh_catalog_keeps_protocol_tokens_stable() {
        let i18n = I18n::new(Language::ZhCn);

        assert!(i18n
            .t(MessageId::ModeLanguageFooter)
            .contains("/config language"));
        assert!(i18n
            .t(MessageId::RecommendationFooter)
            .contains("未执行任何命令"));
        assert!(!i18n
            .t(MessageId::RecommendationFooter)
            .contains("[Details]"));
        assert!(i18n.t(MessageId::ApprovalToolInputLabel).contains("Tool"));
        assert!(i18n.t(MessageId::HelpSummaryConfig).contains("语言"));
        assert!(i18n
            .t(MessageId::AgentRecoveryFreshTurnBody)
            .contains("provider"));
        assert!(i18n
            .t(MessageId::AgentStatusWaitingApprovalTool)
            .contains("tool"));
        assert_eq!(
            i18n.t(MessageId::ApprovalResolutionAutoApprovedTitle),
            "已自动批准"
        );
    }

    #[test]
    fn quoted_argument_error_is_localized() {
        let en = I18n::new(Language::EnUs);
        assert_eq!(
            en.t(MessageId::SlashInvalidArgumentsTitle),
            "Invalid slash arguments"
        );
        assert_eq!(
            en.t(MessageId::SlashQuotedArgumentsUnsupported),
            "Quoted arguments are not supported. Use /mode approval trust confirm instead."
        );

        let zh = I18n::new(Language::ZhCn);
        assert_eq!(
            zh.t(MessageId::SlashInvalidArgumentsTitle),
            "Slash 参数错误"
        );
        assert_eq!(
            zh.t(MessageId::SlashQuotedArgumentsUnsupported),
            "不支持带引号的参数。本例请改用 /mode approval trust confirm。"
        );
    }

    #[test]
    fn help_and_mode_messages_distinguish_recommendation_and_insight_scopes() {
        let en = I18n::new(Language::EnUs);
        let zh = I18n::new(Language::ZhCn);

        // /help: /recommendations is scoped to personalization only and points to /mode analysis.
        assert!(en
            .t(MessageId::HelpSummaryRecommendations)
            .contains("personalized prompt recommendations only"));
        assert!(en
            .t(MessageId::HelpSummaryRecommendations)
            .contains("/mode analysis"));
        assert!(zh
            .t(MessageId::HelpSummaryRecommendations)
            .contains("仅管理个性化提示词推荐"));
        assert!(zh
            .t(MessageId::HelpSummaryRecommendations)
            .contains("/mode analysis"));

        // /help: /mode analysis owns passive suggestions and failure insights.
        assert!(en
            .t(MessageId::HelpSummaryModeAnalysis)
            .contains("failure insights"));
        assert!(zh
            .t(MessageId::HelpSummaryModeAnalysis)
            .contains("失败命令 Insight"));

        // /mode analysis manual: footer states insight scope and the asymmetric
        // pause of personalized recommendations.
        assert!(en
            .t(MessageId::AnalysisModeManualFooter)
            .contains("failure insights"));
        assert!(en
            .t(MessageId::AnalysisModeManualFooter)
            .contains("/recommendations"));
        assert!(zh
            .t(MessageId::AnalysisModeManualFooter)
            .contains("失败命令 Insight"));
        assert!(zh
            .t(MessageId::AnalysisModeManualFooter)
            .contains("/recommendations"));
    }
}
