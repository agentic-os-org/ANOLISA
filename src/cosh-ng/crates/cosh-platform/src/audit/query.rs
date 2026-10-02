//! Bounded audit status, pagination, and correlated trace operations.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, Utc};
use cosh_types::audit::{AuditOutcomeStatus, AuditSettings};
use cosh_types::error::{CoshError, ErrorCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::config::AuditRootSource;
use super::reader::{read_all, AuditReadDiagnostic, AuditSchemaGeneration, AuditStoredEvent};
use super::state::{read_state, AuditOperationalState};

/// Maximum accepted page size.
pub const MAX_PAGE_SIZE: usize = 1000;
/// Default page size.
pub const DEFAULT_PAGE_SIZE: usize = 100;

/// Normalized filters shared by events, trace, and export.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditEventFilter {
    /// Inclusive lower event-time bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<DateTime<Utc>>,
    /// Whether `since` was resolved from a relative duration for cursor anchoring.
    #[serde(skip)]
    pub since_is_relative: bool,
    /// Inclusive upper event-time bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<DateTime<Utc>>,
    /// Allowed event names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub event_types: Vec<String>,
    /// Allowed component wire names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<String>,
    /// Allowed outcome wire values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outcomes: Vec<String>,
    /// Match any public identity field or event ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    /// Restrict to one schema generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<AuditSchemaGenerationFilter>,
}

impl AuditEventFilter {
    /// Sorts and deduplicates set-like inputs before fingerprinting.
    pub fn normalize(&mut self) {
        for values in [
            &mut self.event_types,
            &mut self.components,
            &mut self.outcomes,
        ] {
            values.sort();
            values.dedup();
        }
    }
}

/// CLI-facing schema generation filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditSchemaGenerationFilter {
    /// Projected version 0 policy records.
    LegacyV0,
    /// Canonical version 1 events.
    V1,
}

/// One bounded page of ordered audit events.
#[derive(Debug, Clone, Serialize)]
pub struct AuditEventPage {
    /// Public event envelopes in deterministic order.
    pub events: Vec<AuditStoredEvent>,
    /// Visible reader diagnostics.
    pub diagnostics: Vec<AuditReadDiagnostic>,
    /// Opaque continuation cursor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Whether reader safety limits truncated the source set.
    pub safety_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditCursorV1 {
    version: u8,
    filter_fingerprint: String,
    #[serde(default)]
    since_is_relative: bool,
    #[serde(default)]
    anchored_since: Option<DateTime<Utc>>,
    occurred_at: DateTime<Utc>,
    observed_at: DateTime<Utc>,
    component: String,
    segment_id: String,
    sequence: u64,
}

/// Queries one deterministic, cursor-bound page.
///
/// # Errors
///
/// Returns `AuditCursorInvalid` for malformed, unsupported, or filter-mismatched
/// cursors and `InvalidInput` for page sizes outside `1..=1000`.
// Keep the explicit predicates readable across the inclusive filter bounds.
#[allow(clippy::unnecessary_map_or)]
pub fn query_events(
    root: &Path,
    mut filter: AuditEventFilter,
    limit: usize,
    cursor: Option<&str>,
) -> Result<AuditEventPage, CoshError> {
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(CoshError::new(
            ErrorCode::InvalidInput,
            "audit event limit must be between 1 and 1000",
            "audit",
        ));
    }
    let decoded = cursor.map(decode_cursor).transpose()?;
    stabilize_relative_since(&mut filter, decoded.as_ref())?;
    filter.normalize();
    let fingerprint = filter_fingerprint(&filter)?;
    let after = match decoded {
        Some(decoded) => {
            if decoded.filter_fingerprint != fingerprint {
                return Err(cursor_error("cursor does not match normalized filters"));
            }
            Some(decoded)
        }
        None => None,
    };
    let read = read_all(root, true)?;
    let mut matching = read
        .events
        .into_iter()
        .filter(|event| matches_filter(event, &filter))
        .filter(|event| {
            after
                .as_ref()
                .map_or(true, |cursor| ordering_key(event) > cursor_key(cursor))
        })
        .collect::<Vec<_>>();
    let has_more = matching.len() > limit;
    matching.truncate(limit);
    let next_cursor = if has_more {
        matching.last().map(|event| {
            encode_cursor(&AuditCursorV1 {
                version: 1,
                filter_fingerprint: fingerprint.clone(),
                since_is_relative: filter.since_is_relative,
                anchored_since: filter.since_is_relative.then_some(filter.since).flatten(),
                occurred_at: event.event.occurred_at,
                observed_at: event.event.observed_at,
                component: event.event.component.name.as_str().to_string(),
                segment_id: event.segment_id.clone(),
                sequence: event.event.sequence,
            })
        })
    } else {
        None
    }
    .transpose()?;
    Ok(AuditEventPage {
        events: matching,
        diagnostics: read.diagnostics,
        next_cursor,
        safety_truncated: read.truncated,
    })
}

