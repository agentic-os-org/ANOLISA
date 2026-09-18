//! `AgentSight` request preparation and target result classification.

use std::path::Path;

use asc_policy_types::identifiers::{ResourceId, Revision};
use asc_policy_types::target::TargetBindingPlan;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::process::{ProcProcessIdentityResolver, ProcessIdentityError, ProcessIdentityResolver};
use crate::transport::{
    AgentSightClientConfigError, AgentSightHttpMethod, AgentSightHttpRequest,
    AgentSightHttpResponse, AgentSightTransport, AgentSightTransportError, UreqAgentSightTransport,
};
use crate::{
    DEFAULT_AGENTSIGHT_BASE_URL, DEFAULT_AGENTSIGHT_TOKEN_FILE, ENFORCEMENT_BINDINGS_PATH,
    ENFORCEMENT_HEALTH_PATH,
};

mod reconciliation;
pub use reconciliation::{
    AGENTSIGHT_PREPARED_APPLY_FORMAT, AgentSightClientFactory, DEFAULT_AGENTSIGHT_ROUTE,
};

const BINDING_PLAN_FORMAT: &str = "agentsight.actplane.binding.v1";
const BINDING_PLAN_SCHEMA_VERSION: u16 = 1;
const ACTPLANE_POLICY_MEDIA_TYPE: &str = "application/vnd.actplane.dsl.v1";
const MAX_PLAN_BYTES: usize = 1024 * 1024;
const BINDING_ID_NAME_PREFIX: &str = "urn:agentseccore:agentsight-binding:";

/// Stable classification of an `AgentSight` Client failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AgentSightClientErrorKind {
    /// The same desired operation may be attempted again.
    #[error("retryable")]
    Retryable,
    /// The request cannot be accepted without changing the desired operation.
    #[error("rejected")]
    Rejected,
}

/// Stable, sanitized failure returned by the `AgentSight` Client.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind} AgentSight deployment failure: {code}")]
pub struct AgentSightClientError {
    /// Controls whether callers may retry the same desired operation.
    pub kind: AgentSightClientErrorKind,
    /// Stable machine-readable code safe for status projection.
    pub code: String,
}

/// Target state confirmed by one successful Client operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentSightDeploymentState {
    /// `AgentSight` confirmed that the deployment is effective.
    Present,
    /// `AgentSight` confirmed that the deployment is absent.
    Absent,
}

/// Side-effecting Client for one configured `AgentSight` endpoint.
pub struct AgentSightClient<T, R> {
    transport: T,
    process_identity: R,
    reconcile_route: Option<String>,
}

impl<T, R> AgentSightClient<T, R> {
    /// Creates a Client from independently testable transport and process ports.
    pub const fn with_dependencies(transport: T, process_identity: R) -> Self {
        Self {
            transport,
            process_identity,
            reconcile_route: None,
        }
    }
}

impl AgentSightClient<UreqAgentSightTransport, ProcProcessIdentityResolver> {
    /// Creates the production Client using the documented local API and token file.
    ///
    /// # Errors
    /// Returns a sanitized configuration error if the endpoint or credential is invalid.
    pub fn new_default() -> Result<Self, AgentSightClientConfigError> {
        Self::new_with_token_file(DEFAULT_AGENTSIGHT_BASE_URL, DEFAULT_AGENTSIGHT_TOKEN_FILE)
    }

    /// Creates the production Client using an explicit API root and token file.
    ///
    /// # Errors
    /// Returns a sanitized configuration error if the endpoint or credential is invalid.
    pub fn new_with_token_file(
        base_url: &str,
        token_file: impl AsRef<Path>,
    ) -> Result<Self, AgentSightClientConfigError> {
        let transport = UreqAgentSightTransport::from_token_file(base_url, token_file)?;
        Ok(Self::with_dependencies(
            transport,
            ProcProcessIdentityResolver,
        ))
    }
}

