//! Server-owned query authorization and observability application operations.

use std::sync::Arc;
use std::time::Instant;

use asc_security_events::CorrelationCandidate;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::PeerCredentials;

mod correlation;

/// V1 sentinel for a missing run, requiring session-level security correlation.
pub const ZERO_RUN_ID: &str = "00000000-0000-0000-0000-000000000000";

/// SQL row visibility. Never deserialize this type from a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueryScope {
    /// Every owner, including unassigned historical observability rows.
    All,
    /// Records produced by one kernel-authenticated UID.
    Own(u32),
    /// Unassigned historical observability records, visible only to root.
    Unknown,
}

impl QueryScope {
    /// Derives visibility only from the kernel-authenticated socket peer.
    pub const fn from_peer(peer: PeerCredentials) -> Self {
        match peer.uid() {
            0 => Self::All,
            uid => Self::Own(uid),
        }
    }

    /// Narrows an already-authorized result to its owner before correlation.
    pub const fn for_owner(owner: Option<u32>) -> Self {
        match owner {
            Some(uid) => Self::Own(uid),
            None => Self::Unknown,
        }
    }
}

/// Safe query failures without database paths, SQL, or record contents.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    /// Invalid filters, pagination, or an ambiguous cross-owner session.
    #[error("invalid query parameters or session shared by multiple owners")]
    InvalidArgument,
    /// A requested owner is outside the authenticated peer's scope.
    #[error("requested owner is outside the authorized scope")]
    PermissionDenied,
    /// The configured store has not been initialized or migrated.
    #[error("query storage is unavailable")]
    Unavailable,
    /// Cancellation or the shared request deadline interrupted work.
    #[error("query deadline exceeded")]
    DeadlineExceeded,
    /// A bounded result would exceed the query budget.
    #[error("query result exceeds resource limits; narrow the query")]
    ResourceExhausted,
    /// A storage or projection failure, distinct from an empty result.
    #[error("query storage failed")]
    Internal,
}

/// Transport-independent deadline and live cancellation signal.
#[derive(Clone)]
pub struct QueryControl {
    deadline: Instant,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl QueryControl {
    /// Binds the transport-owned deadline and cancellation source.
    pub fn new(deadline: Instant, cancelled: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Self {
            deadline,
            cancelled: Arc::new(cancelled),
        }
    }

    /// Tests the live cancellation source as well as the deadline.
    pub fn is_cancelled(&self) -> bool {
        Instant::now() >= self.deadline || (self.cancelled)()
    }

    /// Stops work once the caller no longer has a query budget.
    ///
    /// # Errors
    /// Returns `DeadlineExceeded` after cancellation or expiry.
    pub fn check(&self) -> Result<(), QueryError> {
        if self.is_cancelled() {
            Err(QueryError::DeadlineExceeded)
        } else {
            Ok(())
        }
    }
}

/// Normalized half-open UTC epoch interval.
#[derive(Debug, Clone, Copy, Default)]
pub struct QueryWindow {
    /// Inclusive lower bound.
    pub since: Option<f64>,
    /// Exclusive upper bound.
    pub until: Option<f64>,
}

/// Validated bounded pagination, retaining `SQLite`'s signed 64-bit offset range.
#[derive(Debug, Clone, Copy)]
pub struct QueryPage {
    /// Maximum records in one response, at most 1000.
    pub limit: u32,
    /// Rows to skip after authorization and ordering.
    pub offset: i64,
}

/// A page whose total is independent of its offset.
#[derive(Debug, Serialize, Deserialize)]
pub struct QueryResult<T> {
    /// Authorized rows in deterministic order.
    pub items: Vec<T>,
    /// Total authorized matches before pagination.
    pub total: u64,
}

/// One owner-qualified session summary.
#[derive(Debug, Serialize, Deserialize)]
pub struct QuerySession {
    /// None denotes an unassigned historical owner, never UID zero.
    pub uid: Option<u32>,
    /// Caller-assigned session label, unique only within its owner.
    pub session_id: String,
    /// Root-only query label for a session shared by multiple owners.
    #[serde(skip)]
    pub qualified_id: Option<String>,
    /// Earliest matching observation.
    pub first_seen_epoch: f64,
    /// Latest matching observation.
    pub last_seen_epoch: f64,
    /// Distinct runs within this owner and session.
    pub turn_count: u64,
    /// Observation count before pagination.
    pub observability_event_count: u64,
}

/// One run within an owner-qualified session.
#[derive(Debug, Serialize, Deserialize)]
pub struct QueryRun {
    /// Persisted owner of the run's observations.
    pub uid: Option<u32>,
    /// Caller-assigned run label.
    pub run_id: String,
    /// Earliest matching observation.
    pub started_at_epoch: f64,
    /// Latest matching observation.
    pub ended_at_epoch: f64,
    /// Bounded preview from the first `before_agent_run` observation.
    pub user_input_preview: Option<String>,
    /// Matching observations before pagination.
    pub observability_event_count: u64,
}

/// One authorized observation with parsed payloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryObservation {
    /// `SQLite` row identity used for stable paging and correlations.
    pub id: i64,
    /// Persisted owner; unknown records cannot correlate to security events.
    pub uid: Option<u32>,
    /// Observability hook name.
    pub hook: String,
    /// Original normalized timestamp.
    pub timestamp: String,
    /// Numeric timestamp for ordering and matching.
    pub timestamp_epoch: f64,
    /// Session correlation label.
    pub session_id: String,
    /// Run correlation label.
    pub run_id: String,
    /// Optional model call label.
    pub call_id: Option<String>,
    /// Optional tool call label.
    pub tool_call_id: Option<String>,
    /// Hook-specific metadata.
    pub metadata: Value,
    /// Hook-specific measurements and observed content.
    pub metrics: Value,
}

