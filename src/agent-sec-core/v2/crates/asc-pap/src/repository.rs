use asc_foundation_types::{ResourceId, Revision};
use asc_policy_types::binding::BindingView;
use asc_policy_types::policy::PreparedPolicy;
use asc_policy_types::scope::PreparedScope;

use crate::error::PapError;
use crate::model::{Page, PolicyRevisionState};

/// Determines which current source policies a PEP can read in one bounded snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicySnapshotKind {
    /// Include every current policy for request-time authorization.
    AllCurrent,
    /// Include only v3 source policies eligible for automatic deployment.
    Automatic,
}

/// Current source policies in identity order and their transaction's policy-set generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicySnapshot {
    /// Advances on every committed change to an authored policy.
    pub generation: u64,
    /// All current records in the requested snapshot, including an empty set.
    pub policies: Vec<PreparedPolicy>,
}

/// Persistence port owned by PAP.
///
/// Repositories bound serialized records and list pages to the transport budget.
/// A page may contain fewer than `limit` items; advance by its actual item count.
///
/// All list implementations must apply pagination in the repository. They
/// first order the complete matching result as documented by the individual
/// method, then skip `offset` records and return at most `limit` records.
/// Identity ordering is the lexicographic byte order of `ResourceId::as_str()`.
/// `Page::total` is the matching count before `offset` and `limit` are applied.
/// These values are per-query inputs and must not be persisted.
pub trait PapRepository: Send + Sync {
    /// Creates or replaces the current Policy record.
    ///
    /// Validate the complete snapshot before writing. Cedar language admission
    /// is performed by PAP; snapshot validation checks its source envelope only.
    ///
    /// Implementations must atomically accept a changed record only when its
    /// revision is exactly the next never-reused revision for the Policy identity.
    /// An exact replay of the current record is idempotent; every other stale,
    /// reused, or skipped revision must return [`PapError::Conflict`]. A successful
    /// changed write replaces the previously retained content; only the current
    /// record remains.
    ///
    /// # Errors
    /// Returns validation, conflict or persistence failures.
    fn put_policy(&self, policy: &PreparedPolicy) -> Result<PreparedPolicy, PapError>;

    /// Gets Policy allocation state and its optional current record.
    ///
    /// # Errors
    /// Returns a persistence failure when the query cannot complete.
    fn get_policy_revision_state(
        &self,
        id: &ResourceId,
    ) -> Result<Option<PolicyRevisionState>, PapError>;

    /// Gets the current Policy only when its revision equals `revision`.
    ///
    /// # Errors
    /// Returns not-found or persistence failures.
    fn get_policy(&self, id: &ResourceId, revision: Revision) -> Result<PreparedPolicy, PapError>;

    /// Reads 1..32 selected current policies in one consistent storage snapshot.
    /// Returns records in requested order; no missing policy may be omitted.
    ///
    /// # Errors
    /// Rejects invalid selection sizes, missing policies, or persistence failures.
    fn current_policies(&self, ids: &[ResourceId]) -> Result<Vec<PreparedPolicy>, PapError>;

    /// Reads the committed policy-set generation without loading or parsing policies.
    ///
    /// # Errors
    /// Returns a storage failure if the current generation cannot be read.
    fn policy_generation(&self) -> Result<u64, PapError>;

    /// Reads the generation and complete selected source set in the same storage snapshot.
    ///
    /// # Errors
    /// Returns a storage or capacity failure without a partial result.
    fn policy_snapshot(&self, kind: PolicySnapshotKind) -> Result<PolicySnapshot, PapError>;

    /// Reads every current policy in identity order in one consistent storage snapshot.
    /// Never paginate or return a truncated authorization candidate.
    ///
    /// # Errors
    /// Returns storage or capacity failures without a partial result.
    fn all_current_policies(&self) -> Result<Vec<PreparedPolicy>, PapError> {
        Ok(self
            .policy_snapshot(PolicySnapshotKind::AllCurrent)?
            .policies)
    }

