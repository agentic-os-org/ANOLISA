//! Value-free credential pair retirement; public CA queries take no parameters.

use serde::{Deserialize, Serialize};

/// Global retirement of one credential pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayKeyRemoveParams {
    /// Internal pair identifier.
    pub pairid: String,
}
