//! AW Provider projection over public agent-sec-cli scans and Skill checks.

mod config;
pub mod daemon_client;
pub mod health;
pub mod pii;
mod prompt;
mod scan;
mod skill;
mod startup;
mod tool_pii;
mod trace_context;

use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use aw_provider::{Protocol, Request, MAX_MESSAGE_BYTES, VERSION};
use serde_json::{json, Value};

use self::config::{Config, Mode, UnmappedTool};

/// Processes one EOF-terminated AW request and writes one JSON response.
///
/// No daemon is started. The AW Host must bound process lifetime and stdio;
/// the CLI call shares the invocation budget remaining after request parsing.
/// `timeout` caps that execution budget, not blocking input/output operations.
/// Time spent reading is charged once EOF arrives, but this function cannot
/// interrupt an arbitrary `Read` or `Write`. All methods require an external
/// process deadline (as provided by `aw-host` through `aw-exec`); direct callers
/// must close stdin and enforce termination if input or output stalls.
/// Valid method failures and policy blocks are responses, not process failures.
/// No source or findings are logged.
///
/// # Errors
/// Rejects malformed AW input and reports input/output or internal protocol errors.
/// Explicit backend transport mechanism for supported sec-core scans.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Transport {
    /// Invoke `agent-sec-cli` per request (default backwards-compatible mode).
    #[default]
    Cli,
    /// Connect directly to the sec-core daemon Unix domain socket without CLI spawning.
    Daemon,
}

pub fn run(
    input: &mut impl Read,
    output: &mut impl Write,
    cli: &Path,
    socket: &Path,
    timeout: Duration,
) -> Result<(), Error> {
    run_inner(input, output, cli, socket, timeout, false, Transport::Cli)
}

/// Wait for an externally started backend on the first backend-dependent call.
///
/// Discovery and private configuration validation remain offline. Socket
/// appearance is only a startup barrier: the CLI still authenticates its peer
/// and performs the requested check. Waiting and scanning share the invocation
/// budget; no daemon is started and no scan is retried.
///
/// # Errors
/// Returns the same framing and protocol errors as [`run`]. Startup failure is
/// a Provider error response, never a successful scan or a security decision.
pub fn run_with_backend_wait(
    input: &mut impl Read,
    output: &mut impl Write,
    cli: &Path,
    socket: &Path,
    timeout: Duration,
) -> Result<(), Error> {
    run_inner(input, output, cli, socket, timeout, true, Transport::Cli)
}

/// Processes one request using the explicitly selected backend [`Transport`].
///
/// Supported direct-daemon operations (`scan_code`, `scan_prompt`, `scan_user_input`)
/// bypass CLI subprocess creation when [`Transport::Daemon`] is chosen, while keeping
/// strict deadline and peer-authentication invariants with no automatic fallback.
///
/// # Errors
/// Returns the same framing and protocol errors as [`run`].
pub fn run_with_transport(
    input: &mut impl Read,
    output: &mut impl Write,
    cli: &Path,
    socket: &Path,
    timeout: Duration,
    wait_for_backend: bool,
    transport: Transport,
) -> Result<(), Error> {
    run_inner(
        input,
        output,
        cli,
        socket,
        timeout,
        wait_for_backend,
        transport,
    )
}

