//! Durable product-level credential-policy intent and replay metadata.

use agentsight_enforcement_protocol::{
    ApplyCredentialPolicy, Binding, BindingState, CredentialExfiltrationPolicy,
    CredentialPolicySnapshot,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

use super::{
    EnforcementStore, EnforcementStoreError, now_ns, sqlite_i64, state_name, upsert_binding_on,
};

/// Structured credential request paired with its durable lifecycle state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CredentialPolicyIntent {
    /// Exact product request replayed through the credential-specific adapter path.
    pub(crate) request: ApplyCredentialPolicy,
    /// Latest desired lifecycle state mirrored from the compiled binding.
    pub(crate) state: BindingState,
}

impl EnforcementStore {
    /// Persists a structured credential intent and immutable snapshot before mutation.
    ///
    /// # Errors
    ///
    /// Returns a validation, immutable-intent conflict, mutex, JSON, or SQLite error.
    pub(crate) fn begin_credential_policy_intent(
        &self,
        request: &ApplyCredentialPolicy,
    ) -> Result<CredentialPolicyIntent, EnforcementStoreError> {
        validate_request(request)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = credential_policy_intent_on(&transaction, request.binding_id)?;
        if let Some(existing) = &existing {
            if existing.request != *request {
                return Err(EnforcementStoreError::CredentialIntentConflict(
                    request.binding_id,
                ));
            }
        }
        let updated_at_ns = now_ns();
        persist_intent_snapshot_on(
            &transaction,
            request.binding_id,
            &request.policy,
            updated_at_ns,
        )?;
        if let Some(existing) = existing
            && matches!(
                existing.state,
                BindingState::Detaching | BindingState::Detached
            )
        {
            transaction.commit()?;
            return Ok(existing);
        }
        upsert_credential_intent_on(&transaction, request, BindingState::Pending, updated_at_ns)?;
        let intent = credential_policy_intent_on(&transaction, request.binding_id)?
            .ok_or(EnforcementStoreError::MissingBinding(request.binding_id))?;
        transaction.commit()?;
        Ok(intent)
    }

