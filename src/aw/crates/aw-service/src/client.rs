//! Reusable synchronous client; clones can invoke distinct event steps concurrently.

use crate::{deadline, ipc, rejected, Identity, Operation, Request, Response, Result, VERSION};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

/// A same-user local connection bound to one daemon generation and configuration.
/// Every call opens its own bounded connection; failed RPCs are never retried.
#[derive(Clone)]
pub struct Client {
    socket: PathBuf,
    identity: Identity,
}

impl Client {
    /// Discover the daemon identity within the original caller deadline.
    ///
    /// # Errors
    /// Rejects transport, framing, version and response errors.
    pub fn connect(socket: impl AsRef<Path>, expires: Instant) -> Result<Self> {
        let response = exchange(socket.as_ref(), None, Operation::Status, expires, None)?;
        response_result(&response)?;
        Ok(Self {
            socket: socket.as_ref().to_owned(),
            identity: response.identity,
        })
    }

    /// Immutable identity retained by all requests from this client.
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Execute one operation with a complete deadline of at most 60 seconds.
    ///
    /// `InvokeStep` calls may overlap across client clones. The Adapter owns the
    /// native scheduling, collects all required replies and decides adoption.
    /// A timeout is indeterminate, not permission to retry or dispatch a tool.
    ///
    /// # Errors
    /// Returns transport errors or the service's stable rejection code; rejects
    /// stale generations, malformed replies and replies received after deadline.
    pub fn call(&self, operation: Operation, expires: Instant) -> Result<Value> {
        self.call_with_cancellation(operation, expires, None)
    }

    /// Execute one operation, abandoning the reply when cancellation is requested.
    ///
    /// Dropping the connection cancels unfinished server-side binding preparation.
    /// Other operations may already have taken effect; never retry them implicitly.
    ///
    /// # Errors
    /// Returns the same errors as [`Self::call`], or an interrupted I/O error.
    pub fn call_cancellable(
        &self,
        operation: Operation,
        expires: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Value> {
        self.call_with_cancellation(operation, expires, Some(cancelled))
    }

    fn call_with_cancellation(
        &self,
        operation: Operation,
        expires: Instant,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Value> {
        let response = exchange(
            &self.socket,
            Some(self.identity.clone()),
            operation,
            expires,
            cancelled,
        )?;
        if response.identity != self.identity {
            return Err(rejected("stale_service"));
        }
        response_result(&response)
    }
}

fn exchange(
    socket: &Path,
    identity: Option<Identity>,
    operation: Operation,
    expires: Instant,
    cancelled: Option<&AtomicBool>,
) -> Result<Response> {
    if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "socket request cancelled",
        )
        .into());
    }
    if let Operation::OpenEvent { event, .. } | Operation::OpenHookEvent { event, .. } = &operation
    {
        let mut pending = vec![(event, 0)];
        while let Some((value, depth)) = pending.pop() {
            if depth > aw_provider::MAX_DEPTH {
                return Err(rejected("event_nesting_limit"));
            }
            match value {
                Value::Object(values) => pending.extend(values.values().map(|v| (v, depth + 1))),
                Value::Array(values) => pending.extend(values.iter().map(|v| (v, depth + 1))),
                _ => {}
            }
        }
    }
    let request = Request {
        api_version: VERSION.into(),
        identity,
        deadline_ns: deadline::encode(expires)?,
        operation,
    };
    let mut stream = ipc::connect(socket, expires)?;
    crate::endpoint::same_user(&stream)?;
    ipc::write(&mut stream, &request, expires)?;
    let response: Response = ipc::read_cancellable(&mut stream, expires, cancelled)?;
    if Instant::now() >= expires {
        return Err(rejected("deadline_exceeded"));
    }
    if response.api_version != VERSION {
        return Err(rejected("protocol_version"));
    }
    Ok(response)
}

fn response_result(response: &Response) -> Result<Value> {
    match (&response.result, &response.error) {
        (Some(result), None) => Ok(result.clone()),
        (None, Some(error)) => Err(match &response.audit_key {
            Some(key) => crate::Error::Attempt {
                code: error.clone(),
                audit_key: key.clone(),
            },
            None => rejected(error),
        }),
        _ => Err(rejected("invalid_response")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn discovery_rejects_daemons_without_idle_unbind_semantics() {
        use std::os::unix::fs::PermissionsExt;
        for version in [
            "aw-service/v1alpha4",
            "aw-service/v1alpha5",
            "aw-service/v1alpha6",
            VERSION,
        ] {
            let directory = tempfile::tempdir_in("/tmp").unwrap();
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
            let state = directory.path().join("state");
            let endpoint = crate::endpoint::Endpoint::bind(&state).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            std::thread::scope(|scope| {
                let responder = scope.spawn(|| {
                    let mut stream = loop {
                        match endpoint.listener().accept() {
                            Ok((stream, _)) => break stream,
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                assert!(Instant::now() < deadline, "discovery did not connect");
                                std::thread::sleep(Duration::from_millis(2));
                            }
                            Err(error) => panic!("discovery accept: {error}"),
                        }
                    };
                    let request: Request = ipc::read(&mut stream, deadline).unwrap();
                    assert!(matches!(request.operation, Operation::Status));
                    ipc::write(
                        &mut stream,
                        &Response {
                            api_version: version.into(),
                            identity: Identity {
                                generation: "fixture-generation".into(),
                                config_revision: "a".repeat(64),
                            },
                            result: Some(serde_json::json!({"bindings": 0})),
                            error: None,
                            audit_key: None,
                        },
                        deadline,
                    )
                    .unwrap();
                });
                let result = Client::connect(state.join("aw.sock"), deadline);
                if version != VERSION {
                    assert!(
                        matches!(result, Err(crate::Error::Rejected(code)) if code == "protocol_version")
                    );
                } else {
                    assert!(result.is_ok(), "current daemon must remain connectable");
                }
                responder.join().unwrap();
            });
        }
    }

    #[test]
    fn deep_native_values_are_rejected_before_serialization_or_connection() {
        let mut value = Value::Null;
        for _ in 0..256 {
            value = Value::Array(vec![value]);
        }
        for operation in [
            Operation::OpenEvent {
                instance_id: "fixture".into(),
                event: value.clone(),
            },
            Operation::OpenHookEvent {
                instance_id: "fixture".into(),
                event: value.clone(),
                native_input: vec![],
            },
        ] {
            let result = exchange(
                Path::new("/no-service-connection-needed"),
                None,
                operation,
                Instant::now() + Duration::from_secs(1),
                None,
            );
            assert!(
                matches!(result, Err(crate::Error::Rejected(code)) if code == "event_nesting_limit")
            );
        }
    }
}
