//! Decoded inbound facts and the ordering enums the ACP client correlates on.
//!
//! Split from the stateful codec so the wire fact vocabulary stays readable
//! independently of request correlation and bounds enforcement.

use agent_client_protocol::v1::schema::{RequestId, RequestPermissionRequest, StopReason};

use super::super::host_shell::AcpHostExecuteRequest;

/// Hard bound applied before a whole inbound line is accepted.
pub(crate) const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// Bound on simultaneously tracked outbound requests.
pub(super) const MAX_PENDING_REQUESTS: usize = 8;

/// Client-side protocol ordering for one ACP connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Created,
    Initializing,
    Ready,
    Prompting,
    Terminal,
}

/// Outbound request kinds the client correlates responses against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RequestKind {
    Initialize,
    NewSession,
    LoadSession,
    Prompt,
}

/// One decoded inbound fact, in source order.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Inbound {
    Initialized,
    SessionCreated {
        session_id: String,
    },
    Update(agent_client_protocol::v1::schema::SessionUpdate),
    Permission {
        id: RequestId,
        request: RequestPermissionRequest,
    },
    /// A reverse method this client does not implement; answer explicitly.
    UnsupportedRequest {
        id: RequestId,
        method: String,
    },
    /// A host-shell execute request that passed negotiation and validation.
    HostExecute {
        id: RequestId,
        request: AcpHostExecuteRequest,
    },
    /// A host-shell execute request already answered with an error reply.
    HostExecuteRejected {
        id: RequestId,
        code: i32,
    },
    Stopped {
        reason: StopReason,
    },
    Failed {
        kind: RequestKind,
        code: i32,
        message: String,
    },
    TransportClosed,
}

/// Inbound facts plus the replies the caller must write back verbatim.
#[derive(Debug, Default)]
pub(crate) struct DecodedFrame {
    pub events: Vec<Inbound>,
    pub replies: Vec<String>,
}