impl<T, R> AgentSightClient<T, R>
where
    T: AgentSightTransport,
    R: ProcessIdentityResolver,
{
    fn post_request(
        &self,
        request_body: &ApplyBindingRequest,
        body: Vec<u8>,
    ) -> Result<AgentSightDeploymentState, AgentSightClientError> {
        let response = self.send(&AgentSightHttpRequest {
            method: AgentSightHttpMethod::Post,
            path: ENFORCEMENT_BINDINGS_PATH.to_owned(),
            body: Some(body),
        })?;
        if !(200..300).contains(&response.status) {
            return Err(classify_http_error(response.status, &response.body));
        }
        let binding: EnforcementBinding = serde_json::from_slice(&response.body)
            .map_err(|_| retryable("AGENTSIGHT_INVALID_APPLY_RESPONSE"))?;
        if &binding.request != request_body {
            return Err(retryable("AGENTSIGHT_APPLY_RESPONSE_MISMATCH"));
        }

        match (binding.state, binding.domain_id) {
            (EnforcementState::Enforced, Some(_)) => Ok(AgentSightDeploymentState::Present),
            (EnforcementState::Pending | EnforcementState::Degraded, _) => {
                Err(retryable("AGENTSIGHT_DEPLOYMENT_NOT_READY"))
            }
            (EnforcementState::Enforced, None) => {
                Err(retryable("AGENTSIGHT_INVALID_APPLY_RESPONSE"))
            }
            (
                EnforcementState::Failed | EnforcementState::Detaching | EnforcementState::Detached,
                _,
            ) => Err(rejected("AGENTSIGHT_DEPLOYMENT_REJECTED")),
        }
    }

    fn delete_target_id(
        &self,
        target_binding_id: Uuid,
    ) -> Result<AgentSightDeploymentState, AgentSightClientError> {
        let response = self.send(&AgentSightHttpRequest {
            method: AgentSightHttpMethod::Delete,
            path: format!("{ENFORCEMENT_BINDINGS_PATH}/{target_binding_id}"),
            body: None,
        })?;

        if response.status == 204
            || (response.status == 404
                && remote_error_code(&response.body).as_deref() == Some("binding_not_found"))
        {
            return Ok(AgentSightDeploymentState::Absent);
        }
        if (200..300).contains(&response.status) {
            return Err(retryable("AGENTSIGHT_INVALID_DELETE_RESPONSE"));
        }
        Err(classify_http_error(response.status, &response.body))
    }
}

impl<T, R> AgentSightClient<T, R>
where
    T: AgentSightTransport,
{
    fn require_file_delete_capability(&self) -> Result<(), AgentSightClientError> {
        let response = self.send(&AgentSightHttpRequest {
            method: AgentSightHttpMethod::Get,
            path: ENFORCEMENT_HEALTH_PATH.to_owned(),
            body: None,
        })?;
        if !(200..300).contains(&response.status) {
            return Err(classify_http_error(response.status, &response.body));
        }
        let health: EnforcementHealth = serde_json::from_slice(&response.body)
            .map_err(|_| retryable("AGENTSIGHT_INVALID_HEALTH_RESPONSE"))?;
        if !health.ready {
            return Err(retryable("AGENTSIGHT_BACKEND_NOT_READY"));
        }
        if health.backend != "actplane" {
            return Err(rejected("AGENTSIGHT_UNSUPPORTED_BACKEND"));
        }
        if !health.capabilities.file_delete_guard {
            return Err(rejected("AGENTSIGHT_FILE_DELETE_GUARD_UNAVAILABLE"));
        }
        Ok(())
    }

    fn send(
        &self,
        request: &AgentSightHttpRequest,
    ) -> Result<AgentSightHttpResponse, AgentSightClientError> {
        self.transport.send(request).map_err(|error| match error {
            AgentSightTransportError::InvalidRequest => rejected("AGENTSIGHT_INVALID_HTTP_REQUEST"),
            AgentSightTransportError::Unavailable => {
                // A modifying request may already have taken effect. The
                // reconciliation port reports Unknown and retains its identity.
                retryable("AGENTSIGHT_TRANSPORT_UNAVAILABLE")
            }
            AgentSightTransportError::ResponseTooLarge => {
                retryable("AGENTSIGHT_RESPONSE_TOO_LARGE")
            }
        })
    }
}

#[derive(Debug, Deserialize)]
struct EnforcementHealth {
    ready: bool,
    backend: String,
    capabilities: EnforcementCapabilities,
}

