//! Transport-independent Policy Administration Point use cases.
//!
//! PAP owns revisioned Policy records, immutable Scope assignments and system Bindings.
//! Authored templates are validated before storage and compiled by target Adapters.
//! A Binding revision is a complete immutable snapshot, while only the current
//! revision and its lifecycle status are retained by the Repository.
//! Target-specific translation, Adapter dispatch, and retries are intentionally
//! outside this crate.
//!
//! Binding writes commit current intent before notifying [`BindingReconcileEnqueuer`].
//! The daemon wires this port to the Policy Runtime's queue and workers; a
//! successful admission acknowledges intent, not completed target deployment.
//! Confirmed scheduling rejections atomically fail pending intent with a reason.
//! Compensation scans repair missed notifications still eligible in Binding state.
//! Durable intent and revision/status fencing across restart remain acceptance
//! gates for the persistent Repository work package; this crate owns no worker
//! or durable outbox.

#![forbid(unsafe_code)]

mod automatic;
mod error;
mod model;
mod repository;
mod selection;
mod service;
pub use asc_policy_types::policy::AutomaticDiagnostic;
pub use automatic::{ScopePublication, ScopeUpdate, is_automatic_source};
pub use selection::ConsumerPolicySet;

pub use asc_policy_cedar::schema::{PRINCIPAL_SCHEMA_REVISION, SCHEMA_ID};
pub use error::{EnqueueError, PapError};
pub use model::{Page, PolicyRevisionState, ScopeDiscoverySeed};
pub use repository::{PapRepository, PolicySnapshot, PolicySnapshotKind};
pub use service::PapService;

/// Maximum number of active Scopes retained by the repository.
pub const MAX_ACTIVE_SCOPES: usize = 32;
/// Maximum serialized size of one Scope assignment, including pinned Policy snapshots.
pub const MAX_SCOPE_BYTES: usize = 1024 * 1024;

/// Validates server-owned rule associations against the entire pinned source.
/// # Errors
/// Rejects missing, extraneous or malformed credential associations.
pub fn validate_policy_snapshot(
    policy: &asc_policy_types::policy::PreparedPolicy,
) -> Result<(), PapError> {
    use asc_policy_types::{Validate, authoring::PolicySource};
    policy.validate().map_err(PapError::InvalidPolicy)?;
    let rules = match &policy.template {
        PolicySource::Cedar(source) => asc_policy_cedar::http::compile(source)?,
        PolicySource::SecCore(source) if source.uses_credentials() => {
            return Err(PapError::InvalidCedar(
                "native credential snapshots are unsupported".into(),
            ));
        }
        PolicySource::SecCore(_) => Vec::new(),
    };
    if rules.len() != policy.credential_bindings.len()
        || rules.iter().any(|rule| {
            !policy
                .credential_bindings
                .iter()
                .any(|binding| binding.rule_id == rule.rule_id)
        })
    {
        return Err(PapError::InvalidCedar(
            "credential associations differ from pinned source rules".into(),
        ));
    }
    Ok(())
}

/// Post-commit Binding wake-up; notifications contain no command or spec.
pub trait BindingReconcileEnqueuer: Send + Sync {
    /// # Errors
    /// Rejects new Scope assignments when the background service cannot accept work.
    /// Queries, Policy writes and Scope deletion remain available. Individual
    /// attempt errors and temporary scan failures do not close admission.
    fn check_ready(&self) -> Result<(), PapError>;
    /// Returns a typed scheduling rejection after intent was committed.
    /// Existing IDs merge successfully even at capacity.
    /// # Errors
    /// Returns Full or Stopped when this notification cannot be accepted.
    fn enqueue(&self, id: &asc_foundation_types::ResourceId) -> Result<(), EnqueueError>;
}

/// Resolves validated credential sources into non-secret immutable associations.
pub trait CredentialRegistration: Send + Sync {
    /// # Errors
    /// Rejects unsafe source files, invalid pairs or unavailable storage.
    fn register(
        &self,
        source: &asc_policy_types::cedar::CedarPolicySource,
    ) -> Result<Vec<asc_policy_types::policy::CredentialBinding>, PapError>;
}

