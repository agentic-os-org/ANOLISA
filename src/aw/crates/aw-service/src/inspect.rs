//! Bounded presentation reads that never bind, start or authorize an Agent.

use crate::{launch_service, rejected, Client, Error, Operation, Result};
use aw_host::ProcessContext;
use aw_provider::{view, MAX_MESSAGE_BYTES};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

#[cfg(all(test, target_os = "linux"))]
mod package_leases;

/// Collect configured Provider cards and optional public statistics in one snapshot.
///
/// The trusted caller supplies the same user context as Agent launches. Only
/// explicitly registered view commands run; event commands and Host preparation
/// are never invoked. The three-second budget excludes executor cleanup grace.
///
/// # Errors
/// Rejects invalid configuration/context, clock errors or invalid generated data.
/// Offline services and individual view failures become sanitized snapshot states.
pub fn snapshot(
    bytes: &[u8],
    context: &ProcessContext,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<Value> {
    let deadline = deadline.min(Instant::now() + Duration::from_secs(3));
    if !context.cwd.is_absolute() {
        return Err(rejected("inspect_cwd_invalid"));
    }
    let configuration = aw_config::Validator::new()?.parse(bytes)?;
    let runtime = context
        .environment
        .get(std::ffi::OsStr::new("XDG_RUNTIME_DIR"))
        .filter(|value| !value.is_empty());
    let paths = launch_service::resolve(bytes, runtime.map(Path::new))?;
    let protocol = view::Protocol::new().map_err(|_| rejected("view_schema_invalid"))?;
    let mut service = json!({"state":"offline","generation":null,"audit_healthy":null,
        "adoption":"unverified","code":"service_offline"});
    let mut recent = result("unavailable", Some("service_offline"), Value::Null)?;
    let expires = || deadline.min(Instant::now() + Duration::from_millis(250));
    let connected = Client::connect(&paths.socket, expires());
    let mut client = None;
    match connected {
        Ok(value) if value.identity().config_revision == paths.config_revision => {
            match value.call_cancellable(Operation::Status, expires(), cancelled) {
                Ok(status) if status["audit_healthy"].is_boolean() => {
                    service = json!({"state":"online","generation":value.identity().generation,
                        "audit_healthy":status["audit_healthy"],"adoption":"unverified","code":null});
                    recent = match value.call_cancellable(
                        Operation::RecentExecutions,
                        expires(),
                        cancelled,
                    ) {
                        Ok(data) => result("ok", None, data)?,
                        Err(_) => result("unavailable", Some("query_failed"), Value::Null)?,
                    };
                    client = Some(value);
                }
                _ => {
                    service["state"] = json!("unavailable");
                    service["code"] = json!("query_failed");
                    recent = result("unavailable", Some("query_failed"), Value::Null)?;
                }
            }
        }
        Ok(_) => {
            service["state"] = json!("unavailable");
            service["code"] = json!("identity_mismatch");
            recent = result("unavailable", Some("identity_mismatch"), Value::Null)?;
        }
        Err(Error::Io(error))
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) => {}
        Err(_) => {
            service["state"] = json!("unavailable");
            service["code"] = json!("query_failed");
            recent = result("unavailable", Some("query_failed"), Value::Null)?;
        }
    }
    let spec = &configuration.as_value()["spec"];
    let mut providers = Vec::new();
    // Reserve the other half for all 128 bounded card headers and service data.
    let mut remaining_output = MAX_MESSAGE_BYTES / 2;
    for (id, provider) in spec["providers"]
        .as_object()
        .ok_or_else(|| rejected("providers_invalid"))?
    {
        let enabled = spec["events"]
            .as_object()
            .ok_or_else(|| rejected("events_invalid"))?
            .values()
            .any(|event| {
                event["enabled"] == true
                    && event["steps"].as_array().is_some_and(|steps| {
                        steps
                            .iter()
                            .any(|step| step["provider"] == *id && step["enabled"] != false)
                    })
            });
        let mut card = json!({"id":id,"implementation":provider["view"]["implementation"],"version":null,
            "configured":true,"enabled":enabled,"admission":"unknown","native_readiness":"unknown",
            "capabilities":["health.summary"],"views":{
                "health.summary":result("ok", None, json!({"configured":true,"enabled":enabled,
                    "admission":"unknown","native_readiness":"unknown","backend_liveness":"unknown"}))?,
                "metrics.summary":result("unsupported", Some("view_unsupported"), Value::Null)?}});
        if !enabled {
            card["views"]["metrics.summary"] = result("disabled", None, Value::Null)?;
        } else if let Some(registration) = provider.get("view") {
            match query(
                &protocol,
                registration,
                id,
                &paths.config_revision,
                context,
                deadline,
                cancelled,
            ) {
                Ok((description, metrics)) => {
                    card["version"] = description["version"].clone();
                    card["capabilities"] = json!(["health.summary", "metrics.summary"]);
                    card["views"]["metrics.summary"] = metrics;
                }
                Err(code) => {
                    card["views"]["metrics.summary"] = result(
                        if code == "view_unsupported" {
                            "unsupported"
                        } else {
                            "unavailable"
                        },
                        Some(code),
                        Value::Null,
                    )?
                }
            }
        }
        let size = serde_json::to_vec(&card)
            .map_err(|_| rejected("inspect_encoding"))?
            .len();
        if size > remaining_output {
            card["views"]["metrics.summary"] =
                result("unavailable", Some("output_limit"), Value::Null)?;
        } else {
            remaining_output -= size;
        }
        providers.push(card);
    }
    if let Some(client) = client {
        let refreshed = client
            .call_cancellable(Operation::Status, expires(), cancelled)
            .and_then(|status| {
                let healthy = status["audit_healthy"]
                    .as_bool()
                    .ok_or_else(|| rejected("inspect_status_invalid"))?;
                service["audit_healthy"] = json!(healthy);
                Ok(())
            });
        if let Err(error) = refreshed {
            let code = if Instant::now() >= deadline {
                "query_timeout"
            } else if matches!(error, Error::Rejected(ref code) if code == "stale_service") {
                "identity_mismatch"
            } else {
                "query_failed"
            };
            service = json!({"state":"unavailable","generation":null,"audit_healthy":null,
                "adoption":"unverified","code":code});
            recent = result("stale", Some(code), Value::Null)?;
        }
    }
    let snapshot = json!({"api_version":view::PANEL_VERSION,"generated_at_ms":timestamp()?,
        "config_revision":paths.config_revision,"service":service,"providers":providers,
        "recent_execution":recent});
    let bytes = serde_json::to_vec(&snapshot).map_err(|_| rejected("inspect_encoding"))?;
    protocol
        .check_panel(&bytes)
        .map_err(|_| rejected("inspect_snapshot_invalid"))
}

