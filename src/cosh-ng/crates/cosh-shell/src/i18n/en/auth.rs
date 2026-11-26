use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    match id {
        MessageId::AuthSelectProviderQuestion => {
            Some("\u{1f511} Authentication Required \u{2014} Select your AI provider:")
        }
        MessageId::AuthProviderManagementQuestion => {
            Some("\u{1f511} Provider Management \u{2014} Select your AI provider:")
        }
        MessageId::GuideAiManagementKeys => {
            Some("Enter open  Esc back  ↑↓\nF2/h home  Ctrl-D AI  q quit")
        }
        MessageId::GuideAuthDeleteKeys => {
            Some("↑↓ select  Enter confirm  Esc back  Ctrl-D finish and return to the guide")
        }
        MessageId::GuideAuthFieldKeys => {
            Some("Type  Enter submit  Esc back\nF2 home  Ctrl-D AI service")
        }
        MessageId::GuideAuthPending => Some("Checking the result…"),
        MessageId::GuideAuthTemplateTitle => Some("Choose an AI service to add"),
        MessageId::GuideAuthDiscardTitle => Some("Discard unsaved changes?"),
        MessageId::GuideAuthDiscardBody => Some("This form has unsaved changes. Discard them and return to {destination}?"),
        MessageId::GuideAuthDiscardKeys => Some("Enter {action}  Esc back\n↑↓ select"),
        MessageId::GuideAuthDiscardAction => Some("Discard changes"),
        MessageId::GuideAuthContinueEditing => Some("Continue editing"),
        MessageId::GuideAuthContinueKey => Some("keep editing"),
        MessageId::GuideAuthDestinationHome => Some("the guide home"),
        MessageId::GuideAuthDestinationOverview => Some("the AI service overview"),
        MessageId::GuideAuthDestinationService => Some("the selected service's actions"),
        MessageId::GuideAuthDestinationManagement => Some("AI service management"),
        MessageId::GuideAuthDestinationTemplates => Some("the service template list"),
        MessageId::GuideAuthActivateTitle => Some("Switch AI service"),
        MessageId::GuideAuthActivateBody => Some("Set {service} as the active AI service? Cosh checks the result before reporting the switch."),
        MessageId::GuideAuthActivateAction => Some("Switch service"),
        MessageId::GuideAuthDeleteTitle => Some("Delete AI service"),
        MessageId::GuideAuthDeleteBody => Some("Delete {service}? This removes the saved service and its credentials from Cosh. It cannot be undone and does not remove provider-side records."),
        MessageId::GuideAuthDeleteAction => Some("Delete service"),
        MessageId::GuideAuthEnterField => Some("{service} — Enter {field}"),
        MessageId::GuideAuthEditField => Some("{service} — Edit {field}"),
        MessageId::GuideAuthFieldHint => Some("Hint: {value}"),
        MessageId::GuideAuthFieldError => Some("Error: {value}"),
        MessageId::GuideAuthKeepField => Some("Press Enter to keep the value."),
        MessageId::GuideAuthBaseUrl => Some("Base URL"),
        MessageId::GuideAuthApiKey => Some("API key"),
        MessageId::GuideAuthModel => Some("Model"),
        MessageId::GuideAuthBaseUrlHint => Some("Enter the service's OpenAI-compatible API base URL, including any required /v1 path."),
        MessageId::GuideAuthApiKeyHint => Some("Enter this service's API key. The key is masked on screen."),
        MessageId::GuideAuthModelHint => Some("Enter the model ID exactly as your service specifies it."),
        MessageId::GuideAuthValidationChecks => Some("Checking the endpoint, credentials, and model before saving."),
        MessageId::GuideAuthValidationUsage => Some("If a read-only check cannot verify access, Cosh may send a minimal chat request to the chosen service. It can use a small amount of quota or incur a charge."),
        MessageId::GuideWizardLoginPrivilegeDenied => Some(
            "Administrator authorization did not complete; the login shell was not changed and nothing was written. Try again, or run the login command in a terminal to see sudo's full output.",
        ),
        MessageId::AuthEcsChecking => Some("Checking ECS RAM Role..."),
        MessageId::AuthEcsWaiting => Some(
            "Waiting for ECS RAM Role authorization. Configuration will continue automatically.",
        ),
        MessageId::AuthEcsRefreshing => Some(
            "Waiting for ECS credentials to refresh. Configuration will continue automatically.",
        ),
        MessageId::AuthEcsRetry => Some("Check again"),
        MessageId::AuthEcsCancelling => Some("Stopping ECS check and releasing resources..."),
        MessageId::AuthEcsCleanupFailed => Some(
            "ECS cleanup has not completed. New checks are disabled until resources are released.",
        ),
        MessageId::AuthEcsTimedOut => {
            Some("ECS credential wait timed out. Automatic checks have stopped.")
        }
        MessageId::AuthEcsFailed => Some("ECS authentication check failed."),
        MessageId::AuthEcsSaving => {
            Some("Validating and saving ECS configuration. This submission cannot be cancelled.")
        }
        MessageId::AuthEcsUnknown => {
            Some("Save result is unknown. Check provider management before submitting again.")
        }
        MessageId::AuthEcsReturn => Some("Return to provider management"),
        MessageId::AuthEcsCancelHint => Some("Press Esc or Ctrl+C to cancel."),
        _ => None,
    }
}
