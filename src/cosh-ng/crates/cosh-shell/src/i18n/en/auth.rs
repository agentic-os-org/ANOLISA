use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    match id {
        MessageId::AuthSelectProviderQuestion => {
            Some("\u{1f511} Authentication Required \u{2014} Select your AI provider:")
        }
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
