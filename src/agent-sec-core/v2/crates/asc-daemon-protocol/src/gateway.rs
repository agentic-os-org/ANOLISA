//! Secret-gateway request parameters.
//!
//! Real credential values never travel in these parameters: `gateway.key.add`
//! names a file the daemon reads itself, so the value stays out of request
//! frames and process arguments.

use serde::{Deserialize, Serialize};

/// Parameters for `gateway.key.add`; the Agent domain must already be registered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayKeyAddParams {
    /// Registered Agent domain.
    pub agent: String,
    /// Built-in preset name.
    pub preset: String,
    /// Absolute path of a caller-owned file, mode 0600 or stricter, holding the real value.
    pub secret_file: String,
}

/// Parameters for `gateway.key.remove`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayKeyRemoveParams {
    /// Agent domain.
    pub agent: String,
    /// Built-in preset name.
    pub preset: String,
}

/// Parameters for root-only registration of an already-running Agent process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayRegisterParams {
    /// Agent domain.
    pub agent: String,
    /// Target process user, not the authenticated administrator.
    pub uid: u32,
    /// Already-running target process, not the administrator's process.
    pub pid: u32,
}

/// Parameters for `gateway.unregister` and owner-scoped `gateway.credentials`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayAgentParams {
    /// Registered Agent domain.
    pub agent: String,
}
