use super::*;
use crate::ui::welcome::WelcomeModel;

pub(super) fn welcome_model(
    state: &InlineState,
    environment: Option<(&str, &str)>,
) -> WelcomeModel {
    let i18n = state.i18n();
    let compact = environment.is_none();
    let mut context = Vec::new();
    if let Some((shell, cwd)) = environment {
        context.push(i18n.format(
            MessageId::StartupAdapterLine,
            &[("shell", shell), ("approval", state.approval_mode.label())],
        ));
        context.push(i18n.format(MessageId::StartupCwdLine, &[("cwd", cwd)]));
    }
    let capability = if state.personalization.ai_disabled {
        if compact {
            MessageId::StartupAskOnlyLine
        } else {
            MessageId::StartupAiDisabledLine
        }
    } else {
        MessageId::StartupCommandsLine
    };
    let notice = match state.approval_mode {
        CoshApprovalMode::Recommend => Some(MessageId::ApprovalModeRecommendFooter),
        CoshApprovalMode::Trust => Some(MessageId::ApprovalModeTrustFooter),
        CoshApprovalMode::Auto => None,
    };
    WelcomeModel {
        compact,
        context,
        capability: i18n.t(capability).to_string(),
        notice: notice.map(|id| i18n.t(id).to_string()),
        extra: if !compact
            && !state.personalization.ai_disabled
            && state.startup_auth.ai_unconfigured()
        {
            vec![i18n.t(MessageId::StartupAuthHintLine).to_string()]
        } else {
            Vec::new()
        },
    }
}
