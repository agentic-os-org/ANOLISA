//! Request a cooperative change; only the application can confirm native readiness.

use super::{Arguments, Exit, Result};
use aw_service::{Operation, SdkControl, SdkIntent, SDK_CONTROL_VERSION};
use serde_json::json;
use std::{
    thread,
    time::{Duration, Instant},
};

pub(super) fn run(args: &Arguments) -> Result<Exit> {
    args.check(
        &[
            "--action",
            "--config",
            "--socket",
            "--application",
            "--timeout-ms",
        ],
        false,
    )?;
    let timeout: u64 = args
        .flags
        .get("--timeout-ms")
        .map(String::as_str)
        .unwrap_or("5000")
        .parse()?;
    if !(100..=30000).contains(&timeout) {
        return Err("SDK control timeout must be 100..30000 ms".into());
    }
    super::run::install_signals()?;
    let deadline = Instant::now() + Duration::from_millis(timeout);
    let client = super::connect_service(args, deadline)?;
    let call = |command| {
        client.call_cancellable(
            Operation::SdkControl { command },
            deadline,
            &super::run::CANCELLED,
        )
    };
    let status = client.call_cancellable(Operation::Status, deadline, &super::run::CANCELLED)?;
    if status["sdk_control_protocol"] != SDK_CONTROL_VERSION {
        return Err("sdk_control_unsupported".into());
    }
    let action = args.required("--action")?;
    if action == "list" {
        println!("{}", call(SdkControl::List)?);
        return Ok(Exit::Code(0));
    }
    let intent = match action {
        "attach" => SdkIntent::Attach,
        "detach" => SdkIntent::Detach,
        _ => return Err("unsupported SDK control action".into()),
    };
    let application_id = args.required("--application")?.to_owned();
    let request_id = std::fs::read_to_string("/proc/sys/kernel/random/uuid")?
        .trim()
        .to_owned();
    let mut result = call(SdkControl::Request {
        application_id: application_id.clone(),
        request_id: request_id.clone(),
        intent,
        timeout_ms: timeout,
    })?;
    while result["state"] == "pending" && Instant::now() < deadline {
        thread::sleep(
            Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now())),
        );
        if Instant::now() >= deadline {
            break;
        }
        result = call(SdkControl::Query {
            application_id: application_id.clone(),
            request_id: request_id.clone(),
        })?;
    }
    if result["state"] == "pending" {
        result["error"] = json!("sdk_control_acknowledgement_timeout");
    }
    let success = result["state"] == "completed";
    println!(
        "{}",
        json!({"application_id":application_id,"result":result})
    );
    Ok(Exit::Code(if success { 0 } else { 1 }))
}
