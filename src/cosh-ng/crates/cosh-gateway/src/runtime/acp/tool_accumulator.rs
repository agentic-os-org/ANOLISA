//! Bounded pure state for ACP v1 tool-call creation and partial updates.

use std::collections::BTreeMap;

use agent_client_protocol::schema::v1::{
    SessionUpdate, ToolCall, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use cosh_gateway_contracts::{
    common::{BoundedName, BoundedText, MAX_TEXT_BYTES},
    ids::{ToolUseId, TurnId},
    runtime::{ExecutionAuthority, ToolInvocationSnapshot, ToolInvocationStatus, ToolSummary},
};
use serde_json::Value;
use thiserror::Error;

/// Conservative default bound for active tool invocations on one Runtime connection.
pub const DEFAULT_MAX_TOOL_INVOCATIONS: usize = 256;
/// Conservative bound for Agent-owned session and tool-call identifiers.
pub const DEFAULT_MAX_TOOL_IDENTIFIER_BYTES: usize = 1024;
/// Conservative bound for one canonical ACP tool-call snapshot.
pub const DEFAULT_MAX_TOOL_PAYLOAD_BYTES: usize = 256 * 1024;

/// Memory and input limits enforced by one tool invocation accumulator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpToolAccumulatorLimits {
    /// Maximum distinct `(session, turn, toolCallId)` entries.
    pub max_invocations: usize,
    /// Maximum UTF-8 bytes in an Agent-owned session or tool-call identifier.
    pub max_identifier_bytes: usize,
    /// Maximum serialized bytes in one update or accumulated tool call.
    pub max_payload_bytes: usize,
}

impl Default for AcpToolAccumulatorLimits {
    fn default() -> Self {
        Self {
            max_invocations: DEFAULT_MAX_TOOL_INVOCATIONS,
            max_identifier_bytes: DEFAULT_MAX_TOOL_IDENTIFIER_BYTES,
            max_payload_bytes: DEFAULT_MAX_TOOL_PAYLOAD_BYTES,
        }
    }
}

impl AcpToolAccumulatorLimits {
    fn validate(self) -> Result<Self, AcpToolAccumulatorError> {
        if self.max_invocations == 0
            || self.max_identifier_bytes == 0
            || self.max_payload_bytes == 0
        {
            return Err(AcpToolAccumulatorError::InvalidLimits);
        }
        Ok(self)
    }
}

/// Stable accumulated ACP data plus the provider-neutral domain projection.
#[derive(Debug, Clone, PartialEq)]
pub struct AcpToolInvocationSnapshot {
    /// Agent-owned session identifier scoped to this connection.
    pub session_id: String,
    /// Agent-owned tool-call identifier scoped to the session and turn.
    pub provider_tool_call_id: String,
    /// Latest domain-safe tool projection.
    pub projection: ToolInvocationSnapshot,
    /// Canonical accumulated ACP v1 tool call retained for permission normalization.
    pub tool_call: Value,
}

/// Result of observing one ACP `session/update` value.
#[derive(Debug, Clone, PartialEq)]
pub enum AcpToolAccumulation {
    /// The update belongs to another ACP update family.
    NotToolCall,
    /// A partial update was retained but does not yet contain the required title.
    Buffered {
        /// Stable COSH identity allocated on the first partial update.
        tool_use_id: ToolUseId,
    },
    /// The update repeated the already accumulated state.
    Unchanged {
        /// Current snapshot, if enough fields have arrived to construct one.
        snapshot: Option<AcpToolInvocationSnapshot>,
    },
    /// The invocation was created or advanced to this snapshot.
    Updated(AcpToolInvocationSnapshot),
}

