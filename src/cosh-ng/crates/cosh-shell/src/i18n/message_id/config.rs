macro_rules! config_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            ConfigInvalidLanguageBody,
            ConfigSupportedLanguagesFooter,
            ConfigUnknownKeyBody,
            ConfigHomeMissingBody,
            ConfigHomeMissingFooter,
            ConfigUnchangedTitle,
            ConfigNoFileChangedBody,
            ConfigSavedTitle,
            ConfigSavedValueLine,
            ConfigCurrentSessionLanguageLine,
            ConfigSavedFooter,
            ConfigSaveFailedTitle,
            ConfigSaveFailedBody,
            ConfigSavePromptTitle,
            ConfigFileLine,
            ConfigPendingChangeLine,
            ConfigSaveButton,
            ConfigCancelButton,
            ConfigApplyKeysFooter,
            ConfigLanguageTitle,
            ConfigLanguageAutoLine,
            ConfigLanguageEnLine,
            ConfigLanguageZhLine,
            ConfigLanguageKeysFooter,
        );
    };
}

// #1913 additions live in a trailing segment so the existing MessageId
// discriminants (a registered stable runtime interface) never shift.
macro_rules! capture_notice_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            CaptureInputRejectedTitle,
            CaptureInputRejectedBody,
        );
    };
}

macro_rules! guide_command_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            GuideUsage,
            GuidePreferenceSaved,
            GuidePreferenceDefault,
            GuideSaved,
            GuideDisabledSaved,
            GuideStateOn,
            GuideStateOff,
            GuideAvailableConditional,
            GuideUnconfirmed,
            GuideRuntimeUnconfirmed,
            GuideClosed,
            GuideIndependentHint,
            GuideReadFailed,
            GuideSaveFailed,
            GuideRuntimeSaveFailed,
            GuideRuntimePublishFailed,
            GuideRuntimePublishPartial,
            GuideRuntimeUnavailable,
            GuideErrorCorrupt,
            GuideErrorVersion,
            GuideErrorBusy,
            GuideErrorUnsafe,
            GuideErrorTooLarge,
            GuideErrorIo,
        );
    };
}

macro_rules! login_command_ids {
    ($next:ident, $remaining:tt, $($ids:ident,)*) => {
        $next!(
            $remaining,
            $($ids,)*
            LoginUser,
            LoginCurrentShell,
            LoginOriginalBash,
            LoginTransaction,
            LoginManaged,
            LoginNoManagedRecord,
            LoginManagedYes,
            LoginManagedNo,
            LoginTransactionUnmanaged,
            LoginTransactionPrepared,
            LoginTransactionApplied,
            LoginTransactionRestored,
            LoginTransactionConflict,
            LoginNewLoginsOnly,
            LoginChangePreview,
            LoginConfirm,
            LoginShellChanged,
            LoginChangeComplete,
            LoginAuthorizationRequired,
            LoginCommandFailed,
            LoginUsage,
            LoginIdentityMismatch,
            LoginStateConflict,
            LoginRecoveryMissing,
        );
    };
}
