use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::ApprovalTitle => "Approval",
        MessageId::ApprovalRequiredTitle => "Approval required",
        MessageId::ApprovalResolutionApprovedTitle => "Approved",
        MessageId::ApprovalResolutionAutoApprovedTitle => "Auto-approved",
        MessageId::ApprovalResolutionTrustedTitle => "Trusted",
        MessageId::ApprovalResolutionTurnApprovedTitle => "Approved for this turn",
        MessageId::ApprovalResolutionDeniedTitle => "Denied",
        MessageId::ApprovalResolutionCancelledTitle => "Cancelled",
        MessageId::ApprovalResolutionBlockedTitle => "Blocked",
        MessageId::ApprovalResolutionDeferredTitle => "Deferred",
        MessageId::ApprovalActionAllowOnce => "Allow once",
        MessageId::ApprovalActionApproveTurn => "Allow all this turn",
        MessageId::ApprovalActionAlwaysTrust => "Always trust",
        MessageId::ApprovalActionDeny => "Deny",
        MessageId::ApprovalActionDetails => "Details",
        MessageId::ApprovalToolInputLabel => "Tool input",
        MessageId::ApprovalCommandLabel => "Command",
        MessageId::ApprovalDetailsTitle => "Approval details",
        MessageId::ApprovalDetailsSourceLabel => "Source",
        MessageId::ApprovalDetailsRunLabel => "Run",
        MessageId::ApprovalDetailsExecutionLabel => "Execution",
        MessageId::ApprovalDetailsCommandBlockLabel => "Command block",
        MessageId::ApprovalDetailsRedactionLabel => "Redaction",
        MessageId::ApprovalDetailsProviderRequestLabel => "Provider request",
        MessageId::ApprovalDetailsToolUseLabel => "Tool use",
        MessageId::ApprovalDetailsDefaultDenyLine => "Default: deny",
        MessageId::ApprovalDetailsRequestLabel => "Request",
        MessageId::ApprovalDetailsInputLabel => "Input",
        MessageId::ApprovalDetailsBashCommandSubject => "Bash command",
        MessageId::ApprovalDetailsShellCommandSubject => "Shell command",
        MessageId::ApprovalDetailsToolSubject => "{tool} tool",
        MessageId::ApprovalDetailsPendingValue => "<pending>",
        MessageId::ApprovalDetailsNoneValue => "<none>",
        MessageId::ApprovalDetailsNotApplicableValue => "<not-applicable>",
        MessageId::ApprovalAssessmentSummaryLine => {
            "Assessment: impact {impact}; decision {decision}; confidence {confidence}"
        }
        MessageId::ApprovalAssessmentReasonLine => "Reason: {reason}",
        MessageId::ApprovalJournalTitle => "Approval journal",
        MessageId::ApprovalJournalDecisionCount => "{count} decisions",
        MessageId::ApprovalJournalEmptyBody => {
            "No approval decisions recorded in this shell session."
        }
        MessageId::ApprovalJournalActorLabel => "Actor",
        MessageId::ApprovalJournalPreviewHashLabel => "Preview hash",
        MessageId::ApprovalJournalSubjectLabel => "Subject",
        MessageId::ApprovalJournalPreviewLabel => "Preview",
        MessageId::ApprovalRiskSuffix => "{risk} risk",
        MessageId::ApprovalQueueCompactLine => "Queue: {position}/{total} pending",
        MessageId::ApprovalQueueFullLine => "Queue: {position} of {total} pending",
        MessageId::ApprovalQueueNextSuffix => "; next {next}",
        MessageId::ApprovalSubjectLabel => "Subject: ",
        MessageId::ApprovalNextLabel => "Next: ",
        MessageId::ApprovalKeysPrefix => "Keys: ",
        MessageId::ApprovalKeysText => "Left/Right select  Enter confirm  d details  Esc cancel",
        MessageId::ApprovalExecutableToolPolicy => {
            "Policy: user approval is required before any executable tool request."
        }
        MessageId::ApprovalExecutableToolPolicyExtra => {
            "Only approved read-only Bash/shell tool requests may run in this MVP."
        }
        MessageId::ApprovalCommandDefaultPolicy => {
            "Default: deny. Approved command is rechecked by read-only broker."
        }
        MessageId::ApprovalRunShellCommandPrompt => "Run shell command?",
        MessageId::ApprovalRunBashCommandPrompt => "Run Bash command?",
        MessageId::ApprovalNotFoundTitle => "Approval not found",
        MessageId::ApprovalNotFoundBody => {
            "{id} is not available; the approval card may already be resolved"
        }
        MessageId::ApprovalShellHandoffNotFoundTitle => "Shell handoff not found",
        MessageId::ApprovalShellHandoffNotFoundBody => {
            "{id} is not available; use Details on the provider tool failure first"
        }
        MessageId::ApprovalShellHandoffBlockedTitle => "Shell handoff blocked",
        MessageId::ApprovalShellHandoffBlockedFooter => {
            "The command was not written to the foreground shell."
        }
        MessageId::ApprovalShellHandoffValidationEmptyCommand => "Shell handoff command is empty.",
        MessageId::ApprovalShellHandoffValidationMultilineCommand => {
            "Shell handoff command contains an unsupported line break. Only line feeds inside complete single-quoted arguments are allowed."
        }
        MessageId::ApprovalShellHandoffValidationControlCharacter => {
            "Shell handoff command contains a blocked control character."
        }
        MessageId::ApprovalShellHandoffValidationEmptyPreview => "Shell handoff preview is empty.",
        MessageId::ApprovalShellHandoffValidationEmptyApprovalId => {
            "Shell handoff approval id is empty."
        }
        MessageId::ApprovalShellHandoffValidationEmptyRunId => "Shell handoff run id is empty.",
        MessageId::ApprovalShellHandoffSendingTitle => "Sending to shell",
        MessageId::ApprovalShellHandoffSendingBody => "{id} will run in the foreground shell.",
        MessageId::ApprovalShellHandoffTimeoutTitle => "Shell recovery",
        MessageId::ApprovalShellHandoffTimeoutExceededBody => {
            "Command exceeded configured shell handoff timeout ({seconds}s)."
        }
        MessageId::ApprovalShellHandoffTimeoutInterruptBody => {
            "Sent interrupt to foreground PTY; waiting for shell evidence."
        }
        MessageId::ApprovalShellHandoffInputWaitTimeoutTitle => "Command input-wait timeout",
        MessageId::ApprovalShellHandoffInputWaitTimeoutExceededBody => {
            "Foreground command waited for keyboard input over {seconds}s with no answer (input_wait_timeout_secs)."
        }
        MessageId::ApprovalShellHandoffInputWaitTimeoutInterruptBody => {
            "Interrupted the command (like Ctrl+C); the agent receives the result and can retry non-interactively."
        }
        MessageId::ShellInputWaitHintTitle => "⏳ Command is waiting for input",
        MessageId::ShellInputWaitHintPasswordBody => {
            "The command is waiting for a password/hidden input."
        }
        MessageId::ShellInputWaitHintPagerBody => {
            "Output is being paged; press q to quit the pager and continue."
        }
        MessageId::ShellInputWaitHintRawInteractiveBody => {
            "An interactive program is waiting for keyboard input."
        }
        MessageId::ShellInputWaitHintStdinWaitBody => {
            "The command is waiting for keyboard input/confirmation."
        }
        MessageId::ShellInputWaitHintGuidanceBody => {
            "Type a reply directly, or press Ctrl+C to interrupt the command."
        }
        MessageId::ShellInputWaitHintTimeoutForecastBody => {
            "Auto-interrupts after {seconds}s of unanswered input-wait."
        }
        MessageId::ApprovalReceiptKindToolRequest => "tool request",
        MessageId::ApprovalReceiptKindShellCommandRequest => "shell command request",
        MessageId::ApprovalReceiptKindBashTool => "Bash tool",
        MessageId::ApprovalReceiptDecisionPending => "pending",
        MessageId::ApprovalReceiptDecisionApproved => "approved",
        MessageId::ApprovalReceiptDecisionSentToShell => "sent to shell",
        MessageId::ApprovalReceiptDecisionProviderNativeAllowed => {
            "allowed provider-native execution"
        }
        MessageId::ApprovalReceiptDecisionApprovedDisplayOnly => "approved for display only",
        MessageId::ApprovalReceiptDecisionDenied => "denied",
        MessageId::ApprovalReceiptDecisionCancelled => "cancelled by user",
        MessageId::ApprovalReceiptDecisionBlocked => "blocked by cosh-shell",
        MessageId::ApprovalReceiptSubjectBashSentToShell => "Bash tool: sent to shell",
        MessageId::ApprovalReceiptSubjectBashProviderNative => {
            "Bash tool: provider-native execution"
        }
        MessageId::ApprovalReceiptBashSentToShellMessage => "Bash tool sent to shell",
        MessageId::ApprovalReceiptForegroundInteractiveHint => {
            "This command will run interactively in the foreground; keyboard input goes directly to it. Press q to leave a pager."
        }
        MessageId::ApprovalReceiptProviderNativeAllowedMessage => {
            "Provider-native shell tool allowed"
        }
        MessageId::ApprovalHookHeading => "Hook review",
        MessageId::ApprovalRiskDetailLabel => "Risk: ",
        MessageId::ApprovalRiskLevelHigh => "high risk",
        MessageId::ApprovalRiskLevelMedium => "medium risk",
        MessageId::ApprovalRiskLevelLow => "low risk",
        MessageId::ApprovalQueueMetaSuffix => " · queue {position}/{total}",
        MessageId::ApprovalRiskPhrasePrivilegeEscalation => "privilege escalation",
        MessageId::ApprovalRiskPhraseCredentialAccess => "credential access",
        MessageId::ApprovalRiskPhraseFilesystemDelete => "file deletion",
        MessageId::ApprovalRiskPhraseFilesystemWrite => "filesystem write",
        MessageId::ApprovalRiskPhrasePermissionChange => "permission change",
        MessageId::ApprovalRiskPhraseProcessControl => "process control",
        MessageId::ApprovalRiskPhraseSystemControl => "system reboot/halt",
        MessageId::ApprovalIrrecoverableWarningLine => {
            "irrecoverable: reboots/halts this machine; SSH sessions drop and unsaved work is lost"
        }
        MessageId::ApprovalRiskPhraseServiceControl => "service control",
        MessageId::ApprovalRiskPhraseServiceOrContainerControl => "service/container control",
        MessageId::ApprovalRiskPhrasePackageManagerMutation => "package mutation",
        MessageId::ApprovalRiskPhraseInteractiveEditor => "editor may modify files",
        MessageId::ApprovalRiskPhraseRemoteCodeExecution => "remote code execution",
        MessageId::ApprovalRiskPhraseSensitivePath => "sensitive path",
        MessageId::ApprovalRiskPhraseSensitiveSearch => "sensitive data search",
        MessageId::ApprovalRiskPhraseCommandSubstitution => "command substitution",
        MessageId::ApprovalRiskPhraseRedirectionWrite => "write redirection",
        MessageId::ApprovalRiskPhraseAwkShellExecution => "awk shell execution",
        MessageId::ApprovalRiskLevelUnknown => "unknown risk",
        MessageId::ApprovalTurnExtensionSubject => "Agent turn budget",
        MessageId::ApprovalTurnExtensionPreview => {
            "The Agent used all {turns} configured turns. Continue the same task with {turns} more?"
        }
        MessageId::ApprovalTurnExtensionLabel => "Turn budget",
        MessageId::ApprovalActionContinue => "Continue",
        MessageId::ApprovalActionStop => "Stop",
        MessageId::ApprovalResolutionContinuingTitle => "Continuing",
        MessageId::ApprovalResolutionStoppedTitle => "Stopped",
        MessageId::ApprovalReceiptKindTurnExtension => "turn budget extension",
        MessageId::ApprovalTurnExtensionUnavailableTitle => "Cannot continue",
        MessageId::ApprovalTurnExtensionUnavailableBody => {
            "The persisted provider session changed before the extension was approved."
        }
        MessageId::ApprovalTrustUnknownToolReason => {
            "Outside the trusted tool catalog; explicit approval is required in Trust mode"
        }
        _ => return None,
    })
}
