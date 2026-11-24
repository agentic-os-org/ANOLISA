//! Derives process selectors and deployment views without changing the authored policy.
use std::sync::Arc;

use asc_policy_types::cedar::CedarPolicySource;
use asc_policy_types::policy::{DeploymentBackend, DeploymentSelection};
use cedar_policy::pst::{
    BinaryOp, Clause, Expr, Literal, PolicySet, PrincipalConstraint, StaticPolicy, Var,
};
use cedar_policy::{ActionConstraint, ResourceConstraint};

use crate::CedarError;

/// Positive subject equalities that process discovery can verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectSelector {
    pub agent_type: String,
    pub owner_uid: Option<u32>,
}

#[derive(Default)]
struct Predicates {
    agent: Option<String>,
    uid: Option<u32>,
    has_agent: bool,
    has_uid: bool,
}

fn uses_principal(expr: &Expr) -> bool {
    expr.reduce(
        &|expr| matches!(expr, Expr::Var(Var::Principal)).then_some(true),
        &|a, b| a || b,
        false,
    )
}

impl Predicates {
    fn take(&mut self, expr: &Arc<Expr>) -> Result<Option<Arc<Expr>>, &'static str> {
        match expr.as_ref() {
            Expr::BinaryOp {
                op: BinaryOp::And,
                left,
                right,
            } => {
                let left = self.take(left)?;
                let right = self.take(right)?;
                Ok(match (left, right) {
                    (Some(left), Some(right)) => Some(Arc::new(Expr::BinaryOp {
                        op: BinaryOp::And,
                        left,
                        right,
                    })),
                    (left, right) => left.or(right),
                })
            }
            Expr::HasAttr { expr: base, attrs }
                if matches!(base.as_ref(), Expr::Var(Var::Principal)) && attrs.len() == 1 =>
            {
                match attrs.head.as_str() {
                    "agentType" => self.has_agent = true,
                    "ownerUid" => self.has_uid = true,
                    _ => return Err("unsupported principal attribute in deployment selector"),
                }
                Ok(None)
            }
            Expr::BinaryOp {
                op: BinaryOp::Eq,
                left,
                right,
            } if uses_principal(expr) => {
                let (attr, value) = if matches!(right.as_ref(), Expr::GetAttr { .. }) {
                    (right, left)
                } else {
                    (left, right)
                };
                match (attr.as_ref(), value.as_ref()) {
                    (Expr::GetAttr { expr, attr }, Expr::Literal(Literal::String(value)))
                        if matches!(expr.as_ref(), Expr::Var(Var::Principal))
                            && attr == "agentType"
                            && !value.is_empty() =>
                    {
                        if self
                            .agent
                            .replace(value.to_string())
                            .is_some_and(|previous| previous != value.as_str())
                        {
                            return Err("inconsistent agentType constraints");
                        }
                    }
                    (Expr::GetAttr { expr, attr }, Expr::Literal(Literal::Long(value)))
                        if matches!(expr.as_ref(), Expr::Var(Var::Principal))
                            && attr == "ownerUid" =>
                    {
                        let uid = u32::try_from(*value)
                            .map_err(|_| "ownerUid is outside the supported range")?;
                        if self
                            .uid
                            .replace(uid)
                            .is_some_and(|previous| previous != uid)
                        {
                            return Err("inconsistent ownerUid constraints");
                        }
                    }
                    _ => return Err("deployment requires a constant principal equality"),
                }
                Ok(None)
            }
            _ if uses_principal(expr) => {
                Err("deployment requires positive principal equalities joined by conjunction")
            }
            _ => Ok(Some(Arc::clone(expr))),
        }
    }
}

