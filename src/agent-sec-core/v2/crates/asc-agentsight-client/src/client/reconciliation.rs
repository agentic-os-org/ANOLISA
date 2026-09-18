//! PEP-specific preparation/replay and replacement. No repository dependency.

use asc_policy_target_contracts::{TargetDeploymentClient, TargetDeploymentClientFactory};
use asc_policy_types::target::{
    DeploymentReport, Failure, FailureKind, Observation, PreparedApply, Presence, TargetRef,
};
use sha2::{Digest, Sha256};

use asc_policy_types::identifiers::{ResourceId, Revision};
use asc_policy_types::target::TargetBindingPlan;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use super::{
    AgentSightClient, AgentSightClientError, AgentSightClientErrorKind, AgentSightDeploymentState,
    ApplyBindingRequest, MAX_PLAN_BYTES, classify_process_identity_error, decode_plan, rejected,
    retryable, target_binding_id,
};
use crate::{
    AgentSightClientConfigError, AgentSightTransport, ProcessIdentityError, ProcessIdentityResolver,
};

/// Registers the default PEP without reading credentials or contacting it.
/// Each attempt receives its own Client, including a fresh token-file read.
pub struct AgentSightClientFactory {
    base_url: String,
    token_file: std::path::PathBuf,
}

impl Default for AgentSightClientFactory {
    fn default() -> Self {
        Self::new(
            crate::DEFAULT_AGENTSIGHT_BASE_URL,
            crate::DEFAULT_AGENTSIGHT_TOKEN_FILE,
        )
    }
}

impl AgentSightClientFactory {
    /// Retains configuration without reading credentials or contacting the PEP.
    pub fn new(base_url: impl Into<String>, token_file: impl Into<std::path::PathBuf>) -> Self {
        Self {
            base_url: base_url.into(),
            token_file: token_file.into(),
        }
    }
}

impl TargetDeploymentClientFactory for AgentSightClientFactory {
    fn open(&self) -> Result<Arc<dyn TargetDeploymentClient>, Failure> {
        AgentSightClient::new_with_token_file(&self.base_url, &self.token_file)
            .map(|client| Arc::new(client) as Arc<dyn TargetDeploymentClient>)
            .map_err(configuration_failure)
    }
}

fn configuration_failure(error: AgentSightClientConfigError) -> Failure {
    let (kind, code) = match error {
        AgentSightClientConfigError::InvalidBaseUrl => {
            (FailureKind::Rejected, "AGENTSIGHT_INVALID_BASE_URL")
        }
        AgentSightClientConfigError::CredentialUnavailable => {
            (FailureKind::Retryable, "AGENTSIGHT_CREDENTIAL_UNAVAILABLE")
        }
        AgentSightClientConfigError::InvalidCredential => {
            (FailureKind::Retryable, "AGENTSIGHT_INVALID_CREDENTIAL")
        }
    };
    Failure::new(kind, code)
}