/// Matched identity kind for a trace result.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceIdentityKind {
    /// Event ID.
    Event,
    /// Installation ID.
    Installation,
    /// Shell session ID.
    ShellSession,
    /// Provider session ID.
    ProviderSession,
    /// Run ID.
    Run,
    /// Turn ID.
    Turn,
    /// Request ID.
    Request,
    /// Tool-use ID.
    ToolUse,
    /// Command ID.
    Command,
}

/// Explicit lifecycle inconsistency found by trace analysis.
#[derive(Debug, Clone, Serialize)]
pub struct AuditTraceGap {
    /// Stable gap category.
    pub kind: String,
    /// Lifecycle domain.
    pub lifecycle: String,
}

/// Duration derived only from compatible start and terminal pairs.
#[derive(Debug, Clone, Serialize)]
pub struct AuditTraceDuration {
    /// Lifecycle domain.
    pub lifecycle: String,
    /// Elapsed milliseconds.
    pub duration_ms: u64,
}

/// Correlated audit timeline and gap analysis.
#[derive(Debug, Clone, Serialize)]
pub struct AuditTraceResult {
    /// Ordered matched events.
    pub events: Vec<AuditStoredEvent>,
    /// Identity fields that matched the requested value.
    pub matched_identity_kinds: Vec<TraceIdentityKind>,
    /// Explicit missing, duplicate, or conflicting lifecycle facts.
    pub gaps: Vec<AuditTraceGap>,
    /// Compatible lifecycle durations.
    pub durations: Vec<AuditTraceDuration>,
    /// Visible reader diagnostics.
    pub diagnostics: Vec<AuditReadDiagnostic>,
    /// Whether reader safety limits truncated source data.
    pub safety_truncated: bool,
    /// Opaque continuation cursor for the next trace page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

type LifecycleObservations = (Vec<DateTime<Utc>>, Vec<DateTime<Utc>>);

/// Builds one correlated timeline for an event or identity value.
///
/// # Errors
///
/// Returns a stable reader error when source evaluation cannot proceed safely.
pub fn trace_events(
    root: &Path,
    identity: &str,
    since: Option<DateTime<Utc>>,
    since_is_relative: bool,
    until: Option<DateTime<Utc>>,
    limit: usize,
    cursor: Option<&str>,
) -> Result<AuditTraceResult, CoshError> {
    let mut trace_filter = AuditEventFilter {
        since,
        since_is_relative,
        until,
        identity: Some(identity.to_string()),
        ..AuditEventFilter::default()
    };
    let decoded = cursor.map(decode_cursor).transpose()?;
    stabilize_relative_since(&mut trace_filter, decoded.as_ref())?;
    let effective_since = trace_filter.since;
    let read = read_all(root, true)?;
    let mut matched_kinds = BTreeSet::new();
    let events = read
        .events
        .into_iter()
        .filter(|stored| {
            if effective_since.is_some_and(|value| stored.event.occurred_at < value)
                || until.is_some_and(|value| stored.event.occurred_at > value)
            {
                return false;
            }
            let kinds = matching_identity_kinds(stored, identity);
            matched_kinds.extend(kinds.iter().cloned());
            !kinds.is_empty()
        })
        .collect::<Vec<_>>();
    let (gaps, durations) = analyze_lifecycles(&events);
    let page = query_events(root, trace_filter, limit, cursor)?;
    Ok(AuditTraceResult {
        events: page.events,
        matched_identity_kinds: matched_kinds.into_iter().collect(),
        gaps,
        durations,
        diagnostics: read.diagnostics,
        safety_truncated: read.truncated || page.safety_truncated,
        next_cursor: page.next_cursor,
    })
}

fn stabilize_relative_since(
    filter: &mut AuditEventFilter,
    cursor: Option<&AuditCursorV1>,
) -> Result<(), CoshError> {
    let Some(cursor) = cursor else {
        return Ok(());
    };
    if cursor.since_is_relative != filter.since_is_relative {
        return Err(cursor_error("cursor does not match normalized filters"));
    }
    if cursor.since_is_relative {
        filter.since = Some(
            cursor
                .anchored_since
                .ok_or_else(|| cursor_error("relative-time cursor is missing its anchor"))?,
        );
    }
    Ok(())
}

/// Audit store health and effective policy summary.
#[derive(Debug, Clone, Serialize)]
pub struct AuditStatus {
    /// Root source without exposing the full path.
    pub root_source: AuditRootSource,
    /// Fixed safe root label.
    pub root_label: String,
    /// Effective settings and sources.
    pub settings: AuditSettings,
    /// Parsed state cache when healthy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<AuditOperationalState>,
    /// Safe state-health category.
    pub state_health: String,
    /// Active segment count.
    pub active_segments: usize,
    /// Closed segment count.
    pub closed_segments: usize,
    /// Discovered segment bytes.
    pub segment_bytes: u64,
    /// Earliest event timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub earliest_event: Option<DateTime<Utc>>,
    /// Latest event timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_event: Option<DateTime<Utc>>,
    /// Legacy file count.
    pub legacy_files: usize,
    /// Visible reader diagnostics.
    pub diagnostics: Vec<AuditReadDiagnostic>,
    /// Whether reader safety limits truncated evaluation.
    pub safety_truncated: bool,
}

/// Evaluates store health without requiring live UI state.
///
/// # Errors
///
/// Returns an error only when bounded source evaluation cannot proceed safely.
pub fn audit_status(
    root: &Path,
    root_source: AuditRootSource,
    settings: AuditSettings,
) -> Result<AuditStatus, CoshError> {
    let read = read_all(root, true)?;
    let earliest_event = read.events.first().map(|event| event.event.occurred_at);
    let latest_event = read.events.last().map(|event| event.event.occurred_at);
    let (state, state_health) = match read_state(root) {
        Ok(Some(state)) => (Some(state), "healthy".to_string()),
        Ok(None) => (None, "missing".to_string()),
        Err(_) => (None, "corrupt".to_string()),
    };
    Ok(AuditStatus {
        root_source,
        root_label: "audit/v1".to_string(),
        settings,
        state,
        state_health,
        active_segments: read.active_segments,
        closed_segments: read.closed_segments,
        segment_bytes: read.segment_bytes,
        earliest_event,
        latest_event,
        legacy_files: read.legacy_files,
        diagnostics: read.diagnostics,
        safety_truncated: read.truncated,
    })
}

fn matches_filter(stored: &AuditStoredEvent, filter: &AuditEventFilter) -> bool {
    let event = &stored.event;
    if filter.since.is_some_and(|value| event.occurred_at < value)
        || filter.until.is_some_and(|value| event.occurred_at > value)
    {
        return false;
    }
    if !filter.event_types.is_empty()
        && !filter
            .event_types
            .iter()
            .any(|value| value == event.event_type.as_str())
    {
        return false;
    }
    if !filter.components.is_empty()
        && !filter
            .components
            .iter()
            .any(|value| value == event.component.name.as_str())
    {
        return false;
    }
    if !filter.outcomes.is_empty() {
        let status = outcome_name(&event.outcome.status);
        if !filter.outcomes.iter().any(|value| value == status) {
            return false;
        }
    }
    if let Some(identity) = &filter.identity {
        if matching_identity_kinds(stored, identity).is_empty() {
            return false;
        }
    }
    match filter.generation {
        Some(AuditSchemaGenerationFilter::LegacyV0) => {
            stored.generation == AuditSchemaGeneration::LegacyV0
        }
        Some(AuditSchemaGenerationFilter::V1) => stored.generation == AuditSchemaGeneration::V1,
        None => true,
    }
}

fn matching_identity_kinds(stored: &AuditStoredEvent, value: &str) -> Vec<TraceIdentityKind> {
    let event = &stored.event;
    let identity = &event.identity;
    let pairs = [
        (TraceIdentityKind::Event, Some(event.event_id.as_str())),
        (
            TraceIdentityKind::Installation,
            identity.installation_id.as_deref(),
        ),
        (
            TraceIdentityKind::ShellSession,
            identity.shell_session_id.as_deref(),
        ),
        (
            TraceIdentityKind::ProviderSession,
            identity.provider_session_id.as_deref(),
        ),
        (TraceIdentityKind::Run, identity.run_id.as_deref()),
        (TraceIdentityKind::Turn, identity.turn_id.as_deref()),
        (TraceIdentityKind::Request, identity.request_id.as_deref()),
        (TraceIdentityKind::ToolUse, identity.tool_use_id.as_deref()),
        (TraceIdentityKind::Command, identity.command_id.as_deref()),
    ];
    pairs
        .into_iter()
        .filter_map(|(kind, candidate)| (candidate == Some(value)).then_some(kind))
        .collect()
}

pub(crate) fn analyze_lifecycles(
    events: &[AuditStoredEvent],
) -> (Vec<AuditTraceGap>, Vec<AuditTraceDuration>) {
    let mut grouped: BTreeMap<(String, String), LifecycleObservations> = BTreeMap::new();
    for stored in events {
        let name = stored.event.event_type.as_str();
        let Some((lifecycle, phase)) = lifecycle_phase(name) else {
            continue;
        };
        let Some(identity) = lifecycle_identity(lifecycle, &stored.event) else {
            continue;
        };
        let pair = grouped
            .entry((lifecycle.to_string(), identity.to_string()))
            .or_default();
        if phase == "start" {
            pair.0.push(stored.event.occurred_at);
        } else {
            pair.1.push(stored.event.occurred_at);
        }
    }
    let mut gaps = Vec::new();
    let mut durations = Vec::new();
    for ((lifecycle, _identity), (starts, terminals)) in grouped {
        if starts.is_empty() {
            gaps.push(AuditTraceGap {
                kind: "missing_start".to_string(),
                lifecycle: lifecycle.clone(),
            });
        } else if starts.len() > 1 {
            gaps.push(AuditTraceGap {
                kind: "duplicate_start".to_string(),
                lifecycle: lifecycle.clone(),
            });
        }
        if terminals.is_empty() {
            gaps.push(AuditTraceGap {
                kind: "missing_terminal".to_string(),
                lifecycle: lifecycle.clone(),
            });
        } else if terminals.len() > 1 {
            gaps.push(AuditTraceGap {
                kind: "conflicting_terminals".to_string(),
                lifecycle: lifecycle.clone(),
            });
        }
        if starts.len() == 1 && terminals.len() == 1 && terminals[0] >= starts[0] {
            durations.push(AuditTraceDuration {
                lifecycle,
                duration_ms: (terminals[0] - starts[0]).num_milliseconds() as u64,
            });
        }
    }
    (gaps, durations)
}

fn lifecycle_identity<'a>(
    lifecycle: &str,
    event: &'a cosh_types::audit::AuditEventV1,
) -> Option<&'a str> {
    match lifecycle {
        "session" => event
            .identity
            .provider_session_id
            .as_deref()
            .or(event.identity.shell_session_id.as_deref()),
        "turn" => event.identity.turn_id.as_deref(),
        "provider.request" | "approval" => event.identity.request_id.as_deref(),
        "tool" => event.identity.tool_use_id.as_deref(),
        "shell.command" => event.identity.command_id.as_deref(),
        _ => None,
    }
}

