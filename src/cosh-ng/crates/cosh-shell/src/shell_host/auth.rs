use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::input_intent::shell_intent_helpers;

static MARKER_TOKEN_COUNTER: AtomicU64 = AtomicU64::new(1);

pub(super) fn generate_marker_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let counter = MARKER_TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:x}{:x}{:x}", std::process::id(), nanos, counter)
}

pub(super) fn marker_script_with_token(
    script: &str,
    token: &str,
    recovery_request_file: &str,
    handoff_request_file: &str,
    assistance_state_file: &str,
    ai_enabled: bool,
) -> String {
    let ai_enabled = u8::from(ai_enabled);
    format!(
        "COSH_MARKER_TOKEN='{}'\nCOSH_RECOVERY_REQUEST_FILE='{}'\nCOSH_HANDOFF_REQUEST_FILE='{}'\nCOSH_ASSISTANCE_STATE_FILE='{}'\nreadonly _COSH_SESSION_AI_ENABLED='{}'\n{}\n{}",
        shell_single_quote_value(token),
        shell_single_quote_value(recovery_request_file),
        shell_single_quote_value(handoff_request_file),
        shell_single_quote_value(assistance_state_file),
        ai_enabled,
        shell_intent_helpers(),
        script
    )
}

fn shell_single_quote_value(value: &str) -> String {
    value.replace('\'', "'\\''")
}