/// Security counts for an authorized session/run, independent of matched timeline items.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct QuerySecurityCounts {
    /// All security event categories, for session/run list counts.
    pub total: u64,
    /// V1 report categories grouped by execution result, not detection verdict.
    pub by_category_result:
        std::collections::BTreeMap<String, std::collections::BTreeMap<String, u64>>,
}

/// Observability read port; each operation must apply scope before pagination or aggregation.
pub trait ObservabilityQueries: Send + Sync {
    /// Lists owner-qualified sessions; storage errors must propagate.
    ///
    /// # Errors
    /// Propagates invalid scope, storage, resource, and cancellation failures.
    fn sessions(
        &self,
        scope: QueryScope,
        session: Option<&str>,
        window: QueryWindow,
        page: QueryPage,
        control: &QueryControl,
    ) -> Result<QueryResult<QuerySession>, QueryError>;
    /// Resolves a session to one owner, rejecting ambiguity for an All scope.
    ///
    /// # Errors
    /// Propagates invalid scope, storage, resource, and cancellation failures.
    fn resolve_session(
        &self,
        scope: QueryScope,
        session: &str,
        control: &QueryControl,
    ) -> Result<Option<(QueryScope, String)>, QueryError>;
    /// Lists runs within a resolved owner-qualified session.
    ///
    /// # Errors
    /// Propagates invalid scope, storage, resource, and cancellation failures.
    fn runs(
        &self,
        scope: QueryScope,
        session: &str,
        window: QueryWindow,
        page: QueryPage,
        control: &QueryControl,
    ) -> Result<QueryResult<QueryRun>, QueryError>;
    /// Lists observations within one run in stable chronological order.
    ///
    /// # Errors
    /// Propagates invalid scope, storage, resource, and cancellation failures.
    fn observations(
        &self,
        scope: QueryScope,
        session: &str,
        run: &str,
        window: QueryWindow,
        page: QueryPage,
        control: &QueryControl,
    ) -> Result<QueryResult<QueryObservation>, QueryError>;
}

/// Security read port shared by observability and future sec query services.
pub trait SecurityQueries: Send + Sync {
    /// Counts the exact owner/session/run, preserving uncorrelated security events.
    ///
    /// # Errors
    /// Propagates invalid scope, storage, resource, and cancellation failures.
    fn counts(
        &self,
        scope: QueryScope,
        session: &str,
        run: Option<&str>,
        window: QueryWindow,
        control: &QueryControl,
    ) -> Result<QuerySecurityCounts, QueryError>;
    /// Retrieves scoped candidates with V1 truncation and best-effort storage reads.
    ///
    /// The `SQLite` adapter reads at most 1000 rows, skips malformed candidates, and
    /// returns no correlations on storage failure so observations remain available.
    ///
    /// # Errors
    /// Propagates invalid scope and cancellation failures.
    fn candidates(
        &self,
        scope: QueryScope,
        record: &QueryObservation,
        control: &QueryControl,
    ) -> Result<Vec<CorrelationCandidate>, QueryError>;
}

/// Query use cases composed over authorized read ports, without `SQLite` dependencies.
pub struct ObservabilityQueryService {
    observations: Box<dyn ObservabilityQueries>,
    security: Box<dyn SecurityQueries>,
}

impl ObservabilityQueryService {
    /// Composes the two system stores using the same owner contract.
    pub fn new(
        observations: impl ObservabilityQueries + 'static,
        security: impl SecurityQueries + 'static,
    ) -> Self {
        Self {
            observations: Box::new(observations),
            security: Box::new(security),
        }
    }

    /// Lists sessions and their security aggregates, qualified by persisted owner.
    ///
    /// # Errors
    /// Propagates storage, cancellation, and resource failures; never returns partial success.
    pub fn sessions(
        &self,
        scope: QueryScope,
        session: Option<&str>,
        window: QueryWindow,
        page: QueryPage,
        control: &QueryControl,
    ) -> Result<Value, QueryError> {
        control.check()?;
        let result = self
            .observations
            .sessions(scope, session, window, page, control)?;
        let mut items = Vec::new();
        let mut bytes = 0;
        for session in result.items {
            let counts = self.security.counts(
                QueryScope::for_owner(session.uid),
                &session.session_id,
                None,
                window,
                control,
            )?;
            let mut item = serde_json::to_value(&session).map_err(|_| QueryError::Internal)?;
            if let Some(id) = session.qualified_id {
                item["session_id"] = json!(id);
            }
            item["security_event_count"] = json!(counts.total);
            item["security_by_category_result"] = json!(counts.by_category_result);
            push_bounded(&mut items, item, &mut bytes)?;
        }
        control.check()?;
        Ok(paged(items, result.total, page))
    }