/// Invalid or unsafe ACP tool-call state transition.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AcpToolAccumulatorError {
    /// At least one configured memory limit was zero.
    #[error("ACP tool accumulator limits must be non-zero")]
    InvalidLimits,
    /// An Agent-owned identifier was absent or outside the configured bound.
    #[error("ACP {field} must contain 1..={limit} bytes")]
    InvalidIdentifier {
        /// Rejected identifier field.
        field: &'static str,
        /// Configured maximum bytes.
        limit: usize,
    },
    /// The update was not a valid official ACP v1 session update.
    #[error("invalid ACP v1 tool update: {0}")]
    InvalidUpdate(String),
    /// A serialized update or accumulated snapshot exceeded the configured bound.
    #[error("ACP tool payload exceeds {limit} bytes")]
    PayloadTooLarge {
        /// Configured maximum serialized bytes.
        limit: usize,
    },
    /// The connection already tracks the maximum number of invocations.
    #[error("ACP tool invocation count exceeds {limit}")]
    TooManyInvocations {
        /// Configured maximum entries.
        limit: usize,
    },
    /// A second create attempted to replace an existing invocation.
    #[error("ACP tool call {tool_call_id:?} was created more than once with different state")]
    ConflictingCreate {
        /// Agent-owned conflicting identity.
        tool_call_id: String,
    },
    /// An update attempted to mutate a completed or failed invocation.
    #[error("ACP tool call {tool_call_id:?} changed after reaching terminal status")]
    TerminalMutation {
        /// Agent-owned terminal identity.
        tool_call_id: String,
    },
    /// A status update attempted to move an invocation backwards.
    #[error("ACP tool call status cannot move from {from} to {to}")]
    StatusRegression {
        /// Previously accumulated status.
        from: &'static str,
        /// Rejected later status.
        to: &'static str,
    },
    /// The monotonic invocation revision exhausted its integer range.
    #[error("ACP tool invocation revision overflowed")]
    RevisionOverflow,
    /// The provider-neutral summary exceeded contract bounds.
    #[error("ACP tool call presentation fields exceed Gateway contract bounds")]
    InvalidSummary,
    /// Internal accumulated state did not contain the call required by a transition.
    #[error("ACP accumulated tool call is unexpectedly absent")]
    MissingAccumulatedCall,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ToolInvocationKey {
    session_id: String,
    turn_id: TurnId,
    provider_tool_call_id: String,
}

#[derive(Debug, Clone)]
struct AccumulatedToolCall {
    tool_use_id: ToolUseId,
    revision: u64,
    call: Option<ToolCall>,
    buffered: ToolCallUpdateFields,
}

impl AccumulatedToolCall {
    fn new() -> Self {
        Self {
            tool_use_id: ToolUseId::new(),
            revision: 0,
            call: None,
            buffered: ToolCallUpdateFields::default(),
        }
    }
}

/// Aggregates ACP v1 tool calls by session, turn, and provider tool-call ID.
#[derive(Debug)]
pub struct ToolInvocationAccumulator {
    limits: AcpToolAccumulatorLimits,
    authority: ExecutionAuthority,
    invocations: BTreeMap<ToolInvocationKey, AccumulatedToolCall>,
}

impl ToolInvocationAccumulator {
    /// Builds an empty accumulator for an Agent-native observed execution path.
    #[must_use]
    pub fn provider_native() -> Self {
        Self {
            limits: AcpToolAccumulatorLimits::default(),
            authority: ExecutionAuthority::ProviderNativeObserved,
            invocations: BTreeMap::new(),
        }
    }

    /// Builds an empty accumulator for one explicit execution authority.
    ///
    /// # Errors
    ///
    /// Returns [`AcpToolAccumulatorError::InvalidLimits`] for zero bounds.
    pub fn new(
        limits: AcpToolAccumulatorLimits,
        authority: ExecutionAuthority,
    ) -> Result<Self, AcpToolAccumulatorError> {
        Ok(Self {
            limits: limits.validate()?,
            authority,
            invocations: BTreeMap::new(),
        })
    }