/// Separates exact process predicates from the residual backend rule.
/// Only the caller that established the selector may execute the residual.
/// # Errors
/// Identifies the source statement when its principal cannot be represented by a Scope.
pub fn split_subject(
    policy: &StaticPolicy,
) -> Result<(Option<SubjectSelector>, StaticPolicy), CedarError> {
    let body = policy.body();
    let reject = |reason| CedarError::Unsupported {
        policy_id: policy.id().to_string(),
        reason,
    };
    if !(matches!(body.principal, PrincipalConstraint::Any)
        || matches!(&body.principal, PrincipalConstraint::Is(ty) if ty.to_string()=="SecCore::Subject"))
    {
        return Err(reject("unsupported deployment principal header"));
    }
    let mut predicates = Predicates::default();
    let mut clauses = Vec::new();
    for clause in body.clauses() {
        match clause {
            Clause::When(expr) => {
                if let Some(residual) = predicates.take(expr).map_err(reject)? {
                    clauses.push(Clause::When(residual));
                }
            }
            Clause::Unless(expr) if uses_principal(expr) => {
                return Err(reject(
                    "deployment cannot verify a negated subject condition",
                ));
            }
            Clause::Unless(_) => clauses.push(clause.clone()),
        }
    }
    if (predicates.has_uid && predicates.uid.is_none())
        || (predicates.agent.is_none() && (predicates.has_agent || predicates.uid.is_some()))
    {
        return Err(reject(
            "deployment requires an exact agentType and a value for each guarded attribute",
        ));
    }
    let selector = predicates.agent.map(|agent_type| SubjectSelector {
        agent_type,
        owner_uid: predicates.uid,
    });
    let mut body = body
        .clone()
        .try_with_clauses(clauses)
        .map_err(|_| reject("cannot retain deployment clauses"))?;
    body.principal = PrincipalConstraint::Any;
    Ok((
        selector,
        StaticPolicy::try_from(body).map_err(|_| reject("deployment requires a static policy"))?,
    ))
}

/// Resolves source IDs against their pinned revision and retains original Cedar identities.
/// # Errors
/// Rejects invalid source, missing identities, or a subject that discovery cannot discharge.
pub fn selected_pst(
    source: &CedarPolicySource,
    selection: &DeploymentSelection,
) -> Result<PolicySet, CedarError> {
    let mut syntax = crate::pst(source)?;
    let mut selected = syntax.policies.clone();
    selected.clear();
    for (id, policy) in &syntax.policies {
        if selection.statement_ids.contains(&id.to_string()) {
            selected.insert(id.clone(), split_subject(policy)?.1);
        }
    }
    if selected.len() != selection.statement_ids.len() {
        return Err(CedarError::Unsupported {
            policy_id: "policy-set".into(),
            reason: "deployment references a missing source statement",
        });
    }
    syntax.policies = selected;
    Ok(syntax)
}

/// Process deployment information for an individual authored statement.
pub struct StatementTarget {
    pub id: String,
    pub subject: Result<Option<SubjectSelector>, CedarError>,
    action: ActionConstraint,
    resource: ResourceConstraint,
    pub credential_rule: Option<String>,
}

impl StatementTarget {
    /// Whether the header can match a request in the consumer's action and resource domain.
    #[must_use]
    pub fn may_match(&self, actions: &[&str], resource: &str) -> bool {
        header_may_match(&self.action, &self.resource, actions, resource)
    }
}

/// Match a request domain against a validated header in an embedded `SecCore` schema.
/// Their resource types have no parent types and actions have no groups. Thus `in`
/// can match the referenced resource itself, but never a resource of a different type.
#[must_use]
pub fn header_may_match(
    action: &ActionConstraint,
    resource: &ResourceConstraint,
    actions: &[&str],
    resource_type: &str,
) -> bool {
    let matches_resource = match resource {
        ResourceConstraint::Any => true,
        ResourceConstraint::Is(ty) => ty.to_string() == resource_type,
        ResourceConstraint::IsIn(ty, ancestor) => {
            ty.to_string() == resource_type && ancestor.type_name().to_string() == resource_type
        }
        ResourceConstraint::Eq(entity) | ResourceConstraint::In(entity) => {
            entity.type_name().to_string() == resource_type
        }
    };
    let accepts = |id: &cedar_policy::EntityUid| {
        id.type_name().to_string() == "SecCore::Action" && actions.contains(&id.id().as_ref())
    };
    matches_resource
        && match action {
            ActionConstraint::Any => true,
            ActionConstraint::Eq(id) => accepts(id),
            ActionConstraint::In(ids) => ids.iter().any(accepts),
        }
}

