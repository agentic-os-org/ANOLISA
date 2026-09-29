use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::HelpTitle => "Slash commands",
        MessageId::HelpFooter => {
            "Mode: {mode}. Strategy: {strategy}. Shift+Enter / Alt+Enter insert a newline in the prompt; start prompts with ?? to compose multi-line."
        }
        MessageId::PromptSoftNewlineTip => {
            "Tip: start with ?? to compose multi-line prompts (Shift+Enter for newline)."
        }
        MessageId::PromptDraftTitle => "Prompt draft",
        MessageId::PromptDraftFooterEditing => {
            "Enter send · Shift+Enter newline · Esc cancel"
        }
        MessageId::PromptDraftFooterSubmitted => "Sent to agent",
        MessageId::PromptDraftFooterControlSubmitted => "Command submitted",
        MessageId::PromptDraftFooterCancelled => "Draft cancelled",
        MessageId::AgentComposerTitle => "Agent Composer",
        MessageId::PromptDraftRuntimeLabel => "Runtime",
        MessageId::AgentComposerFooterEditing => {
            "/ commands · ↑↓ select · Tab complete · Enter submit · Shift+Enter newline · Esc cancel"
        }
        MessageId::AgentComposerRejectedTitle => "References skipped",
        MessageId::AgentComposerRejectedInvalidPathLine => {
            "{path}: invalid workspace-relative path"
        }
        MessageId::AgentComposerRejectedUnavailablePathLine => {
            "{path}: path does not exist or is not a file or directory"
        }
        MessageId::AgentComposerRejectedOutsideWorkspaceLine => {
            "{path}: resolved path leaves the workspace"
        }
        MessageId::AgentComposerRejectedWorkspaceUnavailableLine => {
            "{path}: shell workspace is unavailable"
        }
        MessageId::AgentComposerRejectedLimitLine => {
            "{path}: the 16-reference limit was reached"
        }
        MessageId::AgentComposerRejectedFooter => {
            "Skipped references were not sent as Agent context."
        }
        MessageId::HelpGroupPrompt => "Prompt",
        MessageId::HelpSummaryDraft => "open the multi-line prompt draft card (same as ?? + Enter)",
        MessageId::PromptMultilineEntryHint => {
            "For multi-line prompts type ?? then Enter; use /agent for a one-shot cosh-core request"
        }
        MessageId::HelpGroupConfig => "Config",
        MessageId::HelpGroupHealth => "Health",
        MessageId::HelpGroupModes => "Modes",
        MessageId::HelpGroupHooks => "Hooks",
        MessageId::HelpSummaryHelp => "show command reference",
        MessageId::HelpSummaryAuth => "configure AI provider credentials",
        MessageId::HelpSummaryConfig => "configure UI language",
        MessageId::HelpSummaryRecommendations => {
            "manage personalized prompt recommendations only (failure insights: /mode analysis); analysis sends bounded activity to the provider, and local clear does not control provider retention"
        }
        MessageId::HelpSummaryModeApproval => "change approval mode",
        MessageId::HelpSummaryModeAnalysis => {
            "choose suggested mode, automatic analysis, or no proactive assistance; controls passive suggestions and failure insights after failed commands"
        }
        MessageId::HelpSummaryModeRouting => {
            "choose whether unknown natural-language input may route to Agent"
        }
        MessageId::HelpSummaryAgent => "compose a one-shot Agent request",
        MessageId::HelpSummaryExplain => "analyze the last failed command",
        MessageId::HelpSummaryCancel => "cancel active Agent work",
        MessageId::HelpSummaryDetails => "inspect approval/activity details",
        MessageId::HelpSummaryAudit => "show audit entry points",
        MessageId::HelpSummaryHooks => "show hook status",
        MessageId::HelpSummaryHealth => "run an on-demand health check",
        MessageId::HelpSummarySelect => "show a displayed recommendation",
        MessageId::HelpSummaryCopy => "copy a displayed recommendation",
        MessageId::HelpSummaryDebug => "show session debug details",
        MessageId::HelpSummaryClear => "clear local shell state",
        MessageId::HelpSummaryShell => "return to shell input",
        MessageId::HelpSummaryApprovalModeRemoved => "removed approval-mode alias",
        MessageId::SlashHintTitle => "Slash command hint",
        MessageId::SlashHintPrefix => "Prefix: {prefix}",
        MessageId::SlashHintCurrentMode => "Current mode: {mode}",
        MessageId::SlashHintFooter => {
            "Type a full command and press Enter; paths like /tmp/foo stay in shell."
        }
        MessageId::SlashUnknownTitle => "Slash command",
        MessageId::SlashUnknownBody => "Unknown slash command: {command}",
        MessageId::SlashUnknownSuggestionBody => "Did you mean {command}?",
        MessageId::SlashUnknownFooter => "Use /help to see available commands.",
        MessageId::SlashInvalidArgumentsTitle => "Invalid slash arguments",
        MessageId::SlashQuotedArgumentsUnsupported => {
            "Quoted arguments are not supported. Use /mode approval trust confirm instead."
        }
        MessageId::SlashInfoAuditTitle => "Audit",
        MessageId::SlashInfoAuditApprovalsBody => {
            "Approval decisions are available with Details actions."
        }
        MessageId::SlashInfoAuditActivityBody => {
            "Activity output refs are available with Details actions."
        }
        MessageId::SlashInfoAuditFooter => "Audit views are read-only; no shell command runs.",
        MessageId::SlashInfoConfigTitle => "Config",
        MessageId::SlashInfoConfigLanguageLine => "language: {effective} source: {source}",
        MessageId::SlashInfoConfigLanguageEffectiveLine => {
            "language: {effective} effective, setting: {setting}, source: {source}"
        }
        MessageId::SlashInfoConfigPathLine => "config: {path}",
        MessageId::SlashInfoConfigDebugActivityLine => {
            "debug activity: {state} (ui.debug or COSH_SHELL_DEBUG=1)"
        }
        MessageId::SlashInfoConfigAnalysisStrategyLine => {
            "analysis strategy: /mode analysis smart|auto|manual"
        }
        MessageId::SlashInfoConfigRenderFallbackLine => {
            "render fallback: set COSH_SHELL_RENDER=plain before starting cosh-shell."
        }
        MessageId::SlashInfoConfigFooter => {
            "Use /config language [auto|en-US|zh-CN]. Takes effect immediately; agent replies follow your message language."
        }
        MessageId::HelpGroupSessions => "Sessions",
        MessageId::HelpSummarySession => "discover, resume, and clear Agent sessions",
        MessageId::HelpSummaryTask => "delegate and inspect persistent local Agent Tasks",
        MessageId::HelpGroupRegistry => "Registry",
        MessageId::HelpSummaryExtensions => "list/manage cosh-core extensions",
        MessageId::HelpSummarySkills => "list/inspect cosh-core skills",
        MessageId::HelpSummaryMcp => "manage MCP servers",
        MessageId::HelpGroupStatus => "Status",
        MessageId::HelpSummaryStatus => "show version, provider, model, and runtime status",
        MessageId::HelpSummaryStats => "show model and tool session statistics",
        MessageId::SlashValueUnavailable => "unavailable",
        MessageId::SlashValueNotStarted => "not started",
        MessageId::SlashValueIdle => "idle",
        MessageId::SlashValueActive => "active",
        MessageId::SlashStatusTitle => "Status",
        MessageId::SlashStatusVersionLine => "cosh-shell: {version}",
        MessageId::SlashStatusBackendLine => "Backend: {backend}",
        MessageId::SlashStatusProviderLine => "Provider: {provider}",
        MessageId::SlashStatusModelLine => "Model: {model}",
        MessageId::SlashStatusSessionLine => "Session: {session}",
        MessageId::SlashStatusOsLine => "OS: {os}",
        MessageId::SlashStatusModesLine => {
            "Modes: approval={approval}, analysis={analysis}"
        }
        MessageId::SlashStatusProviderUnavailableLine => {
            "Provider details: unavailable from the current backend"
        }
        MessageId::SlashStatusFooter => {
            "/about is an alias for /status. Use /stats [model|tools] for session statistics."
        }
        MessageId::SlashStatsTitle => "Session stats",
        MessageId::SlashStatsModelTitle => "Model stats",
        MessageId::SlashStatsToolsTitle => "Tool stats",
        MessageId::SlashStatsModelLine => "Model: {model}",
        MessageId::SlashStatsBackendLine => "Backend: {backend}",
        MessageId::SlashStatsRunStateLine => "Agent run: {state}",
        MessageId::SlashStatsToolTotalsLine => {
            "Tools: {calls} calls, {successful} successful, {failed} failed, {pending} pending"
        }
        MessageId::SlashStatsNoToolCalls => {
            "No tool calls have been recorded in this session."
        }
        MessageId::SlashStatsToolRow => {
            "{name}: {calls} calls, {successful} successful, {failed} failed, {pending} pending"
        }
        MessageId::SlashStatsTelemetryUnavailable => {
            "Token counts, API errors, and latency are not exposed by the current backend protocol."
        }
        MessageId::SlashStatsUsageLine => "Usage: /stats [model|tools]",
        MessageId::SlashStatsFooter => {
            "Statistics are read-only and cover data observed by this cosh-shell process."
        }
        MessageId::SlashExtensionsTitle => "Extensions",
        MessageId::SlashSkillsTitle => "Skills",
        MessageId::SlashMcpTitle => "MCP Servers",
        MessageId::SlashRegistryUnavailable => {
            "This feature requires cosh-core backend."
        }
        MessageId::SlashHooksShellSection => "Shell Hooks",
        MessageId::SlashHooksAgentSection => "Agent Hooks",
        MessageId::SlashHooksAgentUnavailable => "(cosh-core backend unavailable)",
        MessageId::SlashExtensionsEmptyBody => "No extensions installed.",
        MessageId::SlashSkillsEmptyBody => "No skills found.",
        _ => return None,
    })
}