    /// Applies one official ACP v1 `session/update` projection.
    ///
    /// Updates that precede creation are buffered. Once a title becomes
    /// available, the SDK upsert semantics produce a stable snapshot.
    ///
    /// # Errors
    ///
    /// Rejects malformed, oversized, conflicting, post-terminal, or
    /// over-capacity tool state.
    pub fn observe(
        &mut self,
        session_id: &str,
        turn_id: &TurnId,
        update: &Value,
    ) -> Result<AcpToolAccumulation, AcpToolAccumulatorError> {
        self.validate_identifier("session id", session_id)?;
        self.validate_payload(update)?;
        let update_kind = update.get("sessionUpdate").and_then(Value::as_str);
        if !matches!(update_kind, Some("tool_call" | "tool_call_update")) {
            return Ok(AcpToolAccumulation::NotToolCall);
        }
        let update: SessionUpdate = serde_json::from_value(update.clone())
            .map_err(|error| AcpToolAccumulatorError::InvalidUpdate(error.to_string()))?;
        match update {
            SessionUpdate::ToolCall(call) => self.create(session_id, turn_id, call),
            SessionUpdate::ToolCallUpdate(update) => self.update(session_id, turn_id, update),
            _ => Ok(AcpToolAccumulation::NotToolCall),
        }
    }

    /// Returns the latest stable snapshot for a previously observed invocation.
    #[must_use]
    pub fn snapshot(
        &self,
        session_id: &str,
        turn_id: &TurnId,
        provider_tool_call_id: &str,
    ) -> Option<AcpToolInvocationSnapshot> {
        let key = ToolInvocationKey {
            session_id: session_id.to_owned(),
            turn_id: turn_id.clone(),
            provider_tool_call_id: provider_tool_call_id.to_owned(),
        };
        self.invocations
            .get(&key)
            .and_then(|entry| Self::project(self.authority, &key, entry).ok().flatten())
    }

    /// Promotes a complete permission carrier into the observed tool lifecycle.
    ///
    /// This is intentionally valid only for a truly absent invocation. A
    /// buffered partial update already owns the key and must not be replaced.
    ///
    /// # Errors
    ///
    /// Rejects malformed, non-pending, oversized, conflicting, or
    /// over-capacity carriers.
    pub fn promote_permission_carrier(
        &mut self,
        session_id: &str,
        turn_id: &TurnId,
        carrier: &Value,
    ) -> Result<AcpToolInvocationSnapshot, AcpToolAccumulatorError> {
        self.validate_identifier("session id", session_id)?;
        self.validate_payload(carrier)?;
        let call: ToolCall = serde_json::from_value(carrier.clone())
            .map_err(|error| AcpToolAccumulatorError::InvalidUpdate(error.to_string()))?;
        let provider_tool_call_id = call.tool_call_id.to_string();
        self.validate_identifier("tool call id", &provider_tool_call_id)?;
        if call.title.is_empty() || call.status != ToolCallStatus::Pending {
            return Err(AcpToolAccumulatorError::InvalidUpdate(
                "permission carrier requires a nonempty title and pending status".into(),
            ));
        }
        let key = ToolInvocationKey {
            session_id: session_id.to_owned(),
            turn_id: turn_id.clone(),
            provider_tool_call_id: provider_tool_call_id.clone(),
        };
        self.ensure_capacity_for(&key)?;
        if self.invocations.contains_key(&key) {
            return Err(AcpToolAccumulatorError::ConflictingCreate {
                tool_call_id: provider_tool_call_id,
            });
        }
        match self.create(session_id, turn_id, call)? {
            AcpToolAccumulation::Updated(snapshot) => Ok(snapshot),
            _ => Err(AcpToolAccumulatorError::MissingAccumulatedCall),
        }
    }

