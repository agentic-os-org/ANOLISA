//! Closed daemon method inventory and access metadata.
//!
//! Two method families exist: PAP administration, which requires a Policy
//! administrator, and Action capabilities, which any authenticated local peer
//! may call. Keeping both in one closed inventory means an unregistered method
//! is rejected before authorization rather than defaulting into a family.

/// Create one Policy identity from an authored template.
pub const POLICY_TEMPLATES_CREATE: &str = "policy.templates.create";
/// Update one existing Policy identity.
pub const POLICY_TEMPLATES_UPDATE: &str = "policy.templates.update";
/// Read one exact current Policy revision.
pub const POLICY_TEMPLATES_GET: &str = "policy.templates.get";
/// List current Policies.
pub const POLICY_TEMPLATES_LIST: &str = "policy.templates.list";
/// Delete one exact current Policy revision.
pub const POLICY_TEMPLATES_DELETE: &str = "policy.templates.delete";
/// Create one Scope identity from an authored selector.
pub const POLICY_SCOPES_CREATE: &str = "policy.scopes.create";
/// Update one existing Scope identity.
pub const POLICY_SCOPES_UPDATE: &str = "policy.scopes.update";
/// Read one exact current Scope revision.
pub const POLICY_SCOPES_GET: &str = "policy.scopes.get";
/// List current Scopes.
pub const POLICY_SCOPES_LIST: &str = "policy.scopes.list";
/// Delete one exact current Scope revision.
pub const POLICY_SCOPES_DELETE: &str = "policy.scopes.delete";
/// Create one Binding Apply intent.
pub const POLICY_BINDINGS_CREATE: &str = "policy.bindings.create";
/// Update one existing Binding and request Apply.
pub const POLICY_BINDINGS_UPDATE: &str = "policy.bindings.update";
/// Read one current Binding spec and lifecycle status.
pub const POLICY_BINDINGS_GET: &str = "policy.bindings.get";
/// List current Bindings and lifecycle statuses.
pub const POLICY_BINDINGS_LIST: &str = "policy.bindings.list";
/// Request deletion of one current Binding.
pub const POLICY_BINDINGS_DELETE: &str = "policy.bindings.delete";

/// Scan one Bash or Python snippet for pre-execution security issues.
pub const ACTION_CODE_SCAN: &str = "action.code_scan";

/// Complete PAP method inventory for this protocol version.
pub const PAP_METHODS: [&str; 15] = [
    POLICY_TEMPLATES_CREATE,
    POLICY_TEMPLATES_UPDATE,
    POLICY_TEMPLATES_GET,
    POLICY_TEMPLATES_LIST,
    POLICY_TEMPLATES_DELETE,
    POLICY_SCOPES_CREATE,
    POLICY_SCOPES_UPDATE,
    POLICY_SCOPES_GET,
    POLICY_SCOPES_LIST,
    POLICY_SCOPES_DELETE,
    POLICY_BINDINGS_CREATE,
    POLICY_BINDINGS_UPDATE,
    POLICY_BINDINGS_GET,
    POLICY_BINDINGS_LIST,
    POLICY_BINDINGS_DELETE,
];

/// Complete Action-capability method inventory for this protocol version.
pub const ACTION_METHODS: [&str; 1] = [ACTION_CODE_SCAN];

/// One Policy operation resolved from its exact wire method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyMethod {
    /// Create.
    Create,
    /// Update.
    Update,
    /// Get.
    Get,
    /// List.
    List,
    /// Delete.
    Delete,
}

/// One Scope operation resolved from its exact wire method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeMethod {
    /// Create.
    Create,
    /// Update.
    Update,
    /// Get.
    Get,
    /// List.
    List,
    /// Delete.
    Delete,
}

/// One Binding operation resolved from its exact wire method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingMethod {
    /// Create Apply intent.
    Create,
    /// Update and request Apply.
    Update,
    /// Get current state.
    Get,
    /// List current state.
    List,
    /// Request Delete.
    Delete,
}