    /// Lists runs without merging owners that reused the same session label.
    ///
    /// # Errors
    /// Rejects ambiguous sessions and propagates query failures.
    pub fn runs(
        &self,
        scope: QueryScope,
        session: &str,
        window: QueryWindow,
        page: QueryPage,
        control: &QueryControl,
    ) -> Result<Value, QueryError> {
        let resolved = self.observations.resolve_session(scope, session, control)?;
        let result = match &resolved {
            Some((scope, raw_session)) => {
                self.observations
                    .runs(*scope, raw_session, window, page, control)?
            }
            None => QueryResult {
                items: Vec::new(),
                total: 0,
            },
        };
        let (scope, raw_session) = resolved.unwrap_or((QueryScope::Unknown, session.to_owned()));
        let mut items = Vec::new();
        let mut bytes = 0;
        for run in result.items {
            let counts = self.security.counts(
                QueryScope::for_owner(run.uid),
                &raw_session,
                Some(&run.run_id),
                window,
                control,
            )?;
            let mut item = serde_json::to_value(run).map_err(|_| QueryError::Internal)?;
            item["security_event_count"] = json!(counts.total);
            push_bounded(&mut items, item, &mut bytes)?;
        }
        let mut value = paged(items, result.total, page);
        value["session_id"] = json!(session);
        value["uid"] = match scope {
            QueryScope::Own(uid) => json!(uid),
            _ => Value::Null,
        };
        control.check()?;
        Ok(value)
    }

    /// Produces an observation page and its same-owner security correlations.
    ///
    /// # Errors
    /// Rejects ambiguity and propagates observation, response-budget and cancellation failures.
    #[allow(clippy::too_many_arguments)] // Wire contract has two identifiers plus bounded query options.
    pub fn timeline(
        &self,
        scope: QueryScope,
        session: &str,
        run: &str,
        window: QueryWindow,
        page: QueryPage,
        include_security: bool,
        control: &QueryControl,
    ) -> Result<Value, QueryError> {
        let resolved = self.observations.resolve_session(scope, session, control)?;
        let result = match &resolved {
            Some((scope, raw_session)) => {
                self.observations
                    .observations(*scope, raw_session, run, window, page, control)?
            }
            None => QueryResult {
                items: Vec::new(),
                total: 0,
            },
        };
        let (scope, _) = resolved.unwrap_or((QueryScope::Unknown, session.to_owned()));
        let observation_count = result.items.len();
        let mut items = Vec::new();
        let mut bytes = 0;
        for record in result.items {
            control.check()?;
            let mut item = serde_json::to_value(&record).map_err(|_| QueryError::Internal)?;
            item["kind"] = json!("observability");
            if include_security && record.uid.is_some() {
                for correlated in correlation::correlate(
                    &record,
                    &self.security.candidates(
                        QueryScope::for_owner(record.uid),
                        &record,
                        control,
                    )?,
                    control,
                )? {
                    push_bounded(&mut items, correlated, &mut bytes)?;
                }
            }
            push_bounded(&mut items, item, &mut bytes)?;
        }
        items.sort_by(|a, b| {
            a["timestamp_epoch"]
                .as_f64()
                .unwrap_or_default()
                .total_cmp(&b["timestamp_epoch"].as_f64().unwrap_or_default())
                .then_with(|| a["kind"].as_str().cmp(&b["kind"].as_str()))
        });
        let mut value = paged(items, result.total, page);
        value["next_offset"] = next_offset(page, observation_count, result.total);
        value["session_id"] = json!(session);
        value["uid"] = match scope {
            QueryScope::Own(uid) => json!(uid),
            _ => Value::Null,
        };
        value["run_id"] = json!(run);
        control.check()?;
        Ok(value)
    }
}

fn paged(items: Vec<Value>, total: u64, page: QueryPage) -> Value {
    let next = next_offset(page, items.len(), total);
    let mut result =
        json!({"next_offset":next,"total":total,"limit":page.limit,"offset":page.offset});
    result["items"] = Value::Array(items);
    result
}

fn next_offset(page: QueryPage, returned: usize, total: u64) -> Value {
    let offset = u64::try_from(page.offset).unwrap_or_default();
    let next = offset.saturating_add(returned as u64);
    if returned > 0 && next < total {
        json!(next)
    } else {
        Value::Null
    }
}

fn push_bounded(items: &mut Vec<Value>, item: Value, bytes: &mut usize) -> Result<(), QueryError> {
    *bytes += serde_json::to_vec(&item)
        .map_err(|_| QueryError::Internal)?
        .len();
    if *bytes > 4 * 1024 * 1024 - 4096 {
        return Err(QueryError::ResourceExhausted);
    }
    items.push(item);
    Ok(())
}
