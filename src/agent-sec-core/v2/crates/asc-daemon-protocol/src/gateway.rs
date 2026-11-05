//! Real values stay in caller-owned files, never in RPC frames or CLI arguments.

use serde::{Deserialize, Serialize};

/// Root-only credential import or rotation, independent of Scope authorization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayKeyAddParams {
    /// Credential identifier.
    pub credential: String,
    /// Absolute path of a caller-owned secret file.
    pub secret_file: String,
}

/// Global retirement of one credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayKeyRemoveParams {
    /// Credential identifier.
    pub credential: String,
}

/// Owner-authorized environment query for an existing Scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayScopeParams {
    /// Canonical Scope identity.
    pub scope_id: String,
}
