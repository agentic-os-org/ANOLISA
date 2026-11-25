//! Registry OAuth logout and provider-switch supersede.
//!
//! The browser login itself runs in the dedicated `cosh-core --oauth-login`
//! subprocess (the registry core is short-lived); the registry only commits
//! logout and reports safe results, never proofs or credentials.

use std::sync::Arc;

use serde_json::{json, Value};

use super::super::OutputMessage;
use crate::config::CoreConfig;
use crate::provider::sysom::oauth::client::OAuthClient;
use crate::provider::sysom::oauth::logout::run_logout;

fn response(request_id: &str, data: Value) -> OutputMessage {
    OutputMessage::RegistryResponse {
        request_id: request_id.to_string(),
        success: true,
        data: Some(data),
        error: None,
    }
}

pub(super) async fn logout(request_id: &str, config: &CoreConfig) -> OutputMessage {
    let expected = match crate::auth::oauth::effective(config) {
        crate::auth::oauth::SourceState::Active { generation, .. } => Some(generation),
        _ => None,
    };
    let client = Arc::new(OAuthClient::new(crate::auth::oauth::sysom_base()));
    let store = crate::auth::oauth::store();
    match run_logout(client, store, None, expected).await {
        Ok(report) => response(
            request_id,
            json!({
                "removed_generation": report.removed.as_ref().map(|removed| removed.generation),
                "remote_revoked": report.remote_revoked,
                "remote_error_code": report.remote_error.map(|error| error.code()),
                "superseded": report.superseded,
            }),
        ),
        Err(error) => OutputMessage::RegistryResponse {
            request_id: request_id.to_string(),
            success: false,
            data: Some(json!({"error_code": error.code()})),
            error: Some(error.code().to_string()),
        },
    }
}

/// Explicit provider selection supersedes OAuth: commit local logout and
/// attempt remote revoke in the background, fenced by the observed generation.
/// Reads the store directly because the fresh snapshot already mismatches.
pub(super) fn on_explicit_provider_select(_config: &CoreConfig) {
    let store = crate::auth::oauth::store();
    let Ok(state) = store.read_state() else {
        return;
    };
    let Some(login) = state.active else {
        return;
    };
    if !state.selected {
        return;
    }
    let generation = login.generation;
    let client = Arc::new(OAuthClient::new(crate::auth::oauth::sysom_base()));
    tokio::spawn(async move {
        let _ = run_logout(client, store, None, Some(generation)).await;
    });
}
