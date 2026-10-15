//! Lightweight daemon probe without CLI parsing or telemetry runtime startup.

use std::{ffi::OsStr, process::ExitCode, time::Duration};

use asc_daemon_protocol::{DaemonRequest, DaemonResponse, method::DAEMON_HEALTH};
use asc_foundation_types::{DAEMON_SOCKET_ENV, daemon_socket_path_from_env};

fn main() -> ExitCode {
    match probe() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("agent-sec-daemon health check failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn probe() -> Result<(), String> {
    if let Some(argument) = std::env::args_os().nth(1) {
        if argument == "--help" || argument == "-h" {
            println!(
                "Usage: agent-sec-daemon-health\n\nChecks daemon.health status over its Unix socket."
            );
            return Ok(());
        }
        return Err(format!("unknown argument: {}", argument.to_string_lossy()));
    }
    let socket = std::env::var_os(DAEMON_SOCKET_ENV);
    let socket = daemon_socket_path_from_env(
        socket
            .as_deref()
            .filter(|value| !value.is_empty())
            .or(Some(OsStr::new("/run/agent-sec-core/daemon.sock"))),
    )
    .map_err(|error| error.to_string())?;
    let timeout = std::env::var("AGENT_SEC_HEALTH_TIMEOUT_SECONDS")
        .unwrap_or_else(|_| "1".into())
        .parse::<f64>()
        .ok()
        .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
        .filter(|timeout| !timeout.is_zero())
        .ok_or("AGENT_SEC_HEALTH_TIMEOUT_SECONDS must be a positive finite duration")?;
    let response = asc_daemon_client::call(
        &socket,
        &DaemonRequest {
            method: DAEMON_HEALTH.into(),
            params: serde_json::json!({}),
            trace_context: None,
            compatibility: None,
        },
        timeout,
    )
    .map_err(|error| format!("{error} (socket: {})", socket.display()))?;
    match response {
        DaemonResponse::Success(response) if response.result["status"] == "ok" => Ok(()),
        _ => Err("daemon returned an invalid or unhealthy response".into()),
    }
}