#[derive(Debug, Deserialize)]
struct EnforcementCapabilities {
    file_delete_guard: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ApplyBindingRequest {
    binding_id: String,
    agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    root_pid: i32,
    process_start_time: u64,
    policy_id: String,
    policy_revision: String,
    policy_dsl: String,
    policy_mode: String,
}

#[derive(Debug, Deserialize)]
struct EnforcementBinding {
    request: ApplyBindingRequest,
    state: EnforcementState,
    domain_id: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum EnforcementState {
    Pending,
    Enforced,
    Failed,
    Degraded,
    Detaching,
    Detached,
}

#[derive(Debug, Deserialize)]
struct RemoteErrorEnvelope {
    error: RemoteError,
}

#[derive(Debug, Deserialize)]
struct RemoteError {
    code: String,
    #[serde(default)]
    retryable: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentSightBindingPlan {
    schema_version: u16,
    source: AgentSightSourceBinding,
    policy: AgentSightPolicyPlan,
    scope: AgentSightScopePlan,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentSightSourceBinding {
    binding_id: ResourceId,
    binding_revision: Revision,
    policy_id: ResourceId,
    policy_revision: Revision,
    scope_id: ResourceId,
    // Earlier v1 plans omitted this informational field.
    #[serde(default)]
    scope_revision: Option<Revision>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentSightPolicyPlan {
    media_type: String,
    content: String,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum AgentSightScopePlan {
    ProcessTree { root_pid: i32 },
}

struct DecodedPlan {
    binding_id: ResourceId,
    binding_revision: Revision,
    target_binding_id: Uuid,
    policy_id: ResourceId,
    policy_revision: Revision,
    root_pid: i32,
    policy_dsl: String,
}

impl DecodedPlan {
    fn request(&self, process_start_time: u64) -> ApplyBindingRequest {
        ApplyBindingRequest {
            binding_id: self.target_binding_id.to_string(),
            // No Agent identity is supplied by the product Binding. The generic
            // AgentSight endpoint accepts an empty attribution string.
            agent_id: String::new(),
            session_id: None,
            root_pid: self.root_pid,
            process_start_time,
            policy_id: self.policy_id.to_string(),
            policy_revision: self.policy_revision.get().to_string(),
            policy_dsl: self.policy_dsl.clone(),
            policy_mode: "enforce".to_owned(),
        }
    }
}

fn decode_plan(plan: &TargetBindingPlan) -> Result<DecodedPlan, AgentSightClientError> {
    if plan.format != BINDING_PLAN_FORMAT {
        return Err(rejected("AGENTSIGHT_UNSUPPORTED_PLAN_FORMAT"));
    }
    if plan.content.len() > MAX_PLAN_BYTES {
        return Err(rejected("AGENTSIGHT_INVALID_PLAN"));
    }
    let plan: AgentSightBindingPlan =
        serde_json::from_slice(&plan.content).map_err(|_| rejected("AGENTSIGHT_INVALID_PLAN"))?;
    if plan.schema_version != BINDING_PLAN_SCHEMA_VERSION {
        return Err(rejected("AGENTSIGHT_UNSUPPORTED_PLAN_SCHEMA"));
    }
    let AgentSightSourceBinding {
        binding_id,
        binding_revision,
        policy_id,
        policy_revision,
        scope_id: _scope_id,
        scope_revision: _scope_revision,
    } = plan.source;
    if plan.policy.media_type != ACTPLANE_POLICY_MEDIA_TYPE || plan.policy.content.is_empty() {
        return Err(rejected("AGENTSIGHT_UNSUPPORTED_POLICY_ARTIFACT"));
    }
    let AgentSightScopePlan::ProcessTree { root_pid } = plan.scope;
    if root_pid <= 0 {
        return Err(rejected("AGENTSIGHT_INVALID_SCOPE"));
    }
    Ok(DecodedPlan {
        target_binding_id: target_binding_id(&binding_id, binding_revision),
        binding_id,
        binding_revision,
        policy_id,
        policy_revision,
        root_pid,
        policy_dsl: plan.policy.content,
    })
}

fn target_binding_id(binding_id: &ResourceId, binding_revision: Revision) -> Uuid {
    let name = format!(
        "{BINDING_ID_NAME_PREFIX}{binding_id}:revision:{}",
        binding_revision.get()
    );
    Uuid::new_v5(&Uuid::NAMESPACE_URL, name.as_bytes())
}

fn classify_process_identity_error(error: ProcessIdentityError) -> AgentSightClientError {
    match error {
        ProcessIdentityError::InvalidPid | ProcessIdentityError::Malformed => {
            rejected("AGENTSIGHT_INVALID_PROCESS_IDENTITY")
        }
        ProcessIdentityError::Unavailable => retryable("AGENTSIGHT_PROCESS_IDENTITY_UNAVAILABLE"),
        ProcessIdentityError::Exited => rejected("AGENTSIGHT_PROCESS_EXITED"),
    }
}

fn classify_http_error(status: u16, body: &[u8]) -> AgentSightClientError {
    if status == 401 {
        return rejected("AGENTSIGHT_AUTHENTICATION_FAILED");
    }
    let error = remote_error(body);
    let is_retryable = error.as_ref().is_some_and(|error| error.retryable)
        || status == 429
        || (500..600).contains(&status);
    let code = error
        .and_then(|error| normalize_remote_code(&error.code))
        .unwrap_or_else(|| format!("AGENTSIGHT_HTTP_{status}"));
    AgentSightClientError {
        kind: if is_retryable {
            AgentSightClientErrorKind::Retryable
        } else {
            AgentSightClientErrorKind::Rejected
        },
        code,
    }
}

fn remote_error(body: &[u8]) -> Option<RemoteError> {
    serde_json::from_slice::<RemoteErrorEnvelope>(body)
        .ok()
        .map(|envelope| envelope.error)
}

fn remote_error_code(body: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct Envelope {
        error: Code,
    }
    #[derive(Deserialize)]
    struct Code {
        code: String,
    }
    serde_json::from_slice::<Envelope>(body)
        .ok()
        .map(|envelope| envelope.error.code)
}

fn normalize_remote_code(code: &str) -> Option<String> {
    if code.is_empty()
        || code.len() > 64
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        || !code.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
    {
        return None;
    }
    Some(format!("AGENTSIGHT_{}", code.to_ascii_uppercase()))
}

fn retryable(code: &str) -> AgentSightClientError {
    AgentSightClientError {
        kind: AgentSightClientErrorKind::Retryable,
        code: code.to_owned(),
    }
}

fn rejected(code: &str) -> AgentSightClientError {
    AgentSightClientError {
        kind: AgentSightClientErrorKind::Rejected,
        code: code.to_owned(),
    }
}
