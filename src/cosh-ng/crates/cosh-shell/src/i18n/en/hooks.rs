use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::SlashHooksRegisteredTitle => "Hook status",
        MessageId::SlashHooksNoHooksBody => "No hooks registered.",
        MessageId::SlashHooksStatusCountLine => {
            "Registered: {total}; enabled: {enabled}; disabled: {disabled}."
        }
        MessageId::SlashHooksStatusSourcesLine => {
            "Sources: builtin={builtin}; user={user}; project={project}."
        }
        MessageId::SlashHooksStatusProjectTrustLine => {
            "Project trust: trusted={trusted}; untrusted={untrusted}."
        }
        MessageId::SlashHooksFooterCount => "{count} hook(s) registered.",
        MessageId::SlashHooksFooterMutedTargets => {
            "{count} hook(s) registered. Muted targets: {targets}."
        }
        MessageId::SlashHooksTargetMutedTitle => "Hook target muted",
        MessageId::SlashHooksTargetMutedBody => "Muted hook target '{target}' for this session.",
        MessageId::SlashHooksTargetMutedFooter => {
            "Muted findings are still recorded in /hooks history."
        }
        MessageId::SlashHooksTargetUnmutedTitle => "Hook target unmuted",
        MessageId::SlashHooksTargetUnmutedBody => "Unmuted hook target '{target}'.",
        MessageId::SlashHooksTargetNotMutedBody => "Hook target '{target}' was not muted.",
        MessageId::SlashHooksEnabledTitle => "Hook enabled",
        MessageId::SlashHooksEnabledBody => "Hook '{id}' enabled.",
        MessageId::SlashHooksDisabledTitle => "Hook disabled",
        MessageId::SlashHooksDisabledBody => "Hook '{id}' disabled.",
        MessageId::SlashHooksHistoryTitle => "Hook history",
        MessageId::SlashHooksHistoryEmptyBody => "No hook findings recorded in this session.",
        MessageId::SlashHooksHistoryFooter => {
            "Recent findings are read-only; Analyze still requires user confirmation."
        }
        MessageId::SlashHooksEventsTitle => "Hook display events",
        MessageId::SlashHooksEventsEmptyBody => "No hook display events recorded in this session.",
        MessageId::SlashHooksEventsFooter => {
            "Events are session-local and contain policy metadata, not command output."
        }
        MessageId::SlashHooksUsageTitle => "Usage",
        MessageId::SlashHooksUsageListLine => "/hooks                - show hook status",
        MessageId::SlashHooksUsageHistoryLine => {
            "/hooks history        - show recent hook findings"
        }
        MessageId::SlashHooksUsageEventsLine => {
            "/hooks events         - show recent hook display events"
        }
        MessageId::SlashHooksUsageAnalyzeLine => {
            "/hooks analyze <id>   - analyze a hint finding"
        }
        MessageId::SlashHooksUsageIgnoreLine => {
            "/hooks ignore <id>    - ignore a hint finding"
        }
        MessageId::SlashHooksUsageDetailsLine => {
            "/hooks details <id>   - show hook finding details"
        }
        MessageId::SlashHooksUsageFeedbackLine => {
            "/hooks feedback noisy|useful <id> - record feedback"
        }
        MessageId::SlashHooksUsageClearFeedbackLine => {
            "/hooks clear-feedback - clear hook feedback preferences"
        }
        MessageId::SlashHooksUsageMuteLine => "/hooks mute <target>  - mute a topic or hook id",
        MessageId::SlashHooksUsageUnmuteLine => {
            "/hooks unmute <target>- unmute a topic or hook id"
        }
        MessageId::SlashHooksUsageTrustProjectLine => {
            "/hooks trust-project  - trust project hooks for this session"
        }
        MessageId::SlashHooksUsageUntrustProjectLine => {
            "/hooks untrust-project- untrust project hooks for this session"
        }
        MessageId::SlashHooksUsageClearProjectTrustLine => {
            "/hooks clear-project-trust - clear project hook trust store"
        }
        MessageId::SlashHooksUsageEnableLine => "/hooks enable <id>    - enable a hook",
        MessageId::SlashHooksUsageDisableLine => "/hooks disable <id>   - disable a hook",
        MessageId::SlashHooksProjectTrustedTitle => "Project hooks trusted",
        MessageId::SlashHooksProjectUntrustedTitle => "Project hooks untrusted",
        MessageId::SlashHooksProjectTrustNoHooksBody => {
            "No project hooks are registered in this session."
        }
        MessageId::SlashHooksProjectTrustedBody => "{count} project hook(s) marked trusted.",
        MessageId::SlashHooksProjectUntrustedBody => "{count} project hook(s) marked untrusted.",
        MessageId::SlashHooksProjectTrustNoChangeFooter => "No trust state changed.",
        MessageId::SlashHooksProjectTrustPersistedFooter => {
            "Trust persisted; disabled hooks remain disabled."
        }
        MessageId::SlashHooksProjectTrustRemovedFooter => {
            "Trust removed from persistent store; disabled hooks remain disabled."
        }
        MessageId::SlashHooksProjectTrustPersistenceFailedFooter => {
            "Session state changed, but persistence failed: {failures}"
        }
        MessageId::SlashHooksProjectTrustClearedTitle => "Project hook trust cleared",
        MessageId::SlashHooksProjectTrustClearedBody => {
            "{count} project hook(s) marked untrusted."
        }
        MessageId::SlashHooksProjectTrustClearedFooter => {
            "Project hook trust store cleared; current session project hooks are untrusted."
        }
        MessageId::SlashHooksProjectTrustClearFailedFooter => {
            "Current session project hooks marked untrusted, but clearing persistent trust store failed: {error}"
        }
        MessageId::SlashHooksFeedbackUsageBody => "/hooks feedback noisy|useful <finding_id>",
        MessageId::SlashHooksFeedbackTitle => "Hook feedback",
        MessageId::SlashHooksFeedbackFindingNotFoundBody => {
            "Finding '{finding_id}' was not found in this session."
        }
        MessageId::SlashHooksFeedbackFindingNotFoundFooter => {
            "Use /hooks history to copy a recent finding id."
        }
        MessageId::SlashHooksFeedbackRecordedTitle => "Hook feedback recorded",
        MessageId::SlashHooksFeedbackRecordedBody => {
            "Feedback '{feedback}' recorded for finding '{finding_id}'."
        }
        MessageId::SlashHooksFeedbackHookLine => "Hook: {hook_id}.",
        MessageId::SlashHooksFeedbackPolicyKeyLine => "Policy key: {key}.",
        MessageId::SlashHooksFeedbackPersistedFooter => {
            "Feedback persisted. It affects display strategy only."
        }
        MessageId::SlashHooksFeedbackPersistenceFailedFooter => {
            "Session feedback recorded, but persistence failed: {error}"
        }
        MessageId::SlashHooksFeedbackClearedTitle => "Hook feedback cleared",
        MessageId::SlashHooksFeedbackClearedBody => {
            "{count} feedback preference(s) cleared from this session."
        }
        MessageId::SlashHooksFeedbackClearedFooter => "Hook feedback preferences cleared.",
        MessageId::SlashHooksFeedbackClearFailedFooter => {
            "Session feedback cleared, but persistent store clear failed: {error}"
        }
        _ => return None,
    })
}
