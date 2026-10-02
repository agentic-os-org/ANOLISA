//! Complete target-independent immutable Policy Binding specifications.

use serde::{Deserialize, Serialize};

use crate::error::{Validate, ValidationError};
use crate::identifiers::{ResourceId, Revision};
use crate::policy::PreparedPolicy;
use crate::scope::PreparedScope;

/// Complete Adapter-independent immutable Binding specification.
///
/// Lifecycle and reconciliation state do not belong to this value. The pair
/// `(binding_id, binding_revision)` identifies exactly one immutable snapshot.
/// Repositories retain only the current snapshot; a higher revision replaces
/// the previous current record without reusing its number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedBinding {
    /// Stable Binding identity.
    pub binding_id: ResourceId,
    /// Immutable spec revision.
    pub binding_revision: Revision,
    /// Exactly one authored and lowered Policy revision.
    pub policy: PreparedPolicy,
    /// Exactly one authored Scope revision.
    pub scope: PreparedScope,
}

impl Validate for PreparedBinding {
    fn validate(&self) -> Result<(), ValidationError> {
        self.policy.validate().map_err(|error| {
            ValidationError::new(format!("policy.{}", error.path), error.message)
        })?;
        self.scope.validate().map_err(|error| {
            ValidationError::new(format!("scope.{}", error.path), error.message)
        })?;
        Ok(())
    }
}

/// Complete lifecycle state of one Binding.
///
/// Successful paths:
///
/// ```text
/// CREATE/UPDATE: PendingApply -> Applying -> Ready
/// DELETE: `PendingDelete` -> `Deleting` -> Deleted
/// ```
///
/// Only spec changes increment Binding revision. Same-spec Apply retry and all
/// Delete requests keep the revision. Pending/running Apply and Ready accept an
/// identical UPDATE as a no-op. `ApplyFailed` allows a same-spec retry.
/// Delete intent is irreversible: `PendingDelete`, `Deleting` and `DeleteFailed` reject
/// every UPDATE. DELETE may supersede Applying; its observations must still be
/// recorded before cleanup. Repeated pending/running DELETE is a no-op, while
/// `DeleteFailed` can retry with a fresh retry budget.
///
/// Workers claim pending work, complete Apply as Ready, or return failures to
/// pending/failed status. `Deleted` is an internal completion marker: successful
/// cleanup removes the aggregate atomically; it is not a persisted current record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BindingStatus {
    /// PAP accepted an Apply request; no worker has claimed it yet.
    PendingApply,
    /// A reconciler is applying the referenced immutable spec.
    Applying,
    /// Apply completed successfully.
    Ready,
    /// Apply exhausted retries or failed permanently.
    ApplyFailed,
    /// PAP accepted a Delete request; no worker has claimed it yet.
    PendingDelete,
    /// A reconciler is detaching the referenced immutable spec.
    Deleting,
    /// Internal successful detach outcome; the reconciler removes the record.
    Deleted,
    /// Detach exhausted retries or failed permanently.
    DeleteFailed,
}

impl BindingStatus {
    /// Reports whether target-side reconciliation may currently be running.
    #[must_use]
    pub const fn is_reconciling(self) -> bool {
        matches!(self, Self::Applying | Self::Deleting)
    }

    /// Returns the status produced by an identical-spec UPDATE, without changing
    /// revision. `ApplyFailed` retries; pending/running/successful Apply is a no-op.
    /// # Errors
    /// Rejects every request after Delete intent has been accepted.
    pub fn request_apply(self) -> Result<Self, ValidationError> {
        match self {
            Self::PendingApply | Self::Applying | Self::Ready => Ok(self),
            Self::ApplyFailed => Ok(Self::PendingApply),
            _ => Err(illegal_status("request Apply", self)),
        }
    }

    /// Returns the status produced by DELETE, without changing spec or revision.
    /// A running Apply may be superseded; its remote observations still matter.
    #[must_use]
    pub const fn request_delete(self) -> Self {
        match self {
            Self::PendingDelete | Self::Deleting | Self::Deleted => self,
            _ => Self::PendingDelete,
        }
    }

