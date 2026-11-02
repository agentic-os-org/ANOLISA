//! PAP-backed credential Scope execution and canonical Binding deployment.
//! Execution projections retain cleanup responsibility, never independent authorization.

mod binding;
mod error;
mod files;
mod host;
mod nft;
mod processes;
mod runtime;
mod service;
mod state;

pub use binding::{GATEWAY_BINDING_FORMAT, GatewayBindingClient};
pub use error::{GatewayError, GatewayErrorKind};
pub use host::{GatewayAccount, Host, SystemHost};
pub use processes::{MAX_PROCESS_TREE, ProcessIdentity};
pub use service::{
    CredentialGrant, GatewayConfig, GatewayService, GatewayStatus, KeyList, KeyOutcome, KeyRequest,
    ScopeStatusView,
};

#[cfg(test)]
mod tests;
