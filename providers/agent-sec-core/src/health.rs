//! Explicit, bounded backend diagnostics through sec-core's authenticated probe.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use aw_exec::{CommandSpec, Limits};
use serde_json::{json, Value};

/// Query the public health helper without starting a service or running a scan.
///
/// Success means the helper authenticated the daemon's root peer and accepted
/// its health reply. It says nothing about AW target admission or scan verdicts.
/// The optional diagnostic is independent of the existing event/view protocols.
///
/// # Errors
/// Rejects relative paths, invalid deadlines and a missing working directory.
pub fn query(helper: &Path, socket: &Path, timeout: Duration) -> Result<Value, super::Error> {
    if !helper.is_absolute() || !socket.is_absolute() || timeout.is_zero() {
        return Err(super::Error::InvalidHealthArguments);
    }
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(super::Error::InvalidHealthArguments)?;
    let installed = helper.is_file();
    let mut reply = json!({
        "api_version":"aw-sec-core-health/v1alpha1",
        "helper_installed":installed,
        "socket_configured":true,
        "backend":"unavailable",
        "admission":"unknown",
        "check_result":"unknown",
        "code":"component_missing"
    });
    if !installed {
        return Ok(reply);
    }
    let mut environment: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    environment.remove(std::ffi::OsStr::new("AGENT_SEC_TEST_SKIP_SERVER_UID_CHECK"));
    environment.insert("AGENT_SEC_DAEMON_SOCKET".into(), socket.into());
    environment.insert(
        "AGENT_SEC_HEALTH_TIMEOUT_SECONDS".into(),
        timeout.as_secs_f64().to_string().into(),
    );
    let result = aw_exec::run(
        &CommandSpec {
            program: helper.into(),
            args: Vec::new(),
            cwd: std::env::current_dir()?,
            environment,
        },
        &[],
        Limits {
            input_bytes: 0,
            stdout_bytes: 4096,
            stderr_bytes: 4096,
        },
        deadline,
        &AtomicBool::new(false),
    );
    match result {
        Ok(output) if output.status.success() => {
            reply["backend"] = json!("authenticated_reachable");
            reply["code"] = Value::Null;
        }
        Ok(_) => reply["code"] = json!("probe_failed"),
        Err(aw_exec::Error::DeadlineExceeded) => reply["code"] = json!("deadline_exceeded"),
        Err(_) => reply["code"] = json!("probe_transport_error"),
    }
    Ok(reply)
}
