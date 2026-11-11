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

use crate::service::{Inner, diagnose};
use crate::state::{Attachment, State};
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

struct AttachmentPlan {
    scope_id: ScopeId,
    uid: u32,
    attachment: Attachment,
    cgroups: BTreeSet<String>,
    pins: Vec<ProcessIdentity>,
    unowned: BTreeSet<u32>,
    existing: Vec<(PreparedBinding, Attachment)>,
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
    fn verify_owned_attachment(
        &self,
        state: &State,
        id: &ScopeId,
        process: &ProcessIdentity,
        uid: u32,
    ) -> Result<(), GatewayError> {
        let cgroup = self.check_overlap(state, id, process)?.ok_or_else(|| {
            GatewayError::new(
                GatewayErrorKind::Conflict,
                "process has no execution cgroup owner",
            )
        })?;
        let domain = &state.scopes[id];
        let resource = ResourceId::new(id.as_str())
            .map_err(|_| GatewayError::internal("invalid Scope identity"))?;
        let scope = self.repository.get_scope(&resource)?;
        self.canonical(&scope)?;
        if domain.revoked || scope.status != ScopeStatus::Active || scope.owner_uid != Some(uid) {
            return Err(GatewayError::new(
                GatewayErrorKind::Conflict,
                "execution Scope is not active",
            ));
        }
        let mut live_owner = false;
        for (binding_id, attachment) in &domain.bindings {
            if !attachment.cgroups().any(|path| path == cgroup) {
                continue;
            }
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
                // A stale reference cannot veto another live canonical owner's group.
                continue;
            }
            let owner: Result<bool, GatewayError> = (|| {
                let pins = self.host.process_tree(attachment.process.pid)?;
                let root = pins
                    .first()
                    .ok_or_else(|| GatewayError::internal("empty process snapshot"))?;
                self.verify_instance(root, &attachment.process)?;
                self.check_process(root, uid)?;
                Ok(self.host.process_cgroup(root.pid)? == format!("/{}", attachment.cgroup))
            })();
            match owner {
                Ok(valid) => live_owner |= valid,
                Err(error)
                    if error.is_process_exited()
                        || matches!(
                            error.kind(),
                            GatewayErrorKind::PermissionDenied | GatewayErrorKind::Conflict
                        ) => {}
                Err(error) => return Err(error),
            }
        }
        if !live_owner {
            return Err(GatewayError::new(
                GatewayErrorKind::Conflict,
                "execution cgroup has no live canonical root",
            ));
        }
        self.verify_attachment(process, cgroup, uid)
    }
    pub(crate) fn verify_pinned(
        &self,
        inner: &Inner,
        id: &ResourceId,
        attachment: &Attachment,
        uid: u32,
    ) -> Result<(), GatewayError> {
        match self.check_pinned(inner, id, attachment, uid)? {
            Ok(()) => Ok(()),
            Err(RouteFault::Stale(error) | RouteFault::Members(error)) => Err(error),
        }
    }
    /// Splits Binding-local faults from unknown host evidence, which must fail closed.
    pub(crate) fn check_pinned(
        &self,
        inner: &Inner,
        id: &ResourceId,
        attachment: &Attachment,
        uid: u32,
    ) -> Result<Result<(), RouteFault>, GatewayError> {
        // Failed deployments and unrecoverable instances intentionally keep no pin.
        if !inner.pins.contains_key(id) {
            return Ok(Err(RouteFault::Stale(GatewayError::unavailable(
                "Binding has no live execution pin",
            ))));
        }
        let (scope_id, pins) = match self.verify_root(inner, id, attachment, uid) {
            Ok(found) => found,
            Err(error) => return local(error).map(|error| Err(RouteFault::Stale(error))),
        };
        // Exited members are tolerated, so only enumerating the root itself reports an exit;
        // any other member failure is independent evidence that a later root exit cannot erase.
        match self.verify_members(inner, scope_id, pins, attachment, uid) {
            Ok(()) => {}
            Err(error) if error.is_process_exited() => return Ok(Err(RouteFault::Stale(error))),
            Err(error) => return local(error).map(|error| Err(RouteFault::Members(error))),
        }
        match self.verify_instance(&pins[0], &attachment.process) {
            Ok(()) => Ok(Ok(())),
            Err(error) => local(error).map(|error| Err(RouteFault::Stale(error))),
        }
    }
    fn verify_root<'a>(
        &self,
        inner: &'a Inner,
        id: &ResourceId,
        attachment: &Attachment,
        uid: u32,
    ) -> Result<(&'a ScopeId, &'a [ProcessIdentity]), GatewayError> {
        let scope_id = inner
            .state
            .scopes
            .iter()
            .find(|(_, domain)| domain.uid == uid && domain.bindings.get(id) == Some(attachment))
            .map(|(id, _)| id)
            .ok_or_else(|| GatewayError::internal("Binding has no cleanup ownership"))?;
        let pins = inner
            .pins
            .get(id)
            .ok_or_else(|| GatewayError::unavailable("Binding has no live execution pin"))?;
        let root = pins
            .first()
            .ok_or_else(|| GatewayError::internal("empty execution pin"))?;
        self.verify_instance(root, &attachment.process)?;
        if self.host.process_cgroup(root.pid)? != format!("/{}", attachment.cgroup) {
            return Err(GatewayError::new(
                GatewayErrorKind::Conflict,
                "Binding root left its exact cgroup",
            ));
        }
        self.verify_owned_attachment(&inner.state, scope_id, root, uid)?;
        Ok((scope_id, pins))
    }
    /// Validates every non-root pin, descendant and member of the Binding's cgroups.
    fn verify_members(
        &self,
        inner: &Inner,
        scope_id: &ScopeId,
        pins: &[ProcessIdentity],
        attachment: &Attachment,
        uid: u32,
    ) -> Result<(), GatewayError> {
        let root = &pins[0];
        for process in &pins[1..] {
            self.verify_tree_member(&inner.state, scope_id, process, root, uid)?;
        }
        // Existing descendants may exit; newly inherited descendants still need immediate
        // UID/group/cgroup validation before any route is handed back to the data plane.
        for process in &self.host.process_tree(root.pid)? {
            if process.pid != root.pid {
                self.verify_tree_member(&inner.state, scope_id, process, root, uid)?;
            }
        }
        // Shared groups retain siblings and reparented descendants after root cleanup.
        for path in attachment.cgroups() {
            for process in self.host.scope_processes(path)? {
                if process.pid != root.pid {
                    self.verify_live_member(&inner.state, scope_id, &process, root, uid)?;
                }
            }
        }
        Ok(())
    }
    /// Validates a root-tree member; one outside every gateway cgroup is never routed.
    fn verify_tree_member(
        &self,
        state: &State,
        scope_id: &ScopeId,
        process: &ProcessIdentity,
        root: &ProcessIdentity,
        uid: u32,
    ) -> Result<(), GatewayError> {
        if process.pid != root.pid {
            match self.check_overlap(state, scope_id, process) {
                Ok(Some(_)) => {}
                Ok(None) => return Ok(()),
                Err(error) if error.is_process_exited() => return Ok(()),
                Err(error) => return Err(error),
            }
        }
        self.verify_live_member(state, scope_id, process, root, uid)
    }
    /// Validates a routed-cgroup member; only non-root members may have exited meanwhile.
    fn verify_live_member(
        &self,
        state: &State,
        scope_id: &ScopeId,
        process: &ProcessIdentity,
        root: &ProcessIdentity,
        uid: u32,
    ) -> Result<(), GatewayError> {
        match self.verify_owned_attachment(state, scope_id, process, uid) {
            Err(error) if error.is_process_exited() && process.pid != root.pid => Ok(()),
            result => result,
        }
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
                    if self.host.process_cgroup(root.pid)? != format!("/{}", attachment.cgroup) {
                        return Err(GatewayError::new(
                            GatewayErrorKind::Conflict,
                            "Binding root left its exact cgroup",
                        ));
                    }
                    for process in &pins {
                        self.verify_tree_member(&inner.state, id, process, root, domain.uid)?;
                    }
                    for path in attachment.cgroups() {
                        for process in self.host.scope_processes(path)? {
                            self.verify_live_member(&inner.state, id, &process, root, domain.uid)?;
                        }
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
                        diagnose(
                            "gateway attachment recovery rejected an instance; cleanup responsibility retained",
                            format_args!("binding_id={binding_id} error={error}"),
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
        let mut plan = self.gather_attachment(&inner, &scope, &cleanup)?;
        let mut attached = self
            .gather_shared_attachments(&inner, &binding, &scope, &mut plan)
            .and_then(|()| {
                self.validate_attachment_plan(&inner, &binding, &scope, &plan, &plan.pins[0])
            });
        if attached.is_ok() {
            self.persist_attachment_plan(&mut inner, &binding, &plan)?;
            attached = self
                .attach_unowned(&inner, &binding, &scope, &plan)
                .and_then(|()| self.verify_attachment_plan(&mut inner, &binding, &scope, plan));
        }
        attached.map_err(|error| self.isolate_binding(&mut inner, &binding.binding_id, error))
    }
    /// Withdraws only `id`'s routes after a failed deployment, retaining its cleanup duty;
    /// the gateway fails closed only if recomputing the routes fails.
    fn isolate_binding(
        &self,
        inner: &mut Inner,
        id: &ResourceId,
        error: GatewayError,
    ) -> GatewayError {
        diagnose(
            "gateway Binding deployment failed; withdrawing only its routes",
            format_args!("binding_id={id} error={error}"),
        );
        inner.pins.remove(id);
        let rerouted = if inner.state.enabled {
            self.active_plans(inner)
                .and_then(|plans| self.route(inner, &plans))
                .map(drop)
        } else {
            Ok(())
        };
        match rerouted {
            Ok(()) => error,
            Err(route_error) => self.fail_closed(route_error),
        }
    }
    fn gather_attachment(
        &self,
        inner: &Inner,
        scope: &PreparedScope,
        cleanup: &Cleanup,
    ) -> Result<AttachmentPlan, GatewayError> {
        let id = &cleanup.scope_id;
        let domain = inner
            .state
            .scopes
            .get(id)
            .ok_or_else(|| GatewayError::unavailable("Scope has not been prepared"))?;
        if domain.revoked || scope.owner_uid != Some(domain.uid) {
            return Err(GatewayError::new(
                GatewayErrorKind::Conflict,
                "Scope is revoked or owner differs",
            ));
        }
        let uid = domain.uid;
        let default_cgroup = format!(
            "{}/agent-sec-instance-{}-{}-{}.scope",
            self.config.slice, domain.port, cleanup.process.pid, cleanup.process.start_time
        );
        let mut cgroups = BTreeSet::new();
        if let Some(old) = domain.bindings.get(&cleanup.binding_id) {
            if old.process != cleanup.process {
                return Err(GatewayError::new(
                    GatewayErrorKind::Conflict,
                    "Binding cleanup identity cannot change",
                ));
            }
            cgroups.extend(old.cgroups().map(str::to_owned));
        }
        let pins = self.host.process_tree(cleanup.process.pid)?;
        let root = pins
            .first()
            .ok_or_else(|| GatewayError::internal("empty process snapshot"))?;
        self.verify_instance(root, &cleanup.process)?;
        let root_cgroup = self
            .check_overlap(&inner.state, id, root)?
            .map(str::to_owned);
        let mut unowned = BTreeSet::new();
        for process in &pins {
            self.check_process(process, uid)?;
            if let Some(cgroup) = self.check_overlap(&inner.state, id, process)? {
                cgroups.insert(cgroup.to_owned());
            } else {
                unowned.insert(process.pid);
            }
        }
        // Existing roots never move. Unowned members reuse an intersecting destination;
        // retries retain the responsibility recorded before a possibly partial attachment.
        let cgroup = root_cgroup
            .or_else(|| {
                domain
                    .bindings
                    .get(&cleanup.binding_id)
                    .map(|old| old.cgroup.clone())
            })
            .or_else(|| cgroups.first().cloned())
            .unwrap_or(default_cgroup);
        if let Some(old) = domain.bindings.get(&cleanup.binding_id)
            && old.cgroup != cgroup
        {
            return Err(GatewayError::new(
                GatewayErrorKind::Conflict,
                "Binding root cgroup cannot change",
            ));
        }
        cgroups.insert(cgroup.clone());
        let mut attachment = domain
            .bindings
            .get(&cleanup.binding_id)
            .cloned()
            .unwrap_or_else(|| Attachment {
                process: cleanup.process.clone(),
                shared_cgroups: BTreeSet::new(),
                cgroup,
            });
        attachment.shared_cgroups.extend(
            cgroups
                .iter()
                .filter(|path| **path != attachment.cgroup)
                .cloned(),
        );
        Ok(AttachmentPlan {
            scope_id: id.clone(),
            uid,
            attachment,
            cgroups,
            pins,
            unowned,
            existing: Vec::new(),
        })
    }
    fn gather_shared_attachments(
        &self,
        inner: &Inner,
        binding: &PreparedBinding,
        scope: &PreparedScope,
        plan: &mut AttachmentPlan,
    ) -> Result<(), GatewayError> {
        for path in &plan.cgroups {
            // systemd unit types are case-sensitive; this is not a file extension check.
            let unit_name = path
                .strip_prefix(&format!("{}/", self.config.slice))
                .and_then(|unit| unit.strip_suffix(".scope"));
            if !crate::nft::is_safe_cgroup_path(path)
                || !unit_name.is_some_and(|name| {
                    name.starts_with("agent-sec-instance-") && !name.contains('/')
                })
            {
                return Err(GatewayError::invalid("unsafe execution slice"));
            }
        }
        if inner.state.scopes.iter().any(|(other_id, domain)| {
            other_id != &plan.scope_id
                && domain
                    .bindings
                    .values()
                    .any(|other| other.cgroups().any(|path| plan.cgroups.contains(path)))
        }) {
            return Err(GatewayError::new(
                GatewayErrorKind::Conflict,
                "execution cgroup belongs to another Scope",
            ));
        }
        for (binding_id, old) in &inner.state.scopes[&plan.scope_id].bindings {
            // A complete pin cannot be reattached after escape; an interrupted Applying
            // attempt may have only durable responsibility and still need attachment.
            if binding_id == &binding.binding_id && inner.pins.contains_key(binding_id) {
                self.verify_pinned(inner, binding_id, old, plan.uid)?;
            }
            if binding_id != &binding.binding_id
                && old.cgroups().any(|path| plan.cgroups.contains(path))
            {
                let view = self.repository.get_binding(binding_id)?;
                if !matches!(
                    view.status.phase,
                    BindingStatus::Applying | BindingStatus::Ready
                ) || !Self::binding_matches(&view.spec, scope, old)
                {
                    return Err(GatewayError::new(
                        GatewayErrorKind::Conflict,
                        "overlapping Binding is not canonical or executable",
                    ));
                }
                self.verify_pinned(inner, binding_id, old, plan.uid)?;
                plan.existing.push((view.spec, old.clone()));
            }
        }
        for path in &plan.cgroups {
            let processes = self.host.scope_processes(path)?;
            for process in &processes {
                self.verify_attachment(process, path, plan.uid)?;
                self.check_overlap(&inner.state, &plan.scope_id, process)?;
            }
        }
        Ok(())
    }
    fn validate_attachment_plan(
        &self,
        inner: &Inner,
        binding: &PreparedBinding,
        scope: &PreparedScope,
        plan: &AttachmentPlan,
        root: &ProcessIdentity,
    ) -> Result<(), GatewayError> {
        self.admitted_binding(binding)?;
        for (spec, old) in &plan.existing {
            let view = self.repository.get_binding(&spec.binding_id)?;
            if view.spec != *spec
                || !matches!(
                    view.status.phase,
                    BindingStatus::Applying | BindingStatus::Ready
                )
                || !Self::binding_matches(&view.spec, scope, old)
            {
                return Err(GatewayError::new(
                    GatewayErrorKind::Conflict,
                    "overlapping Binding changed during deployment",
                ));
            }
            let root = inner
                .pins
                .get(&spec.binding_id)
                .and_then(|pins| pins.first())
                .ok_or_else(|| GatewayError::unavailable("Binding has no live execution pin"))?;
            self.verify_instance(root, &old.process)?;
        }
        self.verify_instance(root, &plan.attachment.process)?;
        self.canonical(scope)
    }
    fn persist_attachment_plan(
        &self,
        inner: &mut Inner,
        binding: &PreparedBinding,
        plan: &AttachmentPlan,
    ) -> Result<(), GatewayError> {
        let bindings = &mut inner
            .state
            .scopes
            .get_mut(&plan.scope_id)
            .ok_or_else(|| GatewayError::internal("Scope disappeared"))?
            .bindings;
        bindings.insert(binding.binding_id.clone(), plan.attachment.clone());
        // Retry adds validated shared references without releasing any earlier responsibility.
        inner
            .state
            .save(&self.config.state_dir)
            .map_err(|error| self.fail_closed(error))
    }
    fn attach_unowned(
        &self,
        inner: &Inner,
        binding: &PreparedBinding,
        scope: &PreparedScope,
        plan: &AttachmentPlan,
    ) -> Result<(), GatewayError> {
        let root = plan
            .pins
            .first()
            .ok_or_else(|| GatewayError::internal("empty process snapshot"))?;
        let cgroup = &plan.attachment.cgroup;
        // Never convert an initially owned member into an attachment candidate if it
        // escapes while deployment is in flight; final verification rejects it instead.
        for process in plan
            .pins
            .iter()
            .filter(|process| plan.unowned.contains(&process.pid))
        {
            self.validate_attachment_plan(inner, binding, scope, plan, root)?;
            match self.check_process(process, plan.uid) {
                Ok(()) => {}
                Err(error) if error.is_process_exited() && process.pid != root.pid => continue,
                Err(error) => return Err(error),
            }
            if let Some(owned) = self.check_overlap(&inner.state, &plan.scope_id, process)? {
                if !plan.cgroups.contains(owned) {
                    return Err(GatewayError::new(
                        GatewayErrorKind::Conflict,
                        "process entered an unrelated execution cgroup",
                    ));
                }
                self.verify_owned_attachment(&inner.state, &plan.scope_id, process, plan.uid)?;
                continue;
            }
            let instance_scope = cgroup
                .rsplit('/')
                .next()
                .ok_or_else(|| GatewayError::internal("empty execution slice"))?;
            let moved = self
                .host
                .attach_to_scope(instance_scope, &self.config.slice, process)
                .and_then(|()| self.verify_attachment(process, cgroup, plan.uid));
            match moved {
                Ok(()) => {}
                // A descendant that could not move stays unrouted; one that moved but no longer
                // validates is rejected by the cgroup membership check before routing.
                Err(error) if process.pid != root.pid => {
                    if !error.is_process_exited() {
                        diagnose(
                            "gateway left a Binding descendant unattached",
                            format_args!(
                                "binding_id={} pid={} error={error}",
                                binding.binding_id, process.pid
                            ),
                        );
                    }
                }
                // systemd reports a vanished PID as a generic call failure; report the exit.
                Err(error) => {
                    return Err(match self.host.verify_process(process) {
                        Err(exited) if exited.is_process_exited() => exited,
                        _ => error,
                    });
                }
            }
        }
        self.validate_attachment_plan(inner, binding, scope, plan, root)
    }
    fn verify_attachment_plan(
        &self,
        inner: &mut Inner,
        binding: &PreparedBinding,
        scope: &PreparedScope,
        mut plan: AttachmentPlan,
    ) -> Result<(), GatewayError> {
        // Keep existing pidfds, including descendants no longer in a fresh root snapshot.
        inner
            .pins
            .entry(binding.binding_id.clone())
            .or_insert(std::mem::take(&mut plan.pins));
        for (spec, _) in &plan.existing {
            self.verify_pinned(
                inner,
                &spec.binding_id,
                &inner.state.scopes[&plan.scope_id].bindings[&spec.binding_id],
                plan.uid,
            )?;
        }
        self.verify_pinned(inner, &binding.binding_id, &plan.attachment, plan.uid)?;
        if inner.state.enabled {
            let plans = self.active_plans(inner)?;
            // Never confirm a deployment whose own credential route was just withheld.
            if let Some(error) = self.route(inner, &plans)?.remove(&binding.binding_id) {
                return Err(error.context("Binding failed routing verification"));
            }
        }
        self.validate_attachment_plan(
            inner,
            binding,
            scope,
            &plan,
            &inner.pins[&binding.binding_id][0],
        )
    }
    fn check_overlap<'a>(
        &self,
        state: &'a State,
        id: &ScopeId,
        process: &ProcessIdentity,
    ) -> Result<Option<&'a str>, GatewayError> {
        let actual = self.host.process_cgroup(process.pid)?;
        let mut owned = None;
        for (other_id, domain) in &state.scopes {
            for cgroup in domain.bindings.values().flat_map(Attachment::cgroups) {
                let path = format!("/{cgroup}");
                if actual == path
                    || actual
                        .strip_prefix(&path)
                        .is_some_and(|tail| tail.starts_with('/'))
                {
                    if other_id != id {
                        return Err(GatewayError::new(
                            GatewayErrorKind::Conflict,
                            "process overlaps another Scope",
                        ));
                    }
                    owned = Some(cgroup);
                }
            }
        }
        // Even a generated destination name is not ownership until a Binding is recorded.
        let prefix = format!("/{}/agent-sec-instance-", self.config.slice);
        if actual.starts_with(&prefix) && owned.is_none() {
            return Err(GatewayError::new(
                GatewayErrorKind::Conflict,
                "process remains in an unowned gateway cgroup",
            ));
        }
        Ok(owned)
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
            .and_then(|domain| domain.bindings.get(&cleanup.binding_id))
            .into_iter()
            .flat_map(Attachment::cgroups)
            .map(str::to_owned)
            .collect();
        for cgroup in cgroups {
            // A shared path belongs to every remaining Binding, not just the original root.
            let retained = inner
                .state
                .scopes
                .values()
                .flat_map(|domain| domain.bindings.values())
                .flat_map(Attachment::cgroups)
                .any(|path| path == cgroup);
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
    fn restore(&self, scope: &PreparedScope) -> Result<(), PapError> {
        self.restore_scope(scope)
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
/// A Binding-local reason to withhold routes.
pub(crate) enum RouteFault {
    /// The Binding's own root is unpinned, gone or changed; only its references lapse.
    Stale(GatewayError),
    /// A live member of its cgroups is invalid; no Binding may route those cgroups.
    Members(GatewayError),
}

/// Keeps process exits and identity/ownership rejections local; anything else is unknown
/// host evidence that must fail closed.
fn local(error: GatewayError) -> Result<GatewayError, GatewayError> {
    if error.is_process_exited()
        || matches!(
            error.kind(),
            GatewayErrorKind::PermissionDenied | GatewayErrorKind::Conflict
        )
    {
        Ok(error)
    } else {
        Err(error)
    }
}

fn failure(error: &GatewayError) -> Failure {
    if error.is_process_exited() {
        return Failure::new(FailureKind::Retryable, "GATEWAY_PROCESS_EXITED");
    }
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
