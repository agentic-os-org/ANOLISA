//! Match server-persisted credential bindings to canonical Cedar rule identities.

use std::collections::BTreeMap;

use asc_credential_store::CredentialId;
use asc_policy_cedar::http::CredentialRule;
use asc_policy_types::policy::CredentialBinding;

use crate::PlanError;

pub(super) fn resolve_bindings<'a>(
    credential_bindings: &'a [CredentialBinding],
    rules: &[CredentialRule],
    selected_only: bool,
) -> Result<BTreeMap<&'a str, CredentialId>, PlanError> {
    let invalid = || PlanError::Invalid {
        field: "credentialBindings",
        reason: "must bind every Cedar rule identity exactly once to a valid persisted pair ID",
    };
    if !selected_only && credential_bindings.len() != rules.len() {
        return Err(invalid());
    }
    let mut bindings = BTreeMap::new();
    // An automatic Scope consumes a subset of the associations on the source snapshot.
    for binding in credential_bindings
        .iter()
        .filter(|binding| rules.iter().any(|rule| rule.rule_id == binding.rule_id))
    {
        let pair_id = CredentialId::new(binding.pair_id.clone()).map_err(|_| invalid())?;
        if bindings.insert(binding.rule_id.as_str(), pair_id).is_some() {
            return Err(invalid());
        }
    }
    if bindings.len() != rules.len() {
        return Err(invalid());
    }
    Ok(bindings)
}
