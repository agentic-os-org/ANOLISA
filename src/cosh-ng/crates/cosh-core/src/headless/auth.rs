//! Startup authentication exchange for the headless runtime.

use std::io::Write;

use tokio::io::AsyncBufReadExt;

use crate::auth::request_validated_auth;
use crate::config::CoreConfig;
use crate::protocol::{AuthReason, OutputMessage};

/// Requests credentials until they validate or the shell cancels the exchange.
pub(super) async fn request_auth<W, R>(
    config: &mut CoreConfig,
    lines: &mut tokio::io::Lines<R>,
    writer: &mut W,
    buffered: &mut Vec<String>,
) -> Option<Box<dyn crate::provider::ContentGenerator>>
where
    W: Write,
    R: AsyncBufReadExt + Unpin,
{
    let validated = request_validated_auth(
        config,
        lines,
        writer,
        "auth-init",
        AuthReason::NotConfigured,
        None,
        buffered,
    )
    .await?;

    *config = validated.candidate;

    if let Ok(json) = serde_json::to_string(&OutputMessage::system_status("auth_ok")) {
        let _ = writeln!(writer, "{json}");
        let _ = writer.flush();
    }

    Some(crate::create_provider(config))
}
