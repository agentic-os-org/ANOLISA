//! Bounded JSON transport for application-owned instances; never starts a daemon.

use super::{hook, Exit, Result};
use aw_service::{Client, Error, Operation, RegistrationRequest};
use serde_json::{json, Value};
use std::{
    io::Write,
    time::{Duration, Instant},
};

pub(crate) fn run(socket: &str, connect: bool, timeout_ms: u64) -> Result<Exit> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut identity = None;
    let result = (|| -> std::result::Result<Value, Error> {
        let request = if connect {
            None
        } else {
            let input = hook::read_input(deadline)
                .map_err(|_| Error::Rejected("registration_input_unavailable".into()))?;
            Some(
                RegistrationRequest::from_json(&input).map_err(|error| match error {
                    Error::Io(_) => Error::Rejected("registration_input_invalid".into()),
                    error => error,
                })?,
            )
        };
        let client = Client::connect(socket, deadline)?;
        identity = Some(client.identity().clone());
        if let Some(request) = request {
            if &request.identity != client.identity() {
                return Err(Error::Rejected("stale_service".into()));
            }
            client.call(
                Operation::Registration {
                    command: request.command,
                },
                deadline,
            )
        } else {
            let status = client.call(Operation::Status, deadline)?;
            if status["registration_protocol"] != aw_service::REGISTRATION_VERSION {
                return Err(Error::Rejected("registration_unsupported".into()));
            }
            Ok(json!({"connected":true}))
        }
    })();
    let (result, error, audit_key, exit) = match result {
        Ok(value) => (Some(value), None, None, 0),
        Err(Error::Rejected(code)) => (None, Some(code), None, 1),
        Err(Error::Attempt { code, audit_key }) => (None, Some(code), Some(audit_key), 1),
        Err(Error::Io(_)) => (None, Some("transport_unavailable".into()), None, 1),
        Err(Error::Configuration(_)) => (None, Some("configuration_rejected".into()), None, 1),
    };
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(
        &mut stdout,
        &json!({
            "api_version":aw_service::REGISTRATION_VERSION, "service_protocol":aw_service::VERSION,
            "identity":identity, "result":result, "error":error, "audit_key":audit_key
        }),
    )?;
    writeln!(stdout)?;
    Ok(Exit::Code(exit))
}