    /// Lists structured credential intents in stable binding order.
    ///
    /// # Errors
    ///
    /// Returns a mutex, SQLite, JSON, validation, or stored-state error.
    pub(crate) fn credential_policy_intents(
        &self,
    ) -> Result<Vec<CredentialPolicyIntent>, EnforcementStoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT i.request_json, i.state, s.snapshot_json
             FROM enforcement_credential_policy_intents AS i
             LEFT JOIN enforcement_credential_policy_snapshots AS s
               ON s.binding_id = i.binding_id
             ORDER BY i.binding_id ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;
        let mut intents = Vec::new();
        for row in rows {
            intents.push(decode_intent(row?)?);
        }
        Ok(intents)
    }

    /// Records one terminal adapter rejection without discarding typed provenance.
    ///
    /// # Errors
    ///
    /// Returns a missing-intent, mutex, SQLite, or JSON error.
    pub(crate) fn mark_credential_policy_intent_failed(
        &self,
        binding_id: Uuid,
        message: &str,
    ) -> Result<(), EnforcementStoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(intent) = credential_policy_intent_on(&transaction, binding_id)? else {
            return Err(EnforcementStoreError::MissingBinding(binding_id));
        };
        if let Some(json) = transaction
            .query_row(
                "SELECT desired_json FROM enforcement_bindings WHERE binding_id = ?1",
                params![binding_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            let mut binding: Binding = serde_json::from_str(&json)?;
            binding.state = BindingState::Failed;
            binding.message = Some(message.to_string());
            binding.domain_id = None;
            upsert_binding_on(&transaction, &binding, now_ns())?;
        } else {
            upsert_credential_intent_on(
                &transaction,
                &intent.request,
                BindingState::Failed,
                now_ns(),
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
}

fn persist_intent_snapshot_on(
    connection: &Connection,
    binding_id: Uuid,
    policy: &CredentialExfiltrationPolicy,
    updated_at_ns: u64,
) -> Result<(), EnforcementStoreError> {
    let snapshot = CredentialPolicySnapshot::capture(policy.clone()).map_err(|error| {
        EnforcementStoreError::InvalidCredentialPolicySnapshot {
            binding_id,
            reason: error.to_string(),
        }
    })?;
    let existing_json = connection
        .query_row(
            "SELECT snapshot_json FROM enforcement_credential_policy_snapshots
             WHERE binding_id = ?1",
            params![binding_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(existing_json) = existing_json {
        let existing: CredentialPolicySnapshot =
            serde_json::from_str(&existing_json).map_err(|error| {
                EnforcementStoreError::InvalidCredentialPolicySnapshot {
                    binding_id,
                    reason: error.to_string(),
                }
            })?;
        existing.policy().map_err(|error| {
            EnforcementStoreError::InvalidCredentialPolicySnapshot {
                binding_id,
                reason: error.to_string(),
            }
        })?;
        if existing != snapshot {
            return Err(EnforcementStoreError::InvalidCredentialPolicySnapshot {
                binding_id,
                reason: "immutable snapshot conflicts with the original policy".into(),
            });
        }
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
    Ok(())
}

/// Backfills typed replay metadata for credential snapshots created by older releases.
pub(super) fn migrate_credential_policy_intents(
    connection: &mut Connection,
) -> Result<(), EnforcementStoreError> {
    let transaction = connection.transaction()?;
    let rows = {
        let mut statement = transaction.prepare(
            "SELECT bindings.desired_json, snapshots.snapshot_json
             FROM enforcement_credential_policy_snapshots AS snapshots
             JOIN enforcement_bindings AS bindings
               ON bindings.binding_id = snapshots.binding_id
             ORDER BY snapshots.binding_id ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut values = Vec::new();
        for row in rows {
            values.push(row?);
        }
        values
    };
    for (binding_json, snapshot_json) in rows {
        let binding: Binding = serde_json::from_str(&binding_json)?;
        let snapshot: CredentialPolicySnapshot =
            serde_json::from_str(&snapshot_json).map_err(|error| {
                EnforcementStoreError::InvalidCredentialPolicySnapshot {
                    binding_id: binding.request.binding_id,
                    reason: error.to_string(),
                }
            })?;
        let policy = snapshot.policy().map_err(|error| {
            EnforcementStoreError::InvalidCredentialPolicySnapshot {
                binding_id: binding.request.binding_id,
                reason: error.to_string(),
            }
        })?;
        upsert_credential_intent_from_binding_on(&transaction, &binding, policy, now_ns())?;
    }
    transaction.commit()?;
    Ok(())
}

/// Mirrors a compiled binding state when its ID has structured credential intent.
pub(super) fn sync_credential_intent_state_on(
    connection: &Connection,
    binding_id: Uuid,
    state: BindingState,
    updated_at_ns: u64,
) -> Result<(), EnforcementStoreError> {
    connection.execute(
        "UPDATE enforcement_credential_policy_intents
         SET state = ?1, updated_at_ns = ?2
         WHERE binding_id = ?3",
        params![
            state_name(state),
            sqlite_i64(updated_at_ns),
            binding_id.to_string(),
        ],
    )?;
    Ok(())
}

/// Validates a compiled binding and persists its reconstructable structured request.
pub(super) fn upsert_credential_intent_from_binding_on(
    connection: &Connection,
    binding: &Binding,
    policy: &CredentialExfiltrationPolicy,
    updated_at_ns: u64,
) -> Result<(), EnforcementStoreError> {
    let request = ApplyCredentialPolicy {
        binding_id: binding.request.binding_id,
        agent_id: binding.request.agent_id.clone(),
        session_id: binding.request.session_id.clone(),
        root_pid: binding.request.root_pid,
        process_start_time: binding.request.process_start_time,
        policy: policy.clone(),
    };
    validate_binding(&request, binding)?;
    upsert_credential_intent_on(connection, &request, binding.state, updated_at_ns)
}

/// Checks that an enforced acknowledgement exactly represents a structured request.
pub(crate) fn credential_binding_matches_request(
    request: &ApplyCredentialPolicy,
    binding: &Binding,
) -> bool {
    binding.state == BindingState::Enforced
        && binding.domain_id.is_some()
        && binding.request.binding_id == request.binding_id
        && binding.request.agent_id == request.agent_id
        && binding.request.session_id == request.session_id
        && binding.request.root_pid == request.root_pid
        && binding.request.process_start_time == request.process_start_time
        && binding.request.policy_id == request.policy.policy_id
        && binding.request.policy_revision == request.policy.revision.to_string()
        && binding.request.policy_mode == Some(request.policy.mode)
}

fn validate_request(request: &ApplyCredentialPolicy) -> Result<(), EnforcementStoreError> {
    request.policy.validate().map_err(|error| {
        EnforcementStoreError::InvalidCredentialPolicySnapshot {
            binding_id: request.binding_id,
            reason: error.to_string(),
        }
    })
}

fn validate_binding(
    request: &ApplyCredentialPolicy,
    binding: &Binding,
) -> Result<(), EnforcementStoreError> {
    validate_request(request)?;
    let identity_matches = binding.request.binding_id == request.binding_id
        && binding.request.agent_id == request.agent_id
        && binding.request.session_id == request.session_id
        && binding.request.root_pid == request.root_pid
        && binding.request.process_start_time == request.process_start_time
        && binding.request.policy_id == request.policy.policy_id
        && binding.request.policy_revision == request.policy.revision.to_string()
        && binding.request.policy_mode == Some(request.policy.mode);
    let persisted_non_active = matches!(
        binding.state,
        BindingState::Pending
            | BindingState::Failed
            | BindingState::Degraded
            | BindingState::Detaching
            | BindingState::Detached
    );
    if credential_binding_matches_request(request, binding)
        || (persisted_non_active && identity_matches)
    {
        Ok(())
    } else {
        Err(EnforcementStoreError::InvalidCredentialPolicySnapshot {
            binding_id: request.binding_id,
            reason: "compiled acknowledgement does not match structured intent".into(),
        })
    }
}

fn upsert_credential_intent_on(
    connection: &Connection,
    request: &ApplyCredentialPolicy,
    state: BindingState,
    updated_at_ns: u64,
) -> Result<(), EnforcementStoreError> {
    validate_request(request)?;
    if let Some(existing) = credential_policy_intent_on(connection, request.binding_id)? {
        if existing.request != *request {
            return Err(EnforcementStoreError::CredentialIntentConflict(
                request.binding_id,
            ));
        }
    }
    connection.execute(
        "INSERT INTO enforcement_credential_policy_intents
           (binding_id, request_json, state, updated_at_ns)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(binding_id) DO UPDATE SET
           state = excluded.state,
           updated_at_ns = excluded.updated_at_ns",
        params![
            request.binding_id.to_string(),
            serde_json::to_string(request)?,
            state_name(state),
            sqlite_i64(updated_at_ns),
        ],
    )?;
    Ok(())
}

fn credential_policy_intent_on(
    connection: &Connection,
    binding_id: Uuid,
) -> Result<Option<CredentialPolicyIntent>, EnforcementStoreError> {
    let row = connection
        .query_row(
            "SELECT i.request_json, i.state, s.snapshot_json
             FROM enforcement_credential_policy_intents AS i
             LEFT JOIN enforcement_credential_policy_snapshots AS s
               ON s.binding_id = i.binding_id
             WHERE i.binding_id = ?1",
            params![binding_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?;
    row.map(decode_intent).transpose()
}

fn decode_intent(
    row: (String, String, Option<String>),
) -> Result<CredentialPolicyIntent, EnforcementStoreError> {
    let request: ApplyCredentialPolicy = serde_json::from_str(&row.0)?;
    validate_request(&request)?;
    let snapshot_json =
        row.2
            .ok_or_else(|| EnforcementStoreError::InvalidCredentialPolicySnapshot {
                binding_id: request.binding_id,
                reason: "durable credential intent is missing its immutable snapshot".into(),
            })?;
    let snapshot: CredentialPolicySnapshot =
        serde_json::from_str(&snapshot_json).map_err(|error| {
            EnforcementStoreError::InvalidCredentialPolicySnapshot {
                binding_id: request.binding_id,
                reason: error.to_string(),
            }
        })?;
    let policy = snapshot.policy().map_err(|error| {
        EnforcementStoreError::InvalidCredentialPolicySnapshot {
            binding_id: request.binding_id,
            reason: error.to_string(),
        }
    })?;
    if policy != &request.policy {
        return Err(EnforcementStoreError::InvalidCredentialPolicySnapshot {
            binding_id: request.binding_id,
            reason: "durable credential intent does not match its immutable snapshot".into(),
        });
    }
    Ok(CredentialPolicyIntent {
        request,
        state: parse_state(&row.1)?,
    })
}

fn parse_state(value: &str) -> Result<BindingState, EnforcementStoreError> {
    match value {
        "pending" => Ok(BindingState::Pending),
        "enforced" => Ok(BindingState::Enforced),
        "failed" => Ok(BindingState::Failed),
        "degraded" => Ok(BindingState::Degraded),
        "detaching" => Ok(BindingState::Detaching),
        "detached" => Ok(BindingState::Detached),
        _ => Err(EnforcementStoreError::InvalidTransitionState {
            field: "credential_intent_state",
            value: value.into(),
        }),
    }
}
