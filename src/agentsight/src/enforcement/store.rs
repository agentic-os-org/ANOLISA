//! SQLite persistence for desired bindings and violation facts.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(test)]
use std::cell::RefCell;

use agentsight_enforcement_protocol::{
    Binding, BindingState, CredentialExfiltrationPolicy, CredentialPolicySnapshot, ViolationEvent,
};
use agentsight_sqlite_lifecycle::{
    CheckpointOutcome, ConnectionOptions, MaintenanceReport, MaintenanceStatus, SizeBasis,
    SizePolicy, checkpoint_truncate, enforce_size_policy, measure_database, open_connection,
    retention_cutoff_ns,
};
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;
use uuid::Uuid;

mod credential;
mod transition;

pub(crate) use credential::credential_binding_matches_request;

const MIN_REASONABLE_UNIX_EPOCH_NS: u64 = 946_684_800_000_000_000;
const MAX_STORED_VIOLATIONS: usize = 100_000;

#[cfg(test)]
type DegradationHook = Box<dyn FnOnce()>;

#[cfg(test)]
thread_local! {
    static DEGRADATION_HOOK: RefCell<Option<DegradationHook>> = const { RefCell::new(None) };
}

#[cfg(test)]
fn set_degradation_hook(hook: impl FnOnce() + 'static) {
    DEGRADATION_HOOK.with(|slot| {
        assert!(
            slot.borrow_mut().replace(Box::new(hook)).is_none(),
            "degradation hook must be consumed before replacement"
        );
    });
}

