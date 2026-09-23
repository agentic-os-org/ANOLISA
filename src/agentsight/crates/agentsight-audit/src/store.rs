//! SQLite persistence for immutable normalized audit events.

mod containment;
mod retention;

#[cfg(target_os = "linux")]
pub use containment::DueContainmentAction;
pub use containment::{ContainmentActivationResult, ContainmentClaimResult};
pub use retention::{AuditMaintenancePolicy, AuditMaintenanceReport};

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use agentsight_enforcement_protocol::{DestinationClass, SecurityEvent, SecurityEventKind};
use agentsight_sqlite_lifecycle::{ConnectionOptions, open_connection};
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;
use uuid::Uuid;

use super::{
    AuditCountBy, AuditEventFilter, AuditEventPage, AuditSession, AuditSessionPage, AuditSummary,
    RiskCase, RiskCaseDetail, RiskCaseStatus, RiskCaseSummary, RiskSeverity,
};
const EVENT_QUERY: &str = "SELECT event_json FROM security_events
     WHERE (?1 IS NULL OR occurred_at_ns >= ?1)
       AND (?2 IS NULL OR occurred_at_ns <= ?2)
       AND (?3 IS NULL OR event_type = ?3)
       AND (?4 IS NULL OR result = ?4)
       AND (?5 IS NULL OR policy_id = ?5)
       AND (?6 IS NULL OR agent_id = ?6)
       AND (?7 IS NULL OR session_id = ?7)
       AND (?8 IS NULL OR binding_id = ?8)
     ORDER BY occurred_at_ns DESC, event_id ASC
     LIMIT ?9 OFFSET ?10";

/// Typed local-security persistence failures.
#[derive(Debug, Error)]
pub enum AuditError {
    /// Opening or maintaining the database through the shared lifecycle failed.
    #[error(transparent)]
    Lifecycle(#[from] agentsight_sqlite_lifecycle::LifecycleError),
    /// Opening a private database through the host helper failed.
    #[error("failed to open security database: {0}")]
    Open(String),
    /// SQLite schema, write, or query failed.
    #[error("security database failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Event JSON encoding or decoding failed.
    #[error("security event serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    /// A caller supplied an unsupported grouping or query field.
    #[error("invalid security filter: {0}")]
    InvalidFilter(String),
    /// A requested risk case does not exist.
    #[error("risk case {0} does not exist")]
    MissingCase(Uuid),
    /// A persisted timestamp cannot fit SQLite's signed integer representation.
    #[error("timestamp {0} exceeds SQLite integer range")]
    TimestampOutOfRange(u64),
    /// Another thread poisoned the database connection lock.
    #[error("security database connection lock is poisoned")]
    Poisoned,
    /// Stored identifiers or enum values violate the local schema contract.
    #[error("invalid stored security data: {0}")]
    InvalidData(String),
    /// One immutable policy identity was reused with different contents.
    #[error("policy {policy_id}@{revision} conflicts with the stored immutable revision")]
    PolicyRevisionConflict {
        /// Stable product policy identifier.
        policy_id: String,
        /// Conflicting immutable revision.
        revision: u64,
    },
}

/// Unified interface used by coordinators and API handlers.
pub trait AuditEventStore {
    /// Inserts one immutable event, returning false for a duplicate ID.
    fn insert_event(&self, event: &SecurityEvent) -> Result<bool, AuditError>;

    /// Loads one event by its stable ID.
    fn event(&self, event_id: Uuid) -> Result<Option<SecurityEvent>, AuditError>;

    /// Lists a bounded newest-first event page.
    fn list_events(&self, filter: &AuditEventFilter) -> Result<AuditEventPage, AuditError>;
}

/// AgentSight-owned local SQLite security store.
pub struct AuditStore {
    conn: Mutex<Connection>,
}

impl AuditStore {
    /// Opens a security store at `path` and applies additive schema creation.
    ///
    /// # Errors
    ///
    /// Returns a typed open or schema error.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AuditError> {
        let conn = open_connection(path.as_ref(), ConnectionOptions::default())?;
        Self::from_connection(conn)
    }

