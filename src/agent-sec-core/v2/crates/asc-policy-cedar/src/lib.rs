//! Cedar source validation and official PST generation, independent of PAP and targets.
//!
//! Successful validation checks the shared authoring contract. It does not prove
//! backend support, supply runtime facts, or install a policy.

#![forbid(unsafe_code)]

pub mod advice;
pub mod schema;
mod validation;

use asc_policy_types::cedar::CedarPolicySource;
use cedar_policy::ValidationWarning;

/// Validates source for policy creation and returns non-blocking SDK warnings.
///
/// Revision one retains Scope/Binding-only subject selection. Revision two also
/// accepts subject conditions and validated advice; consumers must implement that
/// execution contract. Target-specific support is checked by Translators.
/// Validation inspects the parsed AST without generating a PST.
///
/// # Errors
/// Returns envelope, schema-resolution, parse, common-subset or validation errors.
/// Action applicability warnings also reject the entire policy set.
pub fn validate(source: &CedarPolicySource) -> Result<Vec<ValidationWarning>, CedarError> {
    let (_, warnings) = validation::parse_and_validate(source)?;
    Ok(warnings)
}

/// Produces official PST for translation after the same checks as [`validate`].
///
/// Accepts source and an exact schema reference; no prior validation call is
/// required. Non-blocking warnings do not affect the returned PST; use [`validate`]
/// when the caller needs to display them. Backend support belongs to Translators.
///
/// # Errors
/// Returns the same admission errors as [`validate`], or an SDK PST conversion error.
pub fn pst(source: &CedarPolicySource) -> Result<cedar_policy::pst::PolicySet, CedarError> {
    let (policies, _) = validation::parse_and_validate(source)?;
    Ok(policies.try_into_pst().map_err(Box::new)?)
}

/// Failures before a Cedar source can become a validated policy representation.
#[derive(Debug, thiserror::Error)]
pub enum CedarError {
    /// Source-envelope validation failed before Cedar parsing.
    #[error("invalid Cedar source: {0}")]
    Source(#[from] asc_policy_types::ValidationError),
    /// The source references a schema revision unavailable in this frontend.
    #[error("unsupported Cedar schema {schema_id} revision {revision}")]
    UnsupportedSchema {
        /// Requested schema identity.
        schema_id: String,
        /// Requested immutable schema revision.
        revision: u32,
    },
    /// The embedded schema failed SDK validation.
    #[error("invalid built-in Cedar schema: {0}")]
    Schema(#[from] Box<cedar_policy::CedarSchemaError>),
    /// Cedar parser diagnostics, including source locations.
    #[error("invalid Cedar syntax: {0}")]
    Parse(#[from] Box<cedar_policy::ParseErrors>),
    /// The policy set violates the application schema.
    #[error("Cedar schema validation failed ({} errors, {} rejected warnings)", .errors.len(), .warnings.len())]
    Validation {
        /// SDK diagnostics retain policy IDs and source locations.
        errors: Vec<cedar_policy::ValidationError>,
        /// Action applicability warnings rejected to preserve admission checks.
        warnings: Vec<cedar_policy::ValidationWarning>,
    },
    /// Valid Cedar outside the common `SecCore` frontend subset.
    #[error("unsupported Cedar construct in {policy_id}: {reason}")]
    Unsupported {
        /// SDK policy ID, or `policy-set` for a set-level failure.
        policy_id: String,
        /// Unsupported construct and its integration boundary.
        reason: &'static str,
    },
    /// SDK conversion to the public syntax tree failed after validation.
    #[error("cannot obtain Cedar public syntax tree: {0}")]
    Representation(#[from] Box<cedar_policy::PolicySetError>),
}
