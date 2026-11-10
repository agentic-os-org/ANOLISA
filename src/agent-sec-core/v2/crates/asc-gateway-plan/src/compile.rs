//! Compile immutable Scope policy snapshots into credential gateway rules.

use std::collections::BTreeSet;

use asc_credential_store::ScopeId;
use asc_policy_types::Validate;
use asc_policy_types::authoring::PolicySource as AuthoredPolicySource;
use asc_policy_types::scope::PreparedScope;

use crate::{
    Plan, PlanError, PlanFormat, PolicySource, RequestMatch, Rule, RuleSource, Scheme,
    credential::resolve_bindings,
};

/// Compiles Cedar credential Policies, skipping independent non-credential file Policies.
///
/// # Errors
/// Rejects invalid snapshots, mixed or unconsumed Cedar, and missing/extra/duplicate bindings.
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
            AuthoredPolicySource::Cedar(source) => asc_policy_cedar::credential_rules(source)?,
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
        let bindings = resolve_bindings(policy, &authored)?;
        if authored.is_empty() {
            continue;
        }
        policies.push(PolicySource {
            id: policy.policy_id.clone(),
            revision: policy.revision,
        });
        for (rule_index, rule) in authored.into_iter().enumerate() {
            let credential = bindings
                .get(rule.rule_id.as_str())
                .ok_or(PlanError::Invalid {
                    field: "credentialBindings",
                    reason: "missing persisted binding for Cedar rule identity",
                })?
                .clone();
            credentials.insert(credential.clone());
            rules.push(Rule {
                source: RuleSource {
                    policy_id: policy.policy_id.clone(),
                    policy_revision: policy.revision,
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
            });
        }
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
