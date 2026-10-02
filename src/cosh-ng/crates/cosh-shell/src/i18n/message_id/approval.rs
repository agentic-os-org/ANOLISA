macro_rules! approval_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            ApprovalTitle,
            ApprovalRequiredTitle,
            ApprovalResolutionApprovedTitle,
            ApprovalResolutionAutoApprovedTitle,
            ApprovalResolutionTrustedTitle,
            ApprovalResolutionDeniedTitle,
            ApprovalResolutionCancelledTitle,
            ApprovalResolutionBlockedTitle,
            ApprovalResolutionDeferredTitle,
            ApprovalActionAllowOnce,
            ApprovalActionAlwaysTrust,
            ApprovalActionDeny,
            ApprovalActionDetails,
            ApprovalToolInputLabel,
            ApprovalCommandLabel,
            ApprovalDetailsTitle,
            ApprovalDetailsSourceLabel,
            ApprovalDetailsRunLabel,
            ApprovalDetailsExecutionLabel,
            ApprovalDetailsCommandBlockLabel,
            ApprovalDetailsRedactionLabel,
            ApprovalDetailsProviderRequestLabel,
            ApprovalDetailsToolUseLabel,
            ApprovalDetailsDefaultDenyLine,
            ApprovalDetailsRequestLabel,
            ApprovalDetailsInputLabel,
            ApprovalDetailsBashCommandSubject,
            ApprovalDetailsShellCommandSubject,
            ApprovalDetailsToolSubject,
            ApprovalDetailsPendingValue,
            ApprovalDetailsNoneValue,
            ApprovalDetailsNotApplicableValue,
            ApprovalAssessmentSummaryLine,
            ApprovalAssessmentReasonLine,
            ApprovalJournalTitle,
            ApprovalJournalDecisionCount,
            ApprovalJournalEmptyBody,
            ApprovalJournalActorLabel,
            ApprovalJournalPreviewHashLabel,
            ApprovalJournalSubjectLabel,
            ApprovalJournalPreviewLabel,
            ApprovalRiskSuffix,
            ApprovalQueueCompactLine,
            ApprovalQueueFullLine,
            ApprovalQueueNextSuffix,
            ApprovalSubjectLabel,
            ApprovalNextLabel,
            ApprovalKeysPrefix,
            ApprovalKeysText,
            ApprovalExecutableToolPolicy,
            ApprovalExecutableToolPolicyExtra,
            ApprovalCommandDefaultPolicy,
            ApprovalRunShellCommandPrompt,
            ApprovalRunBashCommandPrompt,
            ApprovalNotFoundTitle,
            ApprovalNotFoundBody,
            ApprovalShellHandoffNotFoundTitle,
            ApprovalShellHandoffNotFoundBody,
            ApprovalShellHandoffBlockedTitle,
            ApprovalShellHandoffBlockedFooter,
            ApprovalShellHandoffValidationEmptyCommand,
            ApprovalShellHandoffValidationMultilineCommand,
            ApprovalShellHandoffValidationControlCharacter,
            ApprovalShellHandoffValidationEmptyPreview,
            ApprovalShellHandoffValidationEmptyApprovalId,
            ApprovalShellHandoffValidationEmptyRunId,
            ApprovalShellHandoffSendingTitle,
            ApprovalShellHandoffSendingBody,
            ApprovalShellHandoffTimeoutTitle,
            ApprovalShellHandoffTimeoutExceededBody,
            ApprovalShellHandoffTimeoutInterruptBody,
            ApprovalReceiptKindToolRequest,
            ApprovalReceiptKindShellCommandRequest,
            ApprovalReceiptKindBashTool,
            ApprovalReceiptDecisionPending,
            ApprovalReceiptDecisionApproved,
            ApprovalReceiptDecisionSentToShell,
            ApprovalReceiptDecisionProviderNativeAllowed,
            ApprovalReceiptDecisionApprovedDisplayOnly,
            ApprovalReceiptDecisionDenied,
            ApprovalReceiptDecisionCancelled,
            ApprovalReceiptDecisionBlocked,
            ApprovalReceiptSubjectBashSentToShell,
            ApprovalReceiptSubjectBashProviderNative,
            ApprovalReceiptBashSentToShellMessage,
            ApprovalReceiptProviderNativeAllowedMessage,
            ApprovalHookHeading,
        );
    };
}

