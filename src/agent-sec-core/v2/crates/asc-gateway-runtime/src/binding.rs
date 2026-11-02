//! Canonical Binding deployment and per-instance cleanup, independent of Scope retirement.

use std::collections::BTreeSet;
use std::sync::Arc;

use asc_credential_store::ScopeId;
use asc_pap::{PapError, ScopeExecution};
use asc_policy_target_contracts::TargetDeploymentClient;
use asc_policy_types::binding::{BindingStatus, PreparedBinding};
use asc_policy_types::identifiers::ResourceId;
use asc_policy_types::scope::{PreparedScope, ScopeStatus};
use asc_policy_types::target::{
    DeploymentReport, Failure, FailureKind, Observation, PreparedApply, Presence,
    TargetBindingPlan, TargetRef,
};
use serde::{Deserialize, Serialize};

use crate::service::Inner;
use crate::state::Attachment;
use crate::{GatewayError, GatewayErrorKind, GatewayService, ProcessIdentity};

/// Complete `PreparedBinding` wire format accepted by this deployment client.
pub const GATEWAY_BINDING_FORMAT: &str = "agent-sec-gateway-binding/v1";
const MAX_BINDING_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Cleanup {
    scope_id: ScopeId,
    binding_id: ResourceId,
    process: asc_policy_types::process_discovery::ProcessIdentity,
}