    /// Refines an in-progress invocation with a canonical permission carrier.
    ///
    /// Some ACP adapters first publish a presentation-oriented tool call and
    /// later provide the exact executable operation in the permission request.
    /// This transition preserves the existing COSH tool identity while making
    /// the permission-bound operation the latest durable projection.
    ///
    /// # Errors
    ///
    /// Rejects absent, buffered, terminal, non-pending, oversized, or
    /// mismatched carriers. Callers must additionally authenticate the
    /// adapter-specific refinement shape before invoking this method.
    pub fn refine_in_progress_permission_carrier(
        &mut self,
        session_id: &str,
        turn_id: &TurnId,
        carrier: &Value,
    ) -> Result<AcpToolInvocationSnapshot, AcpToolAccumulatorError> {
        self.validate_identifier("session id", session_id)?;
        self.validate_payload(carrier)?;
        let call: ToolCall = serde_json::from_value(carrier.clone())
            .map_err(|error| AcpToolAccumulatorError::InvalidUpdate(error.to_string()))?;
        let provider_tool_call_id = call.tool_call_id.to_string();
        self.validate_identifier("tool call id", &provider_tool_call_id)?;
        if call.title.is_empty() || call.status != ToolCallStatus::Pending {
            return Err(AcpToolAccumulatorError::InvalidUpdate(
                "permission refinement requires a nonempty title and pending status".into(),
            ));
        }
        let key = ToolInvocationKey {
            session_id: session_id.to_owned(),
            turn_id: turn_id.clone(),
            provider_tool_call_id: provider_tool_call_id.clone(),
        };
        let entry = self.invocations.get_mut(&key).ok_or_else(|| {
            AcpToolAccumulatorError::ConflictingCreate {
                tool_call_id: provider_tool_call_id.clone(),
            }
        })?;
        let Some(current) = &entry.call else {
            return Err(AcpToolAccumulatorError::ConflictingCreate {
                tool_call_id: provider_tool_call_id,
            });
        };
        if current.status != ToolCallStatus::InProgress || status_is_terminal(current.status) {
            return Err(AcpToolAccumulatorError::ConflictingCreate {
                tool_call_id: provider_tool_call_id,
            });
        }
        validate_payload_limit(self.limits.max_payload_bytes, &call)?;
        let revision = next_revision(entry.revision)?;
        entry.call = Some(call);
        entry.buffered = ToolCallUpdateFields::default();
        entry.revision = revision;
        Self::project(self.authority, &key, entry)?
            .ok_or(AcpToolAccumulatorError::MissingAccumulatedCall)
    }

    /// Drops all retained invocation state for a completed turn.
    ///
    /// The caller must persist any audit snapshot before releasing the state.
    pub fn release_turn(&mut self, session_id: &str, turn_id: &TurnId) {
        self.invocations
            .retain(|key, _| key.session_id != session_id || &key.turn_id != turn_id);
    }

    fn create(
        &mut self,
        session_id: &str,
        turn_id: &TurnId,
        mut call: ToolCall,
    ) -> Result<AcpToolAccumulation, AcpToolAccumulatorError> {
        let provider_tool_call_id = call.tool_call_id.to_string();
        self.validate_identifier("tool call id", &provider_tool_call_id)?;
        let key = ToolInvocationKey {
            session_id: session_id.to_owned(),
            turn_id: turn_id.clone(),
            provider_tool_call_id: provider_tool_call_id.clone(),
        };
        self.ensure_capacity_for(&key)?;
        let entry = self
            .invocations
            .entry(key.clone())
            .or_insert_with(AccumulatedToolCall::new);
        if let Some(existing) = &entry.call {
            if existing == &call {
                return Ok(AcpToolAccumulation::Unchanged {
                    snapshot: Self::project(self.authority, &key, entry)?,
                });
            }
            return Err(AcpToolAccumulatorError::ConflictingCreate {
                tool_call_id: provider_tool_call_id,
            });
        }
        let buffered = entry.buffered.clone();
        if let Some(buffered_status) = buffered.status {
            validate_status_transition(call.status, buffered_status)?;
        }
        call.update(buffered);
        validate_payload_limit(self.limits.max_payload_bytes, &call)?;
        let revision = next_revision(entry.revision)?;
        entry.call = Some(call);
        entry.buffered = ToolCallUpdateFields::default();
        entry.revision = revision;
        let snapshot = Self::project(self.authority, &key, entry)?
            .ok_or(AcpToolAccumulatorError::MissingAccumulatedCall)?;
        Ok(AcpToolAccumulation::Updated(snapshot))
    }