fn lifecycle_phase(event_type: &str) -> Option<(&'static str, &'static str)> {
    match event_type {
        "session.started" => Some(("session", "start")),
        "session.ended" => Some(("session", "terminal")),
        "turn.started" => Some(("turn", "start")),
        "turn.completed" | "turn.failed" => Some(("turn", "terminal")),
        "provider.request.started" => Some(("provider.request", "start")),
        "provider.request.completed" | "provider.request.failed" | "provider.request.cancelled" => {
            Some(("provider.request", "terminal"))
        }
        "tool.requested" => Some(("tool", "start")),
        // Execution start is an intermediate phase of the request lifecycle.
        "tool.execution.started" => None,
        "tool.completed" | "tool.failed" | "tool.cancelled" => Some(("tool", "terminal")),
        "approval.requested" => Some(("approval", "start")),
        "approval.resolved" => Some(("approval", "terminal")),
        "shell.command.started" => Some(("shell.command", "start")),
        "shell.command.completed" | "shell.command.failed" => Some(("shell.command", "terminal")),
        _ => None,
    }
}

fn outcome_name(status: &AuditOutcomeStatus) -> &'static str {
    match status {
        AuditOutcomeStatus::Started => "started",
        AuditOutcomeStatus::Success => "success",
        AuditOutcomeStatus::Failed => "failed",
        AuditOutcomeStatus::Cancelled => "cancelled",
        AuditOutcomeStatus::Allowed => "allowed",
        AuditOutcomeStatus::Denied => "denied",
        AuditOutcomeStatus::Degraded => "degraded",
        AuditOutcomeStatus::Recovered => "recovered",
        AuditOutcomeStatus::Unknown => "unknown",
    }
}

