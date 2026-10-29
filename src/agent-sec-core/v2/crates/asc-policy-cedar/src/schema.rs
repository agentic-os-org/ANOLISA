//! Embedded, version-pinned application schema; no external schema registry.

use asc_policy_types::cedar::CedarSchemaReference;
use cedar_policy::Schema;

use crate::CedarError;

/// Shared system-protection schema identity.
pub const SCHEMA_ID: &str = "seccore.system";
/// Revision of the embedded field and action contract.
pub const SCHEMA_REVISION: u32 = 1;
/// Cedar definitions shared by frontend validation and future request mapping.
pub const SCHEMA_TEXT: &str = include_str!("../schema/seccore.cedarschema");

/// Resolves exactly the embedded schema revision without falling back to another.
///
/// # Errors
/// Rejects unknown identities/revisions and malformed embedded definitions.
pub fn load_schema(reference: &CedarSchemaReference) -> Result<Schema, CedarError> {
    if reference.schema_id.as_str() != SCHEMA_ID || reference.revision.get() != SCHEMA_REVISION {
        return Err(CedarError::UnsupportedSchema {
            schema_id: reference.schema_id.to_string(),
            revision: reference.revision.get(),
        });
    }
    let (schema, _) = Schema::from_cedarschema_str(SCHEMA_TEXT).map_err(Box::new)?;
    Ok(schema)
}