    fn update(
        &mut self,
        session_id: &str,
        turn_id: &TurnId,
        update: ToolCallUpdate,
    ) -> Result<AcpToolAccumulation, AcpToolAccumulatorError> {
        let provider_tool_call_id = update.tool_call_id.to_string();
        self.validate_identifier("tool call id", &provider_tool_call_id)?;
        let key = ToolInvocationKey {
            session_id: session_id.to_owned(),
            turn_id: turn_id.clone(),
            provider_tool_call_id: provider_tool_call_id.clone(),
        };
        self.ensure_capacity_for(&key)?;
        let entry = self
            .invocations
            .entry(key.clone())
            .or_insert_with(AccumulatedToolCall::new);
        if let Some(current) = &entry.call {
            let mut candidate = current.clone();
            candidate.update(update.fields);
            validate_payload_limit(self.limits.max_payload_bytes, &candidate)?;
            if &candidate == current {
                return Ok(AcpToolAccumulation::Unchanged {
                    snapshot: Self::project(self.authority, &key, entry)?,
                });
            }
            if status_is_terminal(current.status) {
                return Err(AcpToolAccumulatorError::TerminalMutation {
                    tool_call_id: provider_tool_call_id,
                });
            }
            validate_status_transition(current.status, candidate.status)?;
            let revision = next_revision(entry.revision)?;
            entry.call = Some(candidate);
            entry.revision = revision;
            let snapshot = Self::project(self.authority, &key, entry)?
                .ok_or(AcpToolAccumulatorError::MissingAccumulatedCall)?;
            return Ok(AcpToolAccumulation::Updated(snapshot));
        }

        let previous = entry.buffered.clone();
        if let (Some(current), Some(next)) = (entry.buffered.status, update.fields.status) {
            validate_status_transition(current, next)?;
        }
        let mut candidate_fields = previous.clone();
        merge_fields(&mut candidate_fields, update.fields);
        if candidate_fields == previous {
            return Ok(AcpToolAccumulation::Unchanged { snapshot: None });
        }
        validate_payload_limit(self.limits.max_payload_bytes, &candidate_fields)?;
        let revision = next_revision(entry.revision)?;
        let buffered_update = ToolCallUpdate::new(provider_tool_call_id, candidate_fields.clone());
        let Ok(call) = ToolCall::try_from(buffered_update) else {
            entry.buffered = candidate_fields;
            entry.revision = revision;
            return Ok(AcpToolAccumulation::Buffered {
                tool_use_id: entry.tool_use_id.clone(),
            });
        };
        validate_payload_limit(self.limits.max_payload_bytes, &call)?;
        entry.call = Some(call);
        entry.buffered = ToolCallUpdateFields::default();
        entry.revision = revision;
        let snapshot = Self::project(self.authority, &key, entry)?
            .ok_or(AcpToolAccumulatorError::MissingAccumulatedCall)?;
        Ok(AcpToolAccumulation::Updated(snapshot))
    }

    fn project(
        authority: ExecutionAuthority,
        key: &ToolInvocationKey,
        entry: &AccumulatedToolCall,
    ) -> Result<Option<AcpToolInvocationSnapshot>, AcpToolAccumulatorError> {
        let Some(call) = &entry.call else {
            return Ok(None);
        };
        let name = BoundedName::new(tool_kind_name(call.kind))
            .map_err(|_| AcpToolAccumulatorError::InvalidSummary)?;
        let summary = safe_tool_summary(&call.title)?;
        let tool_call = serde_json::to_value(call)
            .map_err(|error| AcpToolAccumulatorError::InvalidUpdate(error.to_string()))?;
        Ok(Some(AcpToolInvocationSnapshot {
            session_id: key.session_id.clone(),
            provider_tool_call_id: key.provider_tool_call_id.clone(),
            projection: ToolInvocationSnapshot {
                turn_id: key.turn_id.clone(),
                tool_use_id: entry.tool_use_id.clone(),
                revision: entry.revision,
                summary: ToolSummary { name, summary },
                status: map_status(call.status),
                authority,
            },
            tool_call,
        }))
    }

    fn validate_identifier(
        &self,
        field: &'static str,
        value: &str,
    ) -> Result<(), AcpToolAccumulatorError> {
        if value.is_empty() || value.len() > self.limits.max_identifier_bytes {
            return Err(AcpToolAccumulatorError::InvalidIdentifier {
                field,
                limit: self.limits.max_identifier_bytes,
            });
        }
        Ok(())
    }

    fn validate_payload(&self, value: &Value) -> Result<(), AcpToolAccumulatorError> {
        validate_payload_limit(self.limits.max_payload_bytes, value)
    }

