use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::HookFindingTitle => "Hook finding",
        MessageId::HookFindingFooter => "Use /hooks to review findings.",
        MessageId::HookFindingMarkdownTitle => "Command hook finding",
        MessageId::HookFindingMarkdownHookLine => "- Hook: `{hook_id}`.",
        MessageId::HookFindingMarkdownSeverityLine => "- Severity: `{severity}`.",
        MessageId::HookFindingMarkdownFindingLine => "- Finding: {finding}.",
        MessageId::HookFindingMarkdownSuggestionLine => "- Suggestion: {suggestion}.",
        MessageId::HookFindingMarkdownRelatedTitle => "- Related findings:",
        MessageId::HookFindingMarkdownRelatedLine => "  - `{hook_id}` [{severity}]: {finding}",
        MessageId::HookFindingMarkdownAgentFollowUpLine => {
            "Agent follow-up must use bounded cosh-shell evidence before claiming details."
        }
        MessageId::HookHintTitle => "Hook hint",
        MessageId::HookHintNotFoundBody => "Hook hint '{hint_id}' was not found in this session.",
        MessageId::HookHintNotFoundFooter => "Use /hooks history to copy a recent finding id.",
        MessageId::HookHintNoFindingBody => "Hook hint '{hint_id}' has no finding attached.",
        MessageId::HookHintBlockUnavailableBody => {
            "Command block '{block_id}' is no longer available."
        }
        MessageId::HookHintIgnoredTitle => "Hook hint ignored",
        MessageId::HookHintIgnoredBody => "Ignored hook hint '{hint_id}' for this session.",
        MessageId::HookHintIgnoredFooter => "Future matching findings are downgraded by policy.",
        MessageId::HookHintUsageTitle => "Usage",
        MessageId::HookHintUsageBody => "/hooks analyze|ignore|details <hint_id>",
        MessageId::HookFindingDetailsTitle => "Hook finding details",
        MessageId::HookConsultationHookLabel => "Hook",
        MessageId::HookConsultationConfidenceReasonLine => {
            "Confidence: {confidence}; reason: {reason}"
        }
        MessageId::HookConsultationFindingLine => "Finding: {finding}",
        MessageId::HookConsultationSuggestionLine => "Recommended action: {suggestion}",
        MessageId::HookConsultationAnalyzeAction => "Analyze",
        MessageId::HookConsultationIgnoreAction => "Ignore",
        MessageId::HookDetailsConfidenceLine => "Confidence: {confidence}; policy reason: {reason}",
        MessageId::HookDetailsUserInterestLine => "User-interest reason: {code}: {description}",
        MessageId::HookDetailsReasonLookupIntent => {
            "the command targets a specific process or search, so the finding stays low-interruption"
        }
        MessageId::HookDetailsReasonPipelineIntent => {
            "the command pipeline may have transformed output, so missing or uncertain schema is not treated as high-confidence"
        }
        MessageId::HookDetailsReasonScriptIntent => {
            "script or batch output may not reflect the user's immediate focus, so interruption is reduced"
        }
        MessageId::HookDetailsReasonWrapperLowConfidence => {
            "wrapper or remote/container context makes the target view ambiguous, so verification is required"
        }
        MessageId::HookDetailsReasonInteractiveIntent => {
            "interactive output is not a stable diagnostic snapshot, so only sampling guidance is shown"
        }
        MessageId::HookDetailsReasonActiveRunDeferred => {
            "another Agent run was active, so this success-command finding waits and is rechecked before display"
        }
        MessageId::HookDetailsReasonUserContinuedInput => {
            "the user moved on to another input, so this success-command finding does not interrupt"
        }
        MessageId::HookDetailsReasonNonDiagnosticSuccessCommand => {
            "the command does not look like an explicit diagnostic snapshot, so interruption is reduced"
        }
        MessageId::HookDetailsReasonFeedbackNoisy => {
            "prior user feedback says similar findings are noisy, so interruption is reduced"
        }
        MessageId::HookDetailsReasonIgnoredSameFinding => {
            "the user ignored a matching finding earlier in this session"
        }
        MessageId::HookDetailsReasonSameCardAlreadyRendered => {
            "an equal-or-higher severity card was already shown for this finding key"
        }
        MessageId::HookDetailsReasonInterruptionBudget => {
            "recent similar cards already used the session interruption budget"
        }
        MessageId::HookDetailsReasonLowConfidence => {
            "partial evidence requires read-only verification before stronger claims"
        }
        MessageId::HookDetailsReasonDiagnosticIntent => {
            "explicit diagnostic command with sufficient evidence"
        }
        MessageId::HookDetailsReasonOtherIntent => "no explicit diagnostic intent was identified",
        MessageId::HookDetailsTopicLine => "Topic: {topic}; entity: {entity}",
        MessageId::HookDetailsOriginLine => "Command origin: {origin}",
        MessageId::HookDetailsSuppressionKeyLine => "Suppression key: {key}",
        MessageId::HookDetailsOutputRefLine => "Output capture: {ref}",
        MessageId::HookDetailsCreatedAtLine => "Created at: {created_at}",
        MessageId::HookDetailsPromptHintLine => "Prompt hint: {hint}",
        MessageId::HookDetailsRecommendedSkillLine => "Recommended skill: {skill}",
        MessageId::HookDetailsReadOnlyCliHintLine => "Read-only CLI hint: {hint}",
        MessageId::HookDetailsFooter => "Analyze still requires confirmation.",
        _ => return None,
    })
}
