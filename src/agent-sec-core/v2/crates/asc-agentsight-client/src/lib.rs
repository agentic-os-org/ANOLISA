//! Independent `AgentSight` deployment Client for `AgentSecCore` V2.
//!
//! The Client consumes the versioned target plan emitted by the `AgentSight`
//! Adapter, resolves the live Linux process start time, and calls the minimal
//! health/apply/delete surface documented by `AgentSight`. It does not read
//! Binding repository state, own reconciliation, or translate Policy IR.
//! Its `TargetDeploymentClient` implementation prepares stable requests and
//! wraps AgentSight-specific replacement/partial-result semantics.

#![forbid(unsafe_code)]

mod client;
mod process;
mod transport;

pub use client::{
    AGENTSIGHT_PREPARED_APPLY_FORMAT, AgentSightClient, AgentSightClientError,
    AgentSightClientErrorKind, AgentSightClientFactory, DEFAULT_AGENTSIGHT_ROUTE,
};
pub use process::{ProcProcessIdentityResolver, ProcessIdentityError, ProcessIdentityResolver};
pub use transport::{
    AgentSightClientConfigError, AgentSightHttpMethod, AgentSightHttpRequest,
    AgentSightHttpResponse, AgentSightTransport, AgentSightTransportError, UreqAgentSightTransport,
};

/// Default `AgentSight` API root from the integration contract.
pub const DEFAULT_AGENTSIGHT_BASE_URL: &str = "http://127.0.0.1:7396/api";
/// Default `AgentSight` dashboard Bearer-token location.
pub const DEFAULT_AGENTSIGHT_TOKEN_FILE: &str = "/var/log/sysak/.agentsight/.dashboard_token";
/// `AgentSight` enforcement health endpoint relative to the API root.
pub const ENFORCEMENT_HEALTH_PATH: &str = "/enforcement/health";
/// `AgentSight` general enforcement Binding endpoint relative to the API root.
pub const ENFORCEMENT_BINDINGS_PATH: &str = "/enforcement/bindings";
