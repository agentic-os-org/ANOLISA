//! Shared Flow identities, terminal kinds, and accounting contracts for bounded capture.

use crate::aggregator::ConnectionId;
use serde::{Deserialize, Serialize};

/// Capture identity shared by a request, its response, and their fragments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FlowContext {
    /// PID observed at capture time.
    pub pid: u32,
    /// SSL pointer or TCP connection identity.
    pub connection: u64,
    /// Capture sequence identifying the exchange.
    #[serde(alias = "exchange")]
    pub protocol_exchange: u64,
    /// First capture sequence of this connection incarnation.
    #[serde(default)]
    pub connection_generation: u64,
}

impl FlowContext {
    /// Associates an exchange with the connection incarnation observed at capture time.
    pub fn new(
        connection: ConnectionId,
        protocol_exchange: u64,
        connection_generation: u64,
    ) -> Self {
        Self {
            pid: connection.pid,
            connection: connection.ssl_ptr,
            protocol_exchange,
            connection_generation,
        }
    }

    /// Connection used to group exchanges for transport lifecycle handling.
    pub fn connection_id(self) -> ConnectionId {
        ConnectionId {
            pid: self.pid,
            ssl_ptr: self.connection,
        }
    }

    /// Stable storage key including the connection incarnation.
    pub fn ledger_key(self) -> String {
        format!(
            "{}:{:x}:{}:{}",
            self.pid, self.connection, self.protocol_exchange, self.connection_generation
        )
    }
}

/// Exactly one terminal kind is committed for every admitted Flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalKind {
    /// All required evidence and Ring Buffer coverage were established.
    Complete,
    /// Connection closure ended collection; completeness depends on retained evidence.
    Closed,
    /// The idle deadline expired.
    TimedOut,
    /// Protocol evidence could not be decoded safely.
    Malformed,
    /// Some evidence was omitted because of a resource or coverage limit.
    Truncated,
    /// Sufficient evidence to reconstruct the Flow was lost.
    Dropped,
}

impl TerminalKind {
    /// Stable ledger representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "Complete",
            Self::Closed => "Closed",
            Self::TimedOut => "TimedOut",
            Self::Malformed => "Malformed",
            Self::Truncated => "Truncated",
            Self::Dropped => "Dropped",
        }
    }
}

/// Stable loss codes; the first triggering reason remains the primary reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum IncompleteReason {
    /// A trace producer lost a reservation during the Flow's lifetime.
    RingBufferLoss,
    /// Sampling could not prove continuous loss-free coverage.
    RingBufferCoverageUnknown,
    /// The local FFI queue rejected delivery.
    FfiOutboundLoss,
    /// Legacy code; its presence does not imply cross-restart recovery support.
    RestartUncheckpointed,
    /// The memory queue could not retain required evidence.
    QueueCapacity,
    /// The nonblocking writer handoff had no room.
    SpoolHandoffFull,
    /// The configured WAL disk capacity was exhausted.
    WalFull,
    /// WAL append or durable sync failed.
    WalIo,
    /// The writer stopped before accepting required evidence.
    WalWriterStopped,
    /// A WAL record failed checksum or framing validation.
    WalCorruption,
    /// One Flow exceeded its retained byte reservation.
    FlowByteLimit,
    /// Global Flow count or byte reservation was exhausted.
    FlowCacheLimit,
    /// No event arrived before the idle deadline.
    FlowIdleTimeout,
    /// Maximum lifetime expired despite continued activity.
    FlowLifetimeLimit,
    /// The protocol decoder rejected required evidence.
    MalformedProtocol,
    /// A required sink could not durably apply the result.
    StorageFailure,
    /// Metadata survived but the body could not be retained.
    EmergencyBodyOmitted,
    /// Both memory and spool admission were unavailable.
    EmergencyCapacity,
    /// Request evidence was absent at closure.
    MissingRequest,
    /// Response evidence was absent at closure.
    MissingResponse,
    /// Protocol evidence ended without the expected end marker.
    MissingEnd,
    /// Required body evidence was absent at closure.
    MissingBody,
    /// HTTP changed to a protocol that the collector cannot certify.
    UnsupportedProtocol,
}

