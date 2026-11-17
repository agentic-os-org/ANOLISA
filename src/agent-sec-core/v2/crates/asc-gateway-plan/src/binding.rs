//! Network-only deployment input with the identity of its canonical Binding.

use asc_policy_types::binding::{BindingScope, PreparedBinding};
use asc_policy_types::cedar::CedarSchemaReference;
use asc_policy_types::identifiers::{ResourceId, Revision};
use asc_policy_types::policy::CredentialBinding;
use serde::{Deserialize, Serialize};

use crate::Plan;

/// Network plan and process attachment input accepted by the Gateway Client.
pub const GATEWAY_BINDING_FORMAT: &str = "agent-sec-gateway-binding/v2";

/// Validated network projection supplied to the Gateway Translator.
pub struct GatewayBindingView<'a> {
    /// Canonical identity and process context, without authored Policy text.
    pub source: GatewayBindingSource,
    /// Exact schema under which the PST was validated.
    pub schema: &'a CedarSchemaReference,
    /// Server-owned rule associations from the pinned Policy revision.
    pub credential_bindings: &'a [CredentialBinding],
    /// Network-only PST projected from that same Policy snapshot.
    pub network: &'a cedar_policy::pst::PolicySet,
}

/// Identity used to verify the immutable source without transporting mixed authoring policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayBindingSource {
    /// Canonical Binding identity.
    pub binding_id: ResourceId,
    /// Immutable Binding revision.
    pub binding_revision: Revision,
    /// Canonical Policy identity.
    pub policy_id: ResourceId,
    /// Pinned Policy revision.
    pub policy_revision: Revision,
    /// Scope selection and full process instance identity.
    pub scope: BindingScope,
}

impl GatewayBindingSource {
    /// Binds a translated view to its original immutable snapshot.
    pub fn from_binding(binding: &PreparedBinding) -> Self {
        Self {
            binding_id: binding.binding_id.clone(),
            binding_revision: binding.binding_revision,
            policy_id: binding.policy.policy_id.clone(),
            policy_revision: binding.policy.revision,
            scope: binding.scope.clone(),
        }
    }

    /// Checks identity and process context against the stored canonical Binding.
    pub fn matches(&self, binding: &PreparedBinding) -> bool {
        self.binding_id == binding.binding_id
            && self.binding_revision == binding.binding_revision
            && self.policy_id == binding.policy.policy_id
            && self.policy_revision == binding.policy.revision
            && self.scope == binding.scope
    }
}

/// One Binding's translated network contribution and its attachment context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewayBindingPlan {
    /// Original identity, without the authored file/network Policy text.
    pub source: GatewayBindingSource,
    /// Lowered network rules; Scope publication aggregates these contributions.
    pub network: Plan,
}
