//! AW Provider projection over the existing daemon code-scan action.

mod config;
mod scan;

use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use aw_provider::{MAX_MESSAGE_BYTES, Protocol, Request, VERSION};
use serde_json::{Value, json};

use self::config::{Config, Mode};

/// Processes one EOF-terminated AW request and writes one JSON response.
///
/// No daemon is started and no local scan is attempted. The AW Host must bound
/// process lifetime and stdio; the inner daemon call shares the invocation
/// budget remaining after request parsing. Valid method failures and policy
/// blocks are responses, not process failures. No source or findings are logged.
///
/// # Errors
/// Rejects malformed AW input and reports input/output or internal protocol errors.
pub fn run(
    input: &mut impl Read,
    output: &mut impl Write,
    socket: &Path,
    timeout: Duration,
) -> Result<(), Error> {
    let started = Instant::now();
    let mut bytes = Vec::new();
    input
        .take((MAX_MESSAGE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    let protocol = Protocol::new()?;
    let request = protocol.parse_request(&bytes)?;
    let value = request.as_value();
    let mut reply = json!({
        "api_version": VERSION, "request_id": value["request_id"], "status": "ok"
    });
    match dispatch(&request, socket, timeout, started) {
        Ok(fields) => reply.as_object_mut().ok_or(Error::Internal)?.extend(fields),
        Err(code) => {
            reply["status"] = json!("error");
            reply["error_code"] = json!(code);
        }
    }
    let bytes = serde_json::to_vec(&reply).map_err(|_| Error::Internal)?;
    match protocol.check_response(&request, &bytes) {
        Ok(_) | Err(aw_provider::Error::ProviderFailure { .. }) => {}
        Err(error) => return Err(error.into()),
    }
    output.write_all(&bytes)?;
    output.write_all(b"\n")?;
    Ok(())
}

fn dispatch(
    request: &Request,
    socket: &Path,
    timeout: Duration,
    started: Instant,
) -> Result<serde_json::Map<String, Value>, &'static str> {
    let value = request.as_value();
    if value["method"] == "describe" {
        return Ok(serde_json::Map::from_iter([(
            "operations".to_owned(),
            json!([
                {"name":"scan_code", "events":["tool.before"], "effects":["observe","block"]},
                {"name":"observe_tool", "events":["tool.after"], "effects":["observe"]}
            ]),
        )]));
    }
    let config = Config::parse(&value["config"])?;
    if value["method"] == "validate_config" {
        return Ok(serde_json::Map::new());
    }
    let remaining = remaining_budget(value, timeout, started)?;
    let event = &value["event"];
    let (effect, reason) = match (value["operation"].as_str(), event["name"].as_str()) {
        (Some("observe_tool"), Some("tool.after")) => ("observe", "tool_observed"),
        (Some("scan_code"), Some("tool.before")) => {
            // Admission of block is required for this configured mode, even if
            // this particular input later turns out to be harmless.
            if config.mode == Mode::Block && !admitted(value, "block") {
                return Err("block_not_admitted");
            }
            let tool = event["tool"]["name"].as_str().ok_or("invalid_tool")?;
            match config.tools.get(tool) {
                None => ("observe", "tool_unmapped"),
                Some(mapping) => {
                    let code = event["tool"]["input"]
                        .pointer(&mapping.input_pointer)
                        .and_then(Value::as_str)
                        .ok_or("invalid_tool_input")?;
                    let verdict = scan::call(socket, code, mapping.language, remaining)?;
                    match (config.mode, verdict) {
                        (Mode::Block, scan::Verdict::Risk) => ("block", "code_risk"),
                        (_, scan::Verdict::Risk) => ("observe", "code_risk"),
                        (_, scan::Verdict::Pass) => ("observe", "code_pass"),
                    }
                }
            }
        }
        _ => return Err("unsupported_operation_event"),
    };
    remaining_budget(value, timeout, started)?;
    if !admitted(value, effect) {
        return Err("effect_not_admitted");
    }
    Ok(serde_json::Map::from_iter([
        ("input_digest".to_owned(), value["input_digest"].clone()),
        (
            "effects".to_owned(),
            json!([{"type":effect,"reason_code":reason}]),
        ),
    ]))
}

fn admitted(request: &Value, effect: &str) -> bool {
    request["allowed_effects"]
        .as_array()
        .is_some_and(|effects| effects.iter().any(|value| value == effect))
}

fn remaining_budget(
    value: &Value,
    timeout: Duration,
    started: Instant,
) -> Result<Duration, &'static str> {
    // The protocol accepts integral decimal/exponent JSON numbers as integers.
    // Its <= 2^53 - 1 bound makes all admitted values exact as f64.
    let milliseconds = value["budget_ms"].as_f64().ok_or("invalid_budget")?;
    let budget =
        Duration::try_from_secs_f64(milliseconds / 1000.0).map_err(|_| "invalid_budget")?;
    budget
        .min(timeout)
        .checked_sub(started.elapsed())
        .filter(|remaining| !remaining.is_zero())
        .ok_or("deadline_exceeded")
}

/// Transport/input failures do not represent security policy decisions.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Stdio could not be read or written.
    #[error("AW Provider stdio failed: {0}")]
    Io(#[from] std::io::Error),
    /// An untrusted request violated the AW protocol.
    #[error(transparent)]
    Protocol(#[from] aw_provider::Error),
    /// A response could not be constructed.
    #[error("AW Provider response construction failed")]
    Internal,
}