    /// Snapshot current v3 source policies for process deployment.
    /// # Errors
    /// Returns storage or snapshot capacity errors.
    fn automatic_policies(&self) -> Result<Vec<PreparedPolicy>, PapError> {
        Ok(self
            .policy_snapshot(PolicySnapshotKind::Automatic)?
            .policies)
    }

    /// Returns only the ordered v3 identities and revisions used to fence publication.
    ///
    /// # Errors
    /// Returns storage or snapshot capacity errors.
    fn automatic_source_revisions(&self) -> Result<Vec<(ResourceId, Revision)>, PapError>;

    /// Lists current Policy records ordered by Policy identity ascending.
    ///
    /// # Errors
    /// Returns a persistence failure when the query cannot complete.
    fn list_policies(&self, limit: u32, offset: u32) -> Result<Page<PreparedPolicy>, PapError>;

    /// Deletes the current Policy content when its revision equals `revision`.
    ///
    /// Implementations retain the allocation head as a tombstone so a later
    /// update of the same identity cannot reuse the deleted revision.
    ///
    /// # Errors
    /// Returns not-found, conflict, or persistence failures.
    fn delete_policy_revision(
        &self,
        id: &ResourceId,
        revision: Revision,
    ) -> Result<PreparedPolicy, PapError>;

    /// Atomically validates exact current policy snapshots and inserts an immutable Scope.
    /// # Errors
    /// Rejects reused IDs, stale snapshots, invalid assignments, and storage failures.
    fn put_scope(&self, scope: &PreparedScope) -> Result<PreparedScope, PapError>;

    /// Check the entire assignment transition before publishing to the Gateway.
    /// # Errors
    /// Rejects stale sources, invalid assignments and exhausted storage limits.
    fn validate_automatic_scopes(
        &self,
        _updates: &crate::ScopePublication,
        _sources: &[(ResourceId, Revision)],
    ) -> Result<(), PapError> {
        Err(PapError::Unavailable)
    }

    /// Commit a derived Scope only if its old snapshot and all v3 sources still match.
    /// # Errors
    /// Rejects stale source revisions or a changed serving assignment.
    fn replace_automatic_scopes(
        &self,
        _updates: &crate::ScopePublication,
        _sources: &[(ResourceId, Revision)],
    ) -> Result<(), PapError> {
        Err(PapError::Unavailable)
    }

    /// Begin automatic cleanup only while the source set that removed the assignment remains current.
    /// # Errors
    /// Rejects a stale source set or changed assignment.
    fn delete_automatic_scope(
        &self,
        _expected: &PreparedScope,
        _sources: &[(ResourceId, Revision)],
    ) -> Result<PreparedScope, PapError> {
        Err(PapError::Unavailable)
    }

    /// Reads an assignment, including deletion intent.
    /// # Errors
    /// Returns not-found or storage failures.
    fn get_scope(&self, id: &ResourceId) -> Result<PreparedScope, PapError>;

    /// Lists assignments in identity order.
    /// # Errors
    /// Returns storage failures.
    fn list_scopes(&self, limit: u32, offset: u32) -> Result<Page<PreparedScope>, PapError>;

    /// Reads immutable assignment, PID pin and nonretiring instances atomically.
    /// # Errors
    /// Returns not-found or storage failures.
    fn scope_discovery_seed(&self, id: &ResourceId) -> Result<crate::ScopeDiscoverySeed, PapError>;

    /// Stable-ID pagination for recovery that may remove earlier Scope rows.
    /// # Errors
    /// Returns invalid page bounds or storage failures.
    fn scan_scopes(
        &self,
        after: Option<&ResourceId>,
        limit: usize,
    ) -> Result<Vec<PreparedScope>, PapError>;

