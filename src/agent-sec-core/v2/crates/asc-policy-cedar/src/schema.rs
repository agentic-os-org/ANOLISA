//! Embedded, version-pinned application schema; no external schema registry.

use asc_policy_types::cedar::CedarSchemaReference;
use asc_policy_types::identifiers::{ResourceId, Revision};
use cedar_policy::Schema;

use crate::CedarError;

/// Shared system-protection schema identity.
pub const SCHEMA_ID: &str = "seccore.system";
/// Revision of the embedded field and action contract.
pub const SCHEMA_REVISION: u32 = 1;
/// Cedar definitions shared by frontend validation and runtime request mapping.
pub const SCHEMA_TEXT: &str = include_str!("../schema/seccore.cedarschema");
/// HTTP-capable revision, leaving the default reference pinned to revision 1.
pub const HTTP_SCHEMA_REVISION: u32 = 2;
/// Revision 2 extends system actions with the HTTP endpoint/request contract.
pub const HTTP_SCHEMA_TEXT: &str = include_str!("../schema/seccore-v2.cedarschema");

/// Embedded schema identity and immutable revision as a source reference.
///
/// # Panics
/// Panics only if the embedded constants stop being a valid identity or a
/// non-zero revision, which their definitions cannot express.
#[must_use]
pub fn embedded_reference() -> CedarSchemaReference {
    CedarSchemaReference {
        schema_id: ResourceId::new(SCHEMA_ID).expect("schema identity is a valid resource id"),
        revision: Revision::new(SCHEMA_REVISION).expect("schema revision is non-zero"),
    }
}

/// Resolves exactly embedded revision 1 or 2 without falling back to another.
///
/// # Errors
/// Rejects unknown identities/revisions and malformed embedded definitions.
pub fn load_schema(reference: &CedarSchemaReference) -> Result<Schema, CedarError> {
    let text = match (reference.schema_id.as_str(), reference.revision.get()) {
        (SCHEMA_ID, SCHEMA_REVISION) => SCHEMA_TEXT,
        (SCHEMA_ID, HTTP_SCHEMA_REVISION) => HTTP_SCHEMA_TEXT,
        _ => {
            return Err(CedarError::UnsupportedSchema {
                schema_id: reference.schema_id.to_string(),
                revision: reference.revision.get(),
            });
        }
    };
    let (schema, _) = Schema::from_cedarschema_str(text).map_err(Box::new)?;
    Ok(schema)
}
