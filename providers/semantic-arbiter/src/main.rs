//! One AW request per process; semantic recommendations never become AW effects.

mod config;
mod evaluate;
mod mapping;
mod storage;

use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::Duration;

use aw_provider::Protocol;
use semantic_arbiter::deadline::Ingress;
use serde_json::{json, Value};

fn main() {
    // Capture before waiting for stdin, decoding, preparing storage or networking.
    let ingress = Ingress::capture();
    let response = match ingress {
        Ok(ingress) => execute(ingress),
        Err(_) => error("invalid-request", "clock_unavailable"),
    };
    // The process protocol reports failures as structured JSON with zero exit.
    // Broken output transport is left for the AW host to detect.
    let mut stdout = std::io::stdout().lock();
    let _ = serde_json::to_writer(&mut stdout, &response);
    let _ = stdout.write_all(b"\n");
}

fn execute(ingress: Ingress) -> Value {
    if std::env::args_os().len() != 1 {
        return error("invalid-request", "invalid_arguments");
    }
    let bytes = match read_input() {
        Ok(bytes) => bytes,
        Err(code) => return error("invalid-request", code),
    };
    let protocol = match Protocol::new() {
        Ok(value) => value,
        Err(_) => return error("invalid-request", "protocol_unavailable"),
    };
    let parsed = match protocol.parse_request(&bytes) {
        Ok(request) => request,
        Err(_) => return error(&safe_id(&bytes), "invalid_request"),
    };
    let request = parsed.as_value();
    let id = request["request_id"].as_str().unwrap_or("invalid-request");
    let response = match request["method"].as_str() {
        Some("describe") => json!({
            "api_version":"aw-provider/v1alpha1", "request_id":id, "status":"ok",
            "operations":[{"name":"evaluate_task_scope","events":["tool.before"],"effects":["observe"]}]
        }),
        Some("validate_config") => match config::Config::decode(&request["config"]) {
            Ok(_) => json!({"api_version":"aw-provider/v1alpha1","request_id":id,"status":"ok"}),
            Err(code) => error(id, code),
        },
        Some("invoke") => {
            let code = match config::Config::decode(&request["config"]) {
                Ok(config) => match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(evaluate::invoke(request, config, ingress)),
                    Err(_) => "runtime_unavailable",
                },
                Err(code) => code,
            };
            // No model text, task data, input payload, endpoint or credential is
            // exposed through the AW protocol or its diagnostic stream.
            eprintln!("{}", json!({"mode":"shadow","code":code}));
            json!({"api_version":"aw-provider/v1alpha1","request_id":id,"status":"ok",
                "input_digest":request["input_digest"],"effects":[]})
        }
        _ => error(id, "invalid_request"),
    };
    let encoded = match serde_json::to_vec(&response) {
        Ok(encoded) => encoded,
        Err(_) => return error(id, "response_invalid"),
    };
    match protocol.check_response(&parsed, &encoded) {
        Ok(_) | Err(aw_provider::Error::ProviderFailure { .. }) => response,
        Err(_) => error(id, "response_invalid"),
    }
}

fn error(id: &str, code: &str) -> Value {
    json!({"api_version":"aw-provider/v1alpha1","request_id":id,"status":"error","error_code":code})
}

fn safe_id(bytes: &[u8]) -> String {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|value| value["request_id"].as_str().map(str::to_owned))
        .filter(|id| {
            !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| (33..=126).contains(&b))
        })
        .unwrap_or_else(|| "invalid-request".into())
}

fn read_input() -> Result<Vec<u8>, &'static str> {
    let (sender, receiver) = mpsc::sync_channel(1);
    // A detached ordinary thread avoids async-stdin shutdown waiting forever
    // for a blocking read after timeout. The host still owns the shorter budget.
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = std::io::stdin()
            .take(aw_provider::MAX_MESSAGE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = sender.send(result);
    });
    let bytes = receiver
        .recv_timeout(Duration::from_secs(120))
        .map_err(|_| "input_timeout")?
        .map_err(|_| "input_failed")?;
    if bytes.len() > aw_provider::MAX_MESSAGE_BYTES {
        return Err("input_too_large");
    }
    Ok(bytes)
}