    /// Registers immutable policy contents idempotently.
    ///
    /// # Errors
    ///
    /// Returns [`AuditError::PolicyRevisionConflict`] when the same policy
    /// identity already names different serialized contents.
    pub fn register_policy_revision(
        &self,
        policy_id: &str,
        revision: u64,
        policy_json: &str,
        created_at_ns: u64,
    ) -> Result<(), AuditError> {
        let mut conn = self.connection()?;
        let transaction = conn.transaction()?;
        let existing = transaction
            .query_row(
                "SELECT policy_json FROM policy_revisions
                 WHERE policy_id = ?1 AND revision = ?2",
                params![policy_id, sqlite_time(revision)?],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing != policy_json {
                return Err(AuditError::PolicyRevisionConflict {
                    policy_id: policy_id.into(),
                    revision,
                });
            }
            return Ok(());
        }
        transaction.execute(
            "INSERT INTO policy_revisions (policy_id, revision, policy_json, created_at_ns)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                policy_id,
                sqlite_time(revision)?,
                policy_json,
                sqlite_time(created_at_ns)?,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Opens an isolated in-memory security store for tests and no-op modes.
    ///
    /// # Errors
    ///
    /// Returns a SQLite error when the connection or schema cannot be created.
    pub fn open_in_memory() -> Result<Self, AuditError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    /// Inserts one immutable event, returning false for a duplicate ID.
    ///
    /// # Errors
    ///
    /// Returns a typed database, serialization, timestamp, or lock error.
    pub fn insert_event(&self, event: &SecurityEvent) -> Result<bool, AuditError> {
        <Self as AuditEventStore>::insert_event(self, event)
    }

    /// Loads one event by its stable ID.
    ///
    /// # Errors
    ///
    /// Returns a typed database, serialization, or lock error.
    pub fn event(&self, event_id: Uuid) -> Result<Option<SecurityEvent>, AuditError> {
        <Self as AuditEventStore>::event(self, event_id)
    }

    /// Lists a bounded newest-first event page.
    ///
    /// # Errors
    ///
    /// Returns a typed database, serialization, timestamp, or lock error.
    pub fn list_events(&self, filter: &AuditEventFilter) -> Result<AuditEventPage, AuditError> {
        <Self as AuditEventStore>::list_events(self, filter)
    }

    /// Confirms that normalized evidence uniquely maps a live process to an agent identity.
    ///
    /// # Errors
    ///
    /// Returns a typed database, timestamp, or lock error.
    pub fn process_identity_matches(
        &self,
        pid: i32,
        process_start_time: u64,
        agent_id: &str,
        session_id: Option<&str>,
    ) -> Result<bool, AuditError> {
        let (agent_count, matching_rows) = self.connection()?.query_row(
            "SELECT COUNT(DISTINCT agent_id),
                    COALESCE(SUM(CASE WHEN agent_id = ?3 AND (?4 IS NULL OR session_id = ?4)
                                      THEN 1 ELSE 0 END), 0)
             FROM security_events
             WHERE pid = ?1 AND process_start_time = ?2",
            params![pid, sqlite_time(process_start_time)?, agent_id, session_id,],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?;
        Ok(agent_count == 1 && matching_rows > 0)
    }

    /// Builds an audit store over a host-opened SQLite connection.
    ///
    /// Hosts use this boundary to apply their own file-permission policy before
    /// the audit crate initializes its additive schema.
    ///
    /// # Errors
    ///
    /// Returns a typed schema or SQLite configuration error.
    pub fn from_connection(conn: Connection) -> Result<Self, AuditError> {
        conn.busy_timeout(std::time::Duration::from_millis(500))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS security_events (
                event_id TEXT PRIMARY KEY,
                event_type TEXT NOT NULL,
                occurred_at_ns INTEGER NOT NULL,
                observed_at_ns INTEGER NOT NULL,
                agent_id TEXT NOT NULL,
                agent_name TEXT,
                session_id TEXT,
                pid INTEGER NOT NULL,
                process_start_time INTEGER NOT NULL,
                binding_id TEXT NOT NULL,
                policy_id TEXT,
                policy_revision INTEGER,
                result TEXT,
                destination_class TEXT,
                event_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_security_events_time
                ON security_events(occurred_at_ns DESC);
            CREATE INDEX IF NOT EXISTS idx_security_events_session_time
                ON security_events(session_id, occurred_at_ns DESC);
            CREATE TABLE IF NOT EXISTS risk_cases (
                case_id TEXT PRIMARY KEY,
                correlation_key TEXT NOT NULL UNIQUE,
                policy_id TEXT NOT NULL,
                policy_revision INTEGER NOT NULL,
                agent_id TEXT NOT NULL,
                session_id TEXT,
                severity TEXT NOT NULL,
                risk_score INTEGER NOT NULL,
                status TEXT NOT NULL,
                blocked INTEGER NOT NULL,
                opened_at_ns INTEGER NOT NULL,
                updated_at_ns INTEGER NOT NULL,
                summary TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS risk_evidence_links (
                case_id TEXT NOT NULL,
                event_id TEXT NOT NULL,
                position INTEGER NOT NULL,
                PRIMARY KEY(case_id, event_id)
            );
            CREATE TABLE IF NOT EXISTS policy_revisions (
                policy_id TEXT NOT NULL,
                revision INTEGER NOT NULL,
                policy_json TEXT NOT NULL,
                created_at_ns INTEGER NOT NULL,
                PRIMARY KEY(policy_id, revision)
            );
            CREATE TABLE IF NOT EXISTS containment_actions (
                action_id TEXT PRIMARY KEY,
                case_id TEXT NOT NULL,
                binding_id TEXT NOT NULL UNIQUE,
                source_binding_id TEXT,
                agent_id TEXT NOT NULL,
                root_pid INTEGER NOT NULL,
                process_start_time INTEGER NOT NULL,
                source_path TEXT NOT NULL,
                duration_secs INTEGER,
                expires_at_ns INTEGER,
                lifecycle_state TEXT NOT NULL,
                blocked_at_ns INTEGER,
                requested_by TEXT NOT NULL,
                failure_stage TEXT,
                failure_reason TEXT,
                attempt_count INTEGER NOT NULL,
                next_retry_at_ns INTEGER,
                created_at_ns INTEGER NOT NULL,
                updated_at_ns INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_containment_case_time
                ON containment_actions(case_id, created_at_ns DESC);
            CREATE INDEX IF NOT EXISTS idx_containment_due
                ON containment_actions(lifecycle_state, expires_at_ns, next_retry_at_ns);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_containment_live_case
                ON containment_actions(case_id)
                WHERE lifecycle_state IN ('pending', 'active', 'expiring');",
        )?;
        ensure_containment_source_binding_column(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Groups all events by one approved indexed column.
    ///
    /// # Errors
    ///
    /// Returns [`AuditError::InvalidFilter`] for unknown fields.
    pub fn count_by(&self, field: &str) -> Result<Vec<AuditCountBy>, AuditError> {
        let sql = match field {
            "event_type" => {
                "SELECT COALESCE(event_type, 'unknown'), COUNT(*) FROM security_events GROUP BY event_type ORDER BY COUNT(*) DESC"
            }
            "result" => {
                "SELECT COALESCE(result, 'unknown'), COUNT(*) FROM security_events GROUP BY result ORDER BY COUNT(*) DESC"
            }
            "policy_id" => {
                "SELECT COALESCE(policy_id, 'unknown'), COUNT(*) FROM security_events GROUP BY policy_id ORDER BY COUNT(*) DESC"
            }
            "destination_class" => {
                "SELECT COALESCE(destination_class, 'unknown'), COUNT(*) FROM security_events GROUP BY destination_class ORDER BY COUNT(*) DESC"
            }
            _ => return Err(AuditError::InvalidFilter(field.into())),
        };
        let conn = self.connection()?;
        let mut statement = conn.prepare(sql)?;
        let rows = statement.query_map([], |row| {
            Ok(AuditCountBy {
                key: row.get(0)?,
                count: row.get(1)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Returns aggregate totals without deserializing event payloads.
    ///
    /// # Errors
    ///
    /// Returns a typed database error when the summary query fails.
    pub fn summary(&self) -> Result<AuditSummary, AuditError> {
        self.summary_filtered(&AuditEventFilter::default())
    }

    /// Returns aggregate totals over the complete filtered result set.
    ///
    /// Pagination fields are ignored because aggregates describe every match.
    ///
    /// # Errors
    ///
    /// Returns a typed database, timestamp, or lock error.
    pub fn summary_filtered(&self, filter: &AuditEventFilter) -> Result<AuditSummary, AuditError> {
        let binding_id = filter.binding_id.map(|value| value.to_string());
        self.connection()?
            .query_row(
                "SELECT COUNT(*),
                        COALESCE(SUM(CASE WHEN event_type = 'policy_decision'
                                          AND result = 'blocked'
                                     THEN 1 ELSE 0 END), 0),
                        COALESCE(SUM(CASE WHEN event_type = 'enforcement_state'
                                          AND event_json LIKE '%\"evidence_loss\"%'
                                     THEN 1 ELSE 0 END), 0)
                 FROM security_events
                 WHERE (?1 IS NULL OR occurred_at_ns >= ?1)
                   AND (?2 IS NULL OR occurred_at_ns <= ?2)
                   AND (?3 IS NULL OR event_type = ?3)
                   AND (?4 IS NULL OR result = ?4)
                   AND (?5 IS NULL OR policy_id = ?5)
                   AND (?6 IS NULL OR agent_id = ?6)
                   AND (?7 IS NULL OR session_id = ?7)
                   AND (?8 IS NULL OR binding_id = ?8)",
                params![
                    filter.start_ns.map(sqlite_time).transpose()?,
                    filter.end_ns.map(sqlite_time).transpose()?,
                    filter.event_type,
                    filter.result,
                    filter.policy_id,
                    filter.agent_id,
                    filter.session_id,
                    binding_id,
                ],
                |row| {
                    Ok(AuditSummary {
                        total_events: row.get(0)?,
                        blocked_events: row.get(1)?,
                        evidence_loss_events: row.get(2)?,
                    })
                },
            )
            .map_err(Into::into)
    }

    /// Groups the complete filtered event set into a bounded session page.
    ///
    /// # Errors
    ///
    /// Returns a typed database, timestamp, stored-data, or lock error.
    pub fn list_sessions(&self, filter: &AuditEventFilter) -> Result<AuditSessionPage, AuditError> {
        const FILTER: &str = "session_id IS NOT NULL
            AND (?1 IS NULL OR occurred_at_ns >= ?1)
            AND (?2 IS NULL OR occurred_at_ns <= ?2)
            AND (?3 IS NULL OR event_type = ?3)
            AND (?4 IS NULL OR result = ?4)
            AND (?5 IS NULL OR policy_id = ?5)
            AND (?6 IS NULL OR agent_id = ?6)
            AND (?7 IS NULL OR session_id = ?7)
            AND (?8 IS NULL OR binding_id = ?8)";
        let limit = filter.limit.clamp(1, 1_000);
        let offset = filter.offset.max(0);
        let binding_id = filter.binding_id.map(|value| value.to_string());
        let start_ns = filter.start_ns.map(sqlite_time).transpose()?;
        let end_ns = filter.end_ns.map(sqlite_time).transpose()?;
        let conn = self.connection()?;
        let total_sql = format!(
            "SELECT COUNT(*) FROM (
                SELECT session_id FROM security_events WHERE {FILTER} GROUP BY session_id
             )"
        );
        let total = conn.query_row(
            &total_sql,
            params![
                start_ns,
                end_ns,
                filter.event_type,
                filter.result,
                filter.policy_id,
                filter.agent_id,
                filter.session_id,
                binding_id,
            ],
            |row| row.get::<_, i64>(0),
        )?;
        let page_sql = format!(
            "SELECT session_id, MIN(occurred_at_ns), MAX(occurred_at_ns), COUNT(*)
             FROM security_events
             WHERE {FILTER}
             GROUP BY session_id
             ORDER BY MAX(occurred_at_ns) DESC, session_id ASC
             LIMIT ?9 OFFSET ?10"
        );
        let mut statement = conn.prepare(&page_sql)?;
        let rows = statement.query_map(
            params![
                start_ns,
                end_ns,
                filter.event_type,
                filter.result,
                filter.policy_id,
                filter.agent_id,
                filter.session_id,
                binding_id,
                limit as i64,
                offset,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )?;
        let mut items = Vec::new();
        for row in rows {
            let (session_id, first_seen_ns, last_seen_ns, security_event_count) = row?;
            items.push(AuditSession {
                session_id,
                first_seen_ns: unsigned(first_seen_ns, "first_seen_ns")?,
                last_seen_ns: unsigned(last_seen_ns, "last_seen_ns")?,
                security_event_count: unsigned(security_event_count, "security_event_count")?,
            });
        }
        Ok(AuditSessionPage {
            items,
            total: unsigned(total, "session_total")?,
            limit,
            offset,
        })
    }

    /// Creates or updates one idempotent risk case and its evidence links.
    ///
    /// # Errors
    ///
    /// Returns a typed database, timestamp, stored-data, or lock error.
    pub fn upsert_case(&self, case: &RiskCase, evidence_ids: &[Uuid]) -> Result<Uuid, AuditError> {
        let mut conn = self.connection()?;
        let transaction = conn.transaction()?;
        transaction.execute(
            "INSERT INTO risk_cases (
                case_id, correlation_key, policy_id, policy_revision, agent_id, session_id,
                severity, risk_score, status, blocked, opened_at_ns, updated_at_ns, summary
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(correlation_key) DO UPDATE SET
                severity = CASE
                    WHEN CASE excluded.severity
                        WHEN 'critical' THEN 4 WHEN 'high' THEN 3
                        WHEN 'medium' THEN 2 ELSE 1 END
                      > CASE risk_cases.severity
                        WHEN 'critical' THEN 4 WHEN 'high' THEN 3
                        WHEN 'medium' THEN 2 ELSE 1 END
                    THEN excluded.severity ELSE risk_cases.severity END,
                risk_score = MAX(risk_cases.risk_score, excluded.risk_score),
                blocked = MAX(risk_cases.blocked, excluded.blocked),
                updated_at_ns = MAX(risk_cases.updated_at_ns, excluded.updated_at_ns),
                summary = CASE
                    WHEN excluded.updated_at_ns >= risk_cases.updated_at_ns
                    THEN excluded.summary ELSE risk_cases.summary END",
            params![
                case.case_id.to_string(),
                case.correlation_key,
                case.policy_id,
                sqlite_time(case.policy_revision)?,
                case.agent_id,
                case.session_id,
                risk_severity(case.severity),
                i64::from(case.risk_score),
                risk_status(case.status),
                i64::from(case.blocked),
                sqlite_time(case.opened_at_ns)?,
                sqlite_time(case.updated_at_ns)?,
                case.summary,
            ],
        )?;
        let stored_case_id: String = transaction.query_row(
            "SELECT case_id FROM risk_cases WHERE correlation_key = ?1",
            [&case.correlation_key],
            |row| row.get(0),
        )?;
        let next_position: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0)
             FROM risk_evidence_links WHERE case_id = ?1",
            [&stored_case_id],
            |row| row.get(0),
        )?;
        for (position, event_id) in evidence_ids.iter().enumerate() {
            transaction.execute(
                "INSERT OR IGNORE INTO risk_evidence_links (case_id, event_id, position)
                 VALUES (?1, ?2, ?3)",
                params![
                    stored_case_id,
                    event_id.to_string(),
                    next_position + position as i64
                ],
            )?;
        }
        transaction.commit()?;
        parse_uuid(&stored_case_id)
    }

    /// Lists newest risk cases with bounded pagination.
    ///
    /// # Errors
    ///
    /// Returns a typed database, stored-data, or lock error.
    pub fn list_cases(
        &self,
        limit: usize,
        offset: i64,
        agent_id: Option<&str>,
        status: Option<&str>,
        blocked: Option<bool>,
    ) -> Result<Vec<RiskCase>, AuditError> {
        let conn = self.connection()?;
        let mut conditions = Vec::new();
        let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if let Some(id) = agent_id {
            conditions.push("agent_id = ?".to_string());
            params_vec.push(Box::new(id.to_string()));
        }
        if let Some(s) = status {
            conditions.push("status = ?".to_string());
            params_vec.push(Box::new(s.to_string()));
        }
        if let Some(b) = blocked {
            conditions.push("blocked = ?".to_string());
            params_vec.push(Box::new(b as i64));
        }
        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };
        let sql = format!(
            "SELECT case_id, correlation_key, policy_id, policy_revision, agent_id, session_id,
                    severity, risk_score, status, blocked, opened_at_ns, updated_at_ns, summary
             FROM risk_cases
             {where_clause}
             ORDER BY updated_at_ns DESC, case_id ASC
             LIMIT ? OFFSET ?"
        );
        params_vec.push(Box::new(limit.clamp(1, 1_000) as i64));
        params_vec.push(Box::new(offset.max(0)));
        let mut statement = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();
        let row_mapper = |row: &rusqlite::Row<'_>| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, i64>(11)?,
                row.get::<_, String>(12)?,
            ))
        };
        let rows = statement.query_map(param_refs.as_slice(), row_mapper)?;
        rows.map(|row| row.map_err(Into::into).and_then(risk_case_from_row))
            .collect()
    }

    /// Returns the complete number of correlated risk cases.
    ///
    /// # Errors
    ///
    /// Returns a typed database, stored-data, or lock error.
    pub fn case_count(
        &self,
        agent_id: Option<&str>,
        status: Option<&str>,
        blocked: Option<bool>,
    ) -> Result<u64, AuditError> {
        let conn = self.connection()?;
        let mut conditions = Vec::new();
        let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if let Some(id) = agent_id {
            conditions.push("agent_id = ?".to_string());
            params_vec.push(Box::new(id.to_string()));
        }
        if let Some(s) = status {
            conditions.push("status = ?".to_string());
            params_vec.push(Box::new(s.to_string()));
        }
        if let Some(b) = blocked {
            conditions.push("blocked = ?".to_string());
            params_vec.push(Box::new(b as i64));
        }
        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };
        let sql = format!("SELECT COUNT(*) FROM risk_cases {where_clause}");
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();
        let count: i64 = conn.query_row(&sql, param_refs.as_slice(), |row| row.get(0))?;
        unsigned(count, "case_total")
    }

    /// Returns complete risk-case totals independent of list pagination.
    ///
    /// # Errors
    ///
    /// Returns a typed database, stored-data, or lock error.
    pub fn case_summary(&self) -> Result<RiskCaseSummary, AuditError> {
        let (total, open, blocked): (i64, i64, i64) = self.connection()?.query_row(
            "SELECT COUNT(*),
                        COALESCE(SUM(CASE WHEN status = 'open' THEN 1 ELSE 0 END), 0),
                        COALESCE(SUM(CASE WHEN blocked != 0 THEN 1 ELSE 0 END), 0)
                 FROM risk_cases",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        Ok(RiskCaseSummary {
            total: unsigned(total, "case_total")?,
            open: unsigned(open, "case_open")?,
            blocked: unsigned(blocked, "case_blocked")?,
        })
    }

    /// Returns a mapping from `(agent_id, policy_id, policy_revision)` to the most recent case ID.
    ///
    /// # Errors
    ///
    /// Returns a typed database, stored-data, or lock error.
    pub fn case_index_by_agent_policy(
        &self,
    ) -> Result<HashMap<(String, String, String), Uuid>, AuditError> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT agent_id, policy_id, CAST(policy_revision AS TEXT), case_id \
             FROM risk_cases ORDER BY updated_at_ns DESC",
        )?;
        let mut index = HashMap::new();
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (agent_id, policy_id, policy_revision, case_id_str) = row?;
            let case_id = uuid::Uuid::parse_str(&case_id_str)
                .map_err(|_| AuditError::InvalidData(format!("bad case_id: {case_id_str}")))?;
            index
                .entry((agent_id, policy_id, policy_revision))
                .or_insert(case_id);
        }
        Ok(index)
    }

    /// Returns a mapping from `event_id` to the `case_id` that contains it,
    /// using the precise `risk_evidence_links` table. Only events that are
    /// actually linked to a case appear in the result.
    ///
    /// # Errors
    ///
    /// Returns a typed database, stored-data, or lock error.
    pub fn case_ids_for_events(
        &self,
        event_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, Uuid>, AuditError> {
        if event_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let conn = self.connection()?;
        // SQLite limit on host parameters is 999 by default; batch if needed.
        let mut result = HashMap::with_capacity(event_ids.len());
        for chunk in event_ids.chunks(500) {
            let placeholders: String = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT event_id, case_id FROM risk_evidence_links WHERE event_id IN ({placeholders})"
            );
            let mut stmt = conn.prepare(&sql)?;
            let params: Vec<String> = chunk.iter().map(|id| id.to_string()).collect();
            let param_refs: Vec<&dyn rusqlite::types::ToSql> = params
                .iter()
                .map(|s| s as &dyn rusqlite::types::ToSql)
                .collect();
            let rows = stmt.query_map(param_refs.as_slice(), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (eid_str, cid_str) = row?;
                let eid = Uuid::parse_str(&eid_str)
                    .map_err(|_| AuditError::InvalidData(format!("bad event_id: {eid_str}")))?;
                let cid = Uuid::parse_str(&cid_str)
                    .map_err(|_| AuditError::InvalidData(format!("bad case_id: {cid_str}")))?;
                result.insert(eid, cid);
            }
        }
        Ok(result)
    }

    /// Loads one risk case with immutable evidence in correlation order.
    ///
    /// # Errors
    ///
    /// Returns [`AuditError::MissingCase`] when absent, or a typed
    /// database, serialization, stored-data, or lock error.
    pub fn case_detail(&self, case_id: Uuid) -> Result<RiskCaseDetail, AuditError> {
        let conn = self.connection()?;
        let row = conn
            .query_row(
                "SELECT case_id, correlation_key, policy_id, policy_revision, agent_id,
                        session_id, severity, risk_score, status, blocked, opened_at_ns,
                        updated_at_ns, summary
                 FROM risk_cases WHERE case_id = ?1",
                [case_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, i64>(9)?,
                        row.get::<_, i64>(10)?,
                        row.get::<_, i64>(11)?,
                        row.get::<_, String>(12)?,
                    ))
                },
            )
            .optional()?
            .ok_or(AuditError::MissingCase(case_id))?;
        let case = risk_case_from_row(row)?;
        let linked_evidence_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM risk_evidence_links WHERE case_id = ?1",
            [case_id.to_string()],
            |row| row.get(0),
        )?;
        let mut statement = conn.prepare(
            "SELECT events.event_json
             FROM risk_evidence_links AS links
             JOIN security_events AS events ON events.event_id = links.event_id
             WHERE links.case_id = ?1
             ORDER BY links.position ASC",
        )?;
        let rows = statement.query_map([case_id.to_string()], |row| row.get::<_, String>(0))?;
        let mut evidence = Vec::new();
        for row in rows {
            evidence.push(serde_json::from_str(&row?)?);
        }
        if linked_evidence_count != evidence.len() as i64 {
            return Err(AuditError::InvalidData(format!(
                "risk case {case_id} has {linked_evidence_count} evidence links but only {} stored events",
                evidence.len()
            )));
        }
        Ok(RiskCaseDetail { case, evidence })
    }

    /// Updates the human review state without changing immutable policy provenance.
    ///
    /// # Errors
    ///
    /// Returns [`AuditError::MissingCase`] when absent, or a typed
    /// database, timestamp, stored-data, or lock error.
    pub fn review_case(
        &self,
        case_id: Uuid,
        status: RiskCaseStatus,
        updated_at_ns: u64,
    ) -> Result<RiskCase, AuditError> {
        let changed = self.connection()?.execute(
            "UPDATE risk_cases SET status = ?1, updated_at_ns = ?2 WHERE case_id = ?3",
            params![
                risk_status(status),
                sqlite_time(updated_at_ns)?,
                case_id.to_string(),
            ],
        )?;
        if changed == 0 {
            return Err(AuditError::MissingCase(case_id));
        }
        Ok(self.case_detail(case_id)?.case)
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, AuditError> {
        self.conn.lock().map_err(|_| AuditError::Poisoned)
    }
}

