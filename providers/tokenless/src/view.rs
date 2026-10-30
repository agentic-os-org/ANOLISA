//! Whitelist public CLI statistics without duplicating Tokenless storage or policy.

use aw_exec::{CommandSpec, Limits};
use aw_provider::{view, MAX_MESSAGE_BYTES};
use serde_json::{json, Map, Value};
use std::{
    io::{Read, Write},
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

/// Handle one optional view request independently of the event protocol.
///
/// The caller bounds stdin and the whole process. Statistics may maintain an
/// existing store; an absent store is never initialized by this entry point.
///
/// # Errors
/// Returns bounded JSON/stdio errors; component failures become sanitized replies.
pub fn run(
    input: &mut impl Read,
    output: &mut impl Write,
    cli: &Path,
    data_dir: &Path,
    cancelled: &AtomicBool,
) -> Result<(), super::Error> {
    let started = Instant::now();
    let mut bytes = Vec::new();
    input
        .take((MAX_MESSAGE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    let protocol = view::Protocol::new()?;
    let request = protocol.parse_request(&bytes)?;
    let mut reply = request.clone();
    for key in ["budget_ms", "view", "parameters"] {
        reply
            .as_object_mut()
            .ok_or(super::Error::Internal)?
            .remove(key);
    }
    reply["observed_at_ms"] = json!(view::now_ms()?);
    reply["status"] = json!("ok");
    reply["code"] = Value::Null;
    reply["data"] = Value::Null;
    if request["operation"] == "describe" {
        reply["data"] =
            json!({"implementation":"tokenless","version":null,"views":["metrics.summary"]});
    } else {
        let budget = request["budget_ms"]
            .as_u64()
            .ok_or(super::Error::Internal)?;
        match metrics(
            cli,
            data_dir,
            started + Duration::from_millis(budget),
            cancelled,
        ) {
            Ok(data) if data["records"] == 0 => reply["status"] = json!("no_data"),
            Ok(data) => reply["data"] = data,
            Err("store_absent") => {
                reply["status"] = json!("no_data");
                reply["code"] = json!("store_absent");
            }
            Err("view_unsupported") => {
                reply["status"] = json!("unsupported");
                reply["code"] = json!("view_unsupported");
            }
            Err(code) => {
                reply["status"] = json!("unavailable");
                reply["code"] = json!(code);
            }
        }
    }
    reply["observed_at_ms"] = json!(view::now_ms()?);
    let bytes = serde_json::to_vec(&reply).map_err(|_| super::Error::Internal)?;
    protocol.check_response(&request, &bytes)?;
    output.write_all(&bytes)?;
    output.write_all(b"\n")?;
    Ok(())
}

fn metrics(
    cli: &Path,
    data_dir: &Path,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<Value, &'static str> {
    if !cli.is_absolute() || !data_dir.is_absolute() || !cli.is_file() {
        return Err("component_missing");
    }
    // ponytail: only the registered data-dir store is supported; add custom
    // database locations when Tokenless exposes a public resolved-store query.
    let nonempty = |key| std::env::var_os(key).is_some_and(|value| !value.is_empty());
    if nonempty("TOKENLESS_STATS_DB")
        || (std::env::var("DSH_SHELL").as_deref() == Ok("1") && nonempty("DSH_TOKENLESS_STATS_DB"))
    {
        return Err("view_unsupported");
    }
    match std::fs::metadata(data_dir.join("stats.db")) {
        Ok(metadata) if metadata.is_file() => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err("store_absent"),
        _ => return Err("query_failed"),
    }
    let mut environment: std::collections::BTreeMap<_, _> = std::env::vars_os().collect();
    environment.insert("TOKENLESS_DATA_DIR".into(), data_dir.as_os_str().into());
    let result = aw_exec::run(
        &CommandSpec {
            program: cli.into(),
            args: ["stats", "summary", "--json", "--limit", "200"]
                .map(Into::into)
                .to_vec(),
            cwd: std::env::current_dir().map_err(|_| "query_failed")?,
            environment,
        },
        &[],
        Limits {
            input_bytes: 0,
            stdout_bytes: MAX_MESSAGE_BYTES,
            stderr_bytes: 65536,
        },
        deadline,
        cancelled,
    )
    .map_err(|error| match error {
        aw_exec::Error::DeadlineExceeded => "query_timeout",
        aw_exec::Error::OutputLimit { .. } => "output_limit",
        aw_exec::Error::Cancelled => "query_cancelled",
        _ => "query_failed",
    })?;
    if !result.status.success() {
        return Err("query_failed");
    }
    let source = view::decode(&result.stdout).map_err(|_| "query_failed")?;
    if source["schema_version"] != "1.1" {
        return Err("schema_incompatible");
    }
    let mut data = totals(&source["total"])?;
    if data["records"].as_u64().ok_or("query_failed")? > 200 {
        return Err("query_failed");
    }
    let excluded = source["dry_run_records_excluded"]
        .as_u64()
        .ok_or("query_failed")?;
    if excluded > 200 - data["records"].as_u64().ok_or("query_failed")? {
        return Err("query_failed");
    }
    let mut operations = Map::new();
    let by_operation = source["by_operation"].as_object().ok_or("query_failed")?;
    if by_operation.len() > 32 {
        return Err("query_failed");
    }
    let mut sums = [0u64; 3];
    for (name, value) in by_operation {
        if name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err("query_failed");
        }
        let projected = totals(value)?;
        for (index, key) in [
            "records",
            "before_tokens_estimated",
            "after_tokens_estimated",
        ]
        .iter()
        .enumerate()
        {
            sums[index] = sums[index]
                .checked_add(projected[key].as_u64().ok_or("query_failed")?)
                .ok_or("query_failed")?;
        }
        operations.insert(name.clone(), projected);
    }
    for (index, key) in [
        "records",
        "before_tokens_estimated",
        "after_tokens_estimated",
    ]
    .iter()
    .enumerate()
    {
        if data[key] != sums[index] {
            return Err("query_failed");
        }
    }
    data["by_operation"] = Value::Object(operations);
    data["scope"] = json!("component_store_recent_records");
    data["requested_limit"] = json!(200);
    data["range_start"] = Value::Null;
    data["range_end"] = Value::Null;
    data["recording_state"] = json!("unknown");
    data["adoption"] = json!("unverified");
    Ok(data)
}

fn totals(source: &Value) -> Result<Value, &'static str> {
    let read = |key: &str| {
        source[key]
            .as_u64()
            .filter(|v| *v <= 9_007_199_254_740_991)
            .ok_or("query_failed")
    };
    let records = read("records")?;
    let before = read("before_tokens")?;
    let after = read("after_tokens")?;
    let saved = before.checked_sub(after).ok_or("query_failed")?;
    if source["tokens_saved"] != saved || (records == 0 && (before != 0 || after != 0)) {
        return Err("query_failed");
    }
    Ok(
        json!({"records":records,"before_tokens_estimated":before,"after_tokens_estimated":after,
        "saved_tokens_estimated":saved,"saved_percent":if before == 0 { 0.0 } else { saved as f64 / before as f64 * 100.0 }}),
    )
}
