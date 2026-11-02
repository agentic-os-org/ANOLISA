//! Compile immutable Scope policy snapshots into credential gateway rules.

use std::collections::BTreeSet;

use asc_credential_store::{CredentialId, ScopeId};
use asc_policy_types::Validate;
use asc_policy_types::authoring::{PolicySource as AuthoredPolicySource, Resource};
use asc_policy_types::scope::PreparedScope;

use crate::{Plan, PlanError, PlanFormat, PolicySource, RequestMatch, Rule, RuleSource};

/// Compiles all credential Policies, skipping independently validated file Policies.
///
/// # Errors
/// Returns validation errors without producing a partial plan.
pub fn compile_scope(scope: &PreparedScope) -> Result<Plan, PlanError> {
    scope.validate().map_err(PlanError::Policy)?;
    let scope_id = ScopeId::new(scope.scope_id.as_str()).map_err(|_| PlanError::Invalid {
        field: "scopeId",
        reason: "must be a lowercase DNS label",
    })?;
    let mut snapshots: Vec<_> = scope
        .policy_snapshots
        .iter()
        .filter_map(|policy| {
            let AuthoredPolicySource::SecCore(template) = &policy.template else {
                return None;
            };
            template.uses_credentials().then_some((policy, template))
        })
        .collect();
    snapshots.sort_by(|(left, _), (right, _)| left.policy_id.cmp(&right.policy_id));
    let mut credentials = BTreeSet::new();
    let mut policies = Vec::with_capacity(snapshots.len());
    let mut rules = Vec::new();
    for (policy, template) in snapshots {
        policies.push(PolicySource {
            id: policy.policy_id.clone(),
            revision: policy.revision,
        });
        for (rule_index, authored) in template.rules.iter().enumerate() {
            let Resource::HttpRequest {
                credential_id,
                scheme,
                host,
                path,
                methods,
                response_rewrite,
            } = &authored.target
            else {
                return Err(PlanError::Invalid {
                    field: "target",
                    reason: "credential rules require an http_request target",
                });
            };
            let credential =
                CredentialId::new(credential_id.clone()).map_err(|_| PlanError::Invalid {
                    field: "credentialId",
                    reason: "must be a lowercase DNS label",
                })?;
            credentials.insert(credential.clone());
            let mut methods = methods.clone();
            methods.sort_unstable();
            rules.push(Rule {
                source: RuleSource {
                    policy_id: policy.policy_id.clone(),
                    policy_revision: policy.revision,
                    rule_index,
                },
                matcher: RequestMatch {
                    scheme: *scheme,
                    host: host.clone(),
                    path: path.clone(),
                    methods,
                    credential_id: credential,
                },
                response_rewrite: *response_rewrite,
                because: authored
                    .because
                    .clone()
                    .unwrap_or_else(|| "Credential use allowed by policy".to_owned()),
            });
        }
    }
    let plan = Plan {
        format: PlanFormat::V1,
        scope_id,
        policies,
        credentials: credentials.into_iter().collect(),
        rules,
    };
    plan.validate()?;
    Ok(plan)
}
