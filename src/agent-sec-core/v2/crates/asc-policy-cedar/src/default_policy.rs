//! Restrictive baseline used when a Scope has no explicit policy.
//!
//! The text is checked against the embedded schema in `tests/default_policy.rs`;
//! being valid and permitted here says nothing about which Translator can apply
//! which statement.

use asc_policy_types::cedar::CedarPolicySource;

use crate::schema;

/// Cedar text of the default policy, compiled from `policies/default.cedar`.
pub const DEFAULT_POLICY_TEXT: &str = include_str!("../policies/default.cedar");

/// Pairs [`DEFAULT_POLICY_TEXT`] with the embedded schema revision.
///
/// The caller still validates and installs the source; this function only binds
/// the text to the revision it was authored against.
///
/// # Panics
/// Panics only if the embedded schema identity or revision becomes invalid, which
/// the crate's own schema constants cannot express.
#[must_use]
pub fn default_policy_source() -> CedarPolicySource {
    CedarPolicySource {
        schema: schema::embedded_reference(),
        text: DEFAULT_POLICY_TEXT.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_source_uses_the_embedded_schema_revision() {
        // Pinned to literals: bumping the schema revision must consciously update
        // this test together with the embedded policy text it was authored against.
        let source = default_policy_source();
        assert_eq!(source.schema.schema_id.as_str(), "seccore.system");
        assert_eq!(source.schema.revision.get(), 1);
        assert_eq!(source.text, DEFAULT_POLICY_TEXT);
    }
}
