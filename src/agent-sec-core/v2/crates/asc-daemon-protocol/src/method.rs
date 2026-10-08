//! Closed daemon method inventory and access metadata.
//!
//! PAP administration requires a Policy administrator. Action capabilities,
//! observability ingestion, and read-only queries accept any authenticated
//! local peer; the query family is additionally row-scoped to the caller's
//! owner by the handler. The closed inventory rejects unregistered methods
//! before authorization rather than defaulting into a family.

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
/// Create an immutable assignment with exact Policy snapshots.
pub const POLICY_SCOPES_CREATE: &str = "policy.scopes.create";
/// Retry failed work owned by one Scope.
pub const POLICY_SCOPES_RETRY: &str = "policy.scopes.retry";
/// Read an assignment and its lifecycle status.
pub const POLICY_SCOPES_GET: &str = "policy.scopes.get";
/// List current Scopes.
pub const POLICY_SCOPES_LIST: &str = "policy.scopes.list";
/// Stop discovery and request cleanup of all owned Bindings.
pub const POLICY_SCOPES_DELETE: &str = "policy.scopes.delete";
/// Read one current Binding spec and lifecycle status.
pub const POLICY_BINDINGS_GET: &str = "policy.bindings.get";
/// List current Bindings and lifecycle statuses.
pub const POLICY_BINDINGS_LIST: &str = "policy.bindings.list";

/// Append one observability record using `OTel` attribution.
pub const OBS_RECORD: &str = "obs.record";

/// Scan one Bash or Python snippet for pre-execution security issues.
pub const ACTION_CODE_SCAN: &str = "action.code_scan";
/// Detect personal information and credentials without authorizing an operation.
pub const ACTION_PII_SCAN: &str = "action.pii_scan";
/// Scan one prompt for injection or jailbreak attempts before it reaches a model.
pub const ACTION_PROMPT_SCAN: &str = "action.prompt_scan";
/// Probe that the models a prompt-scan mode requires are ready to serve.
pub const ACTION_PROMPT_SCAN_WARMUP: &str = "action.prompt_scan.warmup";
/// Manage Skill scanning, integrity, history and activation.
pub const ACTION_SKILL_SEC: &str = "action.skill_sec";

/// Return the dashboard summary of the caller's own security events.
pub const SEC_SUMMARY: &str = "sec.summary";
/// List the caller's own security events, newest first.
pub const SEC_EVENTS_LIST: &str = "sec.events.list";
/// Return one of the caller's own security events by id.
pub const SEC_EVENTS_GET: &str = "sec.events.get";
/// Count the caller's own security events grouped by one field.
pub const SEC_EVENTS_COUNT_BY: &str = "sec.events.count_by";

/// Complete PAP method inventory for this protocol version.
pub const PAP_METHODS: [&str; 12] = [
    POLICY_TEMPLATES_CREATE,
    POLICY_TEMPLATES_UPDATE,
    POLICY_TEMPLATES_GET,
    POLICY_TEMPLATES_LIST,
    POLICY_TEMPLATES_DELETE,
    POLICY_SCOPES_CREATE,
    POLICY_SCOPES_RETRY,
    POLICY_SCOPES_GET,
    POLICY_SCOPES_LIST,
    POLICY_SCOPES_DELETE,
    POLICY_BINDINGS_GET,
    POLICY_BINDINGS_LIST,
];

/// Complete Action-capability method inventory for this protocol version.
pub const ACTION_METHODS: [&str; 5] = [
    ACTION_CODE_SCAN,
    ACTION_PII_SCAN,
    ACTION_PROMPT_SCAN,
    ACTION_PROMPT_SCAN_WARMUP,
    ACTION_SKILL_SEC,
];

/// Complete read-only query method inventory for this protocol version.
///
/// The v1 observability query methods (`obs.sessions.list`, `obs.runs.list`,
/// `obs.timeline.get`) stay outside this inventory until the observability
/// stream carries an owner column, because exposing them without one would
/// turn the system daemon into a cross-UID reader (issue #6608).
pub const QUERY_METHODS: [&str; 4] = [
    SEC_SUMMARY,
    SEC_EVENTS_LIST,
    SEC_EVENTS_GET,
    SEC_EVENTS_COUNT_BY,
];

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
    /// Retry failed owned work.
    Retry,
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
    /// Get current state.
    Get,
    /// List current state.
    List,
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
    /// PII and credential scan.
    PiiScan,
    /// Pre-execution prompt scan.
    PromptScan,
    /// Readiness probe for the models a prompt-scan mode requires.
    PromptScanWarmup,
    /// `SkillSec` core operations.
    SkillSec,
}

/// One read-only security-event query resolved from its exact wire method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryMethod {
    /// Dashboard aggregates plus the newest rows.
    Summary,
    /// Paginated rows, newest first.
    EventsList,
    /// One row by id, scoped to the caller.
    EventsGet,
    /// Grouped counts over the caller's rows.
    EventsCountBy,
}

/// Closed daemon method identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodId {
    /// PAP administration method.
    Pap(PapMethod),
    /// Action capability method.
    Action(ActionMethod),
    /// Single-record observability ingestion.
    ObservabilityRecord,
    /// Read-only owner-scoped query method.
    Query(QueryMethod),
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
            // Action capabilities, observability ingestion, and read-only
            // queries share one access policy: any kernel-authenticated local
            // peer. The query family adds row-level owner scoping inside its
            // handler, derived from the transport-authenticated principal
            // rather than from the request.
            Self::Action(_) | Self::ObservabilityRecord | Self::Query(_) => Metadata {
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
        POLICY_SCOPES_RETRY => Some(MethodId::Pap(PapMethod::Scope(ScopeMethod::Retry))),
        POLICY_SCOPES_GET => Some(MethodId::Pap(PapMethod::Scope(ScopeMethod::Get))),
        POLICY_SCOPES_LIST => Some(MethodId::Pap(PapMethod::Scope(ScopeMethod::List))),
        POLICY_SCOPES_DELETE => Some(MethodId::Pap(PapMethod::Scope(ScopeMethod::Delete))),
        POLICY_BINDINGS_GET => Some(MethodId::Pap(PapMethod::Binding(BindingMethod::Get))),
        POLICY_BINDINGS_LIST => Some(MethodId::Pap(PapMethod::Binding(BindingMethod::List))),
        OBS_RECORD => Some(MethodId::ObservabilityRecord),
        ACTION_CODE_SCAN => Some(MethodId::Action(ActionMethod::CodeScan)),
        ACTION_PII_SCAN => Some(MethodId::Action(ActionMethod::PiiScan)),
        ACTION_PROMPT_SCAN => Some(MethodId::Action(ActionMethod::PromptScan)),
        ACTION_PROMPT_SCAN_WARMUP => Some(MethodId::Action(ActionMethod::PromptScanWarmup)),
        ACTION_SKILL_SEC => Some(MethodId::Action(ActionMethod::SkillSec)),
        SEC_SUMMARY => Some(MethodId::Query(QueryMethod::Summary)),
        SEC_EVENTS_LIST => Some(MethodId::Query(QueryMethod::EventsList)),
        SEC_EVENTS_GET => Some(MethodId::Query(QueryMethod::EventsGet)),
        SEC_EVENTS_COUNT_BY => Some(MethodId::Query(QueryMethod::EventsCountBy)),
        _ => None,
    }
}

/// Returns metadata for an exact registered method.
pub fn metadata(method: &str) -> Option<Metadata> {
    resolve(method).map(MethodId::metadata)
}
