//! Bridge AW tool lifecycle events through the public Tokenless Protocol v2 CLI.

mod pretool;

use aw_exec::{CommandSpec, Limits};
pub mod view;
use aw_provider::{Protocol, Reply, Request, MAX_MESSAGE_BYTES, VERSION};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: u8,
}

#[derive(Deserialize)]
struct Compression {
    protocol_version: u8,
    operation: String,
    attribution: Value,
    result: Value,
}

/// Process one EOF-terminated AW request using a bounded CLI invocation.
///
/// The Host must bound this process's entire lifetime, including blocking stdio.
/// Only a projected text slot can be replaced; recovery is explicitly unavailable.
/// State is kept in `data_dir`, separate from the immutable installation.
///
/// # Errors
/// Returns stdio or protocol errors; invocation failures become correlated AW errors.
pub fn run(
    input: &mut impl Read,
    output: &mut impl Write,
    cli: &Path,
    data_dir: &Path,
    timeout: Duration,
) -> Result<(), Error> {
    run_with_rtk(input, output, cli, data_dir, timeout, None)
}

/// Process a request with an optional package-owned RTK path and SHA-256.
///
/// Only PreTool validates and exposes RTK to the Tokenless child. Missing or
/// modified payloads return a correlated error without discovering another RTK;
/// PostTool remains usable. Suggested retries carry the absolute managed path.
///
/// # Errors
/// Returns the same stdio and protocol failures as [`run`].
pub fn run_with_rtk(
    input: &mut impl Read,
    output: &mut impl Write,
    cli: &Path,
    data_dir: &Path,
    timeout: Duration,
    managed_rtk: Option<(&Path, &str)>,
) -> Result<(), Error> {
    let started = Instant::now();
    let mut bytes = Vec::new();
    input
        .take((MAX_MESSAGE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    let protocol = Protocol::new()?;
    let request = protocol.parse_request(&bytes)?;
    let value = request.as_value();
    let mut reply = json!({"api_version":VERSION,"request_id":value["request_id"],"status":"ok"});
    match dispatch(&request, cli, data_dir, timeout, started, managed_rtk) {
        Ok(fields) => reply
            .as_object_mut()
            .ok_or(Error::Internal)?
            .extend(fields.as_object().ok_or(Error::Internal)?.clone()),
        Err(code) => {
            reply["status"] = json!("error");
            reply["error_code"] = json!(code);
        }
    }
    let bytes = serde_json::to_vec(&reply).map_err(|_| Error::Internal)?;
    match protocol.check_response(&request, &bytes) {
        Ok(Reply::Description(_) | Reply::Configuration(_) | Reply::Invocation(_))
        | Err(aw_provider::Error::ProviderFailure { .. }) => {}
        Err(error) => return Err(error.into()),
    }
    output.write_all(&bytes)?;
    output.write_all(b"\n")?;
    Ok(())
}

fn dispatch(
    request: &Request,
    cli: &Path,
    data_dir: &Path,
    timeout: Duration,
    started: Instant,
    managed_rtk: Option<(&Path, &str)>,
) -> Result<Value, &'static str> {
    let value = request.as_value();
    if value["method"] == "describe" {
        return Ok(
            json!({"operations":[{"name":"post_tool","events":["tool.after"],"effects":["replace_result"]},{"name":"pre_tool","events":["tool.before"],"effects":["block"]}]}),
        );
    }
    let config: Config =
        serde_json::from_value(value["config"].clone()).map_err(|_| "invalid_config")?;
    if config.version != 1 {
        return Err("invalid_config");
    }
    if value["method"] == "validate_config" {
        return Ok(json!({}));
    }
    let before = value["operation"] == "pre_tool" && value["event"]["name"] == "tool.before";
    if !before && !(value["operation"] == "post_tool" && value["event"]["name"] == "tool.after") {
        return Err("unsupported_operation_event");
    }
    if !value["allowed_effects"].as_array().is_some_and(|effects| {
        effects
            .iter()
            .any(|effect| effect == if before { "block" } else { "replace_result" })
    }) {
        return Err("replacement_not_admitted");
    }
    let neutral = json!({"input_digest":value["input_digest"],"effects":[]});
    let event = &value["event"];
    if before && !pretool::supported(&event["tool"]) {
        return Ok(neutral);
    }
    if before && pretool::optimization(event, data_dir) == "rtk" {
        return Ok(neutral);
    }
    let projection = &event["tool"]["result_projection"];
    if !before && projection["status"] != "success" {
        return Ok(neutral);
    }
    let attribution = json!({"agent_id":event["agent"]["binding_id"], "session_id":event["session_id"], "tool_use_id":event["tool"]["call_id"]});
    // Tokenless rejects null optional IDs; absence is distinct from a native ID.
    let mut attribution = attribution;
    attribution
        .as_object_mut()
        .ok_or("invalid_attribution")?
        .retain(|_, value| !value.is_null());
    let mut operation = json!({"result_kind":"tool", "tool_name":event["tool"]["name"],"content":projection["text"],"status":projection["status"],"content_origin":projection["content_origin"],"output_optimization":pretool::optimization(event, data_dir), "capabilities":{"replace_output":true,"replace_with_text":true,"recovery":{"kind":"none"}}});
    if let Some(command) = projection.get("command") {
        operation["command"] = command.clone();
    }
    if before {
        operation = json!({"tool_name":event["tool"]["name"],"arguments":event["tool"]["input"],"command_field":"command","capabilities":{"replace_arguments":false,"block_and_suggest":true}});
    }
    let operation_name = if before { "pre_tool" } else { "post_tool" };
    let input = serde_json::to_vec(&json!({"protocol_version":2,"operation":operation_name,"attribution":attribution,"input":operation})).map_err(|_| "invalid_compression_input")?;
    let milliseconds = value["budget_ms"].as_f64().ok_or("invalid_budget")?;
    let remaining = Duration::try_from_secs_f64(milliseconds / 1000.0)
        .map_err(|_| "invalid_budget")?
        .min(timeout)
        .checked_sub(started.elapsed())
        .filter(|time| !time.is_zero())
        .ok_or("deadline_exceeded")?;
    let deadline = Instant::now() + remaining;
    let mut environment: std::collections::BTreeMap<_, _> = std::env::vars_os().collect();
    environment.insert("TOKENLESS_DATA_DIR".into(), data_dir.as_os_str().into());
    if before {
        if let Some((rtk, checksum)) = managed_rtk {
            let metadata = rtk
                .symlink_metadata()
                .map_err(|_| "managed_rtk_unavailable")?;
            let directory = rtk.parent().ok_or("managed_rtk_unavailable")?;
            let parent = directory
                .symlink_metadata()
                .map_err(|_| "managed_rtk_unavailable")?;
            // SAFETY: getuid has no arguments or side effects. Match Tokenless's
            // real-UID trust check before it can discover a fallback executable.
            let uid = unsafe { libc::getuid() };
            if !rtk.is_absolute()
                || rtk.file_name().is_none_or(|name| name != "rtk")
                || !metadata.is_file()
                || !parent.is_dir()
                || [&metadata, &parent]
                    .iter()
                    .any(|meta| (meta.uid() != uid && meta.uid() != 0) || meta.mode() & 0o002 != 0)
                || metadata.permissions().mode() & 0o111 == 0
                || rtk_digest(rtk, deadline)? != checksum
            {
                return Err("managed_rtk_unavailable");
            }
            // Tokenless resolves `sh` and RTK through PATH. Limit this child to
            // the verified package directory and standard system dependencies.
            let path = std::env::join_paths([directory, Path::new("/usr/bin"), Path::new("/bin")])
                .map_err(|_| "managed_rtk_unavailable")?;
            environment.insert("PATH".into(), path);
            // Resolve-time failure must not let Tokenless discover a HOME fallback.
            let probe = aw_exec::run(
                &CommandSpec {
                    program: rtk.into(),
                    args: vec!["--version".into()],
                    cwd: std::env::current_dir().map_err(|_| "cli_context_error")?,
                    environment: environment.clone(),
                },
                b"",
                Limits {
                    input_bytes: 0,
                    stdout_bytes: 4096,
                    stderr_bytes: 4096,
                },
                deadline,
                &AtomicBool::new(false),
            )
            .map_err(|_| "managed_rtk_unavailable")?;
            if !probe.status.success()
                || String::from_utf8_lossy(&probe.stdout).trim() != "rtk 0.49.0"
            {
                return Err("managed_rtk_unavailable");
            }
        }
    }
    let command = CommandSpec {
        program: cli.into(),
        args: vec!["compress".into()],
        cwd: std::env::current_dir().map_err(|_| "cli_context_error")?,
        environment,
    };
    let result = aw_exec::run(
        &command,
        &input,
        Limits {
            input_bytes: MAX_MESSAGE_BYTES,
            stdout_bytes: MAX_MESSAGE_BYTES,
            stderr_bytes: 65536,
        },
        deadline,
        &AtomicBool::new(false),
    )
    .map_err(|error| match error {
        aw_exec::Error::DeadlineExceeded => "deadline_exceeded",
        aw_exec::Error::OutputLimit { .. } => "cli_output_limit",
        _ => "cli_transport_error",
    })?;
    if !result.status.success() {
        return Err("cli_failed");
    }
    let result: Compression =
        serde_json::from_slice(&result.stdout).map_err(|_| "invalid_compression_result")?;
    if result.protocol_version != 2
        || result.operation != operation_name
        || result.attribution != attribution
    {
        return Err("invalid_compression_result");
    }
    if before {
        return pretool::reply(value, &result.result);
    }
    let output = result.result["output"]
        .as_str()
        .ok_or("invalid_compression_result")?;
    match result.result["disposition"]
        .as_str()
        .ok_or("invalid_compression_result")?
    {
        "applied"
            if output.is_empty()
                || output.len()
                    >= projection["text"]
                        .as_str()
                        .ok_or("invalid_projection")?
                        .len() =>
        {
            Err("non_shrinking_replacement")
        }
        "applied" => Ok(
            json!({"input_digest":value["input_digest"],"effects":[{"type":"replace_result","text":output,"reason_code":"tokenless_compressed"}]}),
        ),
        "dry_run" | "passthrough" | "no_savings" | "recoverability_unavailable"
            if output == projection["text"] =>
        {
            Ok(neutral)
        }
        "timeout" => Err("compression_timeout"),
        "tool_error" => Err("compression_error"),
        _ => Err("invalid_compression_result"),
    }
}

/// Input and transport failures, separate from a successful compression decision.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Provider stdio failed.
    #[error("AW Tokenless Provider stdio failed: {0}")]
    Io(#[from] std::io::Error),
    /// A request or generated response violated the AW protocol.
    #[error(transparent)]
    Protocol(#[from] aw_provider::Error),
    /// A response could not be constructed.
    #[error("AW Tokenless Provider response construction failed")]
    Internal,
}

fn rtk_digest(path: &Path, deadline: Instant) -> Result<String, &'static str> {
    use sha2::{Digest, Sha256};
    let mut source = std::fs::File::open(path).map_err(|_| "managed_rtk_unavailable")?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        if Instant::now() >= deadline {
            return Err("deadline_exceeded");
        }
        let count = source
            .read(&mut buffer)
            .map_err(|_| "managed_rtk_unavailable")?;
        if count == 0 {
            return Ok(format!("{:x}", hash.finalize()));
        }
        hash.update(&buffer[..count]);
    }
}
