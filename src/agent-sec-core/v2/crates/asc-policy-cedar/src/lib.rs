//! Cedar source validation and official PST generation, independent of PAP and targets.
//!
//! Successful validation checks the shared authoring contract. It does not prove
//! backend support, supply runtime facts, or install a policy.

#![forbid(unsafe_code)]

pub mod advice;
pub mod default_policy;
pub mod http;
mod rewrite;
pub mod schema;
mod validation;

pub use rewrite::rewrite_token_files;

use asc_policy_types::cedar::CedarPolicySource;
use cedar_policy::ValidationWarning;

/// Validates source for policy creation and returns non-blocking SDK warnings.
///
/// Checks the common authoring contract without lowering backend execution rules.
/// Target Translators validate the complete projected view they consume.
///
/// # Errors
/// Returns envelope, schema-resolution, parse, common-subset or validation errors.
/// Action applicability warnings also reject the entire policy set.
pub fn validate(source: &CedarPolicySource) -> Result<Vec<ValidationWarning>, CedarError> {
    let (_, warnings) = validation::parse_and_validate(source)?;
    Ok(warnings)
}

/// Checks the temporary explicit-principal opt-in for request authorization.
/// Bare headers stay on the deployment path, even when conditions mention principal.
///
/// # Errors
/// Rejects invalid source, mixed bare/constrained headers or HTTP execution annotations
/// on an explicit-principal request policy.
pub fn request_authorization_eligible(source: &CedarPolicySource) -> Result<bool, CedarError> {
    let (policies, _) = validation::parse_and_validate(source)?;
    request_policy_headers(&policies)
}

/// Checks request-authorization eligibility for a parsed policy set using the PAP header rules.
///
/// # Errors
/// Rejects mixed principal headers or HTTP execution annotations on request policies.
pub fn request_policy_headers(policies: &cedar_policy::PolicySet) -> Result<bool, CedarError> {
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
    if explicit
        && policies.policies().any(|policy| {
            policy
                .annotations()
                .any(|(name, _)| http::is_annotation(name))
        })
    {
        return Err(CedarError::Unsupported {
            policy_id: "policy-set".into(),
            reason: "OS request authorization cannot discharge HTTP execution annotations",
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
    let (policies, _) = validation::parse_and_validate(source)?;
    Ok(policies.try_into_pst().map_err(Box::new)?)
}

/// Action domains used to project the same Cedar representation for deployment consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyDomain {
    /// File access and executable invocation actions.
    File,
    /// Network permission and HTTP request actions, independent of credential use.
    Network,
}

/// Projects a domain view while preserving statement identities, effects and conditions.
///
/// Only statements provably confined to the other domain are excluded. Unconstrained,
/// cross-domain and unclassified actions remain in both views for explicit consumer
/// validation; projection never silently drops a restriction. Empty views return `None`.
///
/// Borrows validated output from [`pst`], without parsing or validating source again.
/// Only selected statements are cloned; the input remains available to other consumers.
pub fn project_pst(
    syntax: &cedar_policy::pst::PolicySet,
    domain: PolicyDomain,
) -> Option<cedar_policy::pst::PolicySet> {
    use cedar_policy::pst::ActionConstraint;
    let other_domain = |id: &cedar_policy::pst::EntityUID| {
        id.ty.to_string() == "SecCore::Action"
            && match domain {
                PolicyDomain::File => matches!(id.eid.as_str(), "network.use" | "network.send"),
                PolicyDomain::Network => {
                    matches!(id.eid.as_str(), "file.read" | "file.write" | "file.exec")
                }
            }
    };
    let projected = cedar_policy::pst::PolicySet {
        policies: syntax
            .policies
            .iter()
            .filter(|(_, policy)| !match &policy.body().action {
                ActionConstraint::Eq(id) => other_domain(id),
                ActionConstraint::In(ids) => !ids.is_empty() && ids.iter().all(other_domain),
                ActionConstraint::Any => false,
            })
            .map(|(id, policy)| (id.clone(), policy.clone()))
            .collect(),
        templates: syntax.templates.clone(),
        template_links: syntax.template_links.clone(),
    };
    (!projected.policies.is_empty()).then_some(projected)
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