    /// Closes child admission before the discovery worker is joined.
    /// Returns `None` for a previously completed deletion; unknown IDs are errors.
    /// # Errors
    /// Returns not-found or storage failures.
    fn begin_scope_delete(&self, id: &ResourceId) -> Result<Option<PreparedScope>, PapError>;

    /// Records discovery termination and requests all owned Binding deletions.
    /// Removes an empty Scope only after its worker has stopped.
    /// # Errors
    /// Returns storage failures or conflict if deletion was not admitted.
    fn finish_scope_discovery(
        &self,
        id: &ResourceId,
    ) -> Result<Vec<asc_policy_repository::BindingIntentReceipt>, PapError>;

    /// Reports whether discovery has stopped and no Binding responsibility remains.
    /// # Errors
    /// Returns storage failures; never assumes completion from an empty in-memory set.
    fn scope_execution_ready_to_release(&self, id: &ResourceId) -> Result<bool, PapError>;

    /// Confirms successful execution release and finalizes an otherwise empty deletion.
    /// # Errors
    /// Rejects unfinished discovery/Binding cleanup or storage failures.
    fn finish_scope_execution(&self, id: &ResourceId) -> Result<(), PapError>;

    /// Admits missing instances and retires absent ones atomically with Scope lifecycle.
    /// Returns only changed Binding intents; terminal failures are not implicitly retried.
    /// # Errors
    /// Rejects inactive Scopes or storage failures.
    fn sync_scope_instances(
        &self,
        id: &ResourceId,
        instances: &[asc_policy_types::process_discovery::ProcessIdentity],
    ) -> Result<Vec<asc_policy_repository::BindingIntentReceipt>, PapError>;

    /// Current assignment generation, read before a scan to fence stale observations.
    /// # Errors
    /// Returns storage errors or an absent Scope.
    fn automatic_scope_generation(&self, _id: &ResourceId) -> Result<Option<u64>, PapError> {
        Ok(None)
    }

    /// Admits new explicitly UID-scoped assignments only on a matching observation in this scan.
    /// # Errors
    /// Rejects stale observations, inactive Scopes, or storage failures.
    fn sync_observed_scope_instances(
        &self,
        id: &ResourceId,
        instances: &[asc_policy_types::process_discovery::ProcessIdentity],
        _uids: &std::collections::BTreeMap<
            asc_policy_types::process_discovery::ProcessIdentity,
            u32,
        >,
        _generation: Option<u64>,
    ) -> Result<Vec<asc_policy_repository::BindingIntentReceipt>, PapError> {
        self.sync_scope_instances(id, instances)
    }

    /// Explicitly retries failed owned Bindings, preserving nonterminal retry budgets.
    /// # Errors
    /// Returns not-found or storage failures.
    fn retry_scope(
        &self,
        id: &ResourceId,
    ) -> Result<Vec<asc_policy_repository::BindingIntentReceipt>, PapError>;

    /// Atomically fail only the supplied ID, revision and pending status.
    /// Set the failed phase and status error in the same transaction; preserve
    /// spec and deployments. No retry progress is stored. Return false on contention, never retry
    /// against a newer snapshot. No operation identity is implied by revision.
    /// # Errors
    /// Returns persistence failures; a non-pending expectation is a conflict.
    fn fail_pending_binding(
        &self,
        expected: &asc_policy_repository::BindingIntentReceipt,
        reason: crate::EnqueueError,
    ) -> Result<bool, PapError>;

    /// Gets the current Binding spec, status and safe last error as a read-only aggregate.
    ///
    /// # Errors
    /// Returns not-found or persistence failures.
    fn get_binding(&self, id: &ResourceId) -> Result<BindingView, PapError>;

    /// Lists current Binding specs and status ordered by Binding identity
    /// ascending.
    ///
    /// # Errors
    /// Returns a persistence failure when the query cannot complete.
    fn list_bindings(&self, limit: u32, offset: u32) -> Result<Page<BindingView>, PapError>;
}