/// An adapter's decision for a single authored statement.
pub enum Placement<'a> {
    /// This adapter has no use for the statement.
    Ignore,
    /// The complete subject establishes a deployment group for this statement.
    Bind(&'a SubjectSelector),
    /// Include in existing groups that this subject could cover (all groups when unknown).
    Include(Option<&'a SubjectSelector>),
    /// Relevant statement without a usable automatic target.
    Unassigned(String),
}

/// Registered by the daemon; PAP groups the results without interpreting backend policy.
pub struct DeploymentSelector {
    pub backend: DeploymentBackend,
    pub select: for<'a> fn(&'a StatementTarget) -> Placement<'a>,
}

/// Read source identities, subject predicates, and action/resource headers once per Policy.
/// # Errors
/// Rejects source that cannot be validated or represented in the Cedar syntax tree.
pub fn statements(source: &CedarPolicySource) -> Result<Vec<StatementTarget>, CedarError> {
    let (policies, _) = crate::validation::parse_and_validate(source)?;
    policies
        .policies()
        .map(|policy| {
            let id = policy.id().to_string();
            let cedar_policy::pst::Policy::Static(statement) = policy
                .to_pst()
                .map_err(|e| CedarError::Representation(Box::new(e.into())))?
            else {
                return Err(CedarError::Unsupported {
                    policy_id: id,
                    reason: "deployment requires a static policy",
                });
            };
            let credential_rule = policy
                .annotations()
                .any(|(name, _)| crate::http::is_annotation(name))
                .then(|| {
                    policy
                        .annotation("id")
                        .map_or_else(|| id.clone(), str::to_owned)
                });
            Ok(StatementTarget {
                id,
                subject: split_subject(&statement).map(|(subject, _)| subject),
                action: policy.action_constraint(),
                resource: policy.resource_constraint(),
                credential_rule,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use asc_policy_types::{
        cedar::CedarSchemaReference,
        identifiers::{ResourceId, Revision},
        policy::DeploymentBackend,
    };

    #[test]
    fn embedded_resource_types_and_actions_have_no_cross_type_hierarchy() {
        for revision in [1, 2, 3] {
            let reference = CedarSchemaReference {
                schema_id: ResourceId::new(crate::schema::SCHEMA_ID).unwrap(),
                revision: Revision::new(revision).unwrap(),
            };
            let schema = crate::schema::load_schema(&reference).unwrap();
            assert_eq!(schema.action_groups().count(), 0);
            for ty in schema.entity_types() {
                assert!(schema.ancestors(ty).unwrap().all(|ancestor| ancestor == ty));
            }
        }
    }

    #[test]
    fn selects_only_the_exact_source_rule_and_keeps_its_resource_condition() {
        let source=CedarPolicySource{schema:CedarSchemaReference{schema_id:ResourceId::new(crate::schema::SCHEMA_ID).unwrap(),revision:Revision::new(3).unwrap()},
            text:r#"forbid(principal is SecCore::Subject,action == SecCore::Action::"file.read",resource is SecCore::File)
              when {principal has agentType && principal.agentType == "qoder" && resource.path == "/work/A"};
            forbid(principal is SecCore::Subject,action,resource == SecCore::HttpEndpoint::"remote")
              when {principal has agentType && principal.agentType == "qoder"};
            forbid(principal is SecCore::Subject,action == SecCore::Action::"file.read",resource is SecCore::File)
              when {(principal has agentType && principal.agentType == "qoder") ||
                    (principal has ownerUid && principal.ownerUid == 1000)};"#.into()};
        let rules = statements(&source).unwrap();
        let http = rules.iter().find(|rule| rule.id == "policy1").unwrap();
        assert!(!http.may_match(&["file.read"], "SecCore::File"));
        assert!(http.may_match(&["network.send"], "SecCore::HttpEndpoint"));
        assert!(
            rules
                .iter()
                .find(|r| r.id == "policy2")
                .unwrap()
                .subject
                .is_err()
        );
        let selected = selected_pst(
            &source,
            &DeploymentSelection {
                backend: DeploymentBackend::ActPlane,
                statement_ids: ["policy0".into()].into(),
            },
        )
        .unwrap();
        let body = selected.policies.values().next().unwrap().body();
        assert_eq!(body.principal, PrincipalConstraint::Any);
        assert_eq!(body.clauses().len(), 1);
        assert!(body.clauses()[0].to_string().contains("/work/A"));
        assert!(!body.clauses()[0].to_string().contains("agentType"));
    }
}
