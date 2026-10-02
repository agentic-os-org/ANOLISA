#[derive(Debug, Clone)]
struct PendingOutboundRequest {
    kind: AcpV1RequestKind,
    session_id: Option<String>,
}

#[derive(Debug, Clone)]
struct PendingPermission {
    option_ids: BTreeMap<String, AcpV1PermissionOptionKind>,
    response_destination: InboundResponseDestination,
}

#[derive(Debug, Clone, Copy)]
enum InboundResponseDestination {
    Individual,
    Batch { batch_id: u64, slot: usize },
}

#[derive(Debug, Clone)]
struct PendingInboundBatch {
    responses: Vec<Option<RawJsonRpcMessage>>,
}

#[derive(Debug)]
pub(crate) struct AcpV1DecodedFrame {
    pub(crate) observations: Vec<AcpV1Observation>,
    pub(crate) outbound_frames: Vec<String>,
}

/// Stateful encoder and decoder for one ACP v1 process generation.
#[derive(Debug, Clone)]
pub struct AcpV1Codec {
    config: AcpV1ClientConfig,
    phase: AcpV1ProtocolPhase,
    next_request_sequence: u64,
    next_inbound_batch_sequence: u64,
    pending_outbound: BTreeMap<AcpV1RequestId, PendingOutboundRequest>,
    pending_permissions: BTreeMap<AcpV1RequestId, PendingPermission>,
    pending_unsupported: BTreeMap<AcpV1RequestId, InboundResponseDestination>,
    pending_inbound_batches: BTreeMap<u64, PendingInboundBatch>,
    capabilities: Option<AcpV1AgentCapabilities>,
    session_id: Option<String>,
    prompt_request_id: Option<AcpV1RequestId>,
    cancellation_sent: bool,
}
