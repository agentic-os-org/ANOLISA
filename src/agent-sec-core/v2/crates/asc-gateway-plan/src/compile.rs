//! Compile immutable Scope policy snapshots into credential gateway rules.

use std::collections::BTreeSet;

use asc_credential_store::ScopeId;
use asc_policy_types::Validate;
use asc_policy_types::authoring::PolicySource as AuthoredPolicySource;
use asc_policy_types::identifiers::{ResourceId, Revision};
use asc_policy_types::policy::CredentialBinding;
use asc_policy_types::scope::PreparedScope;

use crate::{
    GatewayBindingPlan, GatewayBindingView, Plan, PlanError, PlanFormat, PolicySource,
    RequestMatch, Rule, RuleSource, Scheme, credential::resolve_bindings,
};

/// Compiles Cedar credential Policies, skipping independent non-credential file Policies.
///
/// # Errors
/// Rejects invalid snapshots, overlapping or unconsumed HTTP semantics, and invalid bindings.
pub fn compile_scope(scope: &PreparedScope) -> Result<Plan, PlanError> {
    scope.validate()?;
    let scope_id = ScopeId::new(scope.scope_id.as_str()).map_err(|_| PlanError::Invalid {
        field: "scopeId",
        reason: "must be a valid scope identifier",
    })?;
    let mut snapshots: Vec<_> = scope.policy_snapshots.iter().collect();
    snapshots.sort_by(|a, b| a.policy_id.cmp(&b.policy_id));
    let mut credentials = BTreeSet::new();
    let mut policies = Vec::new();
    let mut rules = Vec::new();
    for policy in snapshots {
        let authored = match &policy.template {
            AuthoredPolicySource::Cedar(source) => {
                let syntax = asc_policy_cedar::pst(source)?;
                let network =
                    asc_policy_cedar::project_pst(&syntax, asc_policy_cedar::PolicyDomain::Network);
                match network {
                    Some(view) => asc_policy_cedar::http::lower(&view, &source.schema)?,
                    None => Vec::new(),
                }
            }
            AuthoredPolicySource::SecCore(template) => {
                if template.uses_credentials() || !policy.credential_bindings.is_empty() {
                    return Err(PlanError::Invalid {
                        field: "template",
                        reason: "credential gateway policies require Cedar source",
                    });
                }
                continue;
            }
        };
        let translated = policy_rules(
            &policy.policy_id,
            policy.revision,
            &policy.credential_bindings,
            authored,
        )?;
        for rule in &translated {
            credentials.insert(rule.matcher.credential_id.clone());
        }
        if translated.is_empty() {
            continue;
        }
        policies.push(PolicySource {
            id: policy.policy_id.clone(),
            revision: policy.revision,
        });
        rules.extend(translated);
    }
    let plan = Plan {
        format: PlanFormat::V2,
        scope_id,
        policies,
        credentials: credentials.into_iter().collect(),
        rules,
    };
    plan.validate()?;
    Ok(plan)
}

/// Translates an already validated network view of this exact immutable Binding.
///
/// # Errors
/// Rejects invalid Binding metadata, unsupported HTTP semantics and credential associations.
pub fn compile_binding(view: GatewayBindingView<'_>) -> Result<GatewayBindingPlan, PlanError> {
    view.source.scope.validate()?;
    let authored = asc_policy_cedar::http::lower(view.network, view.schema)?;
    let rules = policy_rules(
        &view.source.policy_id,
        view.source.policy_revision,
        view.credential_bindings,
        authored,
    )?;
    let plan = Plan {
        format: PlanFormat::V2,
        scope_id: ScopeId::new(view.source.scope.scope_id.as_str()).map_err(|_| {
            PlanError::Invalid {
                field: "scopeId",
                reason: "must be a valid scope identifier",
            }
        })?,
        policies: vec![PolicySource {
            id: view.source.policy_id.clone(),
            revision: view.source.policy_revision,
        }],
        credentials: rules
            .iter()
            .map(|rule| rule.matcher.credential_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        rules,
    };
    plan.validate()?;
    Ok(GatewayBindingPlan {
        source: view.source,
        network: plan,
    })
}

fn policy_rules(
    policy_id: &ResourceId,
    policy_revision: Revision,
    credential_bindings: &[CredentialBinding],
    authored: Vec<asc_policy_cedar::http::CredentialRule>,
) -> Result<Vec<Rule>, PlanError> {
    let bindings = resolve_bindings(credential_bindings, &authored)?;
    authored
        .into_iter()
        .enumerate()
        .map(|(rule_index, rule)| {
            let credential = bindings
                .get(rule.rule_id.as_str())
                .ok_or(PlanError::Invalid {
                    field: "credentialBindings",
                    reason: "missing persisted binding for Cedar rule identity",
                })?
                .clone();
            Ok(Rule {
                source: RuleSource {
                    policy_id: policy_id.clone(),
                    policy_revision,
                    rule_index,
                },
                matcher: RequestMatch {
                    scheme: Scheme::Https,
                    host: rule.host,
                    path: rule.path,
                    methods: rule.methods,
                    credential_id: credential,
                },
                header: rule.header,
                format: rule.format,
                response_rewrite: rule.response_rewrite,
                because: "Credential use allowed by policy".into(),
            })
        })
        .collect()
}