fn timestamp() -> Result<u64> {
    view::now_ms().map_err(|_| rejected("inspect_clock_invalid"))
}

fn result(status: &str, code: Option<&str>, data: Value) -> Result<Value> {
    Ok(json!({"status":status,"observed_at_ms":timestamp()?,"code":code,"data":data}))
}

#[allow(clippy::too_many_arguments)] // One explicit context shared by discovery and query.
fn query(
    protocol: &view::Protocol,
    registration: &Value,
    id: &str,
    revision: &str,
    context: &ProcessContext,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> std::result::Result<(Value, Value), &'static str> {
    let deadline = deadline.min(Instant::now() + Duration::from_secs(2));
    let argv = registration["transport"]["argv"]
        .as_array()
        .ok_or("query_failed")?;
    let mut command = aw_exec::CommandSpec {
        program: argv[0].as_str().ok_or("query_failed")?.into(),
        args: argv[1..]
            .iter()
            .map(|value| value.as_str().map(Into::into).ok_or("query_failed"))
            .collect::<std::result::Result<_, _>>()?,
        cwd: context.cwd.clone(),
        environment: context.environment.clone(),
    };
    let _package_leases =
        aw_exec::PackageLeases::acquire([&mut command]).map_err(|_| "query_failed")?;
    let exchange = |operation: &str| -> std::result::Result<Value, &'static str> {
        if Instant::now() >= deadline {
            return Err("query_timeout");
        }
        let budget = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .clamp(1, 2000) as u64;
        let mut request = json!({"protocol":view::VERSION,"operation":operation,
            "request_id":format!("inspect-{operation}"),"provider_id":id,"config_revision":revision,"budget_ms":budget});
        if operation == "query" {
            request["view"] = json!("metrics.summary");
            request["parameters"] = json!({"limit":200});
        }
        let bytes = serde_json::to_vec(&request).map_err(|_| "query_failed")?;
        protocol.parse_request(&bytes).map_err(|_| "query_failed")?;
        let output = aw_exec::run_cooperative(
            &command,
            &bytes,
            aw_exec::Limits {
                input_bytes: MAX_MESSAGE_BYTES,
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
            aw_exec::Error::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
                "component_missing"
            }
            _ => "query_failed",
        })?;
        if !output.status.success() {
            return Err("query_failed");
        }
        let response = protocol
            .check_response(&request, &output.stdout)
            .map_err(|error| match error {
                aw_provider::Error::Invalid("view response identity mismatch") => {
                    "identity_mismatch"
                }
                _ => "schema_incompatible",
            })?;
        if Instant::now() >= deadline {
            return Err("query_timeout");
        }
        Ok(response)
    };
    let description = exchange("describe")?;
    if description["status"] != "ok" {
        return Err("query_failed");
    }
    let description = description["data"].clone();
    if description["implementation"] != registration["implementation"] {
        return Err("identity_mismatch");
    }
    if !description["views"]
        .as_array()
        .is_some_and(|views| views.iter().any(|view| view == "metrics.summary"))
    {
        return Err("view_unsupported");
    }
    let reply = exchange("query")?;
    let metrics = json!({"status":reply["status"],"observed_at_ms":reply["observed_at_ms"],
        "code":reply["code"],"data":reply["data"]});
    Ok((description, metrics))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ipc, Identity, Request, Response, VERSION};
    use std::{fs, os::unix::net::UnixListener, thread};

    #[test]
    fn final_status_refreshes_audit_health_and_rejects_invalid_health() {
        for health in [json!(false), Value::Null] {
            let directory = tempfile::tempdir().unwrap();
            let mut document = aw_config::Validator::new()
                .unwrap()
                .parse(include_bytes!("../examples/aw.qoder.yaml"))
                .unwrap()
                .as_value()
                .clone();
            document["spec"]["daemon"]["state_dir"] = json!(directory.path());
            document["spec"]["providers"] = json!({});
            document["spec"]["events"] = json!({});
            let bytes = serde_json::to_vec(&document).unwrap();
            let paths = launch_service::resolve(&bytes, None).unwrap();
            let listener = UnixListener::bind(&paths.socket).unwrap();
            listener.set_nonblocking(true).unwrap();
            let identity = Identity {
                generation: "fixture-generation".into(),
                config_revision: paths.config_revision,
            };
            let deadline = Instant::now() + Duration::from_secs(3);
            thread::scope(|scope| {
                let final_health = health.clone();
                let worker = scope.spawn(move || {
                    for index in 0..4 {
                        let mut stream = loop {
                            match listener.accept() {
                                Ok((stream, _)) => break stream,
                                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                    assert!(Instant::now() < deadline);
                                    thread::sleep(Duration::from_millis(1));
                                }
                                Err(error) => panic!("fixture accept: {error}"),
                            }
                        };
                        let request: Request = ipc::read(&mut stream, deadline).unwrap();
                        let data = match request.operation {
                            Operation::Status => json!({"audit_healthy":if index == 3 { final_health.clone() } else { json!(true) }}),
                            Operation::RecentExecutions => json!({"scope":"service_generation","limit":20,"items":[]}),
                            _ => panic!("inspect attempted a write operation"),
                        };
                        ipc::write(&mut stream, &Response {
                            api_version: VERSION.into(), identity: identity.clone(),
                            result: Some(data), error: None, audit_key: None,
                        }, deadline).unwrap();
                    }
                });
                let result = snapshot(
                    &bytes,
                    &ProcessContext {
                        cwd: directory.path().into(),
                        environment: Default::default(),
                        stderr_bytes: 65536,
                    },
                    deadline,
                    &AtomicBool::new(false),
                )
                .unwrap();
                worker.join().unwrap();
                assert_eq!(result["service"]["audit_healthy"], health);
                assert_eq!(
                    result["service"]["state"],
                    if health.is_null() {
                        "unavailable"
                    } else {
                        "online"
                    }
                );
            });
            fs::remove_file(paths.socket).unwrap();
        }
    }
}