#[cfg(test)]
fn run_degradation_hook() {
    DEGRADATION_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

/// Persistence failures for enforcement state.
#[derive(Debug, Error)]
pub enum EnforcementStoreError {
    /// The shared SQLite helper could not open or configure the database.
    #[error("open enforcement database: {0}")]
    Open(String),
    /// SQLite open, schema, or query failure.
    #[error("enforcement SQLite failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Shared retention, checkpoint, or size measurement failure.
    #[error("enforcement SQLite lifecycle failed: {0}")]
    Lifecycle(#[from] agentsight_sqlite_lifecycle::LifecycleError),
    /// Binding or violation JSON could not be encoded or decoded.
    #[error("enforcement JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    /// Another thread panicked while holding the connection.
    #[error("enforcement database mutex is poisoned")]
    Poisoned,
    /// A requested binding is not persisted.
    #[error("binding {0} is not persisted")]
    MissingBinding(Uuid),
    /// A stable idempotency key names a different desired request.
    #[error("binding conflict for {0}")]
    BindingConflict(Uuid),
    /// A credential binding ID names a different structured product policy.
    #[error("credential intent conflict for {0}")]
    CredentialIntentConflict(Uuid),
    /// A stable transition key names a different replacement request.
    #[error("transition conflict for action {0}")]
    TransitionConflict(Uuid),
    /// A requested transition is not persisted.
    #[error("transition for action {0} is not persisted")]
    MissingTransition(Uuid),
    /// A persisted transition contains an unknown enum value.
    #[error("invalid transition {field} value: {value}")]
    InvalidTransitionState {
        /// Column containing the invalid value.
        field: &'static str,
        /// Value rejected by the exhaustive parser.
        value: String,
    },
    /// A durable handoff request failed protocol validation before runtime mutation.
    #[error("invalid transition request for action {action_id}: {reason}")]
    InvalidTransitionRequest {
        /// Action owning the rejected transition.
        action_id: Uuid,
        /// Stable protocol validation detail.
        reason: String,
    },
    /// Persisted structured policy provenance is malformed, unsupported, or mismatched.
    #[error("invalid credential policy snapshot for binding {binding_id}: {reason}")]
    InvalidCredentialPolicySnapshot {
        /// Binding whose immutable provenance could not be trusted.
        binding_id: Uuid,
        /// Decode, version, validation, or identity failure.
        reason: String,
    },
}

/// Outcome of one enforcement database maintenance pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnforcementMaintenanceReport {
    /// Violation rows removed by age retention.
    pub expired_violations: usize,
    /// Terminal transition rows removed by age retention.
    pub expired_transitions: usize,
    /// Result of physical-triggered, logical-targeted capacity enforcement.
    pub size: MaintenanceReport,
}

/// Thread-safe local enforcement state.
#[derive(Clone)]
pub struct EnforcementStore {
    connection: Arc<Mutex<Connection>>,
}

impl EnforcementStore {
    /// Opens a database and creates backward-compatible tables and indexes.
    ///
    /// # Errors
    ///
    /// Returns a SQLite or JSON error when initialization or legacy migration fails.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, EnforcementStoreError> {
        let path = path.as_ref();
        let connection = if path == Path::new(":memory:") {
            Connection::open_in_memory()?
        } else {
            open_connection(path, ConnectionOptions::default())
                .map_err(|error| EnforcementStoreError::Open(error.to_string()))?
        };
        Self::from_connection(connection)
    }

    /// Opens production enforcement state with private database and sidecar permissions.
    ///
    /// # Errors
    ///
    /// Returns a private-path, SQLite, schema, or migration error.
    pub(crate) fn open_private(state_dir: impl AsRef<Path>) -> Result<Self, EnforcementStoreError> {
        let connection = crate::private_sqlite::open_private_connection(
            state_dir.as_ref(),
            crate::config::ENFORCEMENT_DB_NAME,
        )
        .map_err(|error| EnforcementStoreError::Open(error.to_string()))?;
        Self::from_connection(connection)
    }

    fn from_connection(mut connection: Connection) -> Result<Self, EnforcementStoreError> {
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS enforcement_bindings (
                binding_id TEXT PRIMARY KEY,
                desired_json TEXT NOT NULL,
                state TEXT NOT NULL,
                message TEXT,
                domain_id INTEGER,
                updated_at_ns INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS enforcement_violations (
                event_id TEXT PRIMARY KEY,
                binding_id TEXT NOT NULL,
                occurred_at_ns INTEGER NOT NULL,
                event_json TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_enforcement_violations_time
                ON enforcement_violations(occurred_at_ns DESC);
             CREATE TABLE IF NOT EXISTS enforcement_transitions (
                action_id TEXT NOT NULL,
                direction TEXT NOT NULL,
                request_json TEXT NOT NULL,
                phase TEXT NOT NULL,
                acknowledgement_json TEXT,
                failure_code TEXT,
                updated_at_ns INTEGER NOT NULL,
                PRIMARY KEY(action_id, direction)
             );
             CREATE INDEX IF NOT EXISTS idx_enforcement_transitions_phase
                ON enforcement_transitions(phase, updated_at_ns ASC);
             CREATE TABLE IF NOT EXISTS enforcement_credential_policy_snapshots (
                binding_id TEXT PRIMARY KEY,
                snapshot_json TEXT NOT NULL,
                updated_at_ns INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS enforcement_credential_policy_intents (
                binding_id TEXT PRIMARY KEY,
                request_json TEXT NOT NULL,
                state TEXT NOT NULL,
                updated_at_ns INTEGER NOT NULL
             );",
        )?;
        migrate_legacy_violation_timestamps(&mut connection)?;
        credential::migrate_credential_policy_intents(&mut connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Inserts or updates the latest binding state.
    ///
    /// # Errors
    ///
    /// Returns a mutex, serialization, SQLite, or immutable-request conflict error.
    pub fn upsert_binding(&self, binding: &Binding) -> Result<(), EnforcementStoreError> {
        let connection = self.connection()?;
        upsert_binding_on(&connection, binding, now_ns())
    }

    /// Persists a compiled acknowledgement and its exact structured policy atomically.
    ///
    /// # Errors
    ///
    /// Returns a validation, identity, serialization, mutex, or SQLite error.
    pub fn upsert_credential_binding(
        &self,
        binding: &Binding,
        policy: &CredentialExfiltrationPolicy,
    ) -> Result<(), EnforcementStoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let updated_at_ns = now_ns();
        upsert_binding_on(&transaction, binding, updated_at_ns)?;
        upsert_credential_policy_snapshot_on(&transaction, binding, policy, updated_at_ns)?;
        transaction.commit()?;
        Ok(())
    }

    /// Reads the immutable structured policy captured for one credential binding.
    ///
    /// Legacy bindings without a snapshot return `None`; malformed, unknown-version,
    /// and identity-mismatched snapshots fail closed.
    ///
    /// # Errors
    ///
    /// Returns a mutex, SQLite, decode, version, validation, or identity error.
    pub fn credential_policy_snapshot(
        &self,
        binding_id: Uuid,
    ) -> Result<Option<CredentialPolicySnapshot>, EnforcementStoreError> {
        let connection = self.connection()?;
        credential_policy_snapshot_on(&connection, binding_id)
    }

    /// Reads one binding by ID.
    ///
    /// # Errors
    ///
    /// Returns a mutex, deserialization, or SQLite error.
    pub fn binding(&self, binding_id: Uuid) -> Result<Option<Binding>, EnforcementStoreError> {
        let json: Option<String> = self
            .connection()?
            .query_row(
                "SELECT desired_json FROM enforcement_bindings WHERE binding_id = ?1",
                params![binding_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        json.map(|json| serde_json::from_str(&json).map_err(Into::into))
            .transpose()
    }

    /// Lists all persisted bindings in stable ID order.
    ///
    /// # Errors
    ///
    /// Returns a mutex, deserialization, or SQLite error.
    pub fn bindings(&self) -> Result<Vec<Binding>, EnforcementStoreError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT desired_json FROM enforcement_bindings ORDER BY binding_id ASC")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut bindings = Vec::new();
        for row in rows {
            bindings.push(serde_json::from_str(&row?)?);
        }
        Ok(bindings)
    }

    /// Inserts a violation once by event ID.
    ///
    /// # Errors
    ///
    /// Returns a mutex, serialization, or SQLite error.
    pub fn insert_violation(&self, event: &ViolationEvent) -> Result<bool, EnforcementStoreError> {
        self.insert_violation_with_limit(event, MAX_STORED_VIOLATIONS)
    }

    fn insert_violation_with_limit(
        &self,
        event: &ViolationEvent,
        limit: usize,
    ) -> Result<bool, EnforcementStoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            "INSERT OR IGNORE INTO enforcement_violations
               (event_id, binding_id, occurred_at_ns, event_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                event.event_id.to_string(),
                event.binding_id.to_string(),
                sqlite_i64(event.occurred_at_ns),
                serde_json::to_string(event)?,
            ],
        )?;
        transaction.execute(
            "DELETE FROM enforcement_violations
             WHERE event_id IN (
                 SELECT event_id FROM enforcement_violations
                 ORDER BY occurred_at_ns DESC, event_id DESC
                 LIMIT -1 OFFSET ?1
             )",
            params![i64::try_from(limit.max(1)).unwrap_or(i64::MAX)],
        )?;
        transaction.commit()?;
        Ok(changed == 1)
    }

    /// Lists newest violations with a limit clamped to `1..=1000`.
    ///
    /// # Errors
    ///
    /// Returns a mutex, deserialization, or SQLite error.
    pub fn violations(&self, limit: usize) -> Result<Vec<ViolationEvent>, EnforcementStoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT event_json FROM enforcement_violations
             ORDER BY occurred_at_ns DESC LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit.clamp(1, 1000) as i64], |row| {
            row.get::<_, String>(0)
        })?;
        let mut events = Vec::new();
        for row in rows {
            events.push(serde_json::from_str(&row?)?);
        }
        Ok(events)
    }

    /// Applies age retention and capacity limits to deletable enforcement history.
    ///
    /// Only violations, forward `source_restored` transitions, and completed
    /// forward/reverse pairs are eligible. A completed forward transition remains
    /// protected until reverse restoration completes. Bindings, retryable transitions,
    /// credential snapshots, and credential intents are retained. Capacity enforcement
    /// starts when physical bytes exceed the limit and deletes toward 90% of that limit.
    /// A busy checkpoint stops the pass, and this method never runs `VACUUM`.
    /// A zero argument disables the corresponding age or capacity policy.
    ///
    /// # Errors
    ///
    /// Returns a named mutex, SQLite, retention calculation, measurement, or checkpoint error.
    pub fn maintain(
        &self,
        retention_days: u64,
        max_db_size_mb: u64,
    ) -> Result<EnforcementMaintenanceReport, EnforcementStoreError> {
        let (expired_violations, expired_transitions) = if retention_days == 0 {
            (0, 0)
        } else {
            let cutoff_ns = retention_cutoff_ns(now_ns(), retention_days)?;
            self.delete_expired_history(cutoff_ns)?
        };

        let limit_bytes = max_db_size_mb.saturating_mul(1024 * 1024);
        if (expired_violations.saturating_add(expired_transitions) > 0 || limit_bytes > 0)
            && self.checkpoint()? == CheckpointOutcome::Busy
        {
            let snapshot = self.size_snapshot()?;
            return Ok(EnforcementMaintenanceReport {
                expired_violations,
                expired_transitions,
                size: MaintenanceReport {
                    status: MaintenanceStatus::CheckpointBusy,
                    rounds: 0,
                    deleted_rows: 0,
                    before: snapshot,
                    after: snapshot,
                },
            });
        }

        let size = enforce_size_policy::<EnforcementStoreError>(
            SizePolicy {
                limit_bytes,
                trigger_bytes: limit_bytes,
                target_bytes: limit_bytes.saturating_mul(9) / 10,
                trigger_basis: SizeBasis::Physical,
                target_basis: SizeBasis::Logical,
                max_rounds: 20,
                max_stalled_rounds: 3,
            },
            || self.size_snapshot(),
            |fraction| self.delete_oldest_history_fraction(fraction),
            || self.checkpoint(),
        )?;
        Ok(EnforcementMaintenanceReport {
            expired_violations,
            expired_transitions,
            size,
        })
    }

    fn delete_expired_history(
        &self,
        cutoff_ns: u64,
    ) -> Result<(usize, usize), EnforcementStoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let cutoff_ns = sqlite_i64(cutoff_ns);
        let violations = transaction.execute(
            "DELETE FROM enforcement_violations WHERE occurred_at_ns < ?1",
            params![cutoff_ns],
        )?;
        let transitions = transaction.execute(
            "DELETE FROM enforcement_transitions AS transitions
             WHERE updated_at_ns < ?1
               AND (
                   (direction = 'forward' AND phase = 'source_restored')
                   OR (
                       phase = 'completed'
                       AND EXISTS (
                           SELECT 1 FROM enforcement_transitions AS reverse
                           WHERE reverse.action_id = transitions.action_id
                             AND reverse.direction = 'reverse'
                             AND reverse.phase = 'completed'
                       )
                   )
               )",
            params![cutoff_ns],
        )?;
        transaction.commit()?;
        Ok((violations, transitions))
    }

    fn delete_oldest_history_fraction(
        &self,
        fraction: f64,
    ) -> Result<usize, EnforcementStoreError> {
        if fraction <= 0.0 {
            return Ok(0);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let violation_count: i64 =
            transaction.query_row("SELECT COUNT(*) FROM enforcement_violations", [], |row| {
                row.get(0)
            })?;
        let transition_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM enforcement_transitions AS transitions
             WHERE (direction = 'forward' AND phase = 'source_restored')
                OR (
                    phase = 'completed'
                    AND EXISTS (
                        SELECT 1 FROM enforcement_transitions AS reverse
                        WHERE reverse.action_id = transitions.action_id
                          AND reverse.direction = 'reverse'
                          AND reverse.phase = 'completed'
                    )
                )",
            [],
            |row| row.get(0),
        )?;
        let eligible_count = violation_count.saturating_add(transition_count);
        if eligible_count <= 0 {
            transaction.commit()?;
            return Ok(0);
        }

        let batch = ((eligible_count as f64 * fraction.clamp(0.0, 1.0)) as i64).max(1);
        let deleted_violations = transaction.execute(
            "DELETE FROM enforcement_violations WHERE event_id IN (
                 SELECT event_id FROM enforcement_violations
                 ORDER BY occurred_at_ns ASC, event_id ASC LIMIT ?1
             )",
            params![batch],
        )?;
        let remaining = usize::try_from(batch)
            .unwrap_or(usize::MAX)
            .saturating_sub(deleted_violations);
        let deleted_transitions = if remaining == 0 {
            0
        } else {
            transaction.execute(
                "DELETE FROM enforcement_transitions WHERE rowid IN (
                     SELECT transitions.rowid FROM enforcement_transitions AS transitions
                     WHERE (direction = 'forward' AND phase = 'source_restored')
                        OR (
                            phase = 'completed'
                            AND EXISTS (
                                SELECT 1 FROM enforcement_transitions AS reverse
                                WHERE reverse.action_id = transitions.action_id
                                  AND reverse.direction = 'reverse'
                                  AND reverse.phase = 'completed'
                            )
                        )
                     ORDER BY updated_at_ns ASC, action_id ASC, direction ASC LIMIT ?1
                 )",
                params![i64::try_from(remaining).unwrap_or(i64::MAX)],
            )?
        };
        transaction.commit()?;
        Ok(deleted_violations.saturating_add(deleted_transitions))
    }

    fn size_snapshot(
        &self,
    ) -> Result<agentsight_sqlite_lifecycle::SizeSnapshot, EnforcementStoreError> {
        let connection = self.connection()?;
        let path = connection
            .path()
            .filter(|path| !path.is_empty())
            .unwrap_or(":memory:");
        measure_database(Path::new(path), &connection).map_err(Into::into)
    }

    fn checkpoint(&self) -> Result<CheckpointOutcome, EnforcementStoreError> {
        let connection = self.connection()?;
        checkpoint_truncate(&connection).map_err(Into::into)
    }

    /// Marks active desired state degraded without deleting it.
    ///
    /// # Errors
    ///
    /// Returns a mutex, deserialization, serialization, or SQLite error.
    pub fn mark_active_degraded(&self, message: &str) -> Result<(), EnforcementStoreError> {
        #[cfg(test)]
        run_degradation_hook();

        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let updated_at_ns = now_ns();
        transaction.execute(
            "UPDATE enforcement_bindings
             SET desired_json = json_set(
                    desired_json,
                    '$.state', 'degraded',
                    '$.message', ?1
                 ),
                 state = 'degraded',
                 message = ?1,
                 updated_at_ns = ?2
             WHERE state IN ('pending', 'enforced', 'degraded')",
            params![message, sqlite_i64(updated_at_ns)],
        )?;
        transaction.execute(
            "UPDATE enforcement_credential_policy_intents
             SET state = 'degraded', updated_at_ns = ?1
             WHERE binding_id IN (
                 SELECT binding_id FROM enforcement_bindings
                 WHERE state = 'degraded' AND updated_at_ns = ?1
             )",
            params![sqlite_i64(updated_at_ns)],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, EnforcementStoreError> {
        self.connection
            .lock()
            .map_err(|_| EnforcementStoreError::Poisoned)
    }
}

fn upsert_binding_on(
    connection: &Connection,
    binding: &Binding,
    updated_at_ns: u64,
) -> Result<(), EnforcementStoreError> {
    let existing_json: Option<String> = connection
        .query_row(
            "SELECT desired_json FROM enforcement_bindings WHERE binding_id = ?1",
            params![binding.request.binding_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing_json) = existing_json {
        let existing: Binding = serde_json::from_str(&existing_json)?;
        if existing.request != binding.request {
            return Err(EnforcementStoreError::BindingConflict(
                binding.request.binding_id,
            ));
        }
    }
    connection.execute(
        "INSERT INTO enforcement_bindings
           (binding_id, desired_json, state, message, domain_id, updated_at_ns)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(binding_id) DO UPDATE SET
           desired_json=excluded.desired_json,
           state=excluded.state,
           message=excluded.message,
           domain_id=excluded.domain_id,
           updated_at_ns=excluded.updated_at_ns",
        params![
            binding.request.binding_id.to_string(),
            serde_json::to_string(binding)?,
            state_name(binding.state),
            binding.message.as_deref(),
            binding.domain_id.map(i64::from),
            sqlite_i64(updated_at_ns),
        ],
    )?;
    credential::sync_credential_intent_state_on(
        connection,
        binding.request.binding_id,
        binding.state,
        updated_at_ns,
    )?;
    Ok(())
}

pub(super) fn upsert_credential_policy_snapshot_on(
    connection: &Connection,
    binding: &Binding,
    policy: &CredentialExfiltrationPolicy,
    updated_at_ns: u64,
) -> Result<(), EnforcementStoreError> {
    let binding_id = binding.request.binding_id;
    let snapshot = CredentialPolicySnapshot::capture(policy.clone())
        .map_err(|error| invalid_credential_snapshot(binding_id, error))?;
    validate_snapshot_binding(binding_id, binding, &snapshot)?;
    let existing_json: Option<String> = connection
        .query_row(
            "SELECT snapshot_json FROM enforcement_credential_policy_snapshots
             WHERE binding_id = ?1",
            params![binding_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing_json) = existing_json {
        let existing: CredentialPolicySnapshot = serde_json::from_str(&existing_json)
            .map_err(|error| invalid_credential_snapshot(binding_id, error))?;
        existing
            .policy()
            .map_err(|error| invalid_credential_snapshot(binding_id, error))?;
        if existing != snapshot {
            return Err(invalid_credential_snapshot(
                binding_id,
                "immutable snapshot conflicts with the original policy",
            ));
        }
        credential::upsert_credential_intent_from_binding_on(
            connection,
            binding,
            policy,
            updated_at_ns,
        )?;
        return Ok(());
    }
    connection.execute(
        "INSERT INTO enforcement_credential_policy_snapshots
           (binding_id, snapshot_json, updated_at_ns)
         VALUES (?1, ?2, ?3)",
        params![
            binding_id.to_string(),
            serde_json::to_string(&snapshot)?,
            sqlite_i64(updated_at_ns),
        ],
    )?;
    credential::upsert_credential_intent_from_binding_on(
        connection,
        binding,
        policy,
        updated_at_ns,
    )?;
    Ok(())
}

pub(super) fn credential_policy_snapshot_on(
    connection: &Connection,
    binding_id: Uuid,
) -> Result<Option<CredentialPolicySnapshot>, EnforcementStoreError> {
    let row: Option<(String, String)> = connection
        .query_row(
            "SELECT snapshots.snapshot_json, bindings.desired_json
             FROM enforcement_credential_policy_snapshots AS snapshots
             JOIN enforcement_bindings AS bindings
               ON bindings.binding_id = snapshots.binding_id
             WHERE snapshots.binding_id = ?1",
            params![binding_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((snapshot_json, binding_json)) = row else {
        return Ok(None);
    };
    let snapshot: CredentialPolicySnapshot = serde_json::from_str(&snapshot_json)
        .map_err(|error| invalid_credential_snapshot(binding_id, error))?;
    snapshot
        .policy()
        .map_err(|error| invalid_credential_snapshot(binding_id, error))?;
    let binding: Binding = serde_json::from_str(&binding_json)
        .map_err(|error| invalid_credential_snapshot(binding_id, error))?;
    validate_snapshot_binding(binding_id, &binding, &snapshot)?;
    Ok(Some(snapshot))
}

fn validate_snapshot_binding(
    binding_id: Uuid,
    binding: &Binding,
    snapshot: &CredentialPolicySnapshot,
) -> Result<(), EnforcementStoreError> {
    let policy = snapshot
        .policy()
        .map_err(|error| invalid_credential_snapshot(binding_id, error))?;
    if binding.request.binding_id != binding_id
        || binding.request.policy_id != policy.policy_id
        || binding.request.policy_revision != policy.revision.to_string()
    {
        return Err(invalid_credential_snapshot(
            binding_id,
            "snapshot identity does not match compiled binding",
        ));
    }
    Ok(())
}

fn invalid_credential_snapshot(
    binding_id: Uuid,
    error: impl std::fmt::Display,
) -> EnforcementStoreError {
    EnforcementStoreError::InvalidCredentialPolicySnapshot {
        binding_id,
        reason: error.to_string(),
    }
}

fn migrate_legacy_violation_timestamps(
    connection: &mut Connection,
) -> Result<(), EnforcementStoreError> {
    let transaction = connection.transaction()?;
    let candidates = {
        let mut statement = transaction.prepare(
            "SELECT event_id, event_json FROM enforcement_violations
             WHERE occurred_at_ns < ?1",
        )?;
        let rows = statement
            .query_map(params![sqlite_i64(MIN_REASONABLE_UNIX_EPOCH_NS)], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
        let mut candidates = Vec::new();
        for row in rows {
            candidates.push(row?);
        }
        candidates
    };

    for (event_id, event_json) in candidates {
        let mut event: ViolationEvent = serde_json::from_str(&event_json)?;
        if event.occurred_at_ns >= MIN_REASONABLE_UNIX_EPOCH_NS
            || !is_reasonable_unix_epoch_ns(event.observed_at_ns)
        {
            continue;
        }
        event.occurred_at_ns = event.observed_at_ns;
        transaction.execute(
            "UPDATE enforcement_violations
             SET occurred_at_ns = ?1, event_json = ?2
             WHERE event_id = ?3 AND occurred_at_ns < ?4",
            params![
                event.observed_at_ns as i64,
                serde_json::to_string(&event)?,
                event_id,
                sqlite_i64(MIN_REASONABLE_UNIX_EPOCH_NS),
            ],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

fn is_reasonable_unix_epoch_ns(value: u64) -> bool {
    (MIN_REASONABLE_UNIX_EPOCH_NS..=i64::MAX as u64).contains(&value)
}

/// Returns the stable SQLite representation for a binding lifecycle state.
pub(super) fn state_name(state: BindingState) -> &'static str {
    match state {
        BindingState::Pending => "pending",
        BindingState::Enforced => "enforced",
        BindingState::Failed => "failed",
        BindingState::Degraded => "degraded",
        BindingState::Detaching => "detaching",
        BindingState::Detached => "detached",
    }
}

/// Returns a saturating Unix-epoch nanosecond timestamp for persistence metadata.
pub(super) fn now_ns() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    nanos.min(u64::MAX as u128) as u64
}

/// Clamps an unsigned timestamp or count into SQLite's signed integer range.
pub(super) fn sqlite_i64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use agentsight_enforcement_protocol::{
        ApplyCredentialPolicy, ApplyPolicy, CredentialExfiltrationPolicy, DestinationScope, Effect,
        PolicyMode, ReplaceFailureCode, ReplacePolicy, ReplacementPolicy, ReplacementSource,
    };

    use super::*;
    use crate::enforcement::{
        PolicyTransition, TransitionDirection, TransitionKey, TransitionPhase,
    };

    const LEGACY_OCCURRED_AT_NS: u64 = 270_000_000_000_000;
    const LEGACY_OBSERVED_AT_NS: u64 = 1_784_000_000_000_000_000;
    const VALID_OCCURRED_AT_NS: u64 = 1_783_000_000_000_000_000;

    struct TestDatabase {
        path: PathBuf,
    }

    impl TestDatabase {
        fn new() -> Self {
            Self {
                path: std::env::temp_dir().join(format!(
                    "agentsight-enforcement-store-{}.db",
                    Uuid::new_v4()
                )),
            }
        }
    }

    impl Drop for TestDatabase {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
            let _ = fs::remove_file(format!("{}-wal", self.path.display()));
            let _ = fs::remove_file(format!("{}-shm", self.path.display()));
        }
    }

    fn violation(occurred_at_ns: u64, observed_at_ns: u64) -> ViolationEvent {
        ViolationEvent {
            event_id: Uuid::new_v4(),
            binding_id: Uuid::new_v4(),
            agent_id: "test-agent".into(),
            session_id: Some("test-session".into()),
            policy_id: "test-policy".into(),
            policy_revision: "revision-1".into(),
            pid: 42,
            ppid: Some(1),
            process_start_time: 99,
            operation: "open".into(),
            target: "/tmp/secret".into(),
            effect: Effect::Block,
            blocked: true,
            killed: false,
            rule_id: Some("block-secret".into()),
            reason: Some("test fixture".into()),
            occurred_at_ns,
            observed_at_ns,
            actplane_revision: "test-revision".into(),
        }
    }

    fn transition_binding(state: BindingState) -> Binding {
        Binding {
            request: ApplyPolicy {
                binding_id: Uuid::new_v4(),
                agent_id: "transition-agent".into(),
                session_id: Some("transition-session".into()),
                root_pid: 42,
                process_start_time: 99,
                policy_id: "transition-policy".into(),
                policy_revision: "1".into(),
                policy_dsl: "source AGENT = exec \"**\"".into(),
                policy_mode: Some(PolicyMode::Audit),
            },
            state,
            message: None,
            domain_id: Some(7),
        }
    }

    fn credential_policy(ttl_secs: u64) -> CredentialExfiltrationPolicy {
        CredentialExfiltrationPolicy {
            policy_id: "transition-policy".into(),
            revision: 1,
            source_patterns: vec!["/tmp/credential".into()],
            trusted_endpoints: vec!["trusted.example:443".into()],
            taint_label: "CREDENTIAL".into(),
            taint_ttl_secs: ttl_secs,
            destination_scope: DestinationScope::PublicIpv4,
            mode: PolicyMode::Audit,
        }
    }

    fn credential_request(binding_id: Uuid, ttl_secs: u64) -> ApplyCredentialPolicy {
        ApplyCredentialPolicy {
            binding_id,
            agent_id: "transition-agent".into(),
            session_id: Some("transition-session".into()),
            root_pid: 42,
            process_start_time: 99,
            policy: credential_policy(ttl_secs),
        }
    }

    fn insert_raw_transition(store: &EnforcementStore, phase: &str, updated_at_ns: u64) -> Uuid {
        let action_id = Uuid::new_v4();
        insert_raw_transition_for(store, action_id, "forward", phase, updated_at_ns);
        action_id
    }

    fn insert_raw_transition_for(
        store: &EnforcementStore,
        action_id: Uuid,
        direction: &str,
        phase: &str,
        updated_at_ns: u64,
    ) {
        store
            .connection()
            .expect("test connection should lock")
            .execute(
                "INSERT INTO enforcement_transitions
                   (action_id, direction, request_json, phase, updated_at_ns)
                 VALUES (?1, ?2, '{}', ?3, ?4)",
                params![
                    action_id.to_string(),
                    direction,
                    phase,
                    sqlite_i64(updated_at_ns)
                ],
            )
            .expect("raw transition should insert");
    }

    fn table_count(store: &EnforcementStore, table: &str) -> i64 {
        let sql = match table {
            "bindings" => "SELECT COUNT(*) FROM enforcement_bindings",
            "violations" => "SELECT COUNT(*) FROM enforcement_violations",
            "transitions" => "SELECT COUNT(*) FROM enforcement_transitions",
            "snapshots" => "SELECT COUNT(*) FROM enforcement_credential_policy_snapshots",
            "intents" => "SELECT COUNT(*) FROM enforcement_credential_policy_intents",
            _ => panic!("unknown test table {table}"),
        };
        store
            .connection()
            .expect("test connection should lock")
            .query_row(sql, [], |row| row.get(0))
            .expect("test row count should load")
    }

    fn transition_exists(store: &EnforcementStore, action_id: Uuid) -> bool {
        store
            .connection()
            .expect("test connection should lock")
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM enforcement_transitions WHERE action_id = ?1
                 )",
                params![action_id.to_string()],
                |row| row.get(0),
            )
            .expect("transition existence should load")
    }

    #[test]
    fn credential_intent_is_durable_before_backend_ack_and_immutable() {
        let database = TestDatabase::new();
        let binding_id = Uuid::new_v4();
        let request = credential_request(binding_id, 300);
        let store = EnforcementStore::open(&database.path).expect("test store should open");

        let intent = store
            .begin_credential_policy_intent(&request)
            .expect("typed intent should persist before backend mutation");
        assert_eq!(intent.request, request);
        assert_eq!(intent.state, BindingState::Pending);
        assert_eq!(
            store
                .binding(binding_id)
                .expect("binding lookup should work"),
            None
        );
        let snapshot_json: String = store
            .connection()
            .expect("test connection should lock")
            .query_row(
                "SELECT snapshot_json
                 FROM enforcement_credential_policy_snapshots
                 WHERE binding_id = ?1",
                params![binding_id.to_string()],
                |row| row.get(0),
            )
            .expect("canonical snapshot must predate backend mutation");
        let snapshot: CredentialPolicySnapshot =
            serde_json::from_str(&snapshot_json).expect("snapshot should decode");
        assert_eq!(
            snapshot.policy().expect("snapshot should validate"),
            &request.policy
        );
        drop(store);

        let reopened =
            EnforcementStore::open(&database.path).expect("test store should reopen cleanly");
        assert_eq!(
            reopened
                .credential_policy_intents()
                .expect("typed intent should survive reopen"),
            vec![intent]
        );
        assert!(matches!(
            reopened.begin_credential_policy_intent(&credential_request(binding_id, 900)),
            Err(EnforcementStoreError::CredentialIntentConflict(conflict))
                if conflict == binding_id
        ));
    }

    #[test]
    fn credential_acknowledgement_must_match_pre_mutation_identity() {
        let store = EnforcementStore::open(":memory:").expect("test store should open");
        let request = credential_request(Uuid::new_v4(), 300);
        store
            .begin_credential_policy_intent(&request)
            .expect("typed intent should seed");
        let mut acknowledgement = transition_binding(BindingState::Enforced);
        acknowledgement.request.binding_id = request.binding_id;
        acknowledgement.request.policy_id = request.policy.policy_id.clone();
        acknowledgement.request.policy_revision = request.policy.revision.to_string();
        acknowledgement.request.agent_id = "wrong-agent".into();

        assert!(matches!(
            store.upsert_credential_binding(&acknowledgement, &request.policy),
            Err(EnforcementStoreError::CredentialIntentConflict(binding_id))
                if binding_id == request.binding_id
        ));
        assert_eq!(
            store
                .binding(request.binding_id)
                .expect("failed acknowledgement must roll back"),
            None
        );
        assert_eq!(
            store
                .credential_policy_intents()
                .expect("intent should remain retryable")[0]
                .state,
            BindingState::Pending
        );
    }

    #[test]
    fn credential_acknowledgement_must_match_structured_policy_mode() {
        let store = EnforcementStore::open(":memory:").expect("test store should open");
        let request = credential_request(Uuid::new_v4(), 300);
        store
            .begin_credential_policy_intent(&request)
            .expect("typed intent should seed");
        let mut acknowledgement = transition_binding(BindingState::Enforced);
        acknowledgement.request.binding_id = request.binding_id;
        acknowledgement.request.policy_id = request.policy.policy_id.clone();
        acknowledgement.request.policy_revision = request.policy.revision.to_string();
        acknowledgement.request.policy_mode = Some(PolicyMode::Enforce);

        assert!(matches!(
            store.upsert_credential_binding(&acknowledgement, &request.policy),
            Err(EnforcementStoreError::InvalidCredentialPolicySnapshot { binding_id, .. })
                if binding_id == request.binding_id
        ));
        assert_eq!(
            store
                .binding(request.binding_id)
                .expect("failed acknowledgement must roll back"),
            None
        );
    }

    #[test]
    fn detached_binding_cannot_be_resurrected_by_degradation() {
        let database = TestDatabase::new();
        let store = EnforcementStore::open(&database.path).expect("test store should open");
        let active = transition_binding(BindingState::Enforced);
        store
            .upsert_binding(&active)
            .expect("active binding should seed");

        let writer =
            EnforcementStore::open(&database.path).expect("independent writer should open");
        let mut detached = active.clone();
        detached.state = BindingState::Detached;
        detached.domain_id = None;
        let detached_for_hook = detached.clone();
        set_degradation_hook(move || {
            writer
                .upsert_binding(&detached_for_hook)
                .expect("concurrent detach should commit");
        });

        store
            .mark_active_degraded("subscription disconnected")
            .expect("conditional degradation should complete");

        assert_eq!(
            store
                .binding(active.request.binding_id)
                .expect("binding should load"),
            Some(detached)
        );
    }

    #[test]
    fn violation_insert_transactionally_caps_oldest_rows() {
        let store = EnforcementStore::open(":memory:").expect("test store should open");
        let events: Vec<_> = (1..=4)
            .map(|second| {
                violation(
                    1_783_000_000_000_000_000 + second,
                    1_783_000_000_000_000_100 + second,
                )
            })
            .collect();

        for event in &events {
            store
                .insert_violation_with_limit(event, 3)
                .expect("bounded violation insert should succeed");
        }

        let retained = store
            .violations(10)
            .expect("bounded violations should load");
        assert_eq!(retained.len(), 3);
        assert_eq!(retained[0].event_id, events[3].event_id);
        assert_eq!(retained[2].event_id, events[1].event_id);
        assert!(
            !retained
                .iter()
                .any(|event| event.event_id == events[0].event_id)
        );
    }

    #[test]
    fn age_retention_only_removes_eligible_history() {
        let database = TestDatabase::new();
        let store = EnforcementStore::open(&database.path).expect("test store should open");
        let old_ns = now_ns().saturating_sub(2 * 24 * 60 * 60 * 1_000_000_000);
        let current_ns = now_ns();
        let old_violation = violation(old_ns, old_ns);
        let current_violation = violation(current_ns, current_ns);
        store
            .insert_violation(&old_violation)
            .expect("old violation should insert");
        store
            .insert_violation(&current_violation)
            .expect("current violation should insert");

        let completed = insert_raw_transition(&store, "completed", old_ns);
        let restored = insert_raw_transition(&store, "source_restored", old_ns);
        let pending = insert_raw_transition(&store, "pending", old_ns);
        let indeterminate = insert_raw_transition(&store, "indeterminate", old_ns);
        let current_terminal = insert_raw_transition(&store, "completed", current_ns);

        let mut binding = transition_binding(BindingState::Enforced);
        let policy = credential_policy(300);
        binding.request.policy_id = policy.policy_id.clone();
        binding.request.policy_revision = policy.revision.to_string();
        binding.request.policy_mode = Some(policy.mode);
        store
            .upsert_credential_binding(&binding, &policy)
            .expect("protected credential state should seed");

        let report = store
            .maintain(1, 0)
            .expect("age maintenance should succeed");

        assert_eq!(report.expired_violations, 1);
        assert_eq!(report.expired_transitions, 1);
        assert_eq!(report.size.status, MaintenanceStatus::Disabled);
        assert_eq!(table_count(&store, "violations"), 1);
        assert!(transition_exists(&store, completed));
        assert!(!transition_exists(&store, restored));
        assert!(transition_exists(&store, pending));
        assert!(transition_exists(&store, indeterminate));
        assert!(transition_exists(&store, current_terminal));
        assert_eq!(table_count(&store, "bindings"), 1);
        assert_eq!(table_count(&store, "snapshots"), 1);
        assert_eq!(table_count(&store, "intents"), 1);
    }

    #[test]
    fn completed_forward_transition_survives_until_reverse_completion() {
        let database = TestDatabase::new();
        let store = EnforcementStore::open(&database.path).expect("test store should open");
        let source = transition_binding(BindingState::Enforced);
        let target = transition_binding(BindingState::Enforced);
        let action_id = Uuid::new_v4();
        let forward = PolicyTransition::pending(
            TransitionKey {
                action_id,
                direction: TransitionDirection::Forward,
            },
            ReplacePolicy {
                expected: source.clone(),
                source: ReplacementSource::Generic,
                replacement: ReplacementPolicy::Generic(target.request.clone()),
            },
        );
        store
            .upsert_binding(&source)
            .expect("source binding should seed");
        store
            .begin_transition(&forward)
            .expect("forward transition should seed");
        store
            .complete_transition(&forward.key, &target)
            .expect("forward transition should complete");
        let old_ns = now_ns().saturating_sub(2 * 24 * 60 * 60 * 1_000_000_000);
        store
            .connection()
            .expect("test connection should lock")
            .execute(
                "UPDATE enforcement_transitions SET updated_at_ns = ?1 WHERE action_id = ?2",
                params![sqlite_i64(old_ns), action_id.to_string()],
            )
            .expect("forward transition should age");

        let report = store.maintain(1, 0).expect("maintenance should succeed");
        assert_eq!(report.expired_transitions, 0);

        let retained_forward = store
            .transition(&forward.key)
            .expect("forward transition should load")
            .expect("forward transition must survive maintenance");
        let reverse = PolicyTransition::pending(
            TransitionKey {
                action_id,
                direction: TransitionDirection::Reverse,
            },
            retained_forward.request.reverse(
                retained_forward
                    .acknowledgement
                    .expect("completed forward acknowledgement should persist"),
            ),
        );
        store
            .begin_transition(&reverse)
            .expect("reverse transition should begin after maintenance");
        store
            .complete_transition(&reverse.key, &source)
            .expect("reverse transition should restore the source");
        store
            .connection()
            .expect("test connection should lock")
            .execute(
                "UPDATE enforcement_transitions SET updated_at_ns = ?1 WHERE action_id = ?2",
                params![sqlite_i64(old_ns), action_id.to_string()],
            )
            .expect("transition pair should age");

        let report = store
            .maintain(1, 0)
            .expect("post-rollback maintenance should succeed");
        assert_eq!(report.expired_transitions, 2);
        assert!(!transition_exists(&store, action_id));
    }

    #[test]
    fn capacity_batches_delete_violations_before_terminal_transitions() {
        let store = EnforcementStore::open(":memory:").expect("test store should open");
        let older_violation = violation(100, 100);
        let newer_violation = violation(200, 200);
        store
            .insert_violation(&older_violation)
            .expect("older violation should insert");
        store
            .insert_violation(&newer_violation)
            .expect("newer violation should insert");
        let older_transition = insert_raw_transition(&store, "source_restored", 1);
        let newer_transition = insert_raw_transition(&store, "completed", 2);

        assert_eq!(
            store
                .delete_oldest_history_fraction(0.25)
                .expect("first capacity batch should delete"),
            1
        );
        let retained = store
            .violations(10)
            .expect("remaining violations should load");
        assert_eq!(retained, [newer_violation]);
        assert!(transition_exists(&store, older_transition));
        assert!(transition_exists(&store, newer_transition));

        assert_eq!(
            store
                .delete_oldest_history_fraction(0.34)
                .expect("second capacity batch should delete"),
            1
        );
        assert_eq!(table_count(&store, "violations"), 0);
        assert!(transition_exists(&store, older_transition));

        assert_eq!(
            store
                .delete_oldest_history_fraction(0.5)
                .expect("terminal capacity batch should delete"),
            1
        );
        assert!(!transition_exists(&store, older_transition));
        assert!(transition_exists(&store, newer_transition));
    }

    #[test]
    fn capacity_maintenance_reports_no_rows_when_only_protected_state_remains() {
        let database = TestDatabase::new();
        let store = EnforcementStore::open(&database.path).expect("test store should open");
        let binding = transition_binding(BindingState::Pending);
        store
            .upsert_binding(&binding)
            .expect("protected binding should seed");
        store
            .connection()
            .expect("test connection should lock")
            .execute(
                "UPDATE enforcement_bindings
                 SET desired_json = desired_json || CAST(zeroblob(1572864) AS TEXT)
                 WHERE binding_id = ?1",
                params![binding.request.binding_id.to_string()],
            )
            .expect("protected state should exceed the size limit");

        let report = store
            .maintain(0, 1)
            .expect("capacity maintenance should complete");

        assert_eq!(report.size.status, MaintenanceStatus::NoRows);
        assert_eq!(report.size.deleted_rows, 0);
        assert!(report.size.before.physical_bytes > 1024 * 1024);
        assert_eq!(table_count(&store, "bindings"), 1);
    }

    #[test]
    fn capacity_maintenance_deletes_history_toward_logical_target() {
        let database = TestDatabase::new();
        let store = EnforcementStore::open(&database.path).expect("test store should open");
        let connection = store.connection().expect("test connection should lock");
        for timestamp in 1..=24 {
            connection
                .execute(
                    "INSERT INTO enforcement_violations
                       (event_id, binding_id, occurred_at_ns, event_json)
                     VALUES (?1, ?2, ?3, CAST(zeroblob(65536) AS TEXT))",
                    params![
                        Uuid::new_v4().to_string(),
                        Uuid::new_v4().to_string(),
                        timestamp,
                    ],
                )
                .expect("sized violation should insert");
        }
        drop(connection);

        let report = store
            .maintain(0, 1)
            .expect("capacity maintenance should succeed");

        assert!(report.size.before.physical_bytes > 1024 * 1024);
        assert!(report.size.deleted_rows > 0);
        assert!(matches!(
            report.size.status,
            MaintenanceStatus::TargetReached | MaintenanceStatus::NoRows
        ));
        assert!(report.size.after.logical_bytes <= 9 * 1024 * 1024 / 10);
    }

    #[test]
    fn credential_binding_snapshot_preserves_exact_normalized_policy() {
        let store = EnforcementStore::open(":memory:").expect("test store should open");
        let source = transition_binding(BindingState::Enforced);
        let policy = credential_policy(300);

        store
            .upsert_credential_binding(&source, &policy)
            .expect("credential binding should persist atomically");

        let snapshot = store
            .credential_policy_snapshot(source.request.binding_id)
            .expect("snapshot should decode")
            .expect("snapshot should exist");
        assert_eq!(
            snapshot.policy().expect("snapshot should validate"),
            &policy
        );
        assert_eq!(
            snapshot
                .policy()
                .expect("snapshot should remain valid")
                .taint_ttl_secs,
            300
        );
    }

    #[test]
    fn malformed_or_missing_credential_snapshot_never_synthesizes_defaults() {
        let store = EnforcementStore::open(":memory:").expect("test store should open");
        let source = transition_binding(BindingState::Enforced);
        store
            .upsert_binding(&source)
            .expect("legacy binding should seed without a snapshot");
        assert_eq!(
            store
                .credential_policy_snapshot(source.request.binding_id)
                .expect("missing snapshot should be represented explicitly"),
            None
        );

        store
            .upsert_credential_binding(&source, &credential_policy(300))
            .expect("valid snapshot should seed");
        store
            .connection()
            .expect("test connection should lock")
            .execute(
                "UPDATE enforcement_credential_policy_snapshots
                 SET snapshot_json = '{\"version\":99,\"policy\":{}}'
                 WHERE binding_id = ?1",
                rusqlite::params![source.request.binding_id.to_string()],
            )
            .expect("test corruption should persist");

        assert!(matches!(
            store.credential_policy_snapshot(source.request.binding_id),
            Err(EnforcementStoreError::InvalidCredentialPolicySnapshot { binding_id, .. })
                if binding_id == source.request.binding_id
        ));
    }

    #[test]
    fn credential_snapshot_is_immutable_for_one_binding_identity() {
        let store = EnforcementStore::open(":memory:").expect("test store should open");
        let source = transition_binding(BindingState::Enforced);
        store
            .upsert_credential_binding(&source, &credential_policy(300))
            .expect("original snapshot should seed");

        assert!(matches!(
            store.upsert_credential_binding(&source, &credential_policy(900)),
            Err(EnforcementStoreError::InvalidCredentialPolicySnapshot { binding_id, .. })
                if binding_id == source.request.binding_id
        ));
        assert_eq!(
            store
                .credential_policy_snapshot(source.request.binding_id)
                .expect("original snapshot should decode")
                .expect("original snapshot should remain")
                .policy()
                .expect("original snapshot should validate")
                .taint_ttl_secs,
            300
        );
    }

    #[test]
    fn transition_rejects_noncanonical_source_snapshot_atomically() {
        let store = EnforcementStore::open(":memory:").expect("test store should open");
        let source = transition_binding(BindingState::Enforced);
        store
            .upsert_credential_binding(&source, &credential_policy(300))
            .expect("canonical source should seed");
        let target = transition_binding(BindingState::Enforced);
        let key = TransitionKey {
            action_id: Uuid::new_v4(),
            direction: TransitionDirection::Forward,
        };
        let request = ReplacePolicy {
            expected: source.clone(),
            source: ReplacementSource::Credential(
                agentsight_enforcement_protocol::CredentialPolicySnapshot::capture(
                    credential_policy(900),
                )
                .expect("mismatched policy is otherwise valid"),
            ),
            replacement: ReplacementPolicy::Generic(target.request),
        };

        assert!(matches!(
            store.begin_transition(&PolicyTransition::pending(key.clone(), request)),
            Err(EnforcementStoreError::InvalidTransitionRequest { action_id, .. })
                if action_id == key.action_id
        ));
        assert_eq!(
            store
                .transition(&key)
                .expect("transition lookup should work"),
            None
        );
        assert_eq!(
            store
                .credential_policy_snapshot(source.request.binding_id)
                .expect("canonical snapshot should decode")
                .expect("canonical snapshot should remain")
                .policy()
                .expect("canonical snapshot should validate")
                .taint_ttl_secs,
            300
        );
    }

    #[test]
    fn verified_transition_reads_share_one_explicit_sqlite_snapshot() {
        for read_pending in [false, true] {
            let database = TestDatabase::new();
            let store = EnforcementStore::open(&database.path).expect("test store should open");
            let source = transition_binding(BindingState::Enforced);
            store
                .upsert_binding(&source)
                .expect("legacy generic source should seed");
            let target = transition_binding(BindingState::Enforced);
            let transition = PolicyTransition::pending(
                TransitionKey {
                    action_id: Uuid::new_v4(),
                    direction: TransitionDirection::Forward,
                },
                ReplacePolicy {
                    expected: source.clone(),
                    source: ReplacementSource::Generic,
                    replacement: ReplacementPolicy::Generic(target.request),
                },
            );
            store
                .begin_transition(&transition)
                .expect("transition should seed");

            let writer = EnforcementStore::open(&database.path)
                .expect("independent fixture writer should open");
            let source_for_writer = source.clone();
            let observed_explicit_transaction = Arc::new(AtomicBool::new(false));
            let writer_committed = Arc::new(AtomicBool::new(false));
            let observed_for_hook = Arc::clone(&observed_explicit_transaction);
            let committed_for_hook = Arc::clone(&writer_committed);
            transition::set_transition_read_hook(move |reader| {
                observed_for_hook.store(!reader.is_autocommit(), Ordering::SeqCst);
                writer
                    .upsert_credential_binding(&source_for_writer, &credential_policy(300))
                    .expect("fixture canonical snapshot should commit");
                committed_for_hook.store(true, Ordering::SeqCst);
            });

            let recovered = if read_pending {
                let pending = store
                    .pending_transitions()
                    .expect("pending recovery should keep its original snapshot");
                assert_eq!(pending.len(), 1);
                pending.into_iter().next()
            } else {
                store
                    .transition(&transition.key)
                    .expect("single lookup should keep its original snapshot")
            }
            .expect("seeded transition should exist");

            assert!(writer_committed.load(Ordering::SeqCst));
            assert!(observed_explicit_transaction.load(Ordering::SeqCst));
            assert_eq!(recovered.key, transition.key);
            assert_eq!(recovered.request.source, ReplacementSource::Generic);
            assert_eq!(
                store
                    .credential_policy_snapshot(source.request.binding_id)
                    .expect("concurrent snapshot should decode")
                    .expect("concurrent snapshot should exist")
                    .policy()
                    .expect("concurrent snapshot should validate")
                    .taint_ttl_secs,
                300
            );
        }
    }

    fn transition(source: Binding, target: &Binding) -> PolicyTransition {
        PolicyTransition::pending(
            TransitionKey {
                action_id: Uuid::new_v4(),
                direction: TransitionDirection::Forward,
            },
            ReplacePolicy {
                expected: source,
                source: ReplacementSource::Generic,
                replacement: ReplacementPolicy::Generic(target.request.clone()),
            },
        )
    }

    #[test]
    fn completing_transition_updates_ownership_atomically() {
        let store = EnforcementStore::open(":memory:").expect("test store should open");
        let source = transition_binding(BindingState::Enforced);
        let target = transition_binding(BindingState::Enforced);
        let transition = transition(source.clone(), &target);
        store.upsert_binding(&source).expect("source should seed");
        store
            .begin_transition(&transition)
            .expect("transition should begin");

        store
            .complete_transition(&transition.key, &target)
            .expect("transition should complete");

        assert_eq!(
            store
                .binding(source.request.binding_id)
                .expect("source should load")
                .expect("source should exist")
                .state,
            BindingState::Detached
        );
        assert_eq!(
            store
                .binding(target.request.binding_id)
                .expect("target should load"),
            Some(target)
        );
        assert_eq!(
            store
                .transition(&transition.key)
                .expect("transition should load")
                .expect("transition should exist")
                .phase,
            TransitionPhase::Completed
        );
    }

    #[test]
    fn failed_completion_rolls_back_all_transition_writes() {
        let store = EnforcementStore::open(":memory:").expect("test store should open");
        let source = transition_binding(BindingState::Enforced);
        let target = transition_binding(BindingState::Enforced);
        let transition = transition(source.clone(), &target);
        store.upsert_binding(&source).expect("source should seed");
        store
            .begin_transition(&transition)
            .expect("transition should begin");
        let wrong_target = transition_binding(BindingState::Enforced);

        assert!(matches!(
            store.complete_transition(&transition.key, &wrong_target),
            Err(EnforcementStoreError::TransitionConflict(_))
        ));

        assert_eq!(
            store
                .binding(source.request.binding_id)
                .expect("source should load"),
            Some(source)
        );
        assert_eq!(
            store
                .transition(&transition.key)
                .expect("transition should load")
                .expect("transition should exist")
                .phase,
            TransitionPhase::Pending
        );
        store
            .mark_transition_indeterminate(&transition.key, ReplaceFailureCode::BindingConflict)
            .expect("transition should remain retryable");
    }

    #[test]
    fn source_retained_outcome_remains_retryable_with_typed_acknowledgement() {
        let store = EnforcementStore::open(":memory:").expect("test store should open");
        let source = transition_binding(BindingState::Enforced);
        let target = transition_binding(BindingState::Enforced);
        let transition = transition(source.clone(), &target);
        store.upsert_binding(&source).expect("source should seed");
        store
            .begin_transition(&transition)
            .expect("transition should begin");

        store
            .retain_transition(&transition.key, &source, ReplaceFailureCode::KernelFailure)
            .expect("source-retained outcome should persist");

        let retained = store
            .transition(&transition.key)
            .expect("transition should load")
            .expect("transition should exist");
        assert_eq!(retained.phase, TransitionPhase::Pending);
        assert_eq!(retained.acknowledgement, Some(source));
        assert_eq!(
            retained.failure_code,
            Some(ReplaceFailureCode::KernelFailure)
        );
        assert_eq!(store.pending_transitions().unwrap(), [retained]);
    }

    #[test]
    fn completion_rejects_target_on_a_different_runtime_domain() {
        let store = EnforcementStore::open(":memory:").expect("test store should open");
        let source = transition_binding(BindingState::Enforced);
        let mut target = transition_binding(BindingState::Enforced);
        let transition = transition(source.clone(), &target);
        target.domain_id = Some(source.domain_id.expect("source domain should exist") + 1);
        store.upsert_binding(&source).expect("source should seed");
        store
            .begin_transition(&transition)
            .expect("transition should begin");

        assert!(matches!(
            store.complete_transition(&transition.key, &target),
            Err(EnforcementStoreError::TransitionConflict(_))
        ));
        assert_eq!(
            store.transition(&transition.key).unwrap().unwrap().phase,
            TransitionPhase::Pending
        );
        assert_eq!(
            store.binding(source.request.binding_id).unwrap(),
            Some(source)
        );
    }

    fn raw_violation(path: &Path, event_id: Uuid) -> (i64, String) {
        Connection::open(path)
            .expect("test database should open")
            .query_row(
                "SELECT occurred_at_ns, event_json
                 FROM enforcement_violations WHERE event_id = ?1",
                params![event_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("test violation should exist")
    }

    #[test]
    fn open_migrates_legacy_violation_idempotently_and_repairs_ordering() {
        let database = TestDatabase::new();
        let legacy = violation(LEGACY_OCCURRED_AT_NS, LEGACY_OBSERVED_AT_NS);
        let valid = violation(VALID_OCCURRED_AT_NS, VALID_OCCURRED_AT_NS + 10);
        let store = EnforcementStore::open(&database.path).expect("test store should open");
        store
            .insert_violation(&legacy)
            .expect("legacy violation should insert");
        store
            .insert_violation(&valid)
            .expect("valid violation should insert");
        drop(store);

        let valid_before = raw_violation(&database.path, valid.event_id);
        let reopened = EnforcementStore::open(&database.path).expect("test store should reopen");
        let events = reopened
            .violations(10)
            .expect("migrated violations should load");
        assert_eq!(events[0].event_id, legacy.event_id);
        assert_eq!(events[0].occurred_at_ns, LEGACY_OBSERVED_AT_NS);
        assert_eq!(events[1], valid);
        drop(reopened);

        let legacy_after_first_open = raw_violation(&database.path, legacy.event_id);
        assert_eq!(legacy_after_first_open.0, LEGACY_OBSERVED_AT_NS as i64);
        let migrated_json: ViolationEvent = serde_json::from_str(&legacy_after_first_open.1)
            .expect("migrated event JSON should deserialize");
        assert_eq!(migrated_json.occurred_at_ns, LEGACY_OBSERVED_AT_NS);
        assert_eq!(raw_violation(&database.path, valid.event_id), valid_before);

        drop(EnforcementStore::open(&database.path).expect("test store should reopen twice"));
        assert_eq!(
            raw_violation(&database.path, legacy.event_id),
            legacy_after_first_open
        );
        assert_eq!(raw_violation(&database.path, valid.event_id), valid_before);
    }

    #[test]
    fn open_propagates_malformed_legacy_event_json() {
        let database = TestDatabase::new();
        let legacy = violation(LEGACY_OCCURRED_AT_NS, LEGACY_OBSERVED_AT_NS);
        let store = EnforcementStore::open(&database.path).expect("test store should open");
        store
            .insert_violation(&legacy)
            .expect("legacy violation should insert");
        drop(store);
        Connection::open(&database.path)
            .expect("test database should open")
            .execute(
                "UPDATE enforcement_violations SET event_json = '{' WHERE event_id = ?1",
                params![legacy.event_id.to_string()],
            )
            .expect("legacy JSON should be corrupted");

        match EnforcementStore::open(&database.path) {
            Err(EnforcementStoreError::Json(_)) => {}
            Err(error) => panic!("expected JSON error, got {error}"),
            Ok(_) => panic!("malformed legacy JSON should fail store open"),
        }
    }
}
