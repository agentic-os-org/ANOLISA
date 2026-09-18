use std::sync::Arc;

use asc_foundation_types::{ResourceId, Revision};
use asc_policy_types::Validate;
use asc_policy_types::authoring::{PolicyTemplate, TemplateEnvelope};
use asc_policy_types::binding::{BindingStatus, BindingView, PreparedBinding};
use asc_policy_types::error::ValidationError;
use asc_policy_types::identifiers::PolicyId;
use asc_policy_types::policy::{PreparedPolicy, validate_policy_name};
use asc_policy_types::scope::{PreparedScope, ScopeSelector};
use uuid::Uuid;

use crate::compiler::PolicyCompiler;
use crate::error::PapError;
use crate::model::Page;
use crate::repository::PapRepository;

const MAX_WRITE_ATTEMPTS: usize = 8;
const MAX_PAGE_SIZE: u32 = 1_000;

#[derive(Clone, Copy)]
enum WriteTarget<'a> {
    Create,
    Update(&'a ResourceId),
}

/// Policy Administration Point for transport-independent desired-state CRUD.
pub struct PapService<R, C> {
    repository: Arc<R>,
    compiler: Arc<C>,
    enqueuer: Option<Arc<dyn crate::BindingReconcileEnqueuer>>,
}

impl<R, C> Clone for PapService<R, C> {
    fn clone(&self) -> Self {
        Self {
            repository: Arc::clone(&self.repository),
            compiler: Arc::clone(&self.compiler),
            enqueuer: self.enqueuer.clone(),
        }
    }
}

