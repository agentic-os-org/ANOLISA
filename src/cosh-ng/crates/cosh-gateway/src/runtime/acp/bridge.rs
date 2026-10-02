//! Minimal synchronous ACP client bridge over [`RuntimeSupervisor`].

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use thiserror::Error;

use super::codec::AcpV1Codec;
use super::types::{
    AcpV1ClientConfig, AcpV1CodecError, AcpV1Observation, AcpV1PermissionDecision,
    AcpV1ProtocolPhase, AcpV1RequestId,
};
use crate::runtime::{
    ProcessTerminal, RuntimeFrameRead, RuntimeLaunchSpec, RuntimeState, RuntimeSupervisor,
    RuntimeSupervisorError,
};

const PROTOCOL_FAILURE_SHUTDOWN_GRACE: Duration = Duration::from_millis(100);

/// Failure returned by the supervised ACP v1 bridge.
#[derive(Debug, Error)]
pub enum AcpV1BridgeError {
    /// Caller or codec state did not permit the requested operation.
    #[error(transparent)]
    Codec(#[from] AcpV1CodecError),
    /// Process supervision or bounded I/O failed.
    #[error(transparent)]
    Supervisor(#[from] RuntimeSupervisorError),
    /// Invalid ACP input was detected and process cleanup also failed.
    #[error("ACP protocol failed: {protocol}; runtime cleanup also failed: {cleanup}")]
    ProtocolCleanup {
        /// Original fail-closed protocol error.
        protocol: AcpV1CodecError,
        /// Cleanup failure after the protocol was made terminal.
        cleanup: RuntimeSupervisorError,
    },
    /// Runtime transport failed and process cleanup also failed.
    #[error("ACP transport failed: {transport}; runtime cleanup also failed: {cleanup}")]
    TransportCleanup {
        /// Original fail-closed transport error.
        transport: RuntimeSupervisorError,
        /// Cleanup failure after the codec was made terminal.
        cleanup: RuntimeSupervisorError,
    },
}

/// Outcome of waiting for one ACP observation with a deadline.
#[derive(Debug, Clone, PartialEq)]
pub enum AcpV1BridgeRead {
    /// One validated ACP observation was received.
    Observation(AcpV1Observation),
    /// No Agent frame arrived within the requested duration.
    TimedOut,
}

/// Owns one ACP codec and the sole supervisor for its Agent subprocess.
#[derive(Debug)]
pub struct AcpV1RuntimeBridge {
    codec: AcpV1Codec,
    supervisor: RuntimeSupervisor,
    terminal: Option<ProcessTerminal>,
    pending_observations: VecDeque<AcpV1Observation>,
}

impl AcpV1RuntimeBridge {
    /// Validates configuration and launches one ACP Agent subprocess.
    ///
    /// The launch uses the existing hardened supervisor: no shell expansion,
    /// an explicit cleared environment, pinned cwd, bounded stdout lines, and
    /// process-group cleanup remain in force.
    ///
    /// # Errors
    ///
    /// Returns client configuration validation or process launch failures.
    pub fn launch(
        spec: &RuntimeLaunchSpec,
        config: AcpV1ClientConfig,
    ) -> Result<Self, AcpV1BridgeError> {
        let codec = AcpV1Codec::new(config)?;
        let mut supervisor = RuntimeSupervisor::new();
        supervisor.launch(spec)?;
        Ok(Self {
            codec,
            supervisor,
            terminal: None,
            pending_observations: VecDeque::new(),
        })
    }

    /// Returns the negotiated codec phase.
    #[must_use]
    pub fn protocol_phase(&self) -> AcpV1ProtocolPhase {
        self.codec.phase()
    }

    /// Returns the supervised process lifecycle state.
    #[must_use]
    pub fn runtime_state(&self) -> RuntimeState {
        self.supervisor.state()
    }

    /// Returns the bound opaque ACP session identifier, when available.
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        self.codec.session_id()
    }

    /// Sends the mandatory ACP v1 initialize request.
    ///
    /// # Errors
    ///
    /// Returns codec state, frame bound, or runtime pipe failures.
    pub fn send_initialize(&mut self) -> Result<(), AcpV1BridgeError> {
        self.commit_frame(AcpV1Codec::initialize_frame)
    }

    /// Sends `session/new` after successful initialization.
    ///
    /// # Errors
    ///
    /// Returns workspace, capability, codec state, frame bound, or runtime
    /// pipe failures.
    pub fn send_new_session(
        &mut self,
        workspace: impl Into<PathBuf>,
        additional_directories: Vec<PathBuf>,
    ) -> Result<(), AcpV1BridgeError> {
        let workspace = workspace.into();
        self.commit_frame(move |codec| codec.new_session_frame(workspace, additional_directories))
    }

    /// Sends one text-only `session/prompt` request.
    ///
    /// # Errors
    ///
    /// Returns codec state, frame bound, or runtime pipe failures.
    pub fn send_prompt(&mut self, text: impl Into<String>) -> Result<(), AcpV1BridgeError> {
        let text = text.into();
        self.commit_frame(move |codec| codec.prompt_frame(text))
    }

    /// Sends session cancellation plus mandatory cancelled permission replies.
    ///
    /// # Errors
    ///
    /// Returns codec state, frame bound, or runtime pipe failures. Frames are
    /// written in their required order and the first write failure is returned.
    pub fn send_cancel(&mut self) -> Result<(), AcpV1BridgeError> {
        let mut candidate = self.codec.clone();
        let frames = candidate.cancel_frames()?;
        for frame in frames {
            if let Err(transport) = self.supervisor.write_frame(&frame) {
                return Err(self.fail_transport(transport));
            }
        }
        self.codec = candidate;
        Ok(())
    }

    /// Sends one governed permission decision to the Agent.
    ///
    /// # Errors
    ///
    /// Rejects unknown correlations/options and runtime pipe failures.
    pub fn send_permission_decision(
        &mut self,
        request_id: &AcpV1RequestId,
        decision: AcpV1PermissionDecision,
    ) -> Result<(), AcpV1BridgeError> {
        let request_id = request_id.clone();
        self.commit_frames(move |codec| codec.permission_response_frames(&request_id, decision))
    }

    /// Sends method-not-found for an unadvertised Agent callback.
    ///
    /// # Errors
    ///
    /// Rejects unknown correlations and runtime pipe failures.
    pub fn reject_unsupported_request(
        &mut self,
        request_id: &AcpV1RequestId,
    ) -> Result<(), AcpV1BridgeError> {
        let request_id = request_id.clone();
        self.commit_frames(move |codec| codec.reject_unsupported_request_frames(&request_id))
    }

    /// Reads and validates the next bounded Agent frame.
    ///
    /// A successful initialization atomically moves the supervisor from
    /// `Initializing` to `Ready`. Any invalid ACP frame makes the codec
    /// terminal and terminates the entire supervised process group.
    ///
    /// # Errors
    ///
    /// Returns bounded I/O, protocol validation, or cleanup failures.
    pub fn read_observation(&mut self) -> Result<Option<AcpV1Observation>, AcpV1BridgeError> {
        loop {
            match self.read_observation_timeout(Duration::from_secs(60))? {
                AcpV1BridgeRead::Observation(observation) => return Ok(Some(observation)),
                AcpV1BridgeRead::TimedOut => {}
            }
        }
    }

    /// Waits at most `timeout` for one validated Agent observation.
    ///
    /// # Errors
    ///
    /// Returns bounded I/O, protocol validation, or cleanup failures.
    pub fn read_observation_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<AcpV1BridgeRead, AcpV1BridgeError> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(observation) = self.pending_observations.pop_front() {
                return Ok(AcpV1BridgeRead::Observation(observation));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(AcpV1BridgeRead::TimedOut);
            }
            let frame = match self.supervisor.read_frame_timeout(remaining) {
                Ok(RuntimeFrameRead::Frame(frame)) => frame,
                Ok(RuntimeFrameRead::Eof) => {
                    let observation = self.codec.finish_stdout();
                    self.terminal = self.supervisor.shutdown(PROTOCOL_FAILURE_SHUTDOWN_GRACE)?;
                    return Ok(AcpV1BridgeRead::Observation(
                        observation.unwrap_or(AcpV1Observation::TransportClosed),
                    ));
                }
                Ok(RuntimeFrameRead::TimedOut) => return Ok(AcpV1BridgeRead::TimedOut),
                Err(transport) => return Err(self.fail_transport(transport)),
            };
            let decoded = match self.codec.decode_transport_frame(frame.as_bytes()) {
                Ok(decoded) => decoded,
                Err(protocol) => return Err(self.fail_protocol(protocol)),
            };
            for frame in decoded.outbound_frames {
                if let Err(transport) = self.supervisor.write_frame(&frame) {
                    return Err(self.fail_transport(transport));
                }
            }
            if decoded
                .observations
                .iter()
                .any(|observation| matches!(observation, AcpV1Observation::Initialized { .. }))
            {
                if let Err(transport) = self.supervisor.mark_ready() {
                    return Err(self.fail_transport(transport));
                }
            }
            self.pending_observations.extend(decoded.observations);
        }
    }

    /// Polls the underlying process terminal without blocking.
    ///
    /// # Errors
    ///
    /// Returns invalid lifecycle state or OS wait failures.
    pub fn poll_terminal(&mut self) -> Result<Option<ProcessTerminal>, AcpV1BridgeError> {
        if self.terminal.is_some() {
            return Ok(self.terminal.take());
        }
        self.supervisor.poll_terminal().map_err(Into::into)
    }

    /// Terminates the Agent process group and reaps the child.
    ///
    /// # Errors
    ///
    /// Returns invalid lifecycle, signalling, or wait failures.
    pub fn shutdown(
        &mut self,
        grace: Duration,
    ) -> Result<Option<ProcessTerminal>, AcpV1BridgeError> {
        if self.terminal.is_some() {
            return Ok(self.terminal.take());
        }
        self.supervisor.shutdown(grace).map_err(Into::into)
    }

    fn fail_protocol(&mut self, protocol: AcpV1CodecError) -> AcpV1BridgeError {
        match self.supervisor.shutdown(PROTOCOL_FAILURE_SHUTDOWN_GRACE) {
            Ok(terminal) => {
                self.terminal = terminal;
                AcpV1BridgeError::Codec(protocol)
            }
            Err(cleanup) => AcpV1BridgeError::ProtocolCleanup { protocol, cleanup },
        }
    }

    fn fail_transport(&mut self, transport: RuntimeSupervisorError) -> AcpV1BridgeError {
        let _ = self.codec.finish_stdout();
        match self.supervisor.shutdown(PROTOCOL_FAILURE_SHUTDOWN_GRACE) {
            Ok(terminal) => {
                self.terminal = terminal;
                AcpV1BridgeError::Supervisor(transport)
            }
            Err(cleanup) => AcpV1BridgeError::TransportCleanup { transport, cleanup },
        }
    }

    fn commit_frame(
        &mut self,
        encode: impl FnOnce(&mut AcpV1Codec) -> Result<String, AcpV1CodecError>,
    ) -> Result<(), AcpV1BridgeError> {
        let mut candidate = self.codec.clone();
        let frame = encode(&mut candidate)?;
        if let Err(transport) = self.supervisor.write_frame(&frame) {
            return Err(self.fail_transport(transport));
        }
        self.codec = candidate;
        Ok(())
    }

    fn commit_frames(
        &mut self,
        encode: impl FnOnce(&mut AcpV1Codec) -> Result<Vec<String>, AcpV1CodecError>,
    ) -> Result<(), AcpV1BridgeError> {
        let mut candidate = self.codec.clone();
        let frames = encode(&mut candidate)?;
        for frame in frames {
            if let Err(transport) = self.supervisor.write_frame(&frame) {
                return Err(self.fail_transport(transport));
            }
        }
        self.codec = candidate;
        Ok(())
    }
}
