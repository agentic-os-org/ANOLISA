//! Control plane of the secret gateway.
//!
//! Keeps the desired configuration (Agent domains, their users and presets),
//! turns it into generations of plan and secrets files for the mitmproxy data
//! plane, starts that data plane, and steers each Agent domain's traffic to
//! its listener by placing the Agent's processes into a systemd scope and
//! matching that scope's cgroup in nftables. Real credential values never
//! leave the credential store except in the gateway-readable snapshot.

mod error;
mod files;
mod host;
mod nft;
mod processes;
mod runtime;
mod service;
mod state;

pub use error::{GatewayError, GatewayErrorKind};
pub use host::{GatewayAccount, Host, SystemHost};
pub use processes::{MAX_PROCESS_TREE, ProcessIdentity};
pub use service::{
    AgentStatus, CredentialChange, CredentialGrant, GatewayConfig, GatewayService, GatewayStatus,
    KeyList, KeyOutcome, KeyRequest, scope_name,
};

#[cfg(test)]
mod tests;
