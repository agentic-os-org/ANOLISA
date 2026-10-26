//! Adapt a boolean command to the existing Provider protocol; configuration owns effects.

use super::Result;
use aw_exec::{CommandSpec, Limits};
use aw_provider::{Protocol, MAX_MESSAGE_BYTES, VERSION};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    path::Path,
    time::{Duration, Instant},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: u8,
    argv: Vec<String>,
    timeout_ms: u64,
    on_true: Effect,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Effect {
    #[serde(rename = "type")]
    kind: String,
    reason_code: String,
}

impl Config {
    fn parse(value: &Value) -> std::result::Result<Self, &'static str> {
        let config: Self = serde_json::from_value(value.clone()).map_err(|_| "invalid_config")?;
        if config.version != 1
            || config.argv.is_empty()
            || config.argv.len() > 128
            || config.argv.iter().any(|arg| arg.contains('\0'))
            || !executable(&config.argv[0])
            || !(1..=60000).contains(&config.timeout_ms)
            || !matches!(config.on_true.kind.as_str(), "observe" | "block")
            || config.on_true.reason_code.is_empty()
            || config.on_true.reason_code.len() > 128
            || !config
                .on_true
                .reason_code
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
        {
            return Err("invalid_config");
        }
        Ok(config)
    }
}

fn executable(program: &str) -> bool {
    let path = Path::new(program);
    if !path.is_absolute() || !path.is_file() {
        return false;
    }
    let Ok(name) = std::ffi::CString::new(program) else {
        return false;
    };
    // SAFETY: name is a live NUL-terminated path; check the command's effective identity.
    unsafe { libc::faccessat(libc::AT_FDCWD, name.as_ptr(), libc::X_OK, libc::AT_EACCESS) == 0 }
}

pub(super) fn run() -> Result<()> {
    let started = Instant::now();
    let mut input = Vec::new();
    std::io::stdin()
        .take((MAX_MESSAGE_BYTES + 1) as u64)
        .read_to_end(&mut input)?;
    let protocol = Protocol::new()?;
    let request = protocol.parse_request(&input)?;
    let value = request.as_value();
    let mut reply = json!({"api_version":VERSION,"request_id":value["request_id"],"status":"ok"});
    match dispatch(value, started) {
        Ok(fields) => reply
            .as_object_mut()
            .ok_or("invalid response")?
            .extend(fields),
        Err(code) => {
            reply["status"] = json!("error");
            reply["error_code"] = json!(code);
        }
    }
    let bytes = serde_json::to_vec(&reply)?;
    match protocol.check_response(&request, &bytes) {
        Ok(_) | Err(aw_provider::Error::ProviderFailure { .. }) => {}
        Err(error) => return Err(error.into()),
    }
    let mut output = std::io::stdout().lock();
    output.write_all(&bytes)?;
    output.write_all(b"\n")?;
    Ok(())
}

fn dispatch(
    request: &Value,
    started: Instant,
) -> std::result::Result<serde_json::Map<String, Value>, &'static str> {
    if request["method"] == "describe" {
        return Ok(serde_json::Map::from_iter([(
            "operations".into(),
            json!([
                {"name":"check","events":["tool.before","tool.after"],"effects":["observe","block"]}
            ]),
        )]));
    }
    let config = Config::parse(&request["config"])?;
    if request["method"] == "validate_config" {
        return Ok(serde_json::Map::new());
    }
    if request["operation"] != "check" {
        return Err("unsupported_operation");
    }
    if !request["allowed_effects"]
        .as_array()
        .is_some_and(|effects| effects.iter().any(|effect| effect == &config.on_true.kind))
    {
        return Err("effect_not_admitted");
    }
    let milliseconds = request["budget_ms"].as_f64().ok_or("invalid_budget")?;
    let budget = Duration::from_millis(config.timeout_ms)
        .min(Duration::try_from_secs_f64(milliseconds / 1000.0).map_err(|_| "invalid_budget")?);
    let deadline = started + budget;
    if Instant::now() >= deadline {
        return Err("deadline_exceeded");
    }
    let command = CommandSpec {
        program: config.argv[0].clone().into(),
        args: config.argv[1..].iter().map(Into::into).collect(),
        cwd: std::env::current_dir().map_err(|_| "invalid_context")?,
        environment: std::env::vars_os().collect(),
    };
    // Scripts receive only the normalized event, without RPC fields or policy configuration.
    let input = serde_json::to_vec(&request["event"]).map_err(|_| "invalid_event")?;
    let output = aw_exec::run(
        &command,
        &input,
        Limits {
            input_bytes: MAX_MESSAGE_BYTES,
            stdout_bytes: 32,
            stderr_bytes: 65536,
        },
        deadline,
        &crate::STOP,
    )
    .map_err(|error| match error {
        aw_exec::Error::DeadlineExceeded => "deadline_exceeded",
        aw_exec::Error::OutputLimit { .. } => "command_output_limit",
        aw_exec::Error::Cleanup { .. } => "command_cleanup_failed",
        _ => "command_execution_failed",
    })?;
    if !output.status.success() {
        return Err("command_exit_failed");
    }
    if output.input_bytes_written != input.len() {
        return Err("command_incomplete_input");
    }
    let matched: bool = serde_json::from_slice(&output.stdout).map_err(|_| "invalid_boolean")?;
    let effects = if matched {
        json!([{"type":config.on_true.kind,"reason_code":config.on_true.reason_code}])
    } else {
        json!([])
    };
    Ok(serde_json::Map::from_iter([
        ("input_digest".into(), request["input_digest"].clone()),
        ("effects".into(), effects),
    ]))
}