fn ensure_containment_source_binding_column(connection: &Connection) -> Result<(), AuditError> {
    let exists = {
        let mut statement = connection.prepare("PRAGMA table_info(containment_actions)")?;
        let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
        let mut exists = false;
        for column in columns {
            exists |= column? == "source_binding_id";
        }
        exists
    };
    if exists {
        return Ok(());
    }
    connection
        .execute_batch("ALTER TABLE containment_actions ADD COLUMN source_binding_id TEXT;")?;
    Ok(())
}

impl AuditEventStore for AuditStore {
    fn insert_event(&self, event: &SecurityEvent) -> Result<bool, AuditError> {
        let metadata = EventMetadata::from_event(event);
        let event_json = serde_json::to_string(event)?;
        let changed = self.connection()?.execute(
            "INSERT OR IGNORE INTO security_events (
                event_id, event_type, occurred_at_ns, observed_at_ns, agent_id, agent_name,
                session_id, pid, process_start_time, binding_id, policy_id, policy_revision,
                result, destination_class, event_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                event.event_id.to_string(),
                metadata.event_type,
                sqlite_time(event.occurred_at_ns)?,
                sqlite_time(event.observed_at_ns)?,
                event.identity.agent_id,
                event.identity.agent_name,
                event.identity.session_id,
                event.identity.pid,
                sqlite_time(event.identity.process_start_time)?,
                event.identity.binding_id.to_string(),
                metadata.policy_id,
                metadata.policy_revision.map(sqlite_time).transpose()?,
                metadata.result,
                metadata.destination_class,
                event_json,
            ],
        )?;
        Ok(changed == 1)
    }

    fn event(&self, event_id: Uuid) -> Result<Option<SecurityEvent>, AuditError> {
        let json: Option<String> = self
            .connection()?
            .query_row(
                "SELECT event_json FROM security_events WHERE event_id = ?1",
                [event_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        json.map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(Into::into)
    }

    fn list_events(&self, filter: &AuditEventFilter) -> Result<AuditEventPage, AuditError> {
        let limit = filter.limit.clamp(1, 1_000);
        let offset = filter.offset.max(0);
        let binding_id = filter.binding_id.map(|value| value.to_string());
        let conn = self.connection()?;
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM security_events
             WHERE (?1 IS NULL OR occurred_at_ns >= ?1)
               AND (?2 IS NULL OR occurred_at_ns <= ?2)
               AND (?3 IS NULL OR event_type = ?3)
               AND (?4 IS NULL OR result = ?4)
               AND (?5 IS NULL OR policy_id = ?5)
               AND (?6 IS NULL OR agent_id = ?6)
               AND (?7 IS NULL OR session_id = ?7)
               AND (?8 IS NULL OR binding_id = ?8)",
            params![
                filter.start_ns.map(sqlite_time).transpose()?,
                filter.end_ns.map(sqlite_time).transpose()?,
                filter.event_type,
                filter.result,
                filter.policy_id,
                filter.agent_id,
                filter.session_id,
                binding_id,
            ],
            |row| row.get(0),
        )?;
        let mut statement = conn.prepare(EVENT_QUERY)?;
        let rows = statement.query_map(
            params![
                filter.start_ns.map(sqlite_time).transpose()?,
                filter.end_ns.map(sqlite_time).transpose()?,
                filter.event_type,
                filter.result,
                filter.policy_id,
                filter.agent_id,
                filter.session_id,
                binding_id,
                limit as i64,
                offset,
            ],
            |row| row.get::<_, String>(0),
        )?;
        let mut items = Vec::new();
        for row in rows {
            items.push(serde_json::from_str(&row?)?);
        }
        Ok(AuditEventPage {
            items,
            total: unsigned(total, "event_total")?,
            limit,
            offset,
        })
    }
}

