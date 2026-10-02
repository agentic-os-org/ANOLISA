//! Durable state model for one audit/enforcement ownership handoff.

use std::time::{SystemTime, UNIX_EPOCH};

use agentsight_enforcement_protocol::{Binding, ReplaceFailureCode, ReplacePolicy};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Direction of a policy ownership handoff.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionDirection {
    /// Replaces an audit policy with a containment policy.
    Forward,
    /// Restores the original audit policy after containment ends.
    Reverse,
}

impl TransitionDirection {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Forward => "forward",
            Self::Reverse => "reverse",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "forward" => Some(Self::Forward),
            "reverse" => Some(Self::Reverse),
            _ => None,
        }
    }
}

/// Persisted progress of one policy ownership handoff.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionPhase {
    /// Intent is durable but no terminal acknowledgement is stored.
    Pending,
    /// The replacement binding owns the runtime.
    Completed,
    /// The source binding remained active or was restored.
    SourceRestored,
    /// Runtime ownership could not be proved and must be reconciled.
    Indeterminate,
}

impl TransitionPhase {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::SourceRestored => "source_restored",
            Self::Indeterminate => "indeterminate",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "completed" => Some(Self::Completed),
            "source_restored" => Some(Self::SourceRestored),
            "indeterminate" => Some(Self::Indeterminate),
            _ => None,
        }
    }
}

/// Stable identity for one directional transition of a containment action.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TransitionKey {
    /// Containment action that owns the transition.
    pub action_id: Uuid,
    /// Forward activation or reverse restoration.
    pub direction: TransitionDirection,
}

/// Durable replacement request and its latest typed acknowledgement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyTransition {
    /// Stable transition identity.
    pub key: TransitionKey,
    /// Immutable compare-and-replace request.
    pub request: ReplacePolicy,
    /// Latest persisted phase.
    pub phase: TransitionPhase,
    /// Binding acknowledged for a terminal phase.
    pub acknowledgement: Option<Binding>,
    /// Stable failure category without backend details.
    pub failure_code: Option<ReplaceFailureCode>,
    /// Last update time as Unix epoch nanoseconds.
    pub updated_at_ns: u64,
}

impl PolicyTransition {
    /// Builds a new transition before any privileged mutation is attempted.
    pub fn pending(key: TransitionKey, request: ReplacePolicy) -> Self {
        Self {
            key,
            request,
            phase: TransitionPhase::Pending,
            acknowledgement: None,
            failure_code: None,
            updated_at_ns: now_ns(),
        }
    }
}

fn now_ns() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    nanos.min(u64::MAX as u128) as u64
}