fn ordering_key(event: &AuditStoredEvent) -> (DateTime<Utc>, DateTime<Utc>, &str, &str, u64) {
    (
        event.event.occurred_at,
        event.event.observed_at,
        event.event.component.name.as_str(),
        &event.segment_id,
        event.event.sequence,
    )
}

fn cursor_key(cursor: &AuditCursorV1) -> (DateTime<Utc>, DateTime<Utc>, &str, &str, u64) {
    (
        cursor.occurred_at,
        cursor.observed_at,
        &cursor.component,
        &cursor.segment_id,
        cursor.sequence,
    )
}

fn filter_fingerprint(filter: &AuditEventFilter) -> Result<String, CoshError> {
    let bytes = serde_json::to_vec(filter).map_err(|_| cursor_error("cannot normalize filters"))?;
    Ok(hex_bytes(&Sha256::digest(bytes)))
}

fn encode_cursor(cursor: &AuditCursorV1) -> Result<String, CoshError> {
    let bytes = serde_json::to_vec(cursor).map_err(|_| cursor_error("cannot encode cursor"))?;
    Ok(format!("v1.{}", hex_bytes(&bytes)))
}

fn decode_cursor(cursor: &str) -> Result<AuditCursorV1, CoshError> {
    let encoded = cursor
        .strip_prefix("v1.")
        .ok_or_else(|| cursor_error("unsupported cursor version"))?;
    let bytes = decode_hex(encoded).ok_or_else(|| cursor_error("malformed cursor"))?;
    let decoded: AuditCursorV1 =
        serde_json::from_slice(&bytes).map_err(|_| cursor_error("malformed cursor"))?;
    if decoded.version != 1 {
        return Err(cursor_error("unsupported cursor version"));
    }
    Ok(decoded)
}

