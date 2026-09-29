//! Typed client boundary for the privileged system-helper protocol.
//!
//! CLI commands use [`HelperClient`] instead of opening the helper socket or
//! interpreting wire responses directly. [`UnixHelperTransport`] is the only
//! production implementation; tests inject scripted transports.

use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use anolisa_core::system_helper::{HelperRequest, HelperResponse, OperationType, operation_type};
use anolisa_platform::ipc;

/// Sends and receives typed messages on one connected helper session.
pub(crate) trait HelperTransport {
    /// Sends one request using the helper wire protocol.
    fn send(&mut self, request: &HelperRequest) -> io::Result<()>;

    /// Receives one response using the helper wire protocol.
    fn receive(&mut self) -> io::Result<HelperResponse>;
}

/// Failure evidence preserved by the helper client boundary.
#[derive(Debug, thiserror::Error)]
pub(crate) enum HelperClientError {
    /// The helper socket could not be connected.
    #[error("failed to connect to system-helper socket {path}: {source}")]
    Connect {
        /// Socket path used for the connection attempt.
        path: PathBuf,
        /// Underlying connection failure.
        #[source]
        source: io::Error,
    },

    /// A typed request could not be written to the helper.
    #[error("failed to send {operation:?} request to system-helper: {source}")]
    Send {
        /// Operation whose request could not be sent.
        operation: OperationType,
        /// Underlying write or serialization failure.
        #[source]
        source: io::Error,
    },

    /// A typed response could not be read from the helper.
    #[error("failed to receive {operation:?} response from system-helper: {source}")]
    Receive {
        /// Operation whose response could not be received.
        operation: OperationType,
        /// Underlying read or deserialization failure.
        #[source]
        source: io::Error,
    },

    /// The helper returned an explicit remote error.
    #[error("system-helper rejected {operation:?}: [{code}] {message}")]
    Remote {
        /// Operation rejected by the helper.
        operation: OperationType,
        /// Stable error code returned by the helper.
        code: String,
        /// Human-readable diagnostic returned by the helper.
        message: String,
    },

    /// The response variant did not match the request contract.
    #[error("unexpected response to {operation:?}: expected {expected}, got {response:?}")]
    UnexpectedResponse {
        /// Operation awaiting a response.
        operation: OperationType,
        /// Expected response variant.
        expected: &'static str,
        /// Actual response, retained as protocol evidence.
        response: HelperResponse,
    },
}

/// Result of the mandatory helper handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HandshakeResult {
    /// Version reported by the connected helper.
    pub(crate) helper_version: String,
    /// Whether the helper accepted the CLI version.
    pub(crate) compatible: bool,
}

/// Result of one osbase-style helper operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HelperOperationOutcome {
    /// Human-readable phase summary returned by the helper.
    pub(crate) message: String,
    /// Domain exit code returned by the helper.
    pub(crate) exit_code: i32,
}

/// Status details returned by the helper daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HelperStatus {
    /// Seconds elapsed since the helper started.
    pub(crate) uptime_secs: u64,
    /// Most recent operation recorded by the helper.
    pub(crate) last_operation: Option<String>,
    /// Timestamp associated with the most recent operation.
    pub(crate) last_operation_time: Option<String>,
}

/// Typed client for one connected system-helper session.
pub(crate) struct HelperClient {
    transport: Box<dyn HelperTransport>,
}

