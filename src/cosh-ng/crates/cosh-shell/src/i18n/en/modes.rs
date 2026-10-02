use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::ApprovalModeRemovedBody => "/approval-mode is not supported.",
        MessageId::ApprovalModeRemovedFooter => "Use /mode approval [recommend|auto|trust].",
        MessageId::ModeTitle => "Mode",
        MessageId::ModesTitle => "Modes",
        MessageId::ModeApprovalLine => "approval: {mode}",
        MessageId::ModeAnalysisLine => "analysis: {mode}",
        MessageId::ModeRoutingLine => "routing: {mode}",
        MessageId::ModeSummaryFooter => {
            "Use /mode approval, /mode analysis, or /mode routing for details."
        }
        MessageId::RoutingModeTitle => "Input routing",
        MessageId::RoutingModeCurrentBody => "Current: {mode}",
        MessageId::RoutingModeSetBody => "Routing mode set to {mode}.",
        MessageId::RoutingModeUnavailableBody => {
            "Routing mode is unavailable in Native sessions; choose Enhanced at startup."
        }
        MessageId::RoutingModeUnknownBody => "Unknown routing mode: {mode}",
        MessageId::RoutingModeUsageFooter => {
            "Use /mode routing assisted|shell-only."
        }
        MessageId::RoutingModeAssistedFooter => {
            "Unknown natural-language input may route to Agent; post-command insights remain available."
        }
        MessageId::RoutingModeShellOnlyFooter => {
            "Input stays with Shell; post-command insights remain available."
        }
        MessageId::ModeRemovedTitle => "Mode command removed",
        MessageId::ModeRemovedBody => "/mode {mode} is not supported.",
        MessageId::ModeRemovedFooter => "Use /mode approval {mode}.",
        MessageId::ModeLanguageBody => "Language is persistent config, not a runtime mode.",
        MessageId::ModeLanguageFooter => "Use /config language [auto|en-US|zh-CN].",
        MessageId::ModeUnknownBody => "Unknown mode: {mode}",
        MessageId::ModeUnknownFooter => {
            "Use /mode approval, /mode analysis, or /mode routing."
        }
        MessageId::ApprovalModeTitle => "Approval mode",
        MessageId::ApprovalModeSetBody => "Mode set to {mode}.",
        MessageId::ApprovalModeUnknownBody => "Unknown approval mode: {mode}",
        MessageId::ApprovalModeUsageFooter => "Use /mode approval recommend|auto|trust.",
        MessageId::ApprovalModeRecommendFooter => {
            "Agent explains and suggests; no tool calls are emitted."
        }
        MessageId::ApprovalModeAutoFooter => {
            "Read-only tools auto-approved; risky requests need confirmation."
        }
        MessageId::ApprovalModeTrustFooter => {
            "All tools auto-approved; audit trail preserved via control protocol."
        }
        MessageId::ApprovalModeTrustConfirmationTitle => "Trust confirmation required",
        MessageId::ApprovalModeTrustConfirmationBody => {
            "Trust mode auto-approves provider tool requests for this session."
        }
        MessageId::ApprovalModeTrustConfirmationCommandBody => {
            "Run /mode approval trust confirm to enable it explicitly."
        }
        MessageId::ApprovalModeTrustConfirmationFooter => {
            "Recommend or auto mode remains active until confirmation."
        }
        MessageId::ApprovalModeCardTitle => "User mode",
        MessageId::ApprovalModeCardCurrentLine => "Current: {mode}",
        MessageId::ApprovalModeCardRecommendLine => {
            "{marker}[ recommend ] Explain and suggest only"
        }
        MessageId::ApprovalModeCardAutoLine => {
            "{marker}[ auto      ] Read-only auto-approved; risky needs confirmation"
        }
        MessageId::ApprovalModeCardTrustLine => {
            "{marker}[ trust     ] All tools auto-approved with audit trail"
        }
        MessageId::ApprovalModeCardFooter => "Keys: Left/Right select | Enter apply | Esc cancel",
        MessageId::ApprovalModeRemainsBody => "Mode remains {mode}.",
        MessageId::ApprovalModeCancelBody => "Mode unchanged: {mode}.",
        MessageId::ApprovalModeCancelFooter => "No shell command ran.",
        MessageId::AnalysisModeTitle => "Analysis mode",
        MessageId::AnalysisModeCurrentBody => "Current: {mode}",
        MessageId::AnalysisModeSetBody => "Mode set to {mode}.",
        MessageId::AnalysisModeUnknownBody => "Unknown analysis mode: {mode}",
        MessageId::AnalysisModeUsageFooter => "Use /mode analysis smart|auto|manual.",
        MessageId::AnalysisModeSmartFooter => {
            "Failures and useful system-diagnostic output are evaluated; insights are shown for review."
        }
        MessageId::AnalysisModeAutoFooter => {
            "Only a narrow set of high-confidence failures auto-starts Agent analysis; other cases remain suggestions."
        }
        MessageId::AnalysisModeManualFooter => {
            "Passive suggestions, failure insights, and automatic analysis are off; use slash commands to trigger analysis. Personalized prompt recommendations also pause; manage them with /recommendations."
        }
        MessageId::AnalysisModeCardSmartLine => {
            "{marker}[ smart  ] Suggested mode (recommended)"
        }
        MessageId::AnalysisModeCardAutoLine => {
            "{marker}[ auto   ] Automatic analysis (may start Agent after a command failure)"
        }
        MessageId::AnalysisModeCardManualLine => {
            "{marker}[ manual ] Disable proactive assistance"
        }
        MessageId::AnalysisModeCardFooter => {
            "Keys: Left/Right or Tab/Shift-Tab select | Enter apply | Esc cancel"
        }
        MessageId::AnalysisModeRemainsBody => "Mode remains {mode}.",
        MessageId::AnalysisModeCancelBody => "Mode unchanged: {mode}.",
        MessageId::AnalysisModeCancelFooter => "No shell command ran.",
        _ => return None,
    })
}