fn cursor_error(message: &str) -> CoshError {
    CoshError::new(ErrorCode::AuditCursorInvalid, message, "audit")
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if value.len() & 1 == 1 {
        return None;
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16)?;
            let low = (pair[1] as char).to_digit(16)?;
            Some(((high << 4) | low) as u8)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::reader::AuditSchemaGeneration;
    use super::*;
    use cosh_types::audit::{
        AuditActor, AuditActorKind, AuditComponent, AuditComponentName, AuditEventOutcome,
        AuditEventType, AuditEventV1, AuditIdentity, AuditProviderData, AuditRedaction,
        AuditRedactionStatus, AuditSubject, AuditToolData, KnownAuditEventType,
    };

    #[test]
    fn cursor_rejects_filter_mismatch() {
        let directory = tempfile::tempdir().unwrap();
        let first = query_events(directory.path(), AuditEventFilter::default(), 1, None).unwrap();
        assert!(first.next_cursor.is_none());
        assert!(decode_cursor("v2.00").is_err());
    }

    #[test]
    fn lifecycle_analysis_keeps_missing_facts_explicit() {
        let (gaps, durations) = analyze_lifecycles(&[]);
        assert!(gaps.is_empty());
        assert!(durations.is_empty());
    }

    fn stored_event(
        event_type: KnownAuditEventType,
        identity: AuditIdentity,
        second: i64,
    ) -> AuditStoredEvent {
        let timestamp = DateTime::from_timestamp(second, 0).unwrap();
        let payload = if event_type.as_str().starts_with("provider.") {
            serde_json::to_value(AuditProviderData {
                provider: "mock".to_string(),
                ..AuditProviderData::default()
            })
            .unwrap()
        } else {
            serde_json::to_value(AuditToolData {
                tool_kind: "shell".to_string(),
                ..AuditToolData::default()
            })
            .unwrap()
        };
        let event = AuditEventV1::new(
            format!("event-{second}-{}", event_type.as_str()),
            AuditEventType::from(event_type),
            timestamp,
            timestamp,
            second as u64,
            AuditComponent {
                name: AuditComponentName::CoshCore,
                version: "test".to_string(),
            },
            identity,
            AuditActor {
                kind: AuditActorKind::Agent,
                uid: None,
                euid: None,
            },
            AuditEventOutcome {
                status: AuditOutcomeStatus::Success,
                code: None,
                retryable: false,
            },
            AuditSubject {
                kind: "lifecycle".to_string(),
                name: None,
            },
            &payload,
            AuditRedaction {
                policy_version: "test".to_string(),
                status: AuditRedactionStatus::Clean,
                fields: Vec::new(),
            },
        )
        .unwrap();
        AuditStoredEvent {
            event,
            generation: AuditSchemaGeneration::V1,
            segment_id: "segment".to_string(),
        }
    }

    #[test]
    fn lifecycle_analysis_separates_ids_and_treats_execution_as_intermediate() {
        let provider = |request: &str| AuditIdentity {
            request_id: Some(request.to_string()),
            ..AuditIdentity::default()
        };
        let tool = AuditIdentity {
            run_id: Some("run-1".to_string()),
            tool_use_id: Some("tool-1".to_string()),
            ..AuditIdentity::default()
        };
        let events = vec![
            stored_event(
                KnownAuditEventType::ProviderRequestStarted,
                provider("a"),
                1,
            ),
            stored_event(
                KnownAuditEventType::ProviderRequestCompleted,
                provider("a"),
                2,
            ),
            stored_event(
                KnownAuditEventType::ProviderRequestStarted,
                provider("b"),
                3,
            ),
            stored_event(
                KnownAuditEventType::ProviderRequestCompleted,
                provider("b"),
                5,
            ),
            stored_event(KnownAuditEventType::ToolRequested, tool.clone(), 6),
            stored_event(KnownAuditEventType::ToolExecutionStarted, tool.clone(), 7),
            stored_event(KnownAuditEventType::ToolCompleted, tool, 9),
        ];

        let (gaps, durations) = analyze_lifecycles(&events);

        assert!(gaps.is_empty());
        assert_eq!(durations.len(), 3);
    }
}