struct EventMetadata<'a> {
    event_type: &'static str,
    policy_id: Option<&'a str>,
    policy_revision: Option<u64>,
    result: &'static str,
    destination_class: Option<&'static str>,
}

impl<'a> EventMetadata<'a> {
    fn from_event(event: &'a SecurityEvent) -> Self {
        match &event.kind {
            SecurityEventKind::FileAction(action) => Self {
                event_type: "file_action",
                policy_id: Some(&action.policy_id),
                policy_revision: Some(action.policy_revision),
                result: if action.succeeded {
                    "allowed"
                } else {
                    "failed"
                },
                destination_class: None,
            },
            SecurityEventKind::TaintTransition(transition) => Self {
                event_type: "taint_transition",
                policy_id: Some(&transition.policy_id),
                policy_revision: Some(transition.policy_revision),
                result: "changed",
                destination_class: None,
            },
            SecurityEventKind::NetworkAction(action) => Self {
                event_type: "network_action",
                policy_id: Some(&action.policy_id),
                policy_revision: Some(action.policy_revision),
                result: if action.succeeded {
                    "allowed"
                } else {
                    "blocked"
                },
                destination_class: Some(destination_class(action.destination_class)),
            },
            SecurityEventKind::PolicyDecision(decision) => Self {
                event_type: "policy_decision",
                policy_id: Some(&decision.policy_id),
                policy_revision: Some(decision.policy_revision),
                result: if decision.blocked {
                    "blocked"
                } else {
                    "allowed"
                },
                destination_class: None,
            },
            SecurityEventKind::EnforcementState(state) => Self {
                event_type: "enforcement_state",
                policy_id: state.policy_id.as_deref(),
                policy_revision: state.policy_revision,
                result: if state.ready { "ready" } else { "degraded" },
                destination_class: None,
            },
        }
    }
}