impl IncompleteReason {
    /// Stable code shared by metrics, the ledger, and downstream records.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RingBufferLoss => "RingBufferLoss",
            Self::RingBufferCoverageUnknown => "RingBufferCoverageUnknown",
            Self::FfiOutboundLoss => "FfiOutboundLoss",
            Self::RestartUncheckpointed => "RestartUncheckpointed",
            Self::QueueCapacity => "QueueCapacity",
            Self::SpoolHandoffFull => "SpoolHandoffFull",
            Self::WalFull => "WalFull",
            Self::WalIo => "WalIo",
            Self::WalWriterStopped => "WalWriterStopped",
            Self::WalCorruption => "WalCorruption",
            Self::FlowByteLimit => "FlowByteLimit",
            Self::FlowCacheLimit => "FlowCacheLimit",
            Self::FlowIdleTimeout => "FlowIdleTimeout",
            Self::FlowLifetimeLimit => "FlowLifetimeLimit",
            Self::MalformedProtocol => "MalformedProtocol",
            Self::StorageFailure => "StorageFailure",
            Self::EmergencyBodyOmitted => "EmergencyBodyOmitted",
            Self::EmergencyCapacity => "EmergencyCapacity",
            Self::MissingRequest => "MissingRequest",
            Self::MissingResponse => "MissingResponse",
            Self::MissingEnd => "MissingEnd",
            Self::MissingBody => "MissingBody",
            Self::UnsupportedProtocol => "UnsupportedProtocol",
        }
    }
}

/// Flow accounting snapshot; lifecycle buckets are mutually exclusive.
/// Coverage, late-event, and duplicate-attempt diagnostics are not extra Flow outcomes.
/// Conserved accounting alone does not prove that admitted Flows reached a terminal.
///
/// `observed_new = admitted + rejected` and
/// `admitted = active + pending_coverage + complete + incomplete + evidence_unavailable`.
/// Only `complete` and `incomplete` count committed terminals; unavailable evidence
/// balances abandoned admissions without satisfying successful drain acceptance.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FlowCounters {
    /// First observations of protocol exchanges.
    pub observed_new: u64,
    /// Exchanges whose first event obtained a reservation.
    pub admitted: u64,
    /// New exchanges rejected before admission; excluded from terminal counts.
    pub rejected: u64,
    /// Admitted exchanges still receiving evidence.
    pub active: u64,
    /// Candidates waiting for Ring Buffer coverage.
    pub pending_coverage: u64,
    /// Committed complete terminals.
    pub complete: u64,
    /// Committed non-complete terminals.
    pub incomplete: u64,
    /// Admitted exchanges abandoned without terminal evidence; never certified as complete.
    pub evidence_unavailable: u64,
    /// Events arriving after terminal commit.
    pub late_event_total: u64,
    /// Attempts to commit an already terminal exchange.
    pub duplicate_terminal_total: u64,
    /// Flows intersecting a trace-producer loss interval.
    pub ring_loss_suspect_flow_total: u64,
    /// Flows whose coverage could not be established.
    pub coverage_unknown_flow_total: u64,
    /// Invalid or missing sampling intervals.
    pub coverage_sample_missing_total: u64,
}

impl FlowCounters {
    /// Checks both accounting equations, rejecting sums that overflow the counter range.
    pub fn is_conserved(self) -> bool {
        let accounted = [
            self.active,
            self.pending_coverage,
            self.complete,
            self.incomplete,
            self.evidence_unavailable,
        ]
        .into_iter()
        .try_fold(0_u64, u64::checked_add);
        self.admitted.checked_add(self.rejected) == Some(self.observed_new)
            && accounted == Some(self.admitted)
    }
}
