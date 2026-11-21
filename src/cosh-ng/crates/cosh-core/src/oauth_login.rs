//! Persistent OAuth login subprocess.
//!
//! The registry core is short-lived, so the browser login runs in this
//! dedicated process that stays alive until the flow completes. Stdout carries
//! only safe JSON events; closing stdin cancels the attempt.

use std::io::Write;
use std::sync::Arc;

use serde_json::json;
use tokio::sync::watch;

use crate::config::CoreConfig;
use crate::provider::sysom::oauth::client::OAuthClient;
use crate::provider::sysom::oauth::login::{run_login, LoginParams, SystemBrowserOpener};

fn emit(value: serde_json::Value) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

pub async fn run(config: CoreConfig) {
    let client = Arc::new(OAuthClient::new(crate::auth::oauth::sysom_base()));
    let store = crate::auth::oauth::store();
    let opener = Arc::new(SystemBrowserOpener);
    let params = LoginParams {
        provider_snapshot: config.ai.active_provider.clone(),
        ..Default::default()
    };
    let (cancel_tx, cancel_rx) = watch::channel(false);
    std::thread::spawn(move || {
        use std::io::Read;
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 64];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => {
                    let _ = cancel_tx.send(true);
                    break;
                }
                Ok(_) => {}
            }
        }
    });
    emit(json!({"phase": "running"}));
    let event = match run_login(client, store, opener, params, cancel_rx).await {
        Ok(outcome) => json!({
            "phase": "done",
            "generation": outcome.generation,
            "client_id": outcome.client_id,
        }),
        Err(error) => json!({"phase": "failed", "code": error.code()}),
    };
    emit(event);
}
