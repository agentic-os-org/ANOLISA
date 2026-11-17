//! Request a cooperative change; only the application can confirm native readiness.

use super::{Arguments, Exit, Result};
use aw_service::{Operation, ProcessIdentity, SdkControl, SdkIntent, SDK_CONTROL_VERSION};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    path::Path,
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
            "--sdk-pid",
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
    let process = args
        .flags
        .get("--sdk-pid")
        .map(|pid| -> Result<ProcessIdentity> { Ok(ProcessIdentity::read(pid.parse()?)?) })
        .transpose()?;
    let _service = if process.is_some() && !args.flags.contains_key("--socket") {
        let config = Path::new(args.required("--config")?);
        let bytes = super::read_file(config, aw_config::MAX_DOCUMENT_BYTES)?;
        if let Some((client, socket)) = super::connection::selected(config, deadline, true)? {
            if client.identity().config_revision != format!("{:x}", Sha256::digest(&bytes)) {
                return Err("service configuration does not match the supplied file; use aw update for active instances or stop the service before attaching the edited configuration".into());
            }
            super::connection::remember(config, &socket, &client)?;
            None
        } else {
            let service = aw_service::launch_service::ensure_service(
                &bytes,
                &std::env::current_exe()?,
                deadline,
            )?;
            super::connection::remember(config, &service.paths.socket, &service.client)?;
            Some(service)
        }
    } else {
        None
    };
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
    let application_id = if let Some(process) = &process {
        loop {
            process.check()?;
            let listing = call(SdkControl::List)?;
            let matches = listing["applications"]
                .as_array()
                .ok_or("sdk_control_invalid_list")?
                .iter()
                .filter(|app| app["process"] == process.metadata())
                .collect::<Vec<_>>();
            if matches.len() > 1 {
                return Err("sdk_control_ambiguous_process".into());
            }
            if let Some(app) = matches.first() {
                break app["application_id"]
                    .as_str()
                    .ok_or("sdk_control_invalid_application")?
                    .to_owned();
            }
            if Instant::now() >= deadline {
                return Err("sdk_control_application_not_found".into());
            }
            thread::sleep(
                Duration::from_millis(25).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    } else {
        args.required("--application")?.to_owned()
    };
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
        if let Some(process) = &process {
            process.check()?;
        }
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
