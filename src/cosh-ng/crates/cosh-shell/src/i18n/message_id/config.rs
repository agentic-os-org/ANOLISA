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
