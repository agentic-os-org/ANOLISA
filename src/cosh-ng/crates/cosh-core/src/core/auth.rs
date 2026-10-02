//! Mid-session authentication retry and provider replacement.

use super::*;
use crate::auth::request_validated_auth;
use crate::protocol::AuthReason;

impl CoshCore {
    /// Attempts to re-authenticate and rebuild the active provider.
    pub(super) async fn try_reauth<W, R>(
        &mut self,
        reader: &mut tokio::io::Lines<R>,
        writer: &mut W,
    ) -> bool
    where
        W: Write,
        R: AsyncBufReadExt + Unpin,
    {
        let request_id = self.next_request_id();
        let mut discarded_lines = Vec::new();
        let validated = request_validated_auth(
            &mut self.config,
            reader,
            writer,
            &request_id,
            AuthReason::Invalid,
            Some("API authentication failed (401/403)".to_string()),
            &mut discarded_lines,
        )
        .await;
        let Some(validated) = validated else {
            return false;
        };

        self.config = validated.candidate;

        let resolved = self.config.resolve_provider();
        if resolved.provider_type == "aliyun" {
            if resolved.auth_source.as_deref() == Some("ecs_ram_role") {
                self.provider = Box::new(crate::provider::sysom::SysomProvider::from_ecs_ram_role(
                    &resolved.sysom_endpoint,
                ));
            } else if !resolved.access_key_id.is_empty() && !resolved.access_key_secret.is_empty() {
                self.provider = Box::new(crate::provider::sysom::SysomProvider::new(
                    &resolved.access_key_id,
                    &resolved.access_key_secret,
                    resolved.security_token.as_deref(),
                    &resolved.sysom_endpoint,
                ));
            } else {
                tracing::warn!("Aliyun auth response missing AK/SK");
                return false;
            }
        } else {
            let profile = crate::provider::profile::profile_from_name(&resolved.provider_type);
            self.provider = Box::new(crate::provider::openai_compat::OpenAICompatProvider::new(
                &resolved.base_url,
                &resolved.api_key,
                profile,
                resolved.explicit_cache,
            ));
        }

        self.emit(writer, &OutputMessage::system_status("auth_ok"));
        true
    }
}
