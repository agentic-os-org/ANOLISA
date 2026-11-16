use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::ConfigInvalidLanguageBody => "Invalid language: {language}",
        MessageId::ConfigSupportedLanguagesFooter => "Supported: auto, en-US, zh-CN.",
        MessageId::ConfigUnknownKeyBody => "Unknown config key: {key}",
        MessageId::ConfigHomeMissingBody => "HOME is not set; cannot persist config.",
        MessageId::ConfigHomeMissingFooter => "Set HOME or edit config manually.",
        MessageId::ConfigUnchangedTitle => "Config unchanged",
        MessageId::ConfigNoFileChangedBody => "No config file was changed.",
        MessageId::ConfigSavedTitle => "Config saved",
        MessageId::ConfigSavedValueLine => "Saved ui.{setting} = \"{value}\".",
        MessageId::ConfigCurrentSessionLanguageLine => "Current session language: {language}.",
        MessageId::ConfigSavedFooter => {
            "Saved setting takes effect immediately; agent replies follow your message language."
        }
        MessageId::ConfigSaveFailedTitle => "Config save failed",
        MessageId::ConfigSaveFailedBody => "Config save failed: {error}",
        MessageId::ConfigSavePromptTitle => "Save config?",
        MessageId::ConfigFileLine => "file: {path}",
        MessageId::ConfigPendingChangeLine => "ui.{setting}: {before} -> {after}",
        MessageId::ConfigSaveButton => "Save",
        MessageId::ConfigCancelButton => "Cancel",
        MessageId::ConfigApplyKeysFooter => "Keys: Left/Right select | Enter apply | Esc cancel",
        MessageId::ConfigLanguageTitle => "Language",
        MessageId::ConfigLanguageAutoLine => "auto    Follow LC_ALL/LC_MESSAGES/LANG",
        MessageId::ConfigLanguageEnLine => "en-US   English",
        MessageId::ConfigLanguageZhLine => "zh-CN   Simplified Chinese",
        MessageId::ConfigLanguageKeysFooter => {
            "Keys: Left/Right select | Enter choose | Esc cancel"
        }
        MessageId::CaptureInputRejectedTitle => "Input not delivered",
        MessageId::CaptureInputRejectedBody => {
            "Input typed while a card action was processing could not be \
             delivered safely; please retype it."
        }
        MessageId::GuideUsage => concat!(
            "Cosh setup and getting started\n",
            "\n",
            "Usage:\n",
            "  cosh guide                 Open the setup wizard in a normal terminal\n",
            "  cosh guide --plain         Print the guide as text, no interaction\n",
            "  cosh guide --help          Show this help\n",
            "  cosh guide on              Enable usage tips\n",
            "  cosh guide off             Disable usage tips\n",
            "  cosh guide status          Show the usage tips preference\n",
        ),
        MessageId::GuidePreferenceSaved => "AI tips: {state} (saved).",
        MessageId::GuidePreferenceDefault => "AI tips: on (default).",
        MessageId::GuideSaved => "AI tips set to {state} and saved.",
        MessageId::GuideDisabledSaved => "AI tips are disabled and saved.",
        MessageId::GuideStateOn => "on",
        MessageId::GuideStateOff => "off",
        MessageId::GuideAvailableConditional => "Guidance appears only in supported shells.",
        MessageId::GuideUnconfirmed => "Automatic guidance in this shell is unconfirmed.",
        MessageId::GuideRuntimeUnconfirmed => "Current guidance availability is unconfirmed: {error}.",
        MessageId::GuideClosed => "Current automatic guidance is disabled.",
        MessageId::GuideIndependentHint => "AI tips cover startup, failure help and login offers. Direct AI use stays available. Hide login summary: cosh health banner off.",
        MessageId::GuideReadFailed => "Cannot read guidance settings: {error}. Check the state and retry.",
        MessageId::GuideSaveFailed => "Saving guidance settings failed: {error}. Retry the command.",
        MessageId::GuideRuntimeSaveFailed => "Runtime guidance was set to {state}, but saving the preference failed: {error}. Retry the command.",
        MessageId::GuideRuntimePublishFailed => "Updating runtime guidance failed; the preference was not saved: {error}. Retry the command.",
        MessageId::GuideRuntimePublishPartial => "The runtime disable marker is active, but publication failed; the preference was not saved: {error}. Retry the command.",
        MessageId::GuideRuntimeUnavailable => "no verified local runtime directory",
        MessageId::GuideErrorCorrupt => "saved state is corrupt",
        MessageId::GuideErrorVersion => "unsupported state version",
        MessageId::GuideErrorBusy => "guidance state is busy",
        MessageId::GuideErrorUnsafe => "the state path is not safely private or owned",
        MessageId::GuideErrorTooLarge => "guidance state exceeds its size or count limit",
        MessageId::GuideErrorIo => "storage I/O failed ({error})",
        MessageId::LoginUser => "User:",
        MessageId::LoginCurrentShell => "Current login shell:",
        MessageId::LoginOriginalBash => "Original Bash:",
        MessageId::LoginTransaction => "Transaction:",
        MessageId::LoginManaged => "Managed:",
        MessageId::LoginNoManagedRecord => "no managed record",
        MessageId::LoginManagedYes => "yes",
        MessageId::LoginManagedNo => "no",
        MessageId::LoginTransactionUnmanaged => "unmanaged",
        MessageId::LoginTransactionPrepared => "prepared",
        MessageId::LoginTransactionApplied => "applied",
        MessageId::LoginTransactionRestored => "restored",
        MessageId::LoginTransactionConflict => "conflict",
        MessageId::LoginNewLoginsOnly => "Changes affect new logins only.",
        MessageId::LoginChangePreview => "User: {user} (UID {uid}): {before} -> {after}. New logins only.",
        MessageId::LoginConfirm => "Continue? [y/N] ",
        MessageId::LoginShellChanged => "Login shell: {shell}. New logins only.",
        MessageId::LoginChangeComplete => "Login shell operation completed. New logins only.",
        MessageId::LoginAuthorizationRequired => "Authorization unavailable or failed; ask an administrator to run: {command}",
        MessageId::LoginCommandFailed => "cosh login: {error}",
        MessageId::LoginUsage => "usage: cosh login status|set|restore [--user USER] [--yes]",
        MessageId::LoginIdentityMismatch => "Privileged account identity does not match the confirmed UID.",
        MessageId::LoginStateConflict => "Account shell or login record conflicts; administrator review is required.",
        MessageId::LoginRecoveryMissing => "No trusted login recovery record; no account change made.",
        MessageId::GuideWizardTitle => "Cosh setup and getting started",
        MessageId::GuideWizardStart => concat!(
            "## Enter the AI Shell\n",
            "\n",
            "- Run `cosh` in your terminal, then use commands such as `df -h`.\n",
            "- `cosh --ask 'one question'` starts an interactive session.\n",
            "- In a session, `/agent` opens the question editor.\n",
            "- The first AI use follows the existing sign-in flow.\n",
            "- `/help` lists session commands; `cosh --help` lists startup options.\n",
            "- `exit` leaves Cosh and returns to the Bash that started it.\n",
            "- As a login shell, `exit` may end the login session.\n",
        ),
        MessageId::GuideWizardEntryLanguage => "Language",
        MessageId::GuideWizardEntryAi => "AI service",
        MessageId::GuideWizardEntryTips => "Tips and recommendations",
        MessageId::GuideWizardEntryHealth => "Login health summary",
        MessageId::GuideWizardEntryLogin => "Default login shell",
        MessageId::GuideWizardEntryStart => "Getting started",
        MessageId::GuideWizardStartSummary => "Commands, questions, and exit",
        MessageId::GuideWizardLanguageTitle => "Language / 语言",
        MessageId::GuideWizardGuideTipLabel => "Usage tips",
        MessageId::GuideWizardLanguageBody => {
            "Interface language. In a session, `/config language` does the same."
        }
        MessageId::GuideWizardAiBody => {
            "Configure and manage AI services. In a session, run `/auth`."
        }
        MessageId::GuideWizardTipsBody => concat!(
            "Usage tips, command analysis mode, and question recommendations.\n",
            "\n",
            "- `cosh guide off` hides usage tips. Manual Cosh use stays available.\n",
            "- `cosh guide on` enables them; `cosh guide status` shows the preference.\n",
            "- `/mode analysis` changes the analysis mode for this session.\n",
            "- `/recommendations` manages recommendations and local data.\n",
        ),
        MessageId::GuideWizardHealthBody => concat!(
            "Login health summary, covering health and resource state.\n",
            "\n",
            "- `cosh health banner off` hides the login summary. AI tips stay unchanged.\n",
            "- `cosh health banner on` enables it; `cosh health banner status` shows it.\n",
        ),
        MessageId::GuideWizardLoginBody => concat!(
            "Default login shell, on systems supporting login switching.\n",
            "\n",
            "- `cosh login status` shows the login shell.\n",
            "- `cosh login set` uses Cosh for new logins (existing sessions unchanged).\n",
            "- `cosh login restore` restores the recorded Bash for new logins.\n",
            "- `cosh login set --user alice` manages another user (administrator only).\n",
            "- `restore` and `status` also accept `--user alice`.\n",
        ),
        MessageId::GuideWizardPlainFooter => {
            "Run `cosh guide` in a normal terminal to change settings interactively."
        }
        MessageId::GuideWizardHomeHint => "↑↓ select  Enter open  Esc quit",
        MessageId::GuideWizardChoiceHint => "↑↓ select  Enter save  Esc back",
        MessageId::GuideWizardScrollHint => "↑↓ scroll  PgUp/PgDn page  Esc back",
        MessageId::GuideWizardSavedLine => "Saved: {value}",
        MessageId::GuideWizardSavedNextLogin => "Saved. Applies to the next login.",
        MessageId::GuideWizardSaveFailed => "Save failed: {error}",
        MessageId::GuideWizardReadFailed => "Unable to read",
        MessageId::GuideWizardLanguageAuto => "Follow system",
        MessageId::GuideWizardLanguageEn => "English",
        MessageId::GuideWizardLanguageZh => "简体中文",
        MessageId::GuideWizardLanguageEffectiveLine => "Effective: {value}",
        MessageId::GuideWizardLanguageSourceLine => "Source: {value}",
        MessageId::GuideWizardSourceConfig => "user config",
        MessageId::GuideWizardSourceEnv => "environment",
        MessageId::GuideWizardSourceDefault => "default",
        MessageId::GuideWizardLanguageEnvNote => {
            "COSH_SHELL_LANG overrides the saved value; saving changes new sessions only."
        }
        MessageId::GuideWizardLanguageScopeNote => {
            "Interface language only; AI replies follow the language of your question."
        }
        MessageId::GuideWizardGuideTipBody => {
            "Shows Cosh usage tips; not an AI master switch. Manual cosh use stays available."
        }
        MessageId::GuideWizardHealthDetailBody => {
            "Shows health and resource state at login; affects later logins."
        }
        MessageId::GuideWizardAiReadonly => {
            "Run /auth in a Cosh session to manage AI services. Configured is not connected."
        }
        MessageId::GuideWizardLoginReadonly => {
            "Run cosh login set or cosh login restore in the terminal to change it."
        }
        MessageId::GuideWizardLoginFailed => "Unable to read the login shell: {error}",
        MessageId::GuideWizardTooSmall => "Terminal too small for the wizard.",
        MessageId::GuideWizardTooSmallHint => {
            "Enlarge the terminal, or run cosh guide --plain. Esc quits."
        }
        MessageId::GuideWizardCurrent => "current",
        MessageId::GuideWizardAiLoading => "Loading…",
        MessageId::GuideWizardAiUnconfigured => "Not configured",
        MessageId::GuideWizardAiServiceLine => "Current AI service: {value}",
        MessageId::GuideWizardAiSourceLine => "Config source: {value}",
        MessageId::GuideWizardAiSourceUser => "user config",
        MessageId::GuideWizardAiSourceSystem => "system config",
        MessageId::GuideWizardAiAutoUseLine => "Zero-config auto use: {value}",
        MessageId::GuideWizardAiAutoUseOn => "on",
        MessageId::GuideWizardAiAutoUseOff => "off",
        MessageId::GuideWizardAiManage => "Configure AI service (opens /auth)",
        MessageId::GuideWizardAiHandoff => {
            "Opening a Cosh session for authentication; type exit to return to the wizard."
        }
        MessageId::GuideWizardAiHandoffFailed => "Could not open a Cosh session: {error}",
        MessageId::GuideWizardTipsAnalysis => "Command analysis",
        MessageId::GuideWizardTipsPersonal => "Personalized prompt recommendations",
        MessageId::GuideWizardTipsData => "Recommendation data & privacy",
        MessageId::GuideWizardAnalysisSmart => "Suggest first",
        MessageId::GuideWizardAnalysisAuto => "Analyze automatically",
        MessageId::GuideWizardAnalysisManual => "Manual only",
        MessageId::GuideWizardEffectiveLine => "Effective: {value}",
        MessageId::GuideWizardSourceLine => "Source: {value}",
        MessageId::GuideWizardSourceUser => "user preference",
        MessageId::GuideWizardNewSessionNote => "Applies to new sessions after saving.",
        MessageId::GuideWizardOverriddenNote => "Saved, but overridden by an environment variable.",
        MessageId::GuideWizardForcedOffNote => {
            "COSH_RECOMMENDATIONS_ENABLED=0 forces recommendations off; an enable request is not saved."
        }
        MessageId::GuideWizardManualPausedNote => {
            "manual pauses recommendations in new sessions but keeps the on preference."
        }
        MessageId::GuideWizardDataImportLine => "Bash history import: {value}",
        MessageId::GuideWizardDataAllowImport => "Allow import and save",
        MessageId::GuideWizardDataStopImport => "Stop import and save",
        MessageId::GuideWizardDataSaveOnPref => "Save on preference",
        MessageId::GuideWizardDataSaveOffPref => "Save off preference",
        MessageId::GuideWizardDataEnvNote => "Effective state is still controlled by the environment.",
        MessageId::GuideWizardDataStopKeepsNote => "Stopping only affects future imports; existing data is kept.",
        MessageId::GuideWizardDataClear => "Clear data only",
        MessageId::GuideWizardDataReset => "Reset data and turn recommendations off",
        MessageId::GuideWizardDataDamaged => "Recommendation data cannot be read.",
        MessageId::GuideWizardDataPrivacy => {
            "## Privacy\n\n- Bash history is not imported by default; enabling needs separate consent. The first sync only reads the tail of history to build a baseline.\n- Activity is kept 7 days, recent profiles 14 days, long-term patterns and evidence snapshots 90 days, subject to capacity limits.\n- A small amount of recent activity may be sent to the current AI service; redaction is best-effort. Clearing locally does not clear provider records."
        }
        MessageId::GuideWizardDataCleared => "Local recommendation data cleared; the on/off setting was kept.",
        MessageId::GuideWizardDataResetDone => "Damaged data reset; recommendations are off.",
        MessageId::GuideWizardConfirm => "Confirm",
        MessageId::GuideWizardCancel => "Cancel",
        MessageId::GuideWizardLoginSet => "Set Cosh as login shell",
        MessageId::GuideWizardLoginRestore => "Restore original login shell",
        MessageId::GuideWizardLoginOther => "Other users (administrator only):",
        MessageId::GuideWizardLoginOtherCommand => "`cosh login set --user USER`",
        MessageId::GuideWizardLoginOtherTerminal => "Run it in a terminal.",
        MessageId::GuideWizardLoginVerified => "Verified: {value}",
        MessageId::GuideWizardLoginUnconfirmed => "Change unconfirmed; re-query before retrying.",
        MessageId::GuideWizardLoginHandoff => "Opening login management; return to the wizard when done.",
        MessageId::GuideWizardLoginSudoNote => "Requires sudo; the password is entered in the terminal, not the wizard.",
        MessageId::GuideWizardConfigUnreadable => "Unable to read the config file. Edit it manually, then reopen the wizard.",
        MessageId::GuideWizardDataClearConfirm => "Clear local recommendation data? The on/off setting will be kept.",
        MessageId::GuideWizardLoginHandoffFailed => "Login management handoff failed: {error}",
        MessageId::GuideWizardLoginConflictNote => {
            "Login state conflicts with the administrator record; no change is offered. Run `cosh login status` for details."
        }
        MessageId::GuideWizardLoginNoRecordNote => {
            "Cosh is the login shell but no recovery record exists; no change is offered. Run `cosh login status` in a terminal."
        }
        MessageId::GuideWizardLoginRootNote => "Running as root; no sudo password is needed.",
        MessageId::GuideWizardLoginCancelled => "Login change cancelled; nothing was verified.",
        MessageId::GuideWizardLoginUsageFailed => {
            "Login command did not run (usage error, exit 2); nothing was changed."
        }
        MessageId::GuideWizardLoginExitFailed => {
            "Login change did not complete (exit {code}); check sudo authorization or the password, then retry."
        }
        _ => return None,
    })
}
