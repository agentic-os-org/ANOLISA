//! Shared journal bounds and read-only counts for the exact transaction shape.

use super::{
    fs::Identity,
    journal::{Action, Journal, MAX_JOURNAL_BYTES},
    prepare, ChangeSet,
};
use crate::{database, require, Result};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
    path::Path,
};

#[cfg(test)]
mod tests;

/// Maximum file actions retained in one recoverable journal.
pub(crate) const MAX_ACTIONS: usize = 32768;
/// Maximum newly created payload-parent directories retained in one journal.
pub(crate) const MAX_DIRECTORIES: usize = 32768;

/// Shared encoded-byte ceiling for planning and locked preparation.
pub(crate) const MAX_BYTES: u64 = MAX_JOURNAL_BYTES;

/// Count payload changes and every locator/legacy action used by preparation.
pub(crate) fn actions(paths: &database::DatabasePaths, changes: &ChangeSet) -> Result<usize> {
    let gate = prepare::gate_bytes(paths, changes)?;
    let gate_write = usize::from(gate.is_some() && gate != changes.marker_before);
    let legacy_removals = changes
        .legacy
        .keys()
        .filter(|name| name.as_str() != "aw-core.json")
        .count();
    let marker_removal =
        usize::from(changes.marker_after.is_none() && changes.marker_before.is_some());
    Ok(
        gate_write
            + legacy_removals
            + changes.removes.len()
            + changes.writes.len()
            + marker_removal,
    )
}

/// Count unique absent parents that payload/locator publication would create.
pub(crate) fn directories(paths: &database::DatabasePaths, changes: &ChangeSet) -> Result<usize> {
    Ok(parent_paths(paths, changes, true)?.len())
}

fn parent_paths(
    paths: &database::DatabasePaths,
    changes: &ChangeSet,
    only_missing: bool,
) -> Result<BTreeSet<String>> {
    let gate = prepare::gate_bytes(paths, changes)?;
    let gate_path =
        (gate.is_some() && gate != changes.marker_before).then_some(".aw-packages/aw-core.json");
    let mut parents = BTreeSet::new();
    for name in changes.writes.keys().map(String::as_str).chain(gate_path) {
        for parent in Path::new(name)
            .ancestors()
            .skip(1)
            .filter(|path| !path.as_os_str().is_empty())
        {
            parents.insert(parent.to_path_buf());
        }
    }
    let mut missing = BTreeSet::new();
    for parent in parents {
        if !only_missing {
            missing.insert(parent.to_string_lossy().into_owned());
            continue;
        }
        match database::safe_directory(&changes.prefix.join(&parent)) {
            Ok(_) => {}
            Err(error) if database::is_missing(&error) => {
                missing.insert(parent.to_string_lossy().into_owned());
            }
            Err(error) => return Err(error),
        }
    }
    Ok(missing)
}

/// Refuse an oversized final journal before preparation creates payload directories.
pub(crate) fn check_bytes(paths: &database::DatabasePaths, changes: &ChangeSet) -> Result<()> {
    let bytes = encoded_bytes(paths, changes, false)?;
    require(
        bytes <= MAX_BYTES,
        format!(
            "transaction's conservative journal bound is {bytes} bytes; maximum is {MAX_BYTES}"
        ),
    )
}

/// Count the real serde representation with the largest possible transaction identities.
pub(crate) fn encoded_bytes(
    paths: &database::DatabasePaths,
    changes: &ChangeSet,
    removal: bool,
) -> Result<u64> {
    let entry = bounded_journal(paths, changes, removal)?;
    let mut counter = ByteCounter::default();
    serde_json::to_writer(&mut counter, &entry)?;
    if removal {
        let mut snapshots = ByteCounter::default();
        for bytes in entry
            .before_database
            .iter()
            .chain(std::iter::once(&entry.after_database))
        {
            let text = std::str::from_utf8(bytes).map_err(|error| {
                crate::Error::Invalid(format!("database snapshot is not UTF-8: {error}"))
            })?;
            serde_json::to_writer(&mut snapshots, text)?;
        }
        // Valid JSON text expands by at most two bytes per input byte when encoded
        // as a JSON string, plus its quotes. Other prefixes may fill the database later.
        let reserved = 2 * (2 * database::MAX_DATABASE_BYTES + 2);
        return Ok(counter.0 - snapshots.0 + reserved);
    }
    Ok(counter.0)
}

fn identity(directory: bool) -> Identity {
    Identity {
        dev: u64::MAX,
        ino: u64::MAX,
        sha256: if directory {
            String::new()
        } else {
            "f".repeat(64)
        },
        // Directory and lock metadata may retain special bits; owned payload modes cannot.
        mode: 0o7777,
    }
}

fn bounded_journal(
    paths: &database::DatabasePaths,
    changes: &ChangeSet,
    removal: bool,
) -> Result<Journal> {
    let gate = prepare::gate_bytes(paths, changes)?;
    let gate_write = removal || (gate.is_some() && gate != changes.marker_before);
    let mut actions = Vec::new();
    let mut push = |path: String, before: bool, after: bool| {
        let index = actions.len();
        actions.push(Action {
            path,
            before: before.then(|| identity(false)),
            after: after.then(|| identity(false)),
            stage: format!("file-{index}-new"),
            backup: format!("file-{index}-old"),
            active: false,
        });
    };
    if gate_write {
        push(
            ".aw-packages/aw-core.json".into(),
            removal || changes.marker_before.is_some(),
            true,
        );
    }
    for name in changes
        .legacy
        .keys()
        .filter(|name| name.as_str() != "aw-core.json")
    {
        push(format!(".aw-packages/{name}"), true, false);
    }
    for name in changes.removes.keys() {
        push(name.clone(), true, false);
    }
    for name in changes.writes.keys() {
        push(name.clone(), changes.verify.contains_key(name), true);
    }
    if changes.marker_after.is_none() && changes.marker_before.is_some() {
        // A preceding gate rewrite fills this before identity while execution proceeds.
        push(".aw-packages/aw-core.json".into(), true, false);
    }
    let directories = if removal {
        BTreeMap::from([(".aw-packages".into(), identity(true))])
    } else {
        parent_paths(paths, changes, false)?
            .into_iter()
            .map(|name| (name, identity(true)))
            .collect()
    };
    let before_database = match &changes.database_before {
        Some(bytes) => bytes.clone(),
        None => serde_json::to_vec(&database::InstalledDatabase {
            schema_version: 1,
            generation: 0,
            installations: Vec::new(),
        })?,
    };
    Ok(Journal {
        schema_version: 1,
        transaction_id: format!("txn-{:x}-{:x}-{:x}", u128::MAX, u32::MAX, u64::MAX),
        database_root: paths.root.clone(),
        prefix: changes.prefix.clone(),
        installation_id: changes.installation_id.clone(),
        before_database: Some(before_database),
        after_database: serde_json::to_vec(&changes.database_after)?,
        prefix_created: false,
        prefix_identity: Some(identity(true)),
        workspace_identity: Some(identity(true)),
        lock_created: false,
        lock_identity: Some(identity(false)),
        directories,
        actions,
        database_stage: Some(identity(false)),
    })
}

#[derive(Default)]
struct ByteCounter(u64);

impl Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len() as u64);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