fn destination_class(class: DestinationClass) -> &'static str {
    match class {
        DestinationClass::Local => "local",
        DestinationClass::Private => "private",
        DestinationClass::Trusted => "trusted",
        DestinationClass::Public => "public",
        DestinationClass::Unknown => "unknown",
    }
}

fn sqlite_time(value: u64) -> Result<i64, AuditError> {
    i64::try_from(value).map_err(|_| AuditError::TimestampOutOfRange(value))
}

type RiskCaseRow = (
    String,
    String,
    String,
    i64,
    String,
    Option<String>,
    String,
    i64,
    String,
    i64,
    i64,
    i64,
    String,
);

fn risk_case_from_row(row: RiskCaseRow) -> Result<RiskCase, AuditError> {
    Ok(RiskCase {
        case_id: parse_uuid(&row.0)?,
        correlation_key: row.1,
        policy_id: row.2,
        policy_revision: unsigned(row.3, "policy_revision")?,
        agent_id: row.4,
        session_id: row.5,
        severity: parse_severity(&row.6)?,
        risk_score: u8::try_from(row.7)
            .map_err(|_| AuditError::InvalidData("risk_score is out of range".into()))?,
        status: parse_status(&row.8)?,
        blocked: row.9 != 0,
        opened_at_ns: unsigned(row.10, "opened_at_ns")?,
        updated_at_ns: unsigned(row.11, "updated_at_ns")?,
        summary: row.12,
    })
}

