//! Match server-persisted credential bindings to canonical Cedar rule identities.

use std::collections::BTreeMap;

use asc_credential_store::CredentialId;
use asc_policy_cedar::http::CredentialRule;
use asc_policy_types::policy::PreparedPolicy;

use crate::PlanError;

pub(super) fn resolve_bindings<'a>(
    policy: &'a PreparedPolicy,
    rules: &[CredentialRule],
) -> Result<BTreeMap<&'a str, CredentialId>, PlanError> {
    let invalid = || PlanError::Invalid {
        field: "credentialBindings",
        reason: "must bind every Cedar rule identity exactly once to a valid persisted pair ID",
    };
    if policy.credential_bindings.len() != rules.len() {
        return Err(invalid());
    }
    let mut bindings = BTreeMap::new();
    for binding in &policy.credential_bindings {
        let pair_id = CredentialId::new(binding.pair_id.clone()).map_err(|_| invalid())?;
        if !rules.iter().any(|rule| rule.rule_id == binding.rule_id)
            || bindings.insert(binding.rule_id.as_str(), pair_id).is_some()
        {
            return Err(invalid());
        }
    }
    Ok(bindings)
}