/// Versioned opaque prepared payload consumed only by this Client.
pub const AGENTSIGHT_PREPARED_APPLY_FORMAT: &str = "agentsight.enforcement.apply.v1";
/// Default stable configuration reference; never reuse it for a different PEP.
pub const DEFAULT_AGENTSIGHT_ROUTE: &str = "agentsight";
const MAX_REQUEST_BYTES: usize = 2 * MAX_PLAN_BYTES;
const MAX_PREPARED_BYTES: usize = 8 * MAX_REQUEST_BYTES + 4096;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Cleanup {
    schema_version: u16,
    binding_id: ResourceId,
    binding_revision: Revision,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreparedRequest {
    schema_version: u16,
    boot_id: String,
    request: Vec<u8>,
    request_digest: String,
}

impl<T, R> AgentSightClient<T, R> {
    /// Binds this Client to a stable, non-secret repository target reference.
    /// The composition root must keep its endpoint mapping stable while any
    /// deployment uses it. Changing endpoints requires a new route identity.
    /// # Errors
    /// Rejects empty, oversized or unsafe configuration reference strings.
    pub fn with_reconcile_route(
        mut self,
        route: impl Into<String>,
    ) -> Result<Self, AgentSightClientError> {
        let route = route.into();
        if route.is_empty()
            || route.len() > 128
            || !route
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        {
            return Err(rejected("AGENTSIGHT_INVALID_ROUTE"));
        }
        self.reconcile_route = Some(route);
        Ok(self)
    }

    fn route(&self) -> &str {
        self.reconcile_route
            .as_deref()
            .unwrap_or(DEFAULT_AGENTSIGHT_ROUTE)
    }

    fn decode_target(&self, target: &TargetRef) -> Result<Uuid, AgentSightClientError> {
        if target.route != self.route() || target.cleanup.len() > 4096 {
            return Err(rejected("AGENTSIGHT_INVALID_TARGET_REFERENCE"));
        }
        let cleanup: Cleanup = serde_json::from_slice(&target.cleanup)
            .map_err(|_| rejected("AGENTSIGHT_INVALID_TARGET_REFERENCE"))?;
        let id = target_binding_id(&cleanup.binding_id, cleanup.binding_revision);
        if cleanup.schema_version != 1 || target.id != id.to_string() {
            return Err(rejected("AGENTSIGHT_INVALID_TARGET_REFERENCE"));
        }
        Ok(id)
    }

    fn decode_prepared(
        &self,
        prepared: &PreparedApply,
    ) -> Result<(PreparedRequest, ApplyBindingRequest), AgentSightClientError> {
        let id = self.decode_target(&prepared.target)?;
        if prepared.format != AGENTSIGHT_PREPARED_APPLY_FORMAT
            || prepared.content.len() > MAX_PREPARED_BYTES
        {
            return Err(rejected("AGENTSIGHT_INVALID_PREPARED"));
        }
        let mut payload: PreparedRequest = serde_json::from_slice(&prepared.content)
            .map_err(|_| rejected("AGENTSIGHT_INVALID_PREPARED"))?;
        let boot = Uuid::parse_str(&payload.boot_id)
            .map_err(|_| rejected("AGENTSIGHT_INVALID_PREPARED"))?;
        if payload.schema_version != 1
            || payload.request.len() > MAX_REQUEST_BYTES
            || payload.request_digest != digest(&payload.request)
            || boot.is_nil()
        {
            return Err(rejected("AGENTSIGHT_INVALID_PREPARED"));
        }
        payload.boot_id = boot.to_string();
        let body: ApplyBindingRequest = serde_json::from_slice(&payload.request)
            .map_err(|_| rejected("AGENTSIGHT_INVALID_PREPARED"))?;
        // Round-trip validation rejects unknown/duplicate fields and noncanonical
        // encodings. Transmission still uses the exact saved bytes, not this value.
        if body.binding_id != id.to_string()
            || body.root_pid <= 0
            || body.process_start_time == 0
            || body.policy_mode != "enforce"
            || body.policy_dsl.is_empty()
            || body.session_id.is_some()
            // New requests leave unknown Agent attribution empty. Existing
            // prepared requests retain their original attribution and bytes.
            || (!body.agent_id.is_empty() && ResourceId::new(&body.agent_id).is_err())
            || ResourceId::new(&body.policy_id).is_err()
            || body
                .policy_revision
                .parse::<u32>()
                .ok()
                .and_then(|r| Revision::new(r).ok())
                .is_none()
            || serde_json::to_vec(&body).ok().as_ref() != Some(&payload.request)
        {
            return Err(rejected("AGENTSIGHT_INVALID_PREPARED"));
        }
        Ok((payload, body))
    }
}

impl<T: AgentSightTransport, R: ProcessIdentityResolver> AgentSightClient<T, R> {
    /// Prepares a deterministic target identity and fixed request without HTTP.
    /// The caller registers its target reference before create/update; request bytes are call-local.
    /// # Errors
    /// Returns a safe plan/process/boot resolution error; no target is modified.
    pub fn prepare_apply(
        &self,
        plan: &TargetBindingPlan,
    ) -> Result<PreparedApply, AgentSightClientError> {
        let plan = decode_plan(plan)?;
        let boot_id = self.current_boot_id()?;
        let start = self
            .process_identity
            .process_start_time(plan.root_pid)
            .map_err(classify_process_identity_error)?;
        if start == 0 {
            return Err(rejected("AGENTSIGHT_INVALID_PROCESS_IDENTITY"));
        }
        let request = serde_json::to_vec(&plan.request(start))
            .map_err(|_| rejected("AGENTSIGHT_REQUEST_SERIALIZATION_FAILED"))?;
        if request.len() > MAX_REQUEST_BYTES {
            return Err(rejected("AGENTSIGHT_REQUEST_TOO_LARGE"));
        }
        let payload = PreparedRequest {
            schema_version: 1,
            boot_id,
            request_digest: digest(&request),
            request,
        };
        let cleanup = Cleanup {
            schema_version: 1,
            binding_id: plan.binding_id,
            binding_revision: plan.binding_revision,
        };
        Ok(PreparedApply {
            target: TargetRef {
                route: self.route().into(),
                id: plan.target_binding_id.to_string(),
                cleanup: serde_json::to_vec(&cleanup)
                    .map_err(|_| rejected("AGENTSIGHT_REQUEST_SERIALIZATION_FAILED"))?,
            },
            format: AGENTSIGHT_PREPARED_APPLY_FORMAT.into(),
            content: serde_json::to_vec(&payload)
                .map_err(|_| rejected("AGENTSIGHT_REQUEST_SERIALIZATION_FAILED"))?,
        })
    }

    /// Sends the exact saved request after capability and process replay checks.
    pub fn create(&self, prepared: &PreparedApply) -> DeploymentReport {
        let result = self.decode_prepared(prepared).and_then(|(payload, body)| {
            self.preflight(&payload, &body)?;
            self.send_prepared(&payload, &body)
        });
        report_one(&prepared.target, result)
    }

    /// `AgentSight` replacement: validate new input, delete every old target, then
    /// create the new one. Never roll back or lose confirmed partial deletions.
    pub fn update(&self, previous: &[TargetRef], prepared: &PreparedApply) -> DeploymentReport {
        let decoded = self.decode_prepared(prepared).and_then(|(payload, body)| {
            let targets = self.validate_targets(previous)?;
            if previous
                .iter()
                .any(|old| old.same_identity(&prepared.target))
            {
                return Err(rejected("AGENTSIGHT_INVALID_PREVIOUS_TARGETS"));
            }
            self.preflight(&payload, &body)?;
            Ok((payload, body, targets))
        });
        let (payload, body, targets) = match decoded {
            Ok(value) => value,
            Err(error) => return report_one(&prepared.target, Err(error)),
        };
        let mut report = self.delete_validated_targets(&targets);
        if report.error.is_some() {
            report.observations.push(Observation {
                target: prepared.target.clone(),
                presence: Presence::Unknown,
            });
            return report;
        }
        let applied = report_one(&prepared.target, self.send_prepared(&payload, &body));
        report.observations.extend(applied.observations);
        report.error = applied.error;
        report
    }

    /// Deletes saved target references independently of current spec/PID/boot.
    /// Only 204 or the typed `binding_not_found` 404 confirms absence.
    pub fn delete_targets(&self, targets: &[TargetRef]) -> DeploymentReport {
        match self.validate_targets(targets) {
            Ok(targets) => self.delete_validated_targets(&targets),
            Err(error) => DeploymentReport {
                observations: vec![],
                error: Some(failure(&error)),
            },
        }
    }

    fn delete_validated_targets(&self, targets: &[(&TargetRef, Uuid)]) -> DeploymentReport {
        let mut report = DeploymentReport {
            observations: vec![],
            error: None,
        };
        for (target, id) in targets {
            // All references were validated before the first modifying request.
            let result = self.delete_target_id(*id);
            let partial = report_one(target, result);
            report.observations.extend(partial.observations);
            if let Some(error) = partial.error
                && (report.error.is_none() || error.kind == FailureKind::Rejected)
            {
                report.error = Some(error);
            }
        }
        report
    }

    fn validate_targets<'a>(
        &self,
        targets: &'a [TargetRef],
    ) -> Result<Vec<(&'a TargetRef, Uuid)>, AgentSightClientError> {
        let mut validated = Vec::with_capacity(targets.len());
        for (index, target) in targets.iter().enumerate() {
            let id = self.decode_target(target)?;
            if targets[..index].iter().any(|old| old.same_identity(target)) {
                return Err(rejected("AGENTSIGHT_DUPLICATE_TARGET_REFERENCE"));
            }
            validated.push((target, id));
        }
        Ok(validated)
    }

    fn current_boot_id(&self) -> Result<String, AgentSightClientError> {
        let value = self
            .process_identity
            .boot_id()
            .map_err(|error| match error {
                ProcessIdentityError::Unavailable => {
                    retryable("AGENTSIGHT_BOOT_IDENTITY_UNAVAILABLE")
                }
                _ => rejected("AGENTSIGHT_INVALID_BOOT_IDENTITY"),
            })?;
        let id =
            Uuid::parse_str(&value).map_err(|_| rejected("AGENTSIGHT_INVALID_BOOT_IDENTITY"))?;
        if id.is_nil() {
            return Err(rejected("AGENTSIGHT_INVALID_BOOT_IDENTITY"));
        }
        Ok(id.to_string())
    }

    fn check_process(
        &self,
        payload: &PreparedRequest,
        body: &ApplyBindingRequest,
    ) -> Result<(), AgentSightClientError> {
        if self.current_boot_id()? != payload.boot_id {
            return Err(rejected("AGENTSIGHT_BOOT_IDENTITY_CHANGED"));
        }
        let start = self
            .process_identity
            .process_start_time(body.root_pid)
            .map_err(classify_process_identity_error)?;
        if start != body.process_start_time {
            return Err(rejected("AGENTSIGHT_PROCESS_IDENTITY_CHANGED"));
        }
        Ok(())
    }

    fn preflight(
        &self,
        payload: &PreparedRequest,
        body: &ApplyBindingRequest,
    ) -> Result<(), AgentSightClientError> {
        self.check_process(payload, body)?;
        self.require_file_delete_capability()
    }

    fn send_prepared(
        &self,
        payload: &PreparedRequest,
        body: &ApplyBindingRequest,
    ) -> Result<AgentSightDeploymentState, AgentSightClientError> {
        // Health checks and old-target deletion can take time; revalidate after
        // those operations without ever replacing the saved request identity.
        self.check_process(payload, body)?;
        self.post_request(body, payload.request.clone())
    }
}