/// One PAP operation resolved before parameter decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PapMethod {
    /// Policy operation.
    Policy(PolicyMethod),
    /// Scope operation.
    Scope(ScopeMethod),
    /// Binding operation.
    Binding(BindingMethod),
}

/// One Action-capability operation resolved from its exact wire method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionMethod {
    /// Pre-execution code scan.
    CodeScan,
}

/// Closed daemon method identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodId {
    /// PAP administration method.
    Pap(PapMethod),
    /// Action capability method.
    Action(ActionMethod),
}

/// Server-owned access policy for a method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessPolicy {
    /// Requires a server-assigned Policy administrator principal.
    PolicyAdministrator,
    /// Requires only a kernel-authenticated local peer, of any role.
    ///
    /// The code scanner is an advisory pre-execution gate an agent consults for
    /// itself; gating it behind Policy administration would put it out of reach
    /// of the very callers it exists to serve. Peer authentication by the
    /// transport is still required — this is not an anonymous method.
    LocalUser,
}

/// Static method metadata used by authorization before application dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metadata {
    /// Required server-owned access policy.
    pub access: AccessPolicy,
}

impl MethodId {
    /// Returns authorization metadata for this exact method.
    pub const fn metadata(self) -> Metadata {
        match self {
            Self::Pap(_) => Metadata {
                access: AccessPolicy::PolicyAdministrator,
            },
            Self::Action(_) => Metadata {
                access: AccessPolicy::LocalUser,
            },
        }
    }
}

/// Resolves an exact wire method without inspecting its parameters.
pub fn resolve(method: &str) -> Option<MethodId> {
    match method {
        POLICY_TEMPLATES_CREATE => Some(MethodId::Pap(PapMethod::Policy(PolicyMethod::Create))),
        POLICY_TEMPLATES_UPDATE => Some(MethodId::Pap(PapMethod::Policy(PolicyMethod::Update))),
        POLICY_TEMPLATES_GET => Some(MethodId::Pap(PapMethod::Policy(PolicyMethod::Get))),
        POLICY_TEMPLATES_LIST => Some(MethodId::Pap(PapMethod::Policy(PolicyMethod::List))),
        POLICY_TEMPLATES_DELETE => Some(MethodId::Pap(PapMethod::Policy(PolicyMethod::Delete))),
        POLICY_SCOPES_CREATE => Some(MethodId::Pap(PapMethod::Scope(ScopeMethod::Create))),
        POLICY_SCOPES_UPDATE => Some(MethodId::Pap(PapMethod::Scope(ScopeMethod::Update))),
        POLICY_SCOPES_GET => Some(MethodId::Pap(PapMethod::Scope(ScopeMethod::Get))),
        POLICY_SCOPES_LIST => Some(MethodId::Pap(PapMethod::Scope(ScopeMethod::List))),
        POLICY_SCOPES_DELETE => Some(MethodId::Pap(PapMethod::Scope(ScopeMethod::Delete))),
        POLICY_BINDINGS_CREATE => Some(MethodId::Pap(PapMethod::Binding(BindingMethod::Create))),
        POLICY_BINDINGS_UPDATE => Some(MethodId::Pap(PapMethod::Binding(BindingMethod::Update))),
        POLICY_BINDINGS_GET => Some(MethodId::Pap(PapMethod::Binding(BindingMethod::Get))),
        POLICY_BINDINGS_LIST => Some(MethodId::Pap(PapMethod::Binding(BindingMethod::List))),
        POLICY_BINDINGS_DELETE => Some(MethodId::Pap(PapMethod::Binding(BindingMethod::Delete))),
        ACTION_CODE_SCAN => Some(MethodId::Action(ActionMethod::CodeScan)),
        _ => None,
    }
}

/// Returns metadata for an exact registered method.
pub fn metadata(method: &str) -> Option<Metadata> {
    resolve(method).map(MethodId::metadata)
}