impl GatewayService {
    pub(crate) fn binding_matches(
        binding: &PreparedBinding,
        scope: &PreparedScope,
        attachment: &Attachment,
    ) -> bool {
        binding.scope.scope_id == scope.scope_id
            && binding.scope.selector == scope.selector
            && binding.scope.process == attachment.process
            && scope
                .policy_snapshots
                .iter()
                .any(|policy| *policy == binding.policy && policy.template.uses_credentials())
    }
    fn admitted_binding(&self, binding: &PreparedBinding) -> Result<PreparedScope, GatewayError> {
        let view = self.repository.get_binding(&binding.binding_id)?;
        if view.spec != *binding || view.status.phase != BindingStatus::Applying {
            return Err(GatewayError::new(
                GatewayErrorKind::Conflict,
                "Binding is stale or is not Applying",
            ));
        }
        let scope = self.repository.get_scope(&binding.scope.scope_id)?;
        self.canonical(&scope)?;
        self.check_scope(&scope)?;
        if binding.scope.selector != scope.selector
            || !scope.policy_snapshots.contains(&binding.policy)
            || !binding.policy.template.uses_credentials()
        {
            return Err(GatewayError::new(
                GatewayErrorKind::Conflict,
                "Binding does not match the canonical Scope policy snapshot",
            ));
        }
        Ok(scope)
    }
    fn verify_instance(
        &self,
        process: &ProcessIdentity,
        expected: &asc_policy_types::process_discovery::ProcessIdentity,
    ) -> Result<(), GatewayError> {
        self.host.verify_process(process)?;
        let (boot, namespace) = self.host.process_fence(process.pid)?;
        if process.pid != expected.pid
            || process.start_time != expected.start_time
            || boot != expected.boot_id
            || namespace != expected.pid_namespace
        {
            return Err(GatewayError::new(
                GatewayErrorKind::PermissionDenied,
                "Binding process identity differs from host boot, PID namespace or start time",
            ));
        }
        self.host.verify_process(process)
    }
    fn verify_attachment(
        &self,
        process: &ProcessIdentity,
        cgroup: &str,
        uid: u32,
    ) -> Result<(), GatewayError> {
        self.check_process(process, uid)?;
        let actual = self.host.process_cgroup(process.pid)?;
        let expected = format!("/{cgroup}");
        if actual != expected
            && !actual
                .strip_prefix(&expected)
                .is_some_and(|tail| tail.starts_with('/'))
        {
            return Err(GatewayError::new(
                GatewayErrorKind::Conflict,
                "process left its pinned execution cgroup",
            ));
        }
        self.host.verify_process(process)
    }
    pub(crate) fn verify_pinned(
        &self,
        inner: &Inner,
        id: &ResourceId,
        attachment: &Attachment,
        uid: u32,
    ) -> Result<(), GatewayError> {
        let pins = inner
            .pins
            .get(id)
            .ok_or_else(|| GatewayError::unavailable("Binding has no live execution pin"))?;
        let root = pins
            .first()
            .ok_or_else(|| GatewayError::internal("empty execution pin"))?;
        self.verify_instance(root, &attachment.process)?;
        for process in pins {
            match self.verify_attachment(process, &attachment.cgroup, uid) {
                Ok(()) => {}
                Err(error)
                    if error.kind() == GatewayErrorKind::Unavailable && process.pid != root.pid => {
                }
                Err(error) => return Err(error),
            }
        }
        // Existing descendants may exit; newly inherited descendants still need immediate
        // UID/group/cgroup validation before any route is handed back to the data plane.
        let current = self.host.process_tree(root.pid)?;
        for process in &current {
            self.verify_attachment(process, &attachment.cgroup, uid)?;
        }
        self.verify_instance(root, &attachment.process)
    }
    pub(crate) fn recover_attachments(&self, inner: &mut Inner) -> Result<(), GatewayError> {
        inner.pins.clear();
        for (id, domain) in &inner.state.scopes {
            if domain.revoked {
                continue;
            }
            let resource = ResourceId::new(id.as_str())
                .map_err(|_| GatewayError::internal("invalid Scope identity"))?;
            let scope = match self.repository.get_scope(&resource) {
                Ok(scope) if scope.status == ScopeStatus::Active => scope,
                Ok(_) | Err(PapError::NotFound) => continue,
                Err(error) => return Err(error.into()),
            };
            for (binding_id, attachment) in &domain.bindings {
                let view = match self.repository.get_binding(binding_id) {
                    Ok(view) => view,
                    Err(PapError::NotFound) => continue,
                    Err(error) => return Err(error.into()),
                };
                if !matches!(
                    view.status.phase,
                    BindingStatus::Applying | BindingStatus::Ready
                ) || !Self::binding_matches(&view.spec, &scope, attachment)
                {
                    continue;
                }
                let recovered: Result<Vec<ProcessIdentity>, GatewayError> = (|| {
                    let pins = self.host.process_tree(attachment.process.pid)?;
                    let root = pins
                        .first()
                        .ok_or_else(|| GatewayError::internal("empty process snapshot"))?;
                    self.verify_instance(root, &attachment.process)?;
                    for process in &pins {
                        self.verify_attachment(process, &attachment.cgroup, domain.uid)?;
                    }
                    self.verify_instance(root, &attachment.process)?;
                    Ok(pins)
                })();
                let pins = match recovered {
                    Ok(pins) => pins,
                    Err(error)
                        if error.is_process_exited()
                            || matches!(
                                error.kind(),
                                GatewayErrorKind::PermissionDenied | GatewayErrorKind::Conflict
                            ) =>
                    {
                        // Keep durable cleanup ownership, but never pin a replacement or
                        // an escaped instance; unrelated healthy Bindings must still recover.
                        eprintln!(
                            "gateway attachment recovery rejected an instance; cleanup responsibility retained"
                        );
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                self.canonical(&scope)?;
                inner.pins.insert(binding_id.clone(), pins);
            }
        }
        Ok(())
    }
    fn deploy(&self, prepared: &PreparedApply) -> Result<(), GatewayError> {
        let binding = decode_binding(&prepared.format, &prepared.content)?;
        let cleanup = cleanup_of(&binding)?;
        if prepared.target != target_of(&cleanup)? {
            return Err(GatewayError::invalid(
                "prepared Binding target differs from cleanup identity",
            ));
        }
        let mut inner = self.lock();
        let scope = self.admitted_binding(&binding)?;
        let id = cleanup.scope_id.clone();
        let domain = inner
            .state
            .scopes
            .get(&id)
            .ok_or_else(|| GatewayError::unavailable("Scope has not been prepared"))?;
        if domain.revoked || scope.owner_uid != Some(domain.uid) {
            return Err(GatewayError::new(
                GatewayErrorKind::Conflict,
                "Scope is revoked or owner differs",
            ));
        }
        let uid = domain.uid;
        let instance_scope = format!(
            "agent-sec-instance-{}-{}-{}.scope",
            domain.port, cleanup.process.pid, cleanup.process.start_time
        );
        let cgroup = format!("{}/{}", self.config.slice, instance_scope);
        if !crate::nft::is_safe_cgroup_path(&cgroup) {
            return Err(GatewayError::invalid("unsafe execution slice"));
        }
        let attachment = Attachment {
            process: cleanup.process.clone(),
            cgroup: cgroup.clone(),
        };
        if domain
            .bindings
            .get(&binding.binding_id)
            .is_some_and(|old| old != &attachment)
        {
            return Err(GatewayError::new(
                GatewayErrorKind::Conflict,
                "Binding cleanup identity cannot change",
            ));
        }
        let pins = self.host.process_tree(cleanup.process.pid)?;
        let root = pins
            .first()
            .ok_or_else(|| GatewayError::internal("empty process snapshot"))?;
        self.verify_instance(root, &cleanup.process)?;
        let members = self.host.scope_processes(&cgroup)?;
        for process in pins.iter().chain(&members) {
            self.check_process(process, uid)?;
            self.check_overlap(&inner, &id, &attachment, process)?;
        }
        // Store cleanup responsibility before the first move, even if attachment later fails.
        inner
            .state
            .scopes
            .get_mut(&id)
            .ok_or_else(|| GatewayError::internal("Scope disappeared"))?
            .bindings
            .insert(binding.binding_id.clone(), attachment.clone());
        inner.state.save(&self.config.state_dir)?;
        inner.pins.insert(binding.binding_id.clone(), pins);
        let result = (|| {
            let pins = &inner.pins[&binding.binding_id];
            for process in pins {
                self.admitted_binding(&binding)?;
                self.check_process(process, uid)?;
                self.check_overlap(&inner, &id, &attachment, process)?;
                let actual = self.host.process_cgroup(process.pid)?;
                let expected = format!("/{cgroup}");
                if actual != expected
                    && !actual
                        .strip_prefix(&expected)
                        .is_some_and(|tail| tail.starts_with('/'))
                {
                    self.host
                        .attach_to_scope(&instance_scope, &self.config.slice, process)?;
                }
                self.verify_attachment(process, &cgroup, uid)?;
            }
            self.admitted_binding(&binding)?;
            let current = self.host.process_tree(cleanup.process.pid)?;
            for process in &current {
                self.verify_attachment(process, &cgroup, uid)?;
            }
            self.verify_pinned(&inner, &binding.binding_id, &attachment, uid)?;
            if inner.state.enabled {
                let plans = self.active_plans(&mut inner)?;
                self.route(&inner, &plans)?;
            }
            self.admitted_binding(&binding)?;
            Ok(())
        })();
        result.map_err(|error| self.fail_closed(error))
    }
    fn check_overlap(
        &self,
        inner: &Inner,
        id: &ScopeId,
        attachment: &Attachment,
        process: &ProcessIdentity,
    ) -> Result<(), GatewayError> {
        let actual = self.host.process_cgroup(process.pid)?;
        for (other_id, domain) in &inner.state.scopes {
            for other in domain.bindings.values() {
                let same_instance = other_id == id && other.process == attachment.process;
                let path = format!("/{}", other.cgroup);
                if !same_instance
                    && (actual == path
                        || actual
                            .strip_prefix(&path)
                            .is_some_and(|tail| tail.starts_with('/')))
                {
                    return Err(GatewayError::new(
                        GatewayErrorKind::Conflict,
                        "process overlaps another execution instance",
                    ));
                }
            }
        }
        // Unowned old gateway cgroups must not silently become new authorization domains.
        let prefix = format!("/{}/agent-sec-instance-", self.config.slice);
        let expected = format!("/{}", attachment.cgroup);
        let owned = actual == expected
            || actual
                .strip_prefix(&expected)
                .is_some_and(|tail| tail.starts_with('/'));
        if actual.starts_with(&prefix) && !owned {
            return Err(GatewayError::new(
                GatewayErrorKind::Conflict,
                "process remains in an unowned gateway cgroup",
            ));
        }
        Ok(())
    }
    fn detach(&self, target: &TargetRef) -> Result<(), GatewayError> {
        let cleanup: Cleanup = serde_json::from_slice(&target.cleanup)
            .map_err(|_| GatewayError::invalid("invalid gateway cleanup"))?;
        if *target != target_of(&cleanup)? {
            return Err(GatewayError::invalid("gateway cleanup target mismatch"));
        }
        let mut inner = self.lock();
        if let Some(domain) = inner.state.scopes.get(&cleanup.scope_id)
            && let Some(attachment) = domain.bindings.get(&cleanup.binding_id)
            && attachment.process != cleanup.process
        {
            return Err(GatewayError::new(
                GatewayErrorKind::Conflict,
                "cleanup belongs to another process instance",
            ));
        }
        let mut desired = inner.state.clone();
        if let Some(domain) = desired.scopes.get_mut(&cleanup.scope_id) {
            domain.bindings.remove(&cleanup.binding_id);
        }
        // Routing sees the candidate without this responsibility. Failed persistence restores
        // the old responsibility in memory, but never resumes the old routing.
        let original = std::mem::replace(&mut inner.state, desired);
        let result = (|| {
            if inner.state.enabled {
                let plans = self.active_plans(&mut inner)?;
                self.route(&inner, &plans)?;
            }
            inner.state.save(&self.config.state_dir)?;
            Ok(())
        })();
        if let Err(error) = result {
            inner.state = original;
            return Err(self.fail_closed(error));
        }
        inner.pins.remove(&cleanup.binding_id);
        let cgroups: BTreeSet<_> = original
            .scopes
            .get(&cleanup.scope_id)
            .into_iter()
            .flat_map(|domain| domain.bindings.values())
            .filter(|attachment| attachment.process == cleanup.process)
            .map(|attachment| attachment.cgroup.clone())
            .collect();
        for cgroup in cgroups {
            let retained = inner
                .state
                .scopes
                .values()
                .flat_map(|domain| domain.bindings.values())
                .any(|attachment| attachment.cgroup == cgroup);
            if !retained
                && self.host.scope_processes(&cgroup)?.is_empty()
                && let Some(scope) = cgroup.rsplit('/').next()
            {
                self.host.remove_scope(scope)?;
            }
        }
        // Never kills or moves a process, and never retires a Scope fake.
        Ok(())
    }
}

/// Shared-service deployment client for the daemon's target registry.
#[derive(Clone)]
pub struct GatewayBindingClient(Arc<GatewayService>);
impl GatewayBindingClient {
    /// Wraps the same service used by PAP `ScopeExecution` callbacks.
    pub fn new(service: Arc<GatewayService>) -> Self {
        Self(service)
    }
}
impl TargetDeploymentClient for GatewayBindingClient {
    fn prepare_apply(&self, plan: &TargetBindingPlan) -> Result<PreparedApply, Failure> {
        let binding =
            decode_binding(&plan.format, &plan.content).map_err(|error| failure(&error))?;
        // Preparation is side-effect-free, but still rejects obsolete canonical snapshots.
        self.0
            .admitted_binding(&binding)
            .map_err(|error| failure(&error))?;
        let cleanup = cleanup_of(&binding).map_err(|error| failure(&error))?;
        Ok(PreparedApply {
            target: target_of(&cleanup).map_err(|error| failure(&error))?,
            format: plan.format.clone(),
            content: plan.content.clone(),
        })
    }
    fn create(&self, prepared: &PreparedApply) -> DeploymentReport {
        report(&prepared.target, Presence::Present, self.0.deploy(prepared))
    }
    fn update(&self, previous: &[TargetRef], prepared: &PreparedApply) -> DeploymentReport {
        let mut observations = Vec::new();
        for target in previous {
            if target.same_identity(&prepared.target) {
                continue;
            }
            let mut removed = report(target, Presence::Absent, self.0.detach(target));
            observations.append(&mut removed.observations);
            if removed.error.is_some() {
                return DeploymentReport {
                    observations,
                    error: removed.error,
                };
            }
        }
        let mut created = self.create(prepared);
        observations.append(&mut created.observations);
        DeploymentReport {
            observations,
            error: created.error,
        }
    }
    fn delete(&self, targets: &[TargetRef]) -> DeploymentReport {
        let mut observations = Vec::new();
        for target in targets {
            let mut removed = report(target, Presence::Absent, self.0.detach(target));
            observations.append(&mut removed.observations);
            if removed.error.is_some() {
                return DeploymentReport {
                    observations,
                    error: removed.error,
                };
            }
        }
        DeploymentReport {
            observations,
            error: None,
        }
    }
}
impl ScopeExecution for GatewayService {
    fn validate(&self, scope: &PreparedScope) -> Result<(), PapError> {
        self.check_scope(scope)
            .map(|_| ())
            .map_err(|error| pap_failure(&error))
    }
    fn prepare(&self, scope: &PreparedScope) -> Result<(), PapError> {
        self.prepare_scope(scope)
            .map_err(|error| pap_failure(&error))
    }
    fn revoke(&self, scope: &PreparedScope) -> Result<(), PapError> {
        self.revoke_scope(scope)
            .map_err(|error| pap_failure(&error))
    }
    fn release(&self, id: &ResourceId) -> Result<(), PapError> {
        self.release_scope(id).map_err(|error| pap_failure(&error))
    }
}
fn pap_failure(error: &GatewayError) -> PapError {
    match error.kind() {
        GatewayErrorKind::Conflict => PapError::Conflict,
        GatewayErrorKind::NotFound => PapError::NotFound,
        GatewayErrorKind::InvalidArgument | GatewayErrorKind::PermissionDenied => {
            PapError::InvalidScope(asc_policy_types::error::ValidationError::new(
                "gateway",
                "credential Scope owner or policy is not executable",
            ))
        }
        GatewayErrorKind::ResourceExhausted
        | GatewayErrorKind::Unavailable
        | GatewayErrorKind::Internal => PapError::Unavailable,
    }
}
fn decode_binding(format: &str, content: &[u8]) -> Result<PreparedBinding, GatewayError> {
    use asc_policy_types::error::Validate;
    if format != GATEWAY_BINDING_FORMAT || content.len() > MAX_BINDING_BYTES {
        return Err(GatewayError::invalid(
            "unsupported or oversized gateway Binding payload",
        ));
    }
    let binding: PreparedBinding = serde_json::from_slice(content)
        .map_err(|_| GatewayError::invalid("invalid gateway Binding payload"))?;
    binding
        .validate()
        .map_err(|_| GatewayError::invalid("invalid gateway Binding specification"))?;
    Ok(binding)
}
fn cleanup_of(binding: &PreparedBinding) -> Result<Cleanup, GatewayError> {
    Ok(Cleanup {
        scope_id: ScopeId::new(binding.scope.scope_id.as_str())?,
        binding_id: binding.binding_id.clone(),
        process: binding.scope.process.clone(),
    })
}
fn target_of(cleanup: &Cleanup) -> Result<TargetRef, GatewayError> {
    Ok(TargetRef {
        route: "gateway".into(),
        id: cleanup.binding_id.as_str().to_owned(),
        cleanup: serde_json::to_vec(cleanup)
            .map_err(|_| GatewayError::internal("cannot encode gateway cleanup"))?,
    })
}
fn failure(error: &GatewayError) -> Failure {
    let (kind, code) = match error.kind() {
        GatewayErrorKind::InvalidArgument => (FailureKind::Rejected, "GATEWAY_INVALID_BINDING"),
        GatewayErrorKind::PermissionDenied => {
            (FailureKind::Rejected, "GATEWAY_PROCESS_IDENTITY_REJECTED")
        }
        GatewayErrorKind::Conflict => (FailureKind::Rejected, "GATEWAY_CANONICAL_BINDING_CONFLICT"),
        GatewayErrorKind::NotFound => (FailureKind::Rejected, "GATEWAY_CANONICAL_RECORD_NOT_FOUND"),
        GatewayErrorKind::ResourceExhausted => {
            (FailureKind::Retryable, "GATEWAY_CAPACITY_EXHAUSTED")
        }
        GatewayErrorKind::Unavailable => (FailureKind::Retryable, "GATEWAY_NOT_READY"),
        GatewayErrorKind::Internal => (FailureKind::Retryable, "GATEWAY_INTERNAL_ERROR"),
    };
    Failure::new(kind, code)
}
fn report(
    target: &TargetRef,
    presence: Presence,
    result: Result<(), GatewayError>,
) -> DeploymentReport {
    match result {
        Ok(()) => DeploymentReport {
            observations: vec![Observation {
                target: target.clone(),
                presence,
            }],
            error: None,
        },
        Err(error) => DeploymentReport {
            observations: Vec::new(),
            error: Some(failure(&error)),
        },
    }
}