fn parse_uuid(value: &str) -> Result<Uuid, AuditError> {
    Uuid::parse_str(value)
        .map_err(|error| AuditError::InvalidData(format!("invalid UUID: {error}")))
}

fn unsigned(value: i64, field: &str) -> Result<u64, AuditError> {
    u64::try_from(value).map_err(|_| AuditError::InvalidData(format!("{field} is negative")))
}

fn risk_severity(value: RiskSeverity) -> &'static str {
    match value {
        RiskSeverity::Low => "low",
        RiskSeverity::Medium => "medium",
        RiskSeverity::High => "high",
        RiskSeverity::Critical => "critical",
    }
}

fn parse_severity(value: &str) -> Result<RiskSeverity, AuditError> {
    match value {
        "low" => Ok(RiskSeverity::Low),
        "medium" => Ok(RiskSeverity::Medium),
        "high" => Ok(RiskSeverity::High),
        "critical" => Ok(RiskSeverity::Critical),
        _ => Err(AuditError::InvalidData(format!(
            "unknown risk severity '{value}'"
        ))),
    }
}

fn risk_status(value: RiskCaseStatus) -> &'static str {
    match value {
        RiskCaseStatus::Open => "open",
        RiskCaseStatus::Confirmed => "confirmed",
        RiskCaseStatus::FalsePositive => "false_positive",
        RiskCaseStatus::AcceptedRisk => "accepted_risk",
        RiskCaseStatus::Resolved => "resolved",
    }
}

fn parse_status(value: &str) -> Result<RiskCaseStatus, AuditError> {
    match value {
        "open" => Ok(RiskCaseStatus::Open),
        "confirmed" => Ok(RiskCaseStatus::Confirmed),
        "false_positive" => Ok(RiskCaseStatus::FalsePositive),
        "accepted_risk" => Ok(RiskCaseStatus::AcceptedRisk),
        "resolved" => Ok(RiskCaseStatus::Resolved),
        _ => Err(AuditError::InvalidData(format!(
            "unknown risk status '{value}'"
        ))),
    }
}