impl HelperClient {
    /// Connects to the production Unix socket transport.
    pub(crate) fn connect(path: &Path) -> Result<Self, HelperClientError> {
        let transport =
            UnixHelperTransport::connect(path).map_err(|source| HelperClientError::Connect {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(Self::with_transport(transport))
    }

    /// Builds a client around an injected transport.
    pub(crate) fn with_transport<T>(transport: T) -> Self
    where
        T: HelperTransport + 'static,
    {
        Self {
            transport: Box::new(transport),
        }
    }

    /// Performs the mandatory version handshake.
    pub(crate) fn handshake(
        &mut self,
        cli_version: &str,
    ) -> Result<HandshakeResult, HelperClientError> {
        let operation = OperationType::Handshake;
        match self.exchange(&HelperRequest::Handshake {
            cli_version: cli_version.to_string(),
        })? {
            HelperResponse::HandshakeOk {
                helper_version,
                compatible,
            } => Ok(HandshakeResult {
                helper_version,
                compatible,
            }),
            HelperResponse::Error { code, message } => Err(HelperClientError::Remote {
                operation,
                code,
                message,
            }),
            response => Err(HelperClientError::UnexpectedResponse {
                operation,
                expected: "HandshakeOk",
                response,
            }),
        }
    }

    /// Executes an operation that must return a success envelope.
    pub(crate) fn execute(
        &mut self,
        request: &HelperRequest,
    ) -> Result<HelperOperationOutcome, HelperClientError> {
        let operation = operation_type(request);
        match self.exchange(request)? {
            HelperResponse::Success { message, exit_code } => {
                Ok(HelperOperationOutcome { message, exit_code })
            }
            HelperResponse::Error { code, message } => Err(HelperClientError::Remote {
                operation,
                code,
                message,
            }),
            response => Err(HelperClientError::UnexpectedResponse {
                operation,
                expected: "Success",
                response,
            }),
        }
    }

    /// Queries the helper daemon status after a successful handshake.
    pub(crate) fn system_status(&mut self) -> Result<HelperStatus, HelperClientError> {
        let request = HelperRequest::SystemStatus;
        let operation = OperationType::SystemStatus;
        match self.exchange(&request)? {
            HelperResponse::Status {
                uptime_secs,
                last_operation,
                last_operation_time,
                ..
            } => Ok(HelperStatus {
                uptime_secs,
                last_operation,
                last_operation_time,
            }),
            HelperResponse::Error { code, message } => Err(HelperClientError::Remote {
                operation,
                code,
                message,
            }),
            response => Err(HelperClientError::UnexpectedResponse {
                operation,
                expected: "Status",
                response,
            }),
        }
    }

    fn exchange(&mut self, request: &HelperRequest) -> Result<HelperResponse, HelperClientError> {
        let operation = operation_type(request);
        self.transport
            .send(request)
            .map_err(|source| HelperClientError::Send { operation, source })?;
        self.transport
            .receive()
            .map_err(|source| HelperClientError::Receive { operation, source })
    }
}

struct UnixHelperTransport {
    stream: UnixStream,
}

impl UnixHelperTransport {
    fn connect(path: &Path) -> io::Result<Self> {
        UnixStream::connect(path).map(|stream| Self { stream })
    }
}

impl HelperTransport for UnixHelperTransport {
    fn send(&mut self, request: &HelperRequest) -> io::Result<()> {
        ipc::send_message(&mut self.stream, request)
    }

    fn receive(&mut self) -> io::Result<HelperResponse> {
        ipc::recv_message(&mut self.stream)
    }
}

#[cfg(test)]
pub(crate) struct ScriptedTransport {
    send_results: VecDeque<io::Result<()>>,
    receive_results: VecDeque<io::Result<HelperResponse>>,
    sent: Rc<RefCell<Vec<HelperRequest>>>,
}

#[cfg(test)]
impl ScriptedTransport {
    pub(crate) fn new(
        send_results: Vec<io::Result<()>>,
        receive_results: Vec<io::Result<HelperResponse>>,
    ) -> (Self, Rc<RefCell<Vec<HelperRequest>>>) {
        let sent = Rc::new(RefCell::new(Vec::new()));
        (
            Self {
                send_results: send_results.into(),
                receive_results: receive_results.into(),
                sent: Rc::clone(&sent),
            },
            sent,
        )
    }
}

#[cfg(test)]
impl HelperTransport for ScriptedTransport {
    fn send(&mut self, request: &HelperRequest) -> io::Result<()> {
        self.sent.borrow_mut().push(request.clone());
        self.send_results.pop_front().unwrap_or(Ok(()))
    }

    fn receive(&mut self) -> io::Result<HelperResponse> {
        self.receive_results.pop_front().expect("scripted response")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client_with_responses(responses: Vec<HelperResponse>) -> HelperClient {
        let results = responses.into_iter().map(Ok).collect();
        let (transport, _) = ScriptedTransport::new(Vec::new(), results);
        HelperClient::with_transport(transport)
    }

    #[test]
    fn handshake_preserves_compatibility_result() {
        for compatible in [true, false] {
            let mut client = client_with_responses(vec![HelperResponse::HandshakeOk {
                helper_version: "0.3.2".to_string(),
                compatible,
            }]);

            let result = client.handshake("0.3.2").expect("handshake");

            assert_eq!(result.helper_version, "0.3.2");
            assert_eq!(result.compatible, compatible);
        }
    }

    #[test]
    fn exchange_distinguishes_send_and_receive_failures() {
        let cases = [
            (
                vec![Err(io::Error::new(io::ErrorKind::BrokenPipe, "send"))],
                Vec::new(),
                "send",
            ),
            (
                vec![Ok(())],
                vec![Err(io::Error::new(io::ErrorKind::UnexpectedEof, "receive"))],
                "receive",
            ),
        ];

        for (send_results, receive_results, expected) in cases {
            let (transport, _) = ScriptedTransport::new(send_results, receive_results);
            let mut client = HelperClient::with_transport(transport);

            let error = client.handshake("0.3.2").expect_err("transport failure");

            match (expected, error) {
                ("send", HelperClientError::Send { operation, source }) => {
                    assert_eq!(operation, OperationType::Handshake);
                    assert_eq!(source.kind(), io::ErrorKind::BrokenPipe);
                }
                ("receive", HelperClientError::Receive { operation, source }) => {
                    assert_eq!(operation, OperationType::Handshake);
                    assert_eq!(source.kind(), io::ErrorKind::UnexpectedEof);
                }
                (_, other) => panic!("unexpected error: {other:?}"),
            }
        }
    }

    #[test]
    fn execute_preserves_all_domain_exit_codes() {
        for exit_code in [0, 2, 1] {
            let mut client = client_with_responses(vec![HelperResponse::Success {
                message: format!("exit {exit_code}"),
                exit_code,
            }]);

            let result = client
                .execute(&HelperRequest::OsbaseStatus { scenario: None })
                .expect("operation outcome");

            assert_eq!(result.exit_code, exit_code);
            assert_eq!(result.message, format!("exit {exit_code}"));
        }
    }

    #[test]
    fn execute_distinguishes_remote_and_unexpected_responses() {
        let responses = [
            (
                HelperResponse::Error {
                    code: "DENIED".to_string(),
                    message: "no access".to_string(),
                },
                "remote",
            ),
            (
                HelperResponse::HandshakeOk {
                    helper_version: "0.3.2".to_string(),
                    compatible: true,
                },
                "unexpected",
            ),
        ];

        for (response, expected) in responses {
            let mut client = client_with_responses(vec![response]);
            let error = client
                .execute(&HelperRequest::OsbaseStatus { scenario: None })
                .expect_err("response failure");

            match (expected, error) {
                (
                    "remote",
                    HelperClientError::Remote {
                        operation,
                        code,
                        message,
                    },
                ) => {
                    assert_eq!(operation, OperationType::OsbaseStatus);
                    assert_eq!(code, "DENIED");
                    assert_eq!(message, "no access");
                }
                (
                    "unexpected",
                    HelperClientError::UnexpectedResponse {
                        operation,
                        expected,
                        ..
                    },
                ) => {
                    assert_eq!(operation, OperationType::OsbaseStatus);
                    assert_eq!(expected, "Success");
                }
                (_, other) => panic!("unexpected error: {other:?}"),
            }
        }
    }

    #[test]
    fn system_status_returns_typed_fields_and_sends_status_request() {
        let (transport, sent) = ScriptedTransport::new(
            Vec::new(),
            vec![Ok(HelperResponse::Status {
                running: true,
                version: "0.3.2".to_string(),
                uptime_secs: 42,
                last_operation: Some("install".to_string()),
                last_operation_time: Some("now".to_string()),
            })],
        );
        let mut client = HelperClient::with_transport(transport);

        let result = client.system_status().expect("status");

        assert_eq!(result.uptime_secs, 42);
        assert_eq!(result.last_operation.as_deref(), Some("install"));
        assert_eq!(result.last_operation_time.as_deref(), Some("now"));
        assert_eq!(sent.borrow().as_slice(), &[HelperRequest::SystemStatus]);
    }
}
