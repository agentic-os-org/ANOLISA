//! Keep admitted prefixes removable within the unchanged recovery journal bounds.

use super::PackagePlan;
use crate::{
    database::{DatabasePaths, RegistrationPointer},
    transaction::{self, ChangeSet},
    Result,
};
#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub actions: usize,
    pub directories: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            actions: transaction::capacity::MAX_ACTIONS,
            directories: transaction::capacity::MAX_DIRECTORIES,
        }
    }
}

pub(super) fn check(
    paths: &DatabasePaths,
    plan: &mut PackagePlan,
    changes: &ChangeSet,
    limits: Limits,
    admission: bool,
) -> Result<()> {
    let actions = transaction::capacity::actions(paths, changes)?;
    if actions > limits.actions {
        plan.problem(
            "transaction_limit",
            format!(
                "operation requires {actions} journal actions; maximum is {}",
                limits.actions
            ),
            None,
            Some(plan.prefix.clone()),
        );
    }
    if admission {
        let files = changes
            .database_after
            .installations
            .iter()
            .filter(|installation| installation.prefix == changes.prefix)
            .flat_map(|installation| &installation.packages)
            .map(|package| package.files.len())
            .sum::<usize>();
        // Reserve a locator rewrite even when today's identical bytes need no action.
        // Later whole-prefix removal must remain possible after metadata re-encoding.
        if files >= limits.actions {
            plan.problem(
                "installation_limit",
                format!("combined installation has {files} payload files; journal capacity {} must also retain one locator action", limits.actions),
                None,
                Some(plan.prefix.clone()),
            );
        }
    }
    let directories = transaction::capacity::directories(paths, changes)?;
    if directories > limits.directories {
        plan.problem(
            "transaction_limit",
            format!(
                "operation creates {directories} journaled directories; maximum is {}",
                limits.directories
            ),
            None,
            Some(plan.prefix.clone()),
        );
    }
    if plan.ready() {
        check_byte_limit(
            paths,
            plan,
            changes,
            admission,
            transaction::capacity::MAX_BYTES,
        )?;
    }
    Ok(())
}

fn check_byte_limit(
    paths: &DatabasePaths,
    plan: &mut PackagePlan,
    changes: &ChangeSet,
    admission: bool,
    byte_limit: u64,
) -> Result<()> {
    let bytes = transaction::capacity::encoded_bytes(paths, changes, false)?;
    if bytes > byte_limit {
        plan.problem(
            "transaction_limit",
            format!(
                "operation's conservative journal bound is {bytes} bytes; maximum is {byte_limit}"
            ),
            None,
            Some(plan.prefix.clone()),
        );
    }
    if admission {
        let mut removal = changes.clone();
        removal.database_before = Some(serde_json::to_vec(&changes.database_after)?);
        removal.database_after.generation = removal
            .database_after
            .generation
            .checked_add(1)
            .ok_or_else(|| crate::Error::Invalid("package database generation exhausted".into()))?;
        let installation = removal
            .database_after
            .installations
            .iter_mut()
            .find(|installation| installation.prefix == changes.prefix)
            .ok_or_else(|| crate::Error::Invalid("planned installation is absent".into()))?;
        removal.removes = installation
            .packages
            .iter()
            .flat_map(|package| package.files.clone())
            .collect();
        removal.verify = removal.removes.clone();
        installation.packages.clear();
        removal.writes.clear();
        removal.legacy.clear();
        removal.source_records.clear();
        let pointer = RegistrationPointer::new(paths, changes.installation_id.clone())?;
        // Reserve a future locator rewrite even when the installed encoding currently matches.
        removal.marker_before = Some(serde_json::to_vec(&pointer)?);
        removal.marker_after = Some(serde_json::to_vec_pretty(&pointer)?);
        let bytes = transaction::capacity::encoded_bytes(paths, &removal, true)?;
        if bytes > byte_limit {
            plan.problem(
                "installation_limit",
                format!("resulting installation reserves {bytes} journal bytes for whole-prefix removal, including maximum database snapshots; maximum is {byte_limit}"),
                None,
                Some(plan.prefix.clone()),
            );
        }
    }
    Ok(())
}