/// Credential execution owned by an immutable Scope, never by a file-only assignment.
/// Operations must be idempotent across retries and restart. Release is called only
/// after discovery termination and all Binding cleanup have been durably confirmed.
pub trait ScopeExecution: Send + Sync {
    /// Publish the candidate and compare-and-swap the serving snapshot under the Gateway lock.
    /// # Errors
    /// Returns admission, publication, or source revision errors.
    fn publish_automatic(
        &self,
        _updates: &ScopePublication,
        _sources: &[(
            asc_foundation_types::ResourceId,
            asc_foundation_types::Revision,
        )],
    ) -> Result<(), PapError> {
        Err(PapError::Unavailable)
    }

    /// Resume recovered automatic attachments only after discovery freshly matched their roots.
    /// # Errors
    /// Returns a stale scan or a failed recovery of execution state.
    fn resume_observed(
        &self,
        _id: &asc_foundation_types::ResourceId,
        _generation: u64,
        _selected: &[asc_policy_types::process_discovery::ProcessIdentity],
    ) -> Result<(), PapError> {
        Ok(())
    }
    /// Checks execution availability without creating resources.
    /// # Errors
    /// Rejects unsupported or unavailable credential execution.
    fn validate(&self, scope: &asc_policy_types::scope::PreparedScope) -> Result<(), PapError>;
    /// Prepares resources after the Scope and its cleanup responsibility are committed.
    /// # Errors
    /// Leaves any partially prepared resources available to revoke and release.
    fn prepare(&self, scope: &asc_policy_types::scope::PreparedScope) -> Result<(), PapError>;
    /// Restores execution for an already admitted Scope during startup recovery.
    ///
    /// Unlike [`Self::prepare`], restoration must tolerate credential assignments
    /// that cannot be reissued so independent file Policies keep their discovery
    /// worker. Admission of a new or explicitly retried Scope stays strict.
    /// # Errors
    /// Rejects assignments that cannot be projected at all.
    fn restore(&self, scope: &asc_policy_types::scope::PreparedScope) -> Result<(), PapError>;
    /// Closes credential injection before discovery and Binding cleanup.
    /// # Errors
    /// Retains durable cleanup responsibility when revocation cannot be confirmed.
    fn revoke(&self, scope: &asc_policy_types::scope::PreparedScope) -> Result<(), PapError>;
    /// Releases remaining Scope resources after Binding cleanup.
    /// # Errors
    /// Retains durable cleanup responsibility until release succeeds.
    fn release(&self, id: &asc_foundation_types::ResourceId) -> Result<(), PapError>;
}

/// Lifecycle of Scope-owned instance discovery.
pub trait ScopeDiscovery: Send + Sync {
    /// Starts one worker from the stored assignment. On error no worker may remain.
    /// # Errors
    /// Rejects unavailable discovery or exhausted job capacity.
    fn start(&self, seed: &ScopeDiscoverySeed) -> Result<(), PapError>;
    /// Cancel and join only this Scope's worker and close its observation stream.
    /// # Errors
    /// Reports failure to stop the worker.
    fn stop(&self, id: &asc_foundation_types::ResourceId) -> Result<(), PapError>;
}

/// Trusted discovery input; repository transactions own admission and deduplication.
pub trait ScopeBindingSink: Send + Sync {
    /// Current Scope generation at scan start.
    /// # Errors
    /// Returns storage errors or an absent Scope.
    fn automatic_scope_generation(
        &self,
        _id: &asc_foundation_types::ResourceId,
    ) -> Result<Option<u64>, PapError> {
        Ok(None)
    }

    /// Current-round owner observations, keyed by pinned process identity.
    /// # Errors
    /// Returns storage or scheduling failures.
    fn sync_observed_instances(
        &self,
        id: &asc_foundation_types::ResourceId,
        instances: &[asc_policy_types::process_discovery::ProcessIdentity],
        _uids: &std::collections::BTreeMap<
            asc_policy_types::process_discovery::ProcessIdentity,
            u32,
        >,
        _generation: Option<u64>,
    ) -> Result<(), PapError> {
        self.sync_instances(id, instances)
    }

    /// Reconciles the complete known instance set, preserving unreadable instances.
    /// # Errors
    /// Returns storage or scheduling failures; the next scan may retry admission.
    fn sync_instances(
        &self,
        id: &asc_foundation_types::ResourceId,
        instances: &[asc_policy_types::process_discovery::ProcessIdentity],
    ) -> Result<(), PapError>;
}