fn run_inner(
    input: &mut impl Read,
    output: &mut impl Write,
    cli: &Path,
    socket: &Path,
    timeout: Duration,
    wait_for_backend: bool,
    transport: Transport,
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
    match dispatch(
        &request,
        cli,
        socket,
        timeout,
        started,
        wait_for_backend,
        transport,
    ) {
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
    cli: &Path,
    socket: &Path,
    timeout: Duration,
    started: Instant,
    wait_for_backend: bool,
    transport: Transport,
) -> Result<serde_json::Map<String, Value>, &'static str> {
    let value = request.as_value();
    if value["method"] == "describe" {
        let mut operations = vec![
            json!({"name":"scan_code", "events":["tool.before"], "effects":["observe","block"]}),
            json!({"name":"observe_tool", "events":["tool.after"], "effects":["observe"]}),
            json!({"name":"scan_prompt", "events":["input.submit"], "effects":["observe","block"]}),
            json!({"name":"scan_user_input", "events":["input.submit"], "effects":["observe","block"]}),
            json!({"name":"scan_tool_input", "events":["tool.before"], "effects":["observe","block"]}),
            json!({"name":"scan_tool_output", "events":["tool.after"], "effects":["observe","replace_result"]}),
            json!({"name":"check_final_tool_output", "events":["security.violation"], "effects":["observe","block"]}),
        ];
        if !matches!(transport, Transport::Daemon) || !cli.as_os_str().is_empty() {
            operations.insert(
                0,
                json!({"name":"check_skill", "events":["tool.before","input.submit"], "effects":["observe","block"], "event_adapters":{"input.submit":["pi"]}}),
            );
        }
        return Ok(serde_json::Map::from_iter([(
            "operations".to_owned(),
            Value::Array(operations),
        )]));
    }
    let config = Config::parse(&value["config"])?;
    if value["method"] == "validate_config" {
        return Ok(serde_json::Map::new());
    }
    remaining_budget(value, timeout, started)?;
    let backend_budget = || {
        if wait_for_backend {
            startup::wait(socket, remaining_budget(value, timeout, started)?)?;
        }
        remaining_budget(value, timeout, started)
    };
    let event = &value["event"];
    if matches!(
        value["operation"].as_str(),
        Some("scan_tool_input" | "scan_tool_output" | "check_final_tool_output")
    ) {
        if matches!(transport, Transport::Cli) && cli.as_os_str().is_empty() {
            return Err("cli_path_required");
        }
        let fields = tool_pii::invoke(value, &config, transport, cli, socket, backend_budget)?;
        remaining_budget(value, timeout, started)?;
        return Ok(fields);
    }
    let (effect, reason) = match (value["operation"].as_str(), event["name"].as_str()) {
        (Some("scan_prompt"), Some("input.submit")) => {
            let mode = config.prompt.as_ref().ok_or("prompt_not_configured")?.mode;
            if mode == Mode::Block && !admitted(value, "block") {
                return Err("block_not_admitted");
            }
            let text = event["input"]["text"]
                .as_str()
                .ok_or("invalid_user_input")?;
            let trace = trace_context::from_event(event);
            let verdict = match transport {
                Transport::Cli => prompt::call(cli, socket, text, &trace, backend_budget)?,
                Transport::Daemon => prompt::call_daemon(socket, text, &trace, backend_budget)?,
            };
            match (mode, verdict) {
                (Mode::Block, scan::Verdict::Risk) => ("block", "prompt_risk"),
                (_, scan::Verdict::Risk) => ("observe", "prompt_risk"),
                (_, scan::Verdict::Pass) => ("observe", "prompt_pass"),
            }
        }
        (Some("scan_user_input"), Some("input.submit")) => {
            let mode = config.pii.as_ref().ok_or("pii_not_configured")?.input_mode;
            if mode == Mode::Block && !admitted(value, "block") {
                return Err("block_not_admitted");
            }
            let text = event["input"]["text"]
                .as_str()
                .ok_or("invalid_user_input")?;
            let trace = trace_context::from_event(event);
            let verdict = match transport {
                Transport::Cli => pii::call(
                    cli,
                    socket,
                    text,
                    pii::Source::UserInput,
                    &trace,
                    backend_budget()?,
                ),
                Transport::Daemon => pii::call_daemon(
                    socket,
                    text,
                    pii::Source::UserInput,
                    &trace,
                    backend_budget()?,
                ),
            }
            .map_err(|error| error.code())?;
            match (mode, verdict) {
                (Mode::Block, pii::Verdict::Risk) => ("block", "pii_risk"),
                (_, pii::Verdict::Risk) => ("observe", "pii_risk"),
                (_, pii::Verdict::Pass) => ("observe", "pii_pass"),
            }
        }
        (Some("observe_tool"), Some("tool.after")) => ("observe", "tool_observed"),
        (Some("check_skill"), Some("tool.before" | "input.submit")) => {
            if config.mode == Mode::Block && !admitted(value, "block") {
                return Err("block_not_admitted");
            }
            if !config.skill {
                return Err("skill_check_disabled");
            }
            if event["name"] == "tool.before" {
                if event["tool"]["native_name"] != "Skill"
                    && !(event["agent"]["adapter"] == "hermes"
                        && event["tool"]["native_name"] == "skill_view")
                {
                    return Err("skill_tool_unmapped");
                }
            } else if event["agent"]["adapter"] != "pi" {
                return Err("skill_source_unmapped");
            }
            if cli.as_os_str().is_empty() {
                return Err("cli_path_required");
            }
            let reason = skill::call(cli, socket, event, backend_budget)?;
            let effect = if config.mode == Mode::Block
                && !matches!(reason, "skill_pass" | "skill_not_requested")
            {
                "block"
            } else {
                "observe"
            };
            (effect, reason)
        }
        (Some("scan_code"), Some("tool.before")) => {
            // Admission of block is required for this configured mode, even if
            // this particular input later turns out to be harmless.
            if config.mode == Mode::Block && !admitted(value, "block") {
                return Err("block_not_admitted");
            }
            let tool = event["tool"]["name"].as_str().ok_or("invalid_tool")?;
            match config.tools.get(tool) {
                None => match config.unmapped_tool {
                    UnmappedTool::Error => return Err("tool_unmapped"),
                    UnmappedTool::Observe => ("observe", "tool_unmapped"),
                },
                Some(mapping) => {
                    let code = event["tool"]["input"]
                        .pointer(&mapping.input_pointer)
                        .and_then(Value::as_str)
                        .ok_or("invalid_tool_input")?;
                    let trace_context = trace_context::from_event(event);
                    let verdict = match transport {
                        Transport::Cli => scan::call(
                            cli,
                            socket,
                            code,
                            mapping.language,
                            &trace_context,
                            backend_budget()?,
                        )?,
                        Transport::Daemon => scan::call_daemon(
                            socket,
                            code,
                            mapping.language,
                            &trace_context,
                            backend_budget()?,
                        )?,
                    };
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
    /// The explicit diagnostic requires absolute paths and a nonzero deadline.
    #[error("health diagnostic requires absolute helper/socket paths and a positive timeout")]
    InvalidHealthArguments,
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
