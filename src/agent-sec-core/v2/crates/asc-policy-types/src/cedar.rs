//! Cedar source and schema-reference contracts, independent of the Cedar SDK.
//!
//! Only source and schema references are serializable. Source deserialization
//! does not establish Cedar syntax, schema validity or supported semantics.

use serde::{Deserialize, Serialize};

use crate::identifiers::{ResourceId, Revision};

/// Exact application schema revision; unrelated to a Policy's content revision.
///
/// References a shared schema defining entity types, actions and context fields;
/// it contains neither those definitions nor policy AST nodes. The integration
/// must resolve the referenced revision during validation and replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CedarSchemaReference {
    /// Stable identity of the application's Cedar schema.
    pub schema_id: ResourceId,
    /// Immutable schema revision to resolve during validation and replay.
    pub revision: Revision,
}

/// Author-supplied Cedar policy set, with original text retained for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CedarPolicySource {
    /// Exact schema revision selected for this source.
    pub schema: CedarSchemaReference,
    /// Original Cedar text, potentially containing multiple policy statements.
    pub text: String,
}
