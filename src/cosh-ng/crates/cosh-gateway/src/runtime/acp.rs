//! ACP v1 client codec and bridge over the supervised local runtime transport.
//!
//! The official Rust SDK version and the negotiated wire version are separate
//! contracts. This module uses SDK 2.0 types while sending ACP wire version 1.

mod bridge;
mod codec;
mod tool_accumulator;
mod types;

#[cfg(test)]
mod tests;

pub use bridge::{AcpV1BridgeError, AcpV1BridgeRead, AcpV1RuntimeBridge};
pub use codec::AcpV1Codec;
pub use tool_accumulator::{
    AcpToolAccumulation, AcpToolAccumulatorError, AcpToolAccumulatorLimits,
    AcpToolInvocationSnapshot, ToolInvocationAccumulator, DEFAULT_MAX_TOOL_IDENTIFIER_BYTES,
};
pub use types::{
    AcpV1AdapterProfile, AcpV1AgentCapabilities, AcpV1AgentInfo, AcpV1ClientConfig,
    AcpV1CodecError, AcpV1Observation, AcpV1PermissionDecision, AcpV1PermissionOption,
    AcpV1PermissionOptionKind, AcpV1PermissionRequest, AcpV1ProtocolPhase, AcpV1RequestId,
    AcpV1RequestKind, AcpV1StopReason, ACP_WIRE_PROTOCOL_VERSION,
};
