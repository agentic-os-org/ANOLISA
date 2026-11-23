//! Startup authentication exchange for the headless runtime.

use std::io::Write;

use tokio::io::AsyncBufReadExt;

use crate::auth::request_validated_auth;
use crate::config::CoreConfig;
use crate::protocol::{AuthReason, OutputMessage};

pub(super) fn defer_auth(args: &crate::cli::CliArgs) -> bool {
    args.headless
        && args.cosh_shell_transport
        && args.prompt.is_none()
        && !args.compact
        && !args.registry
        && !args.session_control
        && !args.execution_profile.is_brokered()
}

pub(crate) fn unconfigured_provider() -> Box<dyn crate::provider::ContentGenerator> {
    unconfigured_provider_for("ModelNotConfigured")
}

pub(crate) fn unconfigured_provider_for(
    code: &'static str,
) -> Box<dyn crate::provider::ContentGenerator> {
    Box::new(UnconfiguredProvider { code })
}

struct UnconfiguredProvider {
    code: &'static str,
}

#[async_trait::async_trait]
impl crate::provider::ContentGenerator for UnconfiguredProvider {
    async fn generate(
        &self,
        _: &[crate::provider::Message],
        _: &[crate::provider::ToolDeclaration],
        _: &crate::provider::GenerateConfig,
    ) -> Result<crate::provider::GenerateStream, String> {
        Err(self.code.to_string())
    }

    fn cancel(&self) {}
}

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