impl<R, C> PapService<R, C>
where
    R: PapRepository,
    C: PolicyCompiler,
{
    /// Creates PAP from explicit persistence and synchronous compiler ports.
    pub fn new(repository: Arc<R>, compiler: Arc<C>) -> Self {
        Self {
            repository,
            compiler,
            enqueuer: None,
        }
    }

    #[must_use]
    pub fn with_reconcile_enqueuer(
        mut self,
        enqueuer: Arc<dyn crate::BindingReconcileEnqueuer>,
    ) -> Self {
        self.enqueuer = Some(enqueuer);
        self
    }
    fn check_ready(&self) -> Result<(), PapError> {
        self.enqueuer.as_ref().map_or(Ok(()), |e| e.check_ready())
    }
    fn notify(&self, mut binding: BindingView) -> Result<BindingView, PapError> {
        let Some(enqueuer) = &self.enqueuer else {
            return Ok(binding);
        };
        // Terminal/running no-ops need no admission and must never be failed.
        if !matches!(
            binding.status.phase,
            BindingStatus::PendingApply | BindingStatus::PendingDelete
        ) {
            return Ok(binding);
        }
        let Err(reason) = enqueuer.enqueue(&binding.spec.binding_id) else {
            return Ok(binding);
        };
        match self.repository.fail_pending_binding(&binding, reason) {
            Ok(true) => {
                // Return the snapshot confirmed by the conditional write, without
                // a second read that could observe a newer request or fail.
                binding.status.phase = match binding.status.phase {
                    BindingStatus::PendingApply => BindingStatus::ApplyFailed,
                    BindingStatus::PendingDelete => BindingStatus::DeleteFailed,
                    _ => unreachable!("only pending requests notify"),
                };
                binding.status.error = Some(reason.failure());
                return Ok(binding);
            }
            Ok(false) => match self.repository.get_binding(&binding.spec.binding_id) {
                Ok(current)
                    if current.spec.binding_revision != binding.spec.binding_revision
                        || current.status != binding.status =>
                {
                    return Ok(current);
                }
                Err(PapError::NotFound) => return Err(PapError::NotFound),
                // Do not retry an old rejection against a fresh pending request.
                // A failed reread also cannot confirm termination.
                _ => {}
            },
            Err(_) => {}
        }
        Err(PapError::SchedulingRejected {
            id: binding.spec.binding_id,
            revision: binding.spec.binding_revision,
            reason,
        })
    }

    /// Creates one Policy identity from an authored template.
    ///
    /// PAP generates the identity and starts at revision 1.
    ///
    /// # Errors
    /// Returns validation, lowering, conflict, revision, or persistence errors.
    pub fn create_policy(
        &self,
        policy_name: &str,
        template: &PolicyTemplate,
    ) -> Result<PreparedPolicy, PapError> {
        self.write_policy(WriteTarget::Create, policy_name, template)
    }

    /// Updates one existing Policy identity to an authored template.
    ///
    /// Identical latest content is idempotent. Changed content receives the
    /// next never-reused revision and is lowered synchronously before storage.
    ///
    /// # Errors
    /// Returns validation, lowering, conflict, revision, or persistence errors.
    pub fn update_policy(
        &self,
        policy_id: &ResourceId,
        policy_name: &str,
        template: &PolicyTemplate,
    ) -> Result<PreparedPolicy, PapError> {
        self.write_policy(WriteTarget::Update(policy_id), policy_name, template)
    }

    fn write_policy(
        &self,
        target: WriteTarget<'_>,
        policy_name: &str,
        template: &PolicyTemplate,
    ) -> Result<PreparedPolicy, PapError> {
        validate_policy_name(policy_name)
            .map_err(|message| PapError::InvalidPolicyName(message.to_owned()))?;
        let (update_existing, mut selected_id) = match target {
            WriteTarget::Create => (false, generated_resource_id()?),
            WriteTarget::Update(id) => (true, id.clone()),
        };

        for _ in 0..MAX_WRITE_ATTEMPTS {
            let state = self.repository.get_policy_revision_state(&selected_id)?;
            if update_existing && state.is_none() {
                return Err(PapError::NotFound);
            }
            if !update_existing && state.is_some() {
                selected_id = generated_resource_id()?;
                continue;
            }
            if let Some(current) = state.as_ref().and_then(|value| value.current.as_ref())
                && current.policy_name == policy_name
                && &current.template == template
            {
                return Ok(current.clone());
            }

            let revision =
                next_revision(state.as_ref().map(|value| value.last_allocated_revision))?;
            let candidate = self.prepare_policy(&selected_id, policy_name, revision, template)?;
            match self.repository.put_policy(&candidate) {
                Err(PapError::Conflict) => {
                    if !update_existing {
                        selected_id = generated_resource_id()?;
                    }
                }
                result => return result,
            }
        }
        Err(PapError::Conflict)
    }

    /// Gets the current Policy when its revision matches exactly.
    ///
    /// # Errors
    /// Returns not-found or persistence errors.
    pub fn get_policy(
        &self,
        id: &ResourceId,
        revision: Revision,
    ) -> Result<PreparedPolicy, PapError> {
        self.repository.get_policy(id, revision)
    }

    /// Lists current Policy records.
    ///
    /// # Errors
    /// Returns invalid-pagination or persistence errors.
    pub fn list_policies(&self, limit: u32, offset: u32) -> Result<Page<PreparedPolicy>, PapError> {
        validate_limit(limit)?;
        self.repository.list_policies(limit, offset)
    }

    /// Deletes the current Policy content without allowing revision reuse.
    ///
    /// # Errors
    /// Returns not-found, conflict, or persistence errors.
    pub fn delete_policy_revision(
        &self,
        id: &ResourceId,
        revision: Revision,
    ) -> Result<PreparedPolicy, PapError> {
        self.repository.delete_policy_revision(id, revision)
    }

    /// Creates one Scope identity from an authored selector.
    ///
    /// PAP generates the identity and starts at revision 1.
    ///
    /// # Errors
    /// Returns validation, conflict, revision, or persistence errors.
    pub fn create_scope(&self, selector: &ScopeSelector) -> Result<PreparedScope, PapError> {
        self.write_scope(WriteTarget::Create, selector)
    }

    /// Updates one existing Scope identity to an authored selector.
    ///
    /// Identical latest content is idempotent. Changed content receives the
    /// next never-reused revision, with the repository atomically rejecting a
    /// stale allocation head so this service can retry from the winning update.
    ///
    /// # Errors
    /// Returns validation, conflict, revision, or persistence errors.
    pub fn update_scope(
        &self,
        scope_id: &ResourceId,
        selector: &ScopeSelector,
    ) -> Result<PreparedScope, PapError> {
        self.write_scope(WriteTarget::Update(scope_id), selector)
    }

    fn write_scope(
        &self,
        target: WriteTarget<'_>,
        selector: &ScopeSelector,
    ) -> Result<PreparedScope, PapError> {
        selector.validate().map_err(PapError::InvalidScope)?;
        let (update_existing, mut selected_id) = match target {
            WriteTarget::Create => (false, generated_resource_id()?),
            WriteTarget::Update(id) => (true, id.clone()),
        };

        for _ in 0..MAX_WRITE_ATTEMPTS {
            let state = self.repository.get_scope_revision_state(&selected_id)?;
            if update_existing && state.is_none() {
                return Err(PapError::NotFound);
            }
            if !update_existing && state.is_some() {
                selected_id = generated_resource_id()?;
                continue;
            }
            if let Some(current) = state.as_ref().and_then(|value| value.current.as_ref())
                && &current.selector == selector
            {
                return Ok(current.clone());
            }

            let revision =
                next_revision(state.as_ref().map(|value| value.last_allocated_revision))?;
            let candidate = PreparedScope {
                scope_id: selected_id.clone(),
                revision,
                selector: selector.clone(),
            };
            // The selector was validated before any repository access; the
            // remaining fields are already validated identifier/revision types.
            match self.repository.put_scope(&candidate) {
                Err(PapError::Conflict) => {
                    if !update_existing {
                        selected_id = generated_resource_id()?;
                    }
                }
                result => return result,
            }
        }
        Err(PapError::Conflict)
    }

    /// Gets the current Scope when its revision matches exactly.
    ///
    /// # Errors
    /// Returns not-found or persistence errors.
    pub fn get_scope(
        &self,
        id: &ResourceId,
        revision: Revision,
    ) -> Result<PreparedScope, PapError> {
        self.repository.get_scope(id, revision)
    }

    /// Lists current Scope records.
    ///
    /// # Errors
    /// Returns invalid-pagination or persistence errors.
    pub fn list_scopes(&self, limit: u32, offset: u32) -> Result<Page<PreparedScope>, PapError> {
        validate_limit(limit)?;
        self.repository.list_scopes(limit, offset)
    }

    /// Deletes the current Scope content without allowing revision reuse.
    ///
    /// # Errors
    /// Returns not-found, conflict, or persistence errors.
    pub fn delete_scope_revision(
        &self,
        id: &ResourceId,
        revision: Revision,
    ) -> Result<PreparedScope, PapError> {
        self.repository.delete_scope_revision(id, revision)
    }

    /// Creates one immutable Binding spec from Policy and Scope references.
    ///
    /// PAP generates the identity, starts at revision 1, and assigns
    /// `PENDING_APPLY`.
    ///
    /// # Errors
    /// Returns not-found, validation, conflict, revision, or persistence errors.
    pub fn create_binding(
        &self,
        policy_id: &ResourceId,
        policy_revision: Revision,
        scope_id: &ResourceId,
        scope_revision: Revision,
    ) -> Result<BindingView, PapError> {
        self.write_binding(
            WriteTarget::Create,
            policy_id,
            policy_revision,
            scope_id,
            scope_revision,
        )
    }

    /// Updates the single current Binding record to Apply intent.
    ///
    /// Policy and Scope references are resolved to complete immutable snapshots.
    /// An identical spec is idempotent while Apply is pending, running, or
    /// complete. Same-spec retry after `ApplyFailed` keeps the revision and target
    /// responsibility; only changed specs receive the next revision. Changed specs are
    /// rejected while Applying, and every UPDATE is rejected after Delete intent.
    /// Admission returns `PENDING_APPLY` and notifies the configured runtime;
    /// translation and target I/O execute outside the request.
    ///
    /// # Errors
    /// Returns not-found, validation, operation-in-progress, conflict, revision,
    /// or persistence errors.
    pub fn update_binding(
        &self,
        binding_id: &ResourceId,
        policy_id: &ResourceId,
        policy_revision: Revision,
        scope_id: &ResourceId,
        scope_revision: Revision,
    ) -> Result<BindingView, PapError> {
        self.write_binding(
            WriteTarget::Update(binding_id),
            policy_id,
            policy_revision,
            scope_id,
            scope_revision,
        )
    }

    fn write_binding(
        &self,
        target: WriteTarget<'_>,
        policy_id: &ResourceId,
        policy_revision: Revision,
        scope_id: &ResourceId,
        scope_revision: Revision,
    ) -> Result<BindingView, PapError> {
        self.check_ready()?;
        let (update_existing, mut selected_id) = match target {
            WriteTarget::Create => (false, generated_resource_id()?),
            WriteTarget::Update(id) => (true, id.clone()),
        };

        for _ in 0..MAX_WRITE_ATTEMPTS {
            let current = match self.repository.get_binding(&selected_id) {
                Ok(current) if update_existing => Some(current),
                Ok(_) => {
                    selected_id = generated_resource_id()?;
                    continue;
                }
                Err(PapError::NotFound) if update_existing => return Err(PapError::NotFound),
                Err(PapError::NotFound) => None,
                Err(error) => return Err(error),
            };
            if let Some(current) = current.as_ref() {
                if matches!(
                    current.status.phase,
                    BindingStatus::PendingDelete
                        | BindingStatus::Deleting
                        | BindingStatus::DeleteFailed
                        | BindingStatus::Deleted
                ) {
                    return Err(PapError::OperationInProgress);
                }
                if current.status == BindingStatus::Applying {
                    let identical_reference = current.spec.policy.policy_id == *policy_id
                        && current.spec.policy.revision == policy_revision
                        && current.spec.scope.scope_id == *scope_id
                        && current.spec.scope.revision == scope_revision;
                    return if identical_reference {
                        self.notify(current.clone())
                    } else {
                        Err(PapError::OperationInProgress)
                    };
                }
            }
            let policy =
                self.resolve_binding_policy(current.as_ref(), policy_id, policy_revision)?;
            let scope = self.resolve_binding_scope(current.as_ref(), scope_id, scope_revision)?;

            if let Some(current) = current.as_ref() {
                let identical = current.spec.policy == policy && current.spec.scope == scope;
                if identical {
                    let next_status = current
                        .status
                        .request_apply()
                        .map_err(|_| PapError::OperationInProgress)?;
                    if next_status == current.status {
                        return self.notify(current.clone());
                    }
                }
            }

            let revision = match current.as_ref() {
                Some(current) if current.spec.policy == policy && current.spec.scope == scope => {
                    current.spec.binding_revision
                }
                _ => next_revision(current.as_ref().map(|value| value.spec.binding_revision))?,
            };
            let spec = PreparedBinding {
                binding_id: selected_id.clone(),
                binding_revision: revision,
                policy: policy.clone(),
                scope: scope.clone(),
            };
            let initial_status = BindingStatus::PendingApply;
            let binding = binding_view(spec, initial_status)?;

            // The conditional write saves pending intent and clears its previous error.
            // Notify only after this write succeeds; rejection is terminalized conditionally by PAP.
            match self.repository.update_binding(current.as_ref(), &binding) {
                Err(PapError::Conflict) => {
                    if !update_existing {
                        selected_id = generated_resource_id()?;
                    }
                }
                result => return result.and_then(|binding| self.notify(binding)),
            }
        }
        Err(PapError::Conflict)
    }

    fn resolve_binding_policy(
        &self,
        current: Option<&BindingView>,
        policy_id: &ResourceId,
        policy_revision: Revision,
    ) -> Result<PreparedPolicy, PapError> {
        match self.repository.get_policy(policy_id, policy_revision) {
            Ok(policy) => Ok(policy),
            Err(PapError::NotFound) => current
                .filter(|binding| {
                    binding.spec.policy.policy_id == *policy_id
                        && binding.spec.policy.revision == policy_revision
                })
                .map(|binding| binding.spec.policy.clone())
                .ok_or(PapError::ReferencedPolicyRevisionNotFound),
            Err(error) => Err(error),
        }
    }

    fn resolve_binding_scope(
        &self,
        current: Option<&BindingView>,
        scope_id: &ResourceId,
        scope_revision: Revision,
    ) -> Result<PreparedScope, PapError> {
        match self.repository.get_scope(scope_id, scope_revision) {
            Ok(scope) => Ok(scope),
            Err(PapError::NotFound) => current
                .filter(|binding| {
                    binding.spec.scope.scope_id == *scope_id
                        && binding.spec.scope.revision == scope_revision
                })
                .map(|binding| binding.spec.scope.clone())
                .ok_or(PapError::ReferencedScopeRevisionNotFound),
            Err(error) => Err(error),
        }
    }

    /// Gets the current Binding snapshot and mutable status.
    ///
    /// # Errors
    /// Returns not-found or persistence errors.
    pub fn get_binding(&self, id: &ResourceId) -> Result<BindingView, PapError> {
        self.repository.get_binding(id)
    }

    /// Lists current Binding specs and status.
    ///
    /// # Errors
    /// Returns invalid-pagination or persistence errors.
    pub fn list_bindings(&self, limit: u32, offset: u32) -> Result<Page<BindingView>, PapError> {
        validate_limit(limit)?;
        self.repository.list_bindings(limit, offset)
    }

    /// Accepts irreversible Delete intent at the current spec/revision.
    /// Pending/running deletion is idempotent. `DeleteFailed` retries with a fresh
    /// retry budget. Spec and target records remain until the reconciler confirms
    /// every target absent and physically removes the aggregate. This returns the
    /// accepted `PENDING_DELETE` view without waiting for remote cleanup.
    ///
    /// # Errors
    /// Returns not-found, conflict or persistence errors.
    pub fn delete_binding(&self, id: &ResourceId) -> Result<BindingView, PapError> {
        self.check_ready()?;
        for _ in 0..MAX_WRITE_ATTEMPTS {
            let current = self.repository.get_binding(id)?;
            let next_status = current.status.request_delete();
            if next_status == current.status {
                return self.notify(current);
            }
            let binding = binding_view(current.spec.clone(), next_status)?;

            // Preserve cleanup responsibility while atomically admitting Delete.
            // The daemon will notify its worker only after this write commits.
            match self.repository.update_binding(Some(&current), &binding) {
                Ok(binding) => return self.notify(binding),
                Err(PapError::Conflict) => {}
                Err(error) => return Err(error),
            }
        }
        Err(PapError::Conflict)
    }

    fn prepare_policy(
        &self,
        policy_id: &ResourceId,
        policy_name: &str,
        revision: Revision,
        template: &PolicyTemplate,
    ) -> Result<PreparedPolicy, PapError> {
        let domain_id = PolicyId::new(policy_id.as_str()).map_err(PapError::InvalidIdentifier)?;
        let input = TemplateEnvelope {
            policy_id: domain_id.clone(),
            revision,
            template: template.clone(),
        };
        let canonical_policy = self
            .compiler
            .lower(&input)
            .map_err(PapError::InvalidPolicy)?;
        if canonical_policy.policy_id != domain_id {
            return Err(PapError::InvalidPolicy(ValidationError::new(
                "canonicalPolicy.policyId",
                "compiler output must match the authored Policy identity",
            )));
        }
        if canonical_policy.revision != revision {
            return Err(PapError::InvalidPolicy(ValidationError::new(
                "canonicalPolicy.revision",
                "compiler output must match the authored Policy revision",
            )));
        }
        canonical_policy
            .validate()
            .map_err(PapError::InvalidPolicy)?;
        // Name was checked at admission; the checks above validate every
        // remaining PreparedPolicy invariant with PAP's compiler error paths.
        Ok(PreparedPolicy {
            policy_id: policy_id.clone(),
            policy_name: policy_name.to_owned(),
            revision,
            template: template.clone(),
            canonical_policy,
        })
    }
}

fn binding_view(spec: PreparedBinding, status: BindingStatus) -> Result<BindingView, PapError> {
    let view = BindingView {
        spec,
        status: status.into(),
    };
    view.validate().map_err(PapError::InvalidBinding)?;
    Ok(view)
}

fn validate_limit(limit: u32) -> Result<(), PapError> {
    if (1..=MAX_PAGE_SIZE).contains(&limit) {
        Ok(())
    } else {
        Err(PapError::InvalidPagination)
    }
}

fn next_revision(current: Option<Revision>) -> Result<Revision, PapError> {
    match current {
        Some(revision) => revision
            .checked_next()
            .map_err(|_| PapError::RevisionExhausted),
        None => Revision::new(1).map_err(|_| PapError::RevisionExhausted),
    }
}

fn generated_resource_id() -> Result<ResourceId, PapError> {
    ResourceId::new(Uuid::new_v4().to_string())
        .map_err(|error| PapError::InvalidIdentifier(error.to_string()))
}
