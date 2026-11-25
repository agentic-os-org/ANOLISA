use std::sync::{Arc, Mutex};

use asc_foundation_types::{ResourceId, Revision};
use asc_policy_types::Validate;
use asc_policy_types::authoring::PolicySource;
use asc_policy_types::binding::{BindingStatus, BindingView};
use asc_policy_types::error::ValidationError;
use asc_policy_types::policy::{PreparedPolicy, validate_policy_name};
use asc_policy_types::scope::{
    PolicyReference, PreparedScope, ScopeDeletion, ScopeSelector, ScopeStatus,
};
use uuid::Uuid;

use crate::error::PapError;
use crate::model::Page;
use crate::repository::PapRepository;

const MAX_WRITE_ATTEMPTS: usize = 8;
const MAX_PAGE_SIZE: u32 = 1_000;

#[derive(Clone, Copy)]
enum WriteTarget<'a> {
    Create,
    Update(&'a ResourceId),
    UpdateFile(&'a ResourceId),
}

/// Policy Administration Point for templates, assignments and owned Binding intent.
pub struct PapService<R> {
    selection_cache: Arc<crate::selection::SelectionCache>,
    scope_mutations: Arc<Mutex<()>>,
    automatic: Option<Arc<AutomaticScopes>>,
    credential_scope_mutations: Arc<Mutex<()>>,
    repository: Arc<R>,
    discovery: Option<Arc<dyn crate::ScopeDiscovery>>,
    execution: Option<Arc<dyn crate::ScopeExecution>>,
    registration: Option<Arc<dyn crate::CredentialRegistration>>,
    enqueuer: Option<Arc<dyn crate::BindingReconcileEnqueuer>>,
}

struct AutomaticScopes {
    wake: Box<dyn Fn() + Send + Sync>,
    selectors: &'static [asc_policy_cedar::deployment::DeploymentSelector],
    matchers:
        Result<std::collections::BTreeMap<String, asc_policy_types::scope::ProcessMatcher>, String>,
    passes: Mutex<()>,
    diagnostics: Mutex<crate::automatic::PlanningDiagnostics>,
}

impl<R> Clone for PapService<R> {
    fn clone(&self) -> Self {
        Self {
            selection_cache: self.selection_cache.clone(),
            scope_mutations: self.scope_mutations.clone(),
            automatic: self.automatic.clone(),
            credential_scope_mutations: self.credential_scope_mutations.clone(),
            repository: Arc::clone(&self.repository),
            enqueuer: self.enqueuer.clone(),
            discovery: self.discovery.clone(),
            execution: self.execution.clone(),
            registration: self.registration.clone(),
        }
    }
}

impl<R> PapService<R>
where
    R: PapRepository,
{
    /// Creates PAP with a repository; templates are validated before admission.
    pub fn new(repository: Arc<R>) -> Self {
        Self {
            selection_cache: Arc::default(),
            scope_mutations: Arc::new(Mutex::new(())),
            automatic: None,
            credential_scope_mutations: Arc::new(Mutex::new(())),
            repository,
            enqueuer: None,
            discovery: None,
            execution: None,
            registration: None,
        }
    }

    /// Enables automatic deployment using the daemon's startup integration configuration.
    #[must_use]
    pub fn with_automatic_scopes(
        mut self,
        matchers: Result<
            std::collections::BTreeMap<String, asc_policy_types::scope::ProcessMatcher>,
            String,
        >,
        selectors: &'static [asc_policy_cedar::deployment::DeploymentSelector],
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        self.automatic = Some(Arc::new(AutomaticScopes {
            wake: Box::new(wake),
            matchers,
            selectors,
            passes: Mutex::new(()),
            diagnostics: Mutex::default(),
        }));
        self
    }

    /// Reads a Policy and the latest planning results for that exact revision.
    ///
    /// # Errors
    /// Returns the normal Policy read errors or an unavailable diagnostic cache.
    pub fn policy_details(
        &self,
        id: &ResourceId,
        revision: Revision,
    ) -> Result<asc_policy_types::policy::PolicyDetails, PapError> {
        let policy = self.get_policy(id, revision)?;
        let automatic_diagnostics = match &self.automatic {
            Some(automatic)
                if crate::is_automatic_source(&policy) && automatic.matchers.is_err() =>
            {
                // The management read still works when the full v3 set exceeds the planning limit.
                let (policies, views) = crate::selection::SelectionCache::isolated_deployment(
                    vec![policy.clone()],
                    automatic.selectors,
                )?;
                crate::automatic::derive(policies, automatic.matchers.as_ref(), views)?.diagnostics
            }
            Some(automatic) if crate::is_automatic_source(&policy) => automatic
                .diagnostics
                .lock()
                .map_err(|_| PapError::Unavailable)?
                .for_policy(id, revision),
            _ => Vec::new(),
        };
        Ok(asc_policy_types::policy::PolicyDetails {
            policy,
            automatic_diagnostics,
        })
    }

    fn notify_policy_change(&self) {
        if let Some(automatic) = &self.automatic {
            (automatic.wake)();
        }
    }

    /// Connects committed Binding intent to the existing reconciliation runtime.
    #[must_use]
    pub fn with_reconcile_enqueuer(
        mut self,
        enqueuer: Arc<dyn crate::BindingReconcileEnqueuer>,
    ) -> Self {
        self.enqueuer = Some(enqueuer);
        self
    }
    /// Connects assignment admission and deletion to owned discovery workers.
    #[must_use]
    pub fn with_scope_discovery(mut self, discovery: Arc<dyn crate::ScopeDiscovery>) -> Self {
        self.discovery = Some(discovery);
        self
    }
    /// Connects credential Scope lifecycle to the execution owner.
    #[must_use]
    pub fn with_scope_execution(mut self, execution: Arc<dyn crate::ScopeExecution>) -> Self {
        self.execution = Some(execution);
        self
    }

    /// Connects root-authorized Policy admission to protected pair storage.
    #[must_use]
    pub fn with_credential_registration(
        mut self,
        registration: Arc<dyn crate::CredentialRegistration>,
    ) -> Self {
        self.registration = Some(registration);
        self
    }

    /// Classifies credentials only after validating the complete authored source.
    /// # Errors
    /// Rejects malformed Cedar and retired native credential policies.
    pub fn source_uses_credentials(template: &PolicySource) -> Result<bool, PapError> {
        match template {
            PolicySource::Cedar(source) => asc_policy_cedar::http::compile(source)
                .map(|rules| !rules.is_empty())
                .map_err(|error| PapError::InvalidCedar(error.diagnostic())),
            PolicySource::SecCore(source) if source.uses_credentials() => {
                Err(PapError::InvalidCedar(
                    "native credential policies are unsupported; use Cedar schema revision 2"
                        .into(),
                ))
            }
            PolicySource::SecCore(_) => Ok(false),
        }
    }

    fn scope_mutation_lock(
        &self,
        scope: &PreparedScope,
    ) -> Result<std::sync::MutexGuard<'_, ()>, PapError> {
        // Pinned policies never change; mixed scopes share the credential domain.
        // Hold only one domain so gateway waits cannot block unrelated file work.
        let mutations = if scope.uses_credentials() {
            &self.credential_scope_mutations
        } else {
            &self.scope_mutations
        };
        mutations.lock().map_err(|_| PapError::Persistence)
    }

    fn check_ready(&self) -> Result<(), PapError> {
        self.enqueuer.as_ref().map_or(Ok(()), |e| e.check_ready())
    }
    fn notify(
        &self,
        receipt: &asc_policy_repository::BindingIntentReceipt,
    ) -> Result<BindingView, PapError> {
        let mut binding = receipt.binding.clone();
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
        match self.repository.fail_pending_binding(receipt, reason) {
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
    /// Returns validation, conflict, revision, or persistence errors.
    #[tracing::instrument(skip_all, name = "pap.create_policy")]
    pub fn create_policy(
        &self,
        policy_name: &str,
        template: &PolicySource,
    ) -> Result<PreparedPolicy, PapError> {
        self.write_policy(WriteTarget::Create, policy_name, template)
    }

    /// Updates one existing Policy identity to an authored template.
    ///
    /// Identical latest content is idempotent. Changed content receives the
    /// next never-reused revision and is validated before storage.
    ///
    /// # Errors
    /// Returns validation, conflict, revision, or persistence errors.
    #[tracing::instrument(skip_all, name = "pap.update_policy")]
    pub fn update_policy(
        &self,
        policy_id: &ResourceId,
        policy_name: &str,
        template: &PolicySource,
    ) -> Result<PreparedPolicy, PapError> {
        self.write_policy(WriteTarget::Update(policy_id), policy_name, template)
    }

    /// Updates a file Policy without allowing a concurrent credential revision to be downgraded.
    /// # Errors
    /// Requires credential authority when either old or new content uses credentials.
    #[tracing::instrument(skip_all, name = "pap.update_policy")]
    pub fn update_file_policy(
        &self,
        policy_id: &ResourceId,
        policy_name: &str,
        template: &PolicySource,
    ) -> Result<PreparedPolicy, PapError> {
        self.write_policy(WriteTarget::UpdateFile(policy_id), policy_name, template)
    }

    fn write_policy(
        &self,
        target: WriteTarget<'_>,
        policy_name: &str,
        template: &PolicySource,
    ) -> Result<PreparedPolicy, PapError> {
        validate_policy_name(policy_name)
            .map_err(|message| PapError::InvalidPolicyName(message.to_owned()))?;
        validate_authored_source(template)?;
        let uses_credentials = Self::source_uses_credentials(template)?;
        if matches!(target, WriteTarget::UpdateFile(_)) && uses_credentials {
            return Err(PapError::CredentialAuthorityRequired);
        }
        let mut credential_bindings = None;
        let (update_existing, mut selected_id) = match target {
            WriteTarget::Create => (false, generated_resource_id()?),
            WriteTarget::Update(id) | WriteTarget::UpdateFile(id) => (true, id.clone()),
        };

        for _ in 0..MAX_WRITE_ATTEMPTS {
            let state = self.repository.get_policy_revision_state(&selected_id)?;
            if matches!(target, WriteTarget::UpdateFile(_))
                && (uses_credentials
                    || state
                        .as_ref()
                        .and_then(|s| s.current.as_ref())
                        .is_some_and(PreparedPolicy::uses_credentials))
            {
                return Err(PapError::CredentialAuthorityRequired);
            }
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

            if credential_bindings.is_none() {
                credential_bindings = Some(if uses_credentials {
                    let PolicySource::Cedar(source) = template else {
                        return Err(PapError::Unavailable);
                    };
                    if let Some(current) = state.as_ref().and_then(|value| value.current.as_ref())
                        && &current.template == template
                    {
                        current.credential_bindings.clone()
                    } else {
                        self.registration
                            .as_ref()
                            .ok_or(PapError::Unavailable)?
                            .register(source)?
                    }
                } else {
                    Vec::new()
                });
            }
            let revision =
                next_revision(state.as_ref().map(|value| value.last_allocated_revision))?;
            let candidate = PreparedPolicy {
                deployment: None,
                policy_id: selected_id.clone(),
                policy_name: policy_name.to_owned(),
                revision,
                template: template.clone(),
                credential_bindings: credential_bindings.clone().ok_or(PapError::Persistence)?,
            };
            crate::validate_policy_snapshot(&candidate)?;
            match self.repository.put_policy(&candidate) {
                Err(PapError::Conflict) => {
                    if !update_existing {
                        selected_id = generated_resource_id()?;
                    }
                }
                Err(error) => {
                    if self
                        .repository
                        .get_policy(&candidate.policy_id, candidate.revision)
                        .as_ref()
                        == Ok(&candidate)
                    {
                        self.notify_policy_change();
                        return Ok(candidate);
                    }
                    return Err(error);
                }
                Ok(policy) => {
                    self.notify_policy_change();
                    return Ok(policy);
                }
            }
        }
        Err(PapError::Conflict)
    }

    /// Gets the current Policy when its revision matches exactly.
    ///
    /// # Errors
    /// Returns not-found or persistence errors.
    #[tracing::instrument(skip_all, name = "pap.get_policy")]
    pub fn get_policy(
        &self,
        id: &ResourceId,
        revision: Revision,
    ) -> Result<PreparedPolicy, PapError> {
        self.repository.get_policy(id, revision)
    }

    /// Reads selected current policies together, in requested order.
    ///
    /// # Errors
    /// Rejects empty or oversized selections, missing policies, and storage failures.
    pub fn current_policies(&self, ids: &[ResourceId]) -> Result<Vec<PreparedPolicy>, PapError> {
        self.repository.current_policies(ids)
    }

    /// Select Sandbox statements using the selector registered by the daemon at startup.
    ///
    /// # Errors
    /// Returns a repository, schema or selection error for the current generation.
    pub fn sandbox_selections(
        &self,
        select: asc_policy_cedar::selection::Select,
    ) -> Result<Arc<crate::ConsumerPolicySet>, PapError> {
        self.selection_cache
            .sandbox(self.repository.as_ref(), select)
    }

    /// Derives request policies independently of Scope and Binding assignments.
    ///
    /// # Errors
    /// Rejects an inconsistent/unavailable read or any invalid Cedar member; never
    /// returns a partial set. Runtime-specific compatibility is checked by the PDP.
    pub fn authorization_policies(
        &self,
    ) -> Result<asc_policy_types::cedar::AuthorizationPolicySet, PapError> {
        use asc_policy_types::cedar::{AuthorizationPolicy, AuthorizationPolicySet};

        let mut policies = Vec::new();
        for policy in self.repository.all_current_policies()? {
            if let PolicySource::Cedar(source) = policy.template {
                let eligible =
                    asc_policy_cedar::request_authorization_eligible(&source).map_err(|error| {
                        PapError::InvalidCedar(format!(
                            "{} revision {}: {}",
                            policy.policy_id,
                            policy.revision.get(),
                            error.diagnostic()
                        ))
                    })?;
                if eligible {
                    policies.push(AuthorizationPolicy {
                        policy_id: policy.policy_id,
                        revision: policy.revision,
                        source,
                    });
                }
            }
        }
        Ok(AuthorizationPolicySet {
            version: crate::selection::authorization_digest(&policies)?,
            policies,
        })
    }

    /// Lists current Policy records.
    ///
    /// # Errors
    /// Returns invalid-pagination or persistence errors.
    #[tracing::instrument(skip_all, name = "pap.list_policies")]
    pub fn list_policies(&self, limit: u32, offset: u32) -> Result<Page<PreparedPolicy>, PapError> {
        validate_limit(limit)?;
        self.repository.list_policies(limit, offset)
    }

    /// Deletes the current Policy content without allowing revision reuse.
    ///
    /// # Errors
    /// Returns not-found, conflict, or persistence errors.
    #[tracing::instrument(skip_all, name = "pap.delete_policy_revision")]
    pub fn delete_policy_revision(
        &self,
        id: &ResourceId,
        revision: Revision,
    ) -> Result<PreparedPolicy, PapError> {
        let deleted = self.repository.delete_policy_revision(id, revision)?;
        self.notify_policy_change();
        Ok(deleted)
    }

    fn manual_scope_snapshots(
        &self,
        policies: &[PolicyReference],
    ) -> Result<Vec<PreparedPolicy>, PapError> {
        let snapshots = policies
            .iter()
            .map(|p| {
                self.repository
                    .get_policy(&p.policy_id, p.policy_revision)
                    .map_err(|e| {
                        if matches!(e, PapError::NotFound) {
                            PapError::ReferencedPolicyRevisionNotFound
                        } else {
                            e
                        }
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        for policy in &snapshots {
            if crate::is_automatic_source(policy) {
                return Err(PapError::InvalidScope(ValidationError::new(
                    "policyTemplates",
                    format!(
                        "policy {} revision {} uses schema v3 and is assigned automatically",
                        policy.policy_id,
                        policy.revision.get()
                    ),
                )));
            }
        }
        Ok(snapshots)
    }

    /// Creates an immutable assignment with optional credential process owner attribution.
    /// # Errors
    /// Rejects invalid snapshots, owner constraints or unavailable execution before persistence.
    #[tracing::instrument(skip_all, name = "pap.create_scope", fields(scope_id = tracing::field::Empty))]
    pub fn create_scope_assignment(
        &self,
        selector: &ScopeSelector,
        policies: &[PolicyReference],
        owner_uid: Option<u32>,
    ) -> Result<PreparedScope, PapError> {
        selector.validate().map_err(PapError::InvalidScope)?;
        // The current AgentSight adapter has no cgroup instance contract.
        if matches!(selector, ScopeSelector::CgroupId { .. }) {
            return Err(PapError::InvalidScope(ValidationError::new(
                "selector",
                "cgroup assignments are not supported",
            )));
        }
        let snapshots = self.manual_scope_snapshots(policies)?;
        let candidate = PreparedScope {
            scope_id: generated_resource_id()?,
            selector: selector.clone(),
            policy_snapshots: snapshots,
            owner_uid,
            status: asc_policy_types::scope::ScopeStatus::Active,
        };
        candidate.validate().map_err(PapError::InvalidScope)?;
        let _guard = self.scope_mutation_lock(&candidate)?;
        let discovery = self.discovery.as_ref().ok_or(PapError::Unavailable)?;
        let credential = candidate.uses_credentials();
        // File admission retains its own readiness contract; credential-only scopes
        // must not depend on a disabled file enforcement backend.
        let mut requires_file = false;
        for policy in &candidate.policy_snapshots {
            requires_file |= if !policy.uses_credentials() {
                true
            } else if let PolicySource::Cedar(source) = &policy.template {
                let syntax = asc_policy_cedar::pst(source)
                    .map_err(|error| PapError::InvalidCedar(error.diagnostic()))?;
                asc_policy_cedar::project_pst(&syntax, asc_policy_cedar::PolicyDomain::File)
                    .is_some()
            } else {
                false
            };
        }
        if requires_file {
            self.check_ready()?;
        }
        if credential {
            self.execution
                .as_ref()
                .ok_or(PapError::Unavailable)?
                .validate(&candidate)?;
        }
        let scope = match self.repository.put_scope(&candidate) {
            // A commit can succeed before its acknowledgement is lost. Only the
            // exact server-generated assignment proves admission in that case.
            Err(PapError::Persistence) => match self.repository.get_scope(&candidate.scope_id) {
                Ok(saved) if saved == candidate => saved,
                _ => return Err(PapError::Persistence),
            },
            result => result?,
        };
        tracing::Span::current().record("scope_id", tracing::field::display(&scope.scope_id));
        let started = (|| {
            if credential {
                self.execution
                    .as_ref()
                    .ok_or(PapError::Unavailable)?
                    .prepare(&scope)?;
            }
            self.repository
                .scope_discovery_seed(&scope.scope_id)
                .and_then(|seed| discovery.start(&seed))
        })();
        tracing::debug!(
            target: "asc_observability::diagnostic", component = "policy_pap",
            scope_id = %scope.scope_id, discovery_result = ?started,
            "scope admitted; discovery start attempted"
        );
        if let Err(error) = started {
            let cleanup = self
                .repository
                .begin_scope_delete(&scope.scope_id)
                .and_then(|saved| match saved {
                    Some(saved) => self.complete_scope_deletion(&saved, credential),
                    None => Ok(()),
                });
            if let Err(cleanup_error) = cleanup {
                tracing::error!(
                    target: "asc_process_diagnostic",
                    scope_id = %scope.scope_id,
                    start_error = %error,
                    %cleanup_error,
                    "scope discovery startup compensation failed; inspect and retry Scope deletion"
                );
            }
            return Err(error);
        }
        Ok(scope)
    }

    /// Reads an assignment by identity.
    /// # Errors
    /// Returns not-found or storage failures.
    pub fn get_scope(&self, id: &ResourceId) -> Result<PreparedScope, PapError> {
        self.repository.get_scope(id)
    }

    /// Lists assignments including those awaiting cleanup.
    /// # Errors
    /// Returns pagination or storage failures.
    pub fn list_scopes(&self, limit: u32, offset: u32) -> Result<Page<PreparedScope>, PapError> {
        validate_limit(limit)?;
        self.repository.list_scopes(limit, offset)
    }

    /// Accepts irreversible deletion, stops discovery and schedules owned cleanup.
    /// A successful response acknowledges intent, not remote completion.
    /// # Errors
    /// Returns not-found, discovery or storage failures, retaining admitted intent.
    pub fn delete_scope(&self, id: &ResourceId) -> Result<ScopeDeletion, PapError> {
        match self.repository.get_scope(id) {
            Ok(scope) => self.delete_scope_snapshot(&scope),
            Err(PapError::NotFound) => Ok(ScopeDeletion {
                scope_id: id.clone(),
                completed: true,
            }),
            Err(error) => Err(error),
        }
    }

    /// Deletes the exact assignment observed by the caller's authority check.
    /// # Errors
    /// Reports scope ownership, concurrent changes and cleanup failures.
    #[tracing::instrument(skip_all, name = "pap.delete_scope", fields(scope_id = %requested.scope_id))]
    pub fn delete_scope_snapshot(
        &self,
        requested: &PreparedScope,
    ) -> Result<ScopeDeletion, PapError> {
        let id = &requested.scope_id;
        if requested.automatic_backend().is_some() && requested.status == ScopeStatus::Active {
            return Err(PapError::InvalidScope(ValidationError::new(
                "scopeId",
                "change or delete the source Policy to withdraw this automatic assignment",
            )));
        }
        let _guard = self.scope_mutation_lock(requested)?;
        let Some(current) = self.match_scope_snapshot(requested)? else {
            return Ok(ScopeDeletion {
                scope_id: id.clone(),
                completed: true,
            });
        };
        let scope = if current.status == ScopeStatus::Deleting {
            current
        } else if let Some(deleting) = self.repository.begin_scope_delete(id)? {
            deleting
        } else {
            return Ok(ScopeDeletion {
                scope_id: id.clone(),
                completed: true,
            });
        };
        tracing::debug!(
            target: "asc_observability::diagnostic", component = "policy_pap",
            scope_id = %id, "scope deletion intent committed"
        );
        // Close child admission before joining discovery. A stop failure retains
        // deletion intent; retry delete/retry_scope after discovery is available.
        self.complete_scope_deletion(&scope, true)?;
        tracing::debug!(
            target: "asc_observability::diagnostic", component = "policy_pap",
            scope_id = %id, "scope discovery stopped; binding cleanup notified"
        );
        let completed = match self.repository.get_scope(id) {
            Ok(_) => false,
            Err(PapError::NotFound) => true,
            Err(error) => return Err(error),
        };
        Ok(ScopeDeletion {
            scope_id: id.clone(),
            completed,
        })
    }

    /// Resolve the current candidate before checking retry authority. A rejected initial
    /// publication has a deterministic Scope ID but no persisted serving snapshot.
    /// # Errors
    /// Returns not found for an absent current assignment or candidate.
    pub fn scope_for_retry(&self, id: &ResourceId) -> Result<PreparedScope, PapError> {
        match self.repository.get_scope(id) {
            Err(PapError::NotFound)
                if self.automatic.is_some() && id.as_str().starts_with("auto-") =>
            {
                let automatic = self.automatic.as_ref().ok_or(PapError::NotFound)?;
                self.automatic_plan(automatic)?.prepare(id)
            }
            other => other,
        }
    }

    /// Explicitly retries failed work owned by an assignment.
    /// # Errors
    /// Returns not-found, discovery, scheduling or storage failures.
    pub fn retry_scope(&self, id: &ResourceId) -> Result<PreparedScope, PapError> {
        self.retry_scope_snapshot(&self.scope_for_retry(id)?)
    }

    /// Retries the assignment observed by the caller's authority check.
    /// # Errors
    /// Reports changed or missing assignments, discovery, scheduling or storage failures.
    pub fn retry_scope_snapshot(
        &self,
        requested: &PreparedScope,
    ) -> Result<PreparedScope, PapError> {
        let id = &requested.scope_id;
        if requested.automatic_backend().is_some() && requested.status == ScopeStatus::Active {
            return self
                .reconcile_automatic(Some(requested))?
                .ok_or(PapError::Unavailable);
        }
        let _guard = self.scope_mutation_lock(requested)?;
        let Some(scope) = self.match_scope_snapshot(requested)? else {
            return if requested.automatic_backend().is_some()
                && requested.status == ScopeStatus::Deleting
            {
                Ok(requested.clone())
            } else {
                Err(PapError::NotFound)
            };
        };
        let mut admission = None;
        if scope.status == ScopeStatus::Deleting {
            self.complete_scope_deletion(&scope, true)?;
        } else if scope.uses_credentials() {
            match self.restore_active_scope(&scope) {
                Ok(()) => {}
                // A retired credential keeps its tombstone, but it must not strand
                // independent file Bindings. Resume their work, then surface it.
                Err(PapError::Conflict) => {
                    self.recover_active_scope(&scope)?;
                    admission = Some(PapError::Conflict);
                }
                Err(error) => return Err(error),
            }
        }
        match self.repository.retry_scope(id) {
            Ok(bindings) => self.notify_all(bindings)?,
            Err(PapError::NotFound) if scope.status == ScopeStatus::Deleting => {}
            Err(error) => return Err(error),
        }
        match admission {
            Some(error) => Err(error),
            None => Ok(scope),
        }
    }

    // Called with the Scope's mutation domain held. Manual assignments may advance from
    // Active to Deleting while an authorized request waits; their contents stay pinned.
    fn match_scope_snapshot(
        &self,
        requested: &PreparedScope,
    ) -> Result<Option<PreparedScope>, PapError> {
        let current = match self.repository.get_scope(&requested.scope_id) {
            Err(PapError::NotFound) => return Ok(None),
            result => result?,
        };
        if requested.automatic_backend().is_none() || &current == requested {
            Ok(Some(current))
        } else if requested.status == ScopeStatus::Deleting {
            // Its replacement now owns the same ID. The original deletion is complete.
            Ok(None)
        } else {
            Err(PapError::Conflict)
        }
    }

    /// Re-admits an explicitly retried Scope, so credential failures must surface.
    fn restore_active_scope(&self, scope: &PreparedScope) -> Result<(), PapError> {
        if scope.uses_credentials() {
            let execution = self.execution.as_ref().ok_or(PapError::Unavailable)?;
            execution.validate(scope)?;
            execution.prepare(scope)?;
        }
        self.start_scope_discovery(scope)
    }

    /// Resumes an existing canonical Scope during startup recovery.
    ///
    /// A retired credential keeps its tombstone, so restoration must not reject
    /// the Scope: independent file Policies still need their discovery worker.
    fn recover_active_scope(&self, scope: &PreparedScope) -> Result<(), PapError> {
        if scope.uses_credentials() {
            let execution = self.execution.as_ref().ok_or(PapError::Unavailable)?;
            execution.validate(scope)?;
            execution.restore(scope)?;
        }
        self.start_scope_discovery(scope)
    }

    fn start_scope_discovery(&self, scope: &PreparedScope) -> Result<(), PapError> {
        self.discovery
            .as_ref()
            .ok_or(PapError::Unavailable)?
            .start(&self.repository.scope_discovery_seed(&scope.scope_id)?)
    }

    fn release_scope_execution(&self, id: &ResourceId) -> Result<(), PapError> {
        self.execution
            .as_ref()
            .ok_or(PapError::Unavailable)?
            .release(id)?;
        self.repository.finish_scope_execution(id)
    }

    fn complete_scope_deletion(
        &self,
        scope: &PreparedScope,
        stop_discovery: bool,
    ) -> Result<(), PapError> {
        // Durable release readiness proves discovery was finished after revocation.
        // Do not republish revocation on every sweep while retrying execution release.
        if scope.uses_credentials() {
            let execution = self.execution.as_ref().ok_or(PapError::Unavailable)?;
            match self
                .repository
                .scope_execution_ready_to_release(&scope.scope_id)
            {
                Ok(true) => return self.release_scope_execution(&scope.scope_id),
                Ok(false) => execution.revoke(scope)?,
                Err(error) => {
                    // Committed deletion must withdraw authority even when storage cannot confirm cleanup.
                    execution.revoke(scope)?;
                    return Err(error);
                }
            }
        }
        if stop_discovery {
            self.discovery
                .as_ref()
                .ok_or(PapError::Unavailable)?
                .stop(&scope.scope_id)?;
        }
        self.notify_all(self.repository.finish_scope_discovery(&scope.scope_id)?)?;
        if scope.uses_credentials()
            && self
                .repository
                .scope_execution_ready_to_release(&scope.scope_id)?
        {
            self.release_scope_execution(&scope.scope_id)?;
        }
        Ok(())
    }

    /// Sweeps durable deletion responsibilities after Binding cleanup, including zero-instance scopes.
    /// Wire to the existing reconciliation compensation worker; no extra scanner is required.
    /// # Errors
    /// Attempts every saved deletion and returns a failure if any responsibility remains blocked.
    pub fn sweep_deleting_scopes(&self) -> Result<(), PapError> {
        self.recover_scope_work(false)
    }

    /// Restores active execution before discovery and resumes durable deletions on restart.
    /// Call after execution, discovery and reconciliation have been wired.
    /// # Errors
    /// Reports file/storage failures; credential failures remain logged, pending and retryable.
    pub fn recover_scopes(&self) -> Result<(), PapError> {
        self.recover_scope_work(true)
    }

    /// Reconcile only after Policy persistence. Runtime storage and discovery failures are scoped
    /// to their assignment and retried by the same periodic worker after restart.
    fn reconcile_automatic(
        &self,
        requested: Option<&PreparedScope>,
    ) -> Result<Option<PreparedScope>, PapError> {
        let Some(automatic) = &self.automatic else {
            return Ok(None);
        };
        // An unreadable mapping cannot authorize changes to persisted assignments.
        // Gateway recovery keeps automatic traffic fenced until planning succeeds.
        if let Err(reason) = &automatic.matchers {
            return match requested {
                Some(_) => Err(PapError::InvalidScope(ValidationError::new(
                    "selector",
                    reason.clone(),
                ))),
                None => Ok(None),
            };
        }
        let _pass = automatic.passes.lock().map_err(|_| PapError::Persistence)?;
        self.reconcile_automatic_scopes(automatic, requested)?;
        let Some(expected) = requested else {
            return Ok(None);
        };
        // Keep the planning pass until the selected revision's failed Bindings are queued.
        let _guard = self.scope_mutation_lock(expected)?;
        let scope = self.repository.get_scope(&expected.scope_id)?;
        self.notify_all(self.repository.retry_scope(&scope.scope_id)?)?;
        Ok(Some(scope))
    }

    fn automatic_plan(
        &self,
        automatic: &AutomaticScopes,
    ) -> Result<crate::automatic::Derived, PapError> {
        let (policies, views) = self
            .selection_cache
            .deployment(self.repository.as_ref(), automatic.selectors)?;
        crate::automatic::derive(policies, automatic.matchers.as_ref(), views)
    }

    fn reconcile_automatic_scopes(
        &self,
        automatic: &AutomaticScopes,
        requested: Option<&PreparedScope>,
    ) -> Result<(), PapError> {
        let retry = requested.map(|scope| &scope.scope_id);
        let desired = self.automatic_plan(automatic)?;
        let mut current = self.current_automatic_scopes()?;
        if let Some(expected) = requested {
            let id = &expected.scope_id;
            // All automatic publication passes share this lock. For an unpublished
            // assignment, compare the candidate computed from the current PAP sources.
            let matches = match current.get(id) {
                Some(saved) => saved == expected,
                None => desired.prepare(id)? == *expected,
            };
            if !matches {
                return Err(PapError::Conflict);
            }
        }
        let publications = crate::automatic::publications(&current, &desired)?;
        let mut outcomes = Vec::new();
        let mut retry_error = None;
        for ids in publications {
            let mut updates = crate::automatic::take_publication(ids, &mut current);
            if let Err(error) = desired.prepare_publication(&mut updates) {
                let error = if let Some(serving) =
                    updates.values().find_map(|update| update.expected.as_ref())
                {
                    let _guard = self.scope_mutation_lock(serving)?;
                    self.withdraw_deleted_grants(&updates, &desired.sources)
                        .err()
                        .unwrap_or(error)
                } else {
                    error
                };
                for id in updates.keys() {
                    outcomes.push((
                        id.clone(),
                        crate::automatic::ScopeOutcome::Failed(error.to_string()),
                    ));
                }
                if retry.is_some_and(|id| updates.contains_key(id)) {
                    retry_error = Some(error);
                }
                continue;
            }
            let Some(candidate) = updates
                .values()
                .find_map(|update| update.candidate.as_ref())
            else {
                continue;
            };
            // All members of one publication belong to the same mutation domain.
            let _guard = self.scope_mutation_lock(candidate)?;
            let publication_error = self
                .apply_automatic(candidate, &updates, &desired.sources)
                .err();
            for (id, update) in &updates {
                let Some(scope) = &update.candidate else {
                    continue;
                };
                let discovery_error = if publication_error.is_none() {
                    self.start_scope_discovery(scope).err()
                } else {
                    None
                };
                let error = publication_error.as_ref().or(discovery_error.as_ref());
                outcomes.push((id.clone(), self.automatic_scope_outcome(id, error)));
                if retry == Some(id) && discovery_error.is_some() {
                    retry_error = discovery_error;
                }
            }
            if retry.is_some_and(|id| updates.contains_key(id)) && publication_error.is_some() {
                retry_error = publication_error;
            }
        }
        for (id, scope) in current {
            if scope.status == ScopeStatus::Deleting {
                continue;
            }
            let _guard = self.scope_mutation_lock(&scope)?;
            let deleting = self
                .repository
                .delete_automatic_scope(&scope, &desired.sources)?;
            if let Err(error) = self.complete_scope_deletion(&deleting, true) {
                tracing::warn!(target:"asc_process_diagnostic",scope_id=%id,%error,"automatic Scope cleanup remains pending");
                if retry == Some(&id) {
                    retry_error = Some(error);
                }
            }
        }
        *automatic
            .diagnostics
            .lock()
            .map_err(|_| PapError::Persistence)? = desired.into_diagnostics(outcomes);
        if let Some(error) = retry_error {
            return Err(error);
        }
        Ok(())
    }

    fn current_automatic_scopes(
        &self,
    ) -> Result<std::collections::BTreeMap<ResourceId, PreparedScope>, PapError> {
        let mut current = std::collections::BTreeMap::new();
        let mut after = None;
        loop {
            let page = self.repository.scan_scopes(after.as_ref(), 128)?;
            if page.is_empty() {
                break;
            }
            for scope in page {
                after = Some(scope.scope_id.clone());
                if scope.automatic_backend().is_some() {
                    current.insert(scope.scope_id.clone(), scope);
                }
            }
        }
        Ok(current)
    }

    fn automatic_scope_outcome(
        &self,
        scope_id: &ResourceId,
        error: Option<&PapError>,
    ) -> crate::automatic::ScopeOutcome {
        if let Some(error) = error {
            tracing::warn!(target:"asc_process_diagnostic", %scope_id, %error, "automatic Scope could not be applied");
            crate::automatic::ScopeOutcome::Failed(error.to_string())
        } else {
            match self.repository.scope_discovery_seed(scope_id) {
                Ok(seed) if seed.instances.is_empty() => {
                    crate::automatic::ScopeOutcome::WaitingForProcess
                }
                Ok(_) => crate::automatic::ScopeOutcome::Assigned,
                Err(error) => crate::automatic::ScopeOutcome::Failed(format!(
                    "cannot read automatic Scope discovery state: {error}"
                )),
            }
        }
    }

    fn apply_automatic(
        &self,
        candidate: &PreparedScope,
        updates: &crate::ScopePublication,
        sources: &[(ResourceId, Revision)],
    ) -> Result<(), PapError> {
        use asc_policy_types::policy::DeploymentBackend;
        if updates
            .values()
            .filter_map(|update| update.expected.as_ref())
            .any(|scope| scope.status == asc_policy_types::scope::ScopeStatus::Deleting)
        {
            return Err(PapError::OperationInProgress);
        }
        if candidate.automatic_backend() == Some(DeploymentBackend::Gateway) {
            let execution = self.execution.as_ref().ok_or(PapError::Unavailable)?;
            if let Err(error) = execution.publish_automatic(updates, sources) {
                self.withdraw_deleted_grants(updates, sources)?;
                return Err(error);
            }
        } else if updates
            .values()
            .any(|update| update.expected != update.candidate)
        {
            self.repository.replace_automatic_scopes(updates, sources)?;
        }
        Ok(())
    }

    fn withdraw_deleted_grants(
        &self,
        updates: &crate::ScopePublication,
        sources: &[(ResourceId, Revision)],
    ) -> Result<(), PapError> {
        // Explicit deletion also withdraws grants when the replacement cannot be admitted.
        for serving in updates
            .values()
            .filter_map(|update| update.expected.as_ref())
        {
            if serving.uses_credentials()
                && serving
                    .policy_snapshots
                    .iter()
                    .any(|policy| !sources.iter().any(|(id, _)| id == &policy.policy_id))
            {
                self.execution
                    .as_ref()
                    .ok_or(PapError::Unavailable)?
                    .revoke(serving)?;
            }
        }
        Ok(())
    }

    fn recover_scope_work(&self, restore_active: bool) -> Result<(), PapError> {
        // A database from before the write limit may need administrative repair.
        // Leave automatic work pending while serving manual Scopes and durable deletions.
        match self.reconcile_automatic(None).map(|_| ()) {
            Err(error @ PapError::AutomaticPolicyCapacity) => {
                tracing::warn!(target: "asc_process_diagnostic", %error, "automatic planning deferred");
            }
            result => result?,
        }
        let mut after = None;
        let mut failure = None;
        loop {
            let scopes = self.repository.scan_scopes(after.as_ref(), 128)?;
            if scopes.is_empty() {
                break;
            }
            for scope in scopes {
                after = Some(scope.scope_id.clone());
                let result = (|| {
                    let _guard = self.scope_mutation_lock(&scope)?;
                    // A scan snapshot may predate deletion by another service clone.
                    let scope = match self.repository.get_scope(&scope.scope_id) {
                        Ok(scope) => scope,
                        Err(PapError::NotFound) => return Ok(()),
                        Err(error) => return Err(error),
                    };
                    if scope.status == asc_policy_types::scope::ScopeStatus::Deleting {
                        self.complete_scope_deletion(&scope, true)
                    } else if restore_active && scope.automatic_backend().is_none() {
                        self.recover_active_scope(&scope)
                    } else {
                        Ok(())
                    }
                })();
                if let Err(error) = result {
                    tracing::error!(
                        target: "asc_process_diagnostic", scope_id = %scope.scope_id,
                        status = ?scope.status, %error,
                        "scope recovery failed; intent retained; inspect and explicitly retry Scope work"
                    );
                    // An optional gateway failure must not block existing file-policy admission.
                    if !restore_active || !scope.uses_credentials() {
                        failure = Some(error);
                    }
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }

    fn notify_all(
        &self,
        bindings: Vec<asc_policy_repository::BindingIntentReceipt>,
    ) -> Result<(), PapError> {
        let mut failure = None;
        for binding in bindings {
            if let Err(error) = self.notify(&binding) {
                failure = Some(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    /// Gets the current Binding snapshot and mutable status.
    ///
    /// # Errors
    /// Returns not-found or persistence errors.
    #[tracing::instrument(skip_all, name = "pap.get_binding")]
    pub fn get_binding(&self, id: &ResourceId) -> Result<BindingView, PapError> {
        self.repository.get_binding(id)
    }

    /// Lists current Binding specs and status.
    ///
    /// # Errors
    /// Returns invalid-pagination or persistence errors.
    #[tracing::instrument(skip_all, name = "pap.list_bindings")]
    pub fn list_bindings(&self, limit: u32, offset: u32) -> Result<Page<BindingView>, PapError> {
        validate_limit(limit)?;
        self.repository.list_bindings(limit, offset)
    }
}

impl<R: PapRepository> crate::ScopeBindingSink for PapService<R> {
    fn automatic_scope_generation(&self, id: &ResourceId) -> Result<Option<u64>, PapError> {
        self.repository.automatic_scope_generation(id)
    }
    fn sync_observed_instances(
        &self,
        id: &ResourceId,
        instances: &[asc_policy_types::process_discovery::ProcessIdentity],
        uids: &std::collections::BTreeMap<
            asc_policy_types::process_discovery::ProcessIdentity,
            u32,
        >,
        generation: Option<u64>,
    ) -> Result<(), PapError> {
        let receipts = self
            .repository
            .sync_observed_scope_instances(id, instances, uids, generation)?;
        let resume = (|| {
            if let (Some(generation), Some(execution)) = (generation, &self.execution)
                && self.repository.get_scope(id)?.automatic_backend()
                    == Some(asc_policy_types::policy::DeploymentBackend::Gateway)
            {
                execution.resume_observed(id, generation, instances)?;
            }
            Ok(())
        })();
        resume.and(self.notify_all(receipts))
    }
    fn sync_instances(
        &self,
        id: &ResourceId,
        instances: &[asc_policy_types::process_discovery::ProcessIdentity],
    ) -> Result<(), PapError> {
        self.notify_all(self.repository.sync_scope_instances(id, instances)?)
    }
}

fn validate_authored_source(template: &PolicySource) -> Result<(), PapError> {
    match template {
        PolicySource::Cedar(cedar) => {
            asc_policy_cedar::request_authorization_eligible(cedar)?;
        }
        PolicySource::SecCore(source) => {
            source.validate().map_err(|error| {
                PapError::InvalidPolicy(ValidationError::new(
                    format!("template.{}", error.path),
                    error.message,
                ))
            })?;
        }
    }
    Ok(())
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
