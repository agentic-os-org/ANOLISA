//! Cedar source validation and official PST generation, independent of PAP and targets.
//!
//! Successful validation checks the shared authoring contract. It does not prove
//! backend support, supply runtime facts, or install a policy.

#![forbid(unsafe_code)]

pub mod advice;
pub mod default_policy;
pub mod deployment;
pub mod file_protection;
pub mod http;
pub mod migration;
mod rewrite;
pub mod schema;
pub mod selection;
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

/// Selects policies with eligible statements for request authorization.
///
/// # Errors
/// Rejects invalid source and incompatible headers or credential annotations.
pub fn request_authorization_eligible(source: &CedarPolicySource) -> Result<bool, CedarError> {
    let (policies, _) = validation::parse_and_validate(source)?;
    sandbox_policy_headers(&deployment::statements_from_policies(&policies)?)
}

/// Checks a parsed policy set against the request-selection contract for its schema.
///
/// # Errors
/// Rejects incompatible headers or a credential operation that could grant OS permissions.
fn sandbox_policy_headers(rules: &[deployment::StatementTarget]) -> Result<bool, CedarError> {
    for rule in rules.iter().filter(|rule| rule.credential_rule.is_some()) {
        let policy = rule.policy();
        let http_action = matches!(policy.action_constraint(), cedar_policy::ActionConstraint::Eq(action)
            if action.type_name().to_string() == "SecCore::Action" && action.id().unescaped() == "network.send");
        let endpoint = matches!(policy.resource_constraint(), cedar_policy::ResourceConstraint::Is(ty)
            if ty.to_string() == "SecCore::HttpEndpoint");
        if policy.effect() != cedar_policy::Effect::Permit || !http_action || !endpoint {
            return Err(CedarError::Unsupported {
                policy_id: policy.id().to_string(),
                reason: "credential annotations require an HTTP permit with exact network.send and resource is SecCore::HttpEndpoint",
            });
        }
    }
    if let Some(rule) = rules.iter().find(|rule| {
        rule.bare_principal()
            && rule.may_match(&["file.read", "file.write", "file.exec"], "SecCore::File")
            && (rule.may_match(&["process.spawn"], "SecCore::Process")
                || rule.may_match(&["network.use"], "SecCore::Network"))
    }) {
        return Err(CedarError::Unsupported {
            policy_id: rule.id.clone(),
            reason: "qualify the principal or split the bare cross-domain statement by action",
        });
    }
    Ok(rules
        .iter()
        .any(deployment::StatementTarget::request_eligible))
}

/// Checks request-authorization eligibility for a parsed policy set for an Intent task draft.
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

// SecCore's resource entities have no parents. Membership in a concrete
// entity therefore constrains the resource type just like equality does.
fn resource_type(
    resource: &cedar_policy::pst::ResourceConstraint,
) -> Option<&cedar_policy::pst::EntityType> {
    use cedar_policy::pst::{EntityOrSlot, ResourceConstraint};
    match resource {
        ResourceConstraint::Is(ty) | ResourceConstraint::IsIn(ty, _) => Some(ty),
        ResourceConstraint::Eq(EntityOrSlot::Entity(id))
        | ResourceConstraint::In(EntityOrSlot::Entity(id)) => Some(&id.ty),
        _ => None,
    }
}

fn resource_domain(resource: &cedar_policy::pst::ResourceConstraint) -> Option<PolicyDomain> {
    match resource_type(resource)?.to_string().as_str() {
        "SecCore::File" => Some(PolicyDomain::File),
        "SecCore::Network" | "SecCore::HttpEndpoint" => Some(PolicyDomain::Network),
        _ => None,
    }
}

/// Projects a domain view while preserving statement identities, effects and conditions.
///
/// A resource or action header can confine a statement to one domain. Ambiguous
/// statements remain in both views for consumer validation. Empty views return `None`.
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
            .filter(|(_, policy)| {
                resource_domain(&policy.body().resource).is_none_or(|r| r == domain)
            })
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

/// Select target plans for one manually assigned Policy in a process Binding.
/// The full Scope network view retains subject-specific restrictions for Gateway
/// admission, including when a sibling Policy supplies the credential grant.
pub fn manual_binding_views(
    syntax: &cedar_policy::pst::PolicySet,
    has_credential_grant: bool,
) -> (
    Option<cedar_policy::pst::PolicySet>,
    Option<cedar_policy::pst::PolicySet>,
) {
    let files = project_pst(syntax, PolicyDomain::File);
    let network = project_pst(syntax, PolicyDomain::Network).filter(|network| {
        // A file Binding does not deploy auxiliary Scope restrictions on its own.
        has_credential_grant || files.is_none() || !network.policies.values().all(scope_http_guard)
    });
    (files, network)
}

fn scope_http_guard(policy: &cedar_policy::pst::StaticPolicy) -> bool {
    use cedar_policy::pst::ActionConstraint;
    let body = policy.body();
    let send = |id: &cedar_policy::pst::EntityUID| {
        id.ty.to_string() == "SecCore::Action" && id.eid == "network.send"
    };
    !deployment::bare_principal(policy)
        && !body
            .annotations
            .keys()
            .any(|key| http::is_annotation(key.as_str()))
        && (resource_type(&body.resource)
            .is_some_and(|ty| ty.to_string() == "SecCore::HttpEndpoint")
            || match &body.action {
                ActionConstraint::Eq(id) => send(id),
                ActionConstraint::In(ids) => !ids.is_empty() && ids.iter().all(send),
                ActionConstraint::Any => false,
            })
}

/// Failures before a Cedar source can become a validated policy representation.
#[derive(Debug, thiserror::Error)]
pub enum CedarError {
    /// Source-envelope validation failed before Cedar parsing.
    #[error("invalid Cedar source: {0}")]
    Source(#[from] asc_policy_types::ValidationError),
    /// The source references a schema revision unavailable in this frontend.
    #[error(
        "unsupported Cedar schema {schema_id} revision {revision}; use seccore.system revision 3"
    )]
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
    /// The former subject attribute is no longer supported.
    #[error(
        "Cedar statement {statement_id} uses the retired agent_type attribute. Use principal.agentType"
    )]
    LegacyAgentType {
        /// Statement that must be migrated.
        statement_id: String,
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
