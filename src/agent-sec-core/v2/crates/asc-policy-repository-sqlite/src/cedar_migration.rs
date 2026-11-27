//! Normalize persisted Cedar references before recovery sees any Policy, Scope or Binding.
use asc_policy_repository::StoreError;
use asc_policy_types::{authoring::PolicySource, policy::PreparedPolicy};
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::Value;

use crate::RepositoryError;

pub(super) fn upgrade(tx: &Transaction<'_>) -> Result<(), RepositoryError> {
    tx.execute_batch("CREATE TABLE cedar_source_history (
        policy_id TEXT NOT NULL, revision INTEGER NOT NULL, source_json TEXT NOT NULL CHECK(json_valid(source_json)),
        PRIMARY KEY(policy_id, revision)) STRICT")?;
    let mut changed_current = false;
    for (table, column) in [
        ("policies", "current_json"),
        ("scopes", "assignment_json"),
        ("bindings", "spec_json"),
    ] {
        let mut next = tx.prepare(&format!("SELECT rowid,{column} FROM {table} WHERE rowid>?1 AND {column} IS NOT NULL ORDER BY rowid LIMIT 1"))?;
        let mut cursor = 0i64;
        while let Some((id, json)) = next
            .query_row([cursor], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .optional()?
        {
            cursor = id;
            let mut record: Value = serde_json::from_str(&json).map_err(|_| StoreError::Corrupt)?;
            let changed = match table {
                "policies" => normalize(tx, &mut record, true)?,
                "bindings" => normalize(
                    tx,
                    record.get_mut("policy").ok_or(StoreError::Corrupt)?,
                    false,
                )?,
                _ => {
                    let mut changed = false;
                    for snapshot in record
                        .get_mut("policySnapshots")
                        .and_then(Value::as_array_mut)
                        .ok_or(StoreError::Corrupt)?
                    {
                        changed |= normalize(tx, snapshot, false)?;
                    }
                    changed
                }
            };
            if changed {
                let json = serde_json::to_string(&record).map_err(|_| StoreError::Corrupt)?;
                if table == "policies" {
                    changed_current = true;
                    if tx.execute("UPDATE policies SET current_json=?1,last_allocated_revision=json_extract(?1,'$.revision')
                        WHERE rowid=?2 AND last_allocated_revision=json_extract(current_json,'$.revision')
                        AND policy_id=json_extract(current_json,'$.policyId')",params![json,id])? != 1 {
                        return Err(StoreError::Corrupt.into());
                    }
                } else {
                    tx.execute(
                        &format!("UPDATE {table} SET {column}=?1 WHERE rowid=?2"),
                        params![json, id],
                    )?;
                }
            }
        }
    }
    if changed_current {
        tx.execute(
            "UPDATE policy_generation SET generation=generation+1 WHERE singleton=1",
            [],
        )?;
    }
    Ok(())
}

fn normalize(
    tx: &Transaction<'_>,
    value: &mut Value,
    current: bool,
) -> Result<bool, RepositoryError> {
    let mut policy: PreparedPolicy =
        serde_json::from_value(value.clone()).map_err(|_| StoreError::Corrupt)?;
    let PolicySource::Cedar(source) = &mut policy.template else {
        return Ok(false);
    };
    let canonical = asc_policy_cedar::schema::embedded_reference();
    let old = source.schema.revision.get();
    if source.schema.schema_id != canonical.schema_id || !(1..=3).contains(&old) {
        return Err(asc_pap::PapError::InvalidCedar(format!(
            "{} revision {}: unknown stored schema {} revision {}",
            policy.policy_id,
            policy.revision.get(),
            source.schema.schema_id,
            old
        ))
        .into());
    }
    let text =
        asc_policy_cedar::migration::upgrade_text(source, old, current).map_err(|error| {
            asc_pap::PapError::InvalidCedar(format!(
                "{} revision {}: {}",
                policy.policy_id,
                policy.revision.get(),
                error.diagnostic()
            ))
        })?;
    if text.is_none() && old == canonical.revision.get() {
        return Ok(false);
    }
    tx.execute("INSERT INTO cedar_source_history(policy_id,revision,source_json) VALUES(?1,?2,?3) ON CONFLICT DO NOTHING",
        params![policy.policy_id.as_str(), policy.revision.get(), value["template"].to_string()])?;
    source.schema = canonical;
    if let Some(text) = text {
        source.text = text;
    }
    if current {
        policy.revision = policy
            .revision
            .checked_next()
            .map_err(|_| StoreError::Incompatible)?;
    }
    *value = serde_json::to_value(policy).map_err(|_| StoreError::Corrupt)?;
    Ok(true)
}