impl<T: AgentSightTransport, R: ProcessIdentityResolver> TargetDeploymentClient
    for AgentSightClient<T, R>
{
    fn prepare_apply(&self, plan: &TargetBindingPlan) -> Result<PreparedApply, Failure> {
        Self::prepare_apply(self, plan).map_err(|error| failure(&error))
    }
    fn create(&self, prepared: &PreparedApply) -> DeploymentReport {
        Self::create(self, prepared)
    }
    fn update(&self, previous: &[TargetRef], prepared: &PreparedApply) -> DeploymentReport {
        Self::update(self, previous, prepared)
    }
    fn delete(&self, targets: &[TargetRef]) -> DeploymentReport {
        self.delete_targets(targets)
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn failure(error: &AgentSightClientError) -> Failure {
    Failure::new(
        match error.kind {
            AgentSightClientErrorKind::Retryable => FailureKind::Retryable,
            AgentSightClientErrorKind::Rejected => FailureKind::Rejected,
        },
        &error.code,
    )
}

fn report_one(
    target: &TargetRef,
    result: Result<AgentSightDeploymentState, AgentSightClientError>,
) -> DeploymentReport {
    let (presence, error) = match result {
        Ok(AgentSightDeploymentState::Present) => (Presence::Present, None),
        Ok(AgentSightDeploymentState::Absent) => (Presence::Absent, None),
        Err(error) => (Presence::Unknown, Some(failure(&error))),
    };
    DeploymentReport {
        observations: vec![Observation {
            target: target.clone(),
            presence,
        }],
        error,
    }
}