    /// Claims pending Apply or Delete work.
    ///
    /// # Errors
    /// Rejects non-pending source states.
    pub fn start_reconcile(self) -> Result<Self, ValidationError> {
        match self {
            Self::PendingApply => Ok(Self::Applying),
            Self::PendingDelete => Ok(Self::Deleting),
            _ => Err(illegal_status("start reconcile", self)),
        }
    }

    /// Records successful reconciliation.
    ///
    /// # Errors
    /// Rejects non-running source states.
    pub fn complete_reconcile(self) -> Result<Self, ValidationError> {
        match self {
            Self::Applying => Ok(Self::Ready),
            Self::Deleting => Ok(Self::Deleted),
            _ => Err(illegal_status("complete reconcile", self)),
        }
    }

    /// Returns failed running work to its pending state for retry.
    ///
    /// # Errors
    /// Rejects non-running source states.
    pub fn retry_reconcile(self) -> Result<Self, ValidationError> {
        match self {
            Self::Applying => Ok(Self::PendingApply),
            Self::Deleting => Ok(Self::PendingDelete),
            _ => Err(illegal_status("retry reconcile", self)),
        }
    }

    /// Records terminal reconciliation failure.
    ///
    /// # Errors
    /// Rejects non-running source states.
    pub fn fail_reconcile(self) -> Result<Self, ValidationError> {
        match self {
            Self::Applying => Ok(Self::ApplyFailed),
            Self::Deleting => Ok(Self::DeleteFailed),
            _ => Err(illegal_status("fail reconcile", self)),
        }
    }

    /// Validates a Repository compare-and-swap successor.
    ///
    /// Identical values are accepted for idempotency. Other successors must be
    /// one of the worker transitions within the same Binding revision. User
    /// requests use a separate conditional write; only spec changes bump revision.
    ///
    /// # Errors
    /// Rejects an illegal status transition.
    pub fn validate_successor(self, next: Self) -> Result<(), ValidationError> {
        if self == next {
            return Ok(());
        }
        let valid = matches!(
            (self, next),
            (Self::PendingApply, Self::Applying | Self::ApplyFailed)
                | (
                    Self::Applying,
                    Self::Ready | Self::PendingApply | Self::ApplyFailed,
                )
                | (Self::PendingDelete, Self::Deleting | Self::DeleteFailed)
                | (
                    Self::Deleting,
                    Self::Deleted | Self::PendingDelete | Self::DeleteFailed,
                )
        );
        if valid {
            Ok(())
        } else {
            Err(illegal_status("persist status", self))
        }
    }
}

fn illegal_status(operation: &str, status: BindingStatus) -> ValidationError {
    ValidationError::new("status", format!("cannot {operation} from {status:?}"))
}

/// Lifecycle and its current explanation are one repository value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingLifecycle {
    pub phase: BindingStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::target::Failure>,
}
impl From<BindingStatus> for BindingLifecycle {
    fn from(phase: BindingStatus) -> Self {
        Self { phase, error: None }
    }
}
impl std::ops::Deref for BindingLifecycle {
    type Target = BindingStatus;
    fn deref(&self) -> &BindingStatus {
        &self.phase
    }
}
impl PartialEq<BindingStatus> for BindingLifecycle {
    fn eq(&self, other: &BindingStatus) -> bool {
        self.phase == *other
    }
}
impl PartialEq<BindingLifecycle> for BindingStatus {
    fn eq(&self, other: &BindingLifecycle) -> bool {
        *self == other.phase
    }
}

/// Current Binding snapshot and its lifecycle status.
///
/// Repositories construct this value for GET/LIST and atomically replace the
/// complete value for a new desired-state revision. Reconciler status-only
/// transitions do not rewrite `spec`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingView {
    /// Immutable Binding spec.
    pub spec: PreparedBinding,
    /// Mutable lifecycle status for `spec`.
    pub status: BindingLifecycle,
}

impl Validate for BindingView {
    fn validate(&self) -> Result<(), ValidationError> {
        self.spec
            .validate()
            .map_err(|error| ValidationError::new(format!("spec.{}", error.path), error.message))
    }
}
