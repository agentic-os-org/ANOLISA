//! Cedar source validation and official PST generation, independent of PAP and targets.
//!
//! Successful validation checks the shared authoring contract. It does not prove
//! backend support, supply runtime facts, or install a policy.

#![forbid(unsafe_code)]

pub mod advice;
pub mod default_policy;
pub mod http;
pub mod schema;
mod validation;

use asc_policy_types::cedar::CedarPolicySource;
use cedar_policy::ValidationWarning;

/// Validates source for policy creation and returns non-blocking SDK warnings.
///
/// Accepts schema-valid subjects and supported permit advice. Credential-annotated
/// sets additionally require exact, bounded HTTP lowering; other translators
/// check which rules their targets can enforce.
///
/// # Errors
/// Returns envelope, schema-resolution, parse, common-subset or validation errors.
/// Action applicability warnings also reject the entire policy set.
pub fn validate(source: &CedarPolicySource) -> Result<Vec<ValidationWarning>, CedarError> {
    let (_, warnings, _) = validation::parse_and_validate(source)?;
    Ok(warnings)
}

/// Checks the temporary explicit-principal opt-in for request authorization.
/// Bare headers stay on the deployment path, even when conditions mention principal.
///
/// # Errors
/// Rejects invalid source or a stored policy mixing bare and constrained headers.
pub fn request_authorization_eligible(source: &CedarPolicySource) -> Result<bool, CedarError> {
    let (policies, _, _) = validation::parse_and_validate(source)?;
    let mut bare = false;
    let mut explicit = false;
    for policy in policies.policies() {
        if matches!(
            policy.principal_constraint(),
            cedar_policy::PrincipalConstraint::Any
        ) {
            bare = true;
        } else {
            explicit = true;
        }
    }
    if bare && explicit {
        return Err(CedarError::Unsupported {
            policy_id: "policy-set".into(),
            reason: "mixed bare and constrained principal headers; request authorization requires \
                     explicit principal headers on every permit and forbid",
        });
    }
    Ok(explicit)
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
    let (policies, _, _) = validation::parse_and_validate(source)?;
    Ok(policies.try_into_pst().map_err(Box::new)?)
}

/// Lowers credential-annotated HTTP rules in canonical rule-ID order without file I/O.
///
/// Non-credential Cedar returns an empty vector. Admission rejects an entire
/// credential set if any statement cannot be consumed exactly by the HTTP model.
///
/// # Errors
/// Returns the admission errors of [`validate`], including unsupported HTTP constructs.
pub fn credential_rules(
    source: &CedarPolicySource,
) -> Result<Vec<http::CredentialRule>, CedarError> {
    let (_, _, rules) = validation::parse_and_validate(source)?;
    Ok(rules)
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

impl CedarError {
    /// Formats admission diagnostics with policy IDs and available byte locations.
    pub fn diagnostic(&self) -> String {
        let details: Vec<String> = match self {
            Self::Validation { errors, warnings } => errors
                .iter()
                .map(|error| format!("{}: {}", error.policy_id(), describe(error)))
                .chain(
                    warnings
                        .iter()
                        .map(|warning| format!("{}: {}", warning.policy_id(), describe(warning))),
                )
                .collect(),
            Self::Parse(error) => error.iter().map(|error| describe(error)).collect(),
            _ => return self.to_string(),
        };
        format!("{self}: {}", details.join("; "))
    }
}

fn describe(error: &dyn miette::Diagnostic) -> String {
    let mut message = error.to_string();
    if let Some(labels) = error.labels() {
        for label in labels {
            use std::fmt::Write as _;
            // Formatting into a String cannot fail.
            let _ = write!(
                message,
                " [bytes {}..{}]",
                label.offset(),
                label.offset() + label.len()
            );
        }
    }
    message
}