macro_rules! approval_reason_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            ApprovalRiskDetailLabel,
            ApprovalRiskLevelHigh,
            ApprovalRiskLevelMedium,
            ApprovalRiskLevelLow,
            ApprovalQueueMetaSuffix,
            ApprovalRiskPhrasePrivilegeEscalation,
            ApprovalRiskPhraseCredentialAccess,
            ApprovalRiskPhraseFilesystemDelete,
            ApprovalRiskPhraseFilesystemWrite,
            ApprovalRiskPhrasePermissionChange,
            ApprovalRiskPhraseProcessControl,
            ApprovalRiskPhraseServiceControl,
            ApprovalRiskPhraseServiceOrContainerControl,
            ApprovalRiskPhrasePackageManagerMutation,
            ApprovalRiskPhraseInteractiveEditor,
            ApprovalRiskPhraseRemoteCodeExecution,
            ApprovalRiskPhraseSensitivePath,
            ApprovalRiskPhraseSensitiveSearch,
            ApprovalRiskPhraseCommandSubstitution,
            ApprovalRiskPhraseRedirectionWrite,
            ApprovalRiskPhraseAwkShellExecution,
            ApprovalRiskLevelUnknown,
        );
    };
}

// Trailing segment (issue #1773): appended after all existing segments so
// every pre-existing MessageId discriminant stays stable, per the
// stable-runtime-api trailing-segment contract established in #1721.
macro_rules! approval_turn_consent_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            ApprovalResolutionTurnApprovedTitle,
            ApprovalActionApproveTurn,
        );
    };
}

// Trailing segment (issue #1988): appended after all existing segments so
// every pre-existing MessageId discriminant stays stable, per the
// stable-runtime-api trailing-segment contract established in #1721.
macro_rules! approval_foreground_interactive_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            ApprovalReceiptForegroundInteractiveHint,
        );
    };
}

// Trailing segment (issue #2029): appended after all existing segments so
// every pre-existing MessageId discriminant stays stable.
macro_rules! approval_turn_extension_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            ApprovalTurnExtensionSubject,
            ApprovalTurnExtensionPreview,
            ApprovalTurnExtensionLabel,
            ApprovalActionContinue,
            ApprovalActionStop,
            ApprovalResolutionContinuingTitle,
            ApprovalResolutionStoppedTitle,
            ApprovalReceiptKindTurnExtension,
            ApprovalTurnExtensionUnavailableTitle,
            ApprovalTurnExtensionUnavailableBody,
        );
    };
}

// #2064 additions live in a trailing segment so the existing MessageId
// discriminants (a registered stable runtime interface) never shift.
macro_rules! approval_system_control_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            ApprovalRiskPhraseSystemControl,
            ApprovalIrrecoverableWarningLine,
        );
    };
}

// #2025/#2161 input-wait hint card + timeout notice. Appended as a
// trailing segment so every pre-existing discriminant stays stable
// (the fieldless enum is a registered runtime interface).
macro_rules! input_wait_hint_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            ApprovalShellHandoffInputWaitTimeoutTitle,
            ApprovalShellHandoffInputWaitTimeoutExceededBody,
            ApprovalShellHandoffInputWaitTimeoutInterruptBody,
            ShellInputWaitHintTitle,
            ShellInputWaitHintPasswordBody,
            ShellInputWaitHintPagerBody,
            ShellInputWaitHintRawInteractiveBody,
            ShellInputWaitHintStdinWaitBody,
            ShellInputWaitHintGuidanceBody,
            ShellInputWaitHintTimeoutForecastBody,
        );
    };
}

// Trust-catalog feedback is appended after the Agent Composer segment so
// every pre-existing MessageId discriminant remains stable.
macro_rules! approval_trust_catalog_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            ApprovalTrustUnknownToolReason,
        );
    };
}
