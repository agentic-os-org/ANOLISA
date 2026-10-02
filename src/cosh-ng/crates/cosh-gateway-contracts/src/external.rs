//! Scoped references for identities allocated outside COSH.

use serde::{Deserialize, Serialize};

use crate::common::{BoundedName, BoundedOpaque, Digest};

/// Namespace of an external reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalRefKind {
    /// Channel conversation scoped by adapter and tenant authority.
    ChannelConversation,
    /// Channel message scoped by its conversation.
    ChannelMessage,
    /// Shell process or interactive session.
    ShellSession,
    /// Shell command scoped by its session.
    ShellCommand,
    /// Provider-owned Agent conversation.
    ProviderSession,
    /// Locally allocated ACP transport connection.
    AcpConnection,
    /// ACP Agent session scoped by a connection.
    AcpSession,
    /// ACP JSON-RPC request scoped by a connection.
    AcpRequest,
    /// ACP message scoped by an Agent session.
    AcpMessage,
    /// ACP tool call scoped by an Agent session.
    AcpToolCall,
    /// ACP terminal scoped by a live Runtime binding.
    Terminal,
}

/// Opaque external identity that is meaningful only in its declared scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExternalRef {
    /// External namespace represented by the value.
    pub kind: ExternalRefKind,
    /// Adapter, tenant, provider, or connection authority.
    pub authority: BoundedName,
    /// Digest of the complete parent scope.
    pub scope_digest: Digest,
    /// Bounded opaque identity supplied by the external system.
    pub value: BoundedOpaque,
}
