//! The security-event query port: scope, filters, and the read contract.
//!
//! v1 split this three ways — the python CLI read its per-user database
//! directly, the dashboard handlers read through a repository, and the scope
//! was implicit in "one user, one database". The v2 daemon serves many local
//! UIDs from one system store, so the read contract lives in one place here:
//! a [`QueryScope`] that only trusted server code constructs, the shared
//! [`EventFilters`] vocabulary, and the [`SecurityEventQueries`] port that
//! keeps daemon handlers free of storage decisions (issue #6608).
//!
//! The port returns [`Result`] on every method on purpose: a store that is
//! present but unreadable (corrupt page, I/O error, interrupted query) must
//! be distinguishable from a store that simply holds no events. v1's
//! swallowing readers stay available for their old consumers; anything
//! exposed as a new RPC goes through this port instead.

use crate::timestamp::utc_iso_to_epoch;
use crate::{SecurityEvent, SecurityEventsSummary, TimestampError};

/// The owner whose rows one server query is authorized to read.
///
/// A scope is constructed only by trusted server code from
/// kernel-authenticated peer credentials — it is never decoded from request
/// parameters, and caller-supplied identity fields never influence it. The
/// `owner_uid` request parameter is a *filter within* the authorized scope,
/// not a way to name one (root may pick any UID, a non-root caller only
/// itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryScope {
    /// Rows whose producing peer was kernel-authenticated as this UID.
    Owner(u32),
    /// Every owner's rows.
    ///
    /// Reserved for the kernel-authenticated root peer. Non-root principals —
    /// including `PolicyAdministrator` — never read through this scope: the
    /// v2 daemon assigns no cross-owner audit role (issue #6608).
    All,
}

impl QueryScope {
    /// Returns the owner UID this scope may read, or `None` for [`All`].
    #[must_use]
    pub const fn owner_uid(self) -> Option<u32> {
        match self {
            Self::Owner(uid) => Some(uid),
            Self::All => None,
        }
    }
}

/// Optional security-event filters, all combined with `AND`.
///
/// Time bounds are stored as epochs because that is what the indexed column
/// holds; use [`EventFilters::since`] / [`EventFilters::until`] to supply the
/// ISO strings the v1 API takes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventFilters {
    /// Exact `event_type`.
    pub event_type: Option<String>,
    /// Exact `category`.
    pub category: Option<String>,
    /// Exact `result`.
    pub result: Option<String>,
    /// Exact `trace_id`.
    pub trace_id: Option<String>,
    /// Exact `session_id`.
    pub session_id: Option<String>,
    /// Exact `run_id`.
    pub run_id: Option<String>,
    /// Exact `call_id`.
    pub call_id: Option<String>,
    /// Exact `tool_call_id`.
    pub tool_call_id: Option<String>,
    /// Exact `verdict`.
    pub verdict: Option<String>,
    /// Inclusive lower bound on `timestamp_epoch`.
    pub since_epoch: Option<f64>,
    /// Exclusive upper bound on `timestamp_epoch`.
    pub until_epoch: Option<f64>,
}

impl EventFilters {
    /// Sets the inclusive lower bound from a UTC ISO timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`TimestampError`] when `value` is not ISO-8601, matching v1,
    /// where `utc_iso_to_epoch` raises out of `query()`.
    pub fn since(mut self, value: &str) -> Result<Self, TimestampError> {
        self.since_epoch = Some(utc_iso_to_epoch(value, "since")?);
        Ok(self)
    }

    /// Sets the exclusive upper bound from a UTC ISO timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`TimestampError`] when `value` is not ISO-8601.
    pub fn until(mut self, value: &str) -> Result<Self, TimestampError> {
        self.until_epoch = Some(utc_iso_to_epoch(value, "until")?);
        Ok(self)
    }
}

/// Counts grouped by one column; `None` is the SQL `NULL` bucket.
pub type GroupCounts = Vec<(Option<String>, u64)>;

/// Group fields accepted by `count_by`.
///
/// Listed in v1's `_COUNT_BY_COLUMNS` order.
pub const VALID_GROUP_FIELDS: &[&str] = &[
    "category",
    "event_type",
    "result",
    "trace_id",
    "session_id",
    "run_id",
    "call_id",
    "tool_call_id",
    "verdict",
];

/// Why a security-event query could not be answered.
///
/// The split is contract: [`QueryError::Unavailable`] means the store is
/// present but cannot be read (corrupt, I/O error, interrupted); the caller
/// must not mistake it for "no events". [`QueryError::InvalidGroupField`] is
/// the caller's fault and maps to `invalid_argument` at the protocol edge.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    /// The store cannot be read right now.
    #[error("security event store is unavailable: {0}")]
    Unavailable(String),
    /// The group field is outside the v1 allowlist.
    #[error("invalid group field: {0}")]
    InvalidGroupField(String),
}

/// Read-only security-event queries one daemon can serve, scoped per owner.
///
/// The port keeps daemon handlers free of storage decisions; the daemon
/// composition root binds it to the same database the writers use.
pub trait SecurityEventQueries: Send + Sync {
    /// Returns the aggregates and newest rows of one scope.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::Unavailable`] when the store cannot be read.
    fn summary(
        &self,
        filters: &EventFilters,
        scope: QueryScope,
        latest_limit: u32,
    ) -> Result<SecurityEventsSummary, QueryError>;
    /// Returns one page of one scope's rows, newest first.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::Unavailable`] when the store cannot be read.
    fn list(
        &self,
        filters: &EventFilters,
        scope: QueryScope,
        limit: u32,
        offset: i64,
    ) -> Result<Vec<SecurityEvent>, QueryError>;
    /// Returns the number of remaining rows of one scope.
    ///
    /// A non-zero `offset` counts what remains *after* skipping that many
    /// rows, which is how v1 keeps pagination counters honest.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::Unavailable`] when the store cannot be read.
    fn count(
        &self,
        filters: &EventFilters,
        scope: QueryScope,
        offset: i64,
    ) -> Result<u64, QueryError>;
    /// Returns grouped counts of one scope.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidGroupField`] when `group_field` is
    /// outside the v1 allowlist and [`QueryError::Unavailable`] when the
    /// store cannot be read.
    fn count_by(
        &self,
        group_field: &str,
        filters: &EventFilters,
        scope: QueryScope,
        offset: i64,
    ) -> Result<GroupCounts, QueryError>;
    /// Returns one row of one scope by id, or `None`.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::Unavailable`] when the store cannot be read.
    fn get(&self, event_id: &str, scope: QueryScope) -> Result<Option<SecurityEvent>, QueryError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_owner_scope_carries_its_uid() {
        assert_eq!(QueryScope::Owner(1000).owner_uid(), Some(1000));
        assert_eq!(QueryScope::All.owner_uid(), None);
    }

    #[test]
    fn iso_bounds_set_epochs() {
        let filters = EventFilters::default()
            .since("2026-01-01T00:00:00+00:00")
            .expect("valid since")
            .until("2026-01-02T00:00:00+00:00")
            .expect("valid until");
        assert_eq!(filters.since_epoch, Some(1_767_225_600.0));
        assert_eq!(filters.until_epoch, Some(1_767_312_000.0));
    }

    #[test]
    fn a_non_iso_bound_is_rejected() {
        assert!(EventFilters::default().since("not a timestamp").is_err());
    }
}
