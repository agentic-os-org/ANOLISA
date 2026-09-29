//! Product-facing policy authoring contracts.

use serde::{Deserialize, Serialize};

use crate::identifiers::{PolicyId, Revision};

/// Immutable product policy template envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateEnvelope {
    /// Stable policy identity retained by the lowered IR.
    pub policy_id: PolicyId,
    /// Immutable template and policy revision.
    pub revision: Revision,
    /// Product-level policy intent.
    pub template: PolicyTemplate,
}

/// Minimal phase-one product policy vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PolicyTemplate {
    /// High-sensitivity files must not be read into Agent context.
    HighSensitivityReadDeny {
        /// Absolute file paths or bounded glob patterns selected by the user.
        files: Vec<String>,
    },
    /// Deny deletion operations targeting matched filesystem directory entries.
    ///
    /// This template does not cover rename, move, link, content mutation, or
    /// other namespace-mutation operations.
    PreventFileDeletion {
        /// Absolute file paths or bounded glob patterns selected by the user.
        files: Vec<String>,
    },
    /// Low-sensitivity data may be read but direct flow to untrusted endpoints is denied.
    LowSensitivityEgress {
        /// Low-sensitivity paths whose direct flow is tracked.
        files: Vec<String>,
        /// Destinations excluded from the deny rule.
        trusted_destinations: Vec<TrustedDestination>,
    },
}

/// Product-level trusted egress destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum TrustedDestination {
    /// Lowercase DNS name or `*.` suffix pattern.
    Host {
        /// Host pattern selected by the user.
        pattern: String,
        /// Destination ports selected by the user.
        ports: Vec<u16>,
    },
    /// Canonical IP network.
    Cidr {
        /// Canonical network and prefix.
        cidr: String,
        /// Destination ports selected by the user.
        ports: Vec<u16>,
    },
}