    fn ensure_capacity_for(&self, key: &ToolInvocationKey) -> Result<(), AcpToolAccumulatorError> {
        if !self.invocations.contains_key(key)
            && self.invocations.len() >= self.limits.max_invocations
        {
            return Err(AcpToolAccumulatorError::TooManyInvocations {
                limit: self.limits.max_invocations,
            });
        }
        Ok(())
    }
}

fn safe_tool_summary(title: &str) -> Result<BoundedText, AcpToolAccumulatorError> {
    let mut summary = title
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>();
    if summary.is_empty() {
        summary.push_str("Agent tool request");
    }
    if summary.len() > MAX_TEXT_BYTES {
        let mut boundary = MAX_TEXT_BYTES;
        while !summary.is_char_boundary(boundary) {
            boundary = boundary.saturating_sub(1);
        }
        summary.truncate(boundary);
    }
    BoundedText::new(summary).map_err(|_| AcpToolAccumulatorError::InvalidSummary)
}

fn validate_payload_limit<T: serde::Serialize>(
    limit: usize,
    value: &T,
) -> Result<(), AcpToolAccumulatorError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| AcpToolAccumulatorError::InvalidUpdate(error.to_string()))?;
    if bytes.len() > limit {
        return Err(AcpToolAccumulatorError::PayloadTooLarge { limit });
    }
    Ok(())
}

fn merge_fields(target: &mut ToolCallUpdateFields, update: ToolCallUpdateFields) {
    if update.kind.is_some() {
        target.kind = update.kind;
    }
    if update.status.is_some() {
        target.status = update.status;
    }
    if update.title.is_some() {
        target.title = update.title;
    }
    if update.content.is_some() {
        target.content = update.content;
    }
    if update.locations.is_some() {
        target.locations = update.locations;
    }
    if update.raw_input.is_some() {
        target.raw_input = update.raw_input;
    }
    if update.raw_output.is_some() {
        target.raw_output = update.raw_output;
    }
}

fn status_is_terminal(status: ToolCallStatus) -> bool {
    matches!(status, ToolCallStatus::Completed | ToolCallStatus::Failed)
}

fn validate_status_transition(
    current: ToolCallStatus,
    next: ToolCallStatus,
) -> Result<(), AcpToolAccumulatorError> {
    let is_regression = matches!(
        (current, next),
        (ToolCallStatus::InProgress, ToolCallStatus::Pending)
            | (
                ToolCallStatus::Completed,
                ToolCallStatus::Pending | ToolCallStatus::InProgress
            )
            | (
                ToolCallStatus::Failed,
                ToolCallStatus::Pending | ToolCallStatus::InProgress
            )
    );
    if is_regression {
        return Err(AcpToolAccumulatorError::StatusRegression {
            from: status_name(current),
            to: status_name(next),
        });
    }
    Ok(())
}

fn next_revision(current: u64) -> Result<u64, AcpToolAccumulatorError> {
    current
        .checked_add(1)
        .ok_or(AcpToolAccumulatorError::RevisionOverflow)
}

fn status_name(status: ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "pending",
        ToolCallStatus::InProgress => "in_progress",
        ToolCallStatus::Completed => "completed",
        ToolCallStatus::Failed => "failed",
        _ => "unknown",
    }
}

fn map_status(status: ToolCallStatus) -> ToolInvocationStatus {
    match status {
        ToolCallStatus::Pending => ToolInvocationStatus::Pending,
        ToolCallStatus::InProgress => ToolInvocationStatus::InProgress,
        ToolCallStatus::Completed => ToolInvocationStatus::Completed,
        ToolCallStatus::Failed => ToolInvocationStatus::Failed,
        _ => ToolInvocationStatus::Pending,
    }
}

fn tool_kind_name(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Read => "read",
        ToolKind::Edit => "edit",
        ToolKind::Delete => "delete",
        ToolKind::Move => "move",
        ToolKind::Search => "search",
        ToolKind::Execute => "execute",
        ToolKind::Think => "think",
        ToolKind::Fetch => "fetch",
        ToolKind::SwitchMode => "switch_mode",
        ToolKind::Other => "agent_tool",
        _ => "agent_tool",
    }
}

#[cfg(test)]
#[path = "tool_accumulator/tests.rs"]
mod tests;
