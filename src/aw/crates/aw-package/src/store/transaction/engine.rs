//! Execute a verified plan while both database and prefix locks remain held through cleanup.

use super::{
    fs,
    journal::{self, Journal},
    prepare, replay, ChangeSet,
};
use crate::store::{
    database::DatabasePaths, require, Error, MutationFailure, MutationNotice, MutationReport,
    MutationResult, NoticeCode, RecoveryState, Result,
};
use aw_exec::PackageLock;
use std::{path::Path, sync::atomic::AtomicBool};

pub(super) fn apply(
    paths: &DatabasePaths,
    changes: &ChangeSet,
    cancel: &AtomicBool,
) -> MutationResult {
    let mut report = MutationReport::default();
    journal::check_cancel(cancel).map_err(|error| MutationFailure {
        error,
        report: MutationReport::default(),
    })?;
    let mut prepared = prepare::begin(paths, changes).map_err(|error| MutationFailure {
        error,
        report: MutationReport::default(),
    })?;
    let result = execute(
        paths,
        changes,
        &mut prepared.journal,
        &mut prepared.prefix_lock,
        cancel,
    );
    let latest = match journal::read(paths, &prepared.journal.directory()) {
        Ok(latest) => latest,
        Err(error) => {
            report.recovery = RecoveryState::Incomplete;
            retained(&mut report, &prepared.journal.directory());
            return Err(MutationFailure {
                error: result.err().unwrap_or(error),
                report,
            });
        }
    };
    let decision = if result.is_err() {
        replay::observed(paths, &latest)
    } else {
        replay::decide(paths, &latest)
    };
    let committed = match decision {
        Ok(committed) => committed,
        Err(error) => {
            report.recovery = RecoveryState::Incomplete;
            retained(&mut report, &latest.directory());
            return Err(MutationFailure {
                error: result.err().unwrap_or(error),
                report,
            });
        }
    };
    let result = match result {
        Err(error) if committed => {
            // Failed commit fsync leaves only visibility evidence. Keep both sides
            // until explicit recovery confirms durability or observes the old database.
            report.recovery = RecoveryState::Incomplete;
            retained(&mut report, &latest.directory());
            return Err(MutationFailure { error, report });
        }
        result => result,
    };
    report.committed = committed;
    if report.committed {
        journal::checkpoint("precleanup");
    }
    match replay::finish(paths, &latest, report.committed) {
        Ok(conflicts) if conflicts.is_empty() => {
            if result.is_err() {
                report.recovery = RecoveryState::Complete;
            }
        }
        Ok(conflicts) => {
            report.recovery = RecoveryState::Incomplete;
            for path in conflicts {
                retained(&mut report, &path);
            }
        }
        Err(error) => {
            report.recovery = RecoveryState::Incomplete;
            retained(&mut report, &latest.directory());
            return Err(MutationFailure {
                error: result.err().unwrap_or(error),
                report,
            });
        }
    }
    match result {
        Ok(()) => Ok(report),
        Err(error) => Err(MutationFailure { error, report }),
    }
}

fn retained(report: &mut MutationReport, path: &Path) {
    report.notices.push(MutationNotice {
        code: NoticeCode::BackupRetained,
        path: path.into(),
        io_kind: None,
    });
}

fn execute(
    paths: &DatabasePaths,
    changes: &ChangeSet,
    entry: &mut Journal,
    prefix_lock: &mut Option<PackageLock>,
    cancel: &AtomicBool,
) -> Result<()> {
    journal::check_cancel(cancel)?;
    if entry.prefix_created {
        fs::mkdir(&entry.prefix, 0o755)?;
        entry.prefix_identity = Some(fs::directory_identity(&entry.prefix)?);
        entry.write()?;
    }
    if prefix_lock.is_none() {
        *prefix_lock = Some(fs::new_lock(&entry.prefix.join(".local-install.lock"))?);
        entry.lock_identity = Some(fs::identity(&entry.prefix.join(".local-install.lock"))?);
        entry.write()?;
    }
    require(
        journal::current_database(paths)? == entry.before_database,
        "package database changed while acquiring the prefix lock",
    )?;
    prepare::recheck(changes)?;
    fs::mkdir(&entry.workspace(), 0o700)?;
    entry.workspace_identity = Some(fs::directory_identity(&entry.workspace())?);
    entry.write()?;
    let locator = changes.marker_after.clone();
    let first_is_locator = locator.is_some() && locator != changes.marker_before;
    for index in 0..entry.actions.len() {
        journal::check_cancel(cancel)?;
        let name = entry.actions[index].path.clone();
        if name == ".local-packages/installation.json" && index == 0 && first_is_locator {
            prepare::stage_bytes(
                entry,
                index,
                locator
                    .as_deref()
                    .ok_or_else(|| Error::Invalid("missing registration locator".into()))?,
            )?;
        } else if let Some(source) = changes.writes.get(&name) {
            prepare::stage_source(entry, index, source)?;
        }
        if entry.actions[index].after.is_some() {
            prepare::parents(entry, Path::new(&name))?;
        }
        let action = entry.actions[index].clone();
        let destination = entry.prefix.join(&action.path);
        match &action.before {
            Some(before) => require(
                fs::same(&destination, before)?,
                format!(
                    "owned file changed before mutation: {}",
                    destination.display()
                ),
            )?,
            None => require(
                !fs::exists(&destination)?,
                format!(
                    "new file appeared before mutation: {}",
                    destination.display()
                ),
            )?,
        }
        // This durable intent precedes either namespace change; replay checks inode plus contents.
        entry.actions[index].active = true;
        entry.write()?;
        let workspace = entry.workspace();
        let backup = workspace.join(&action.backup);
        let stage = workspace.join(&action.stage);
        match (&action.before, &action.after) {
            (Some(before), Some(after)) => {
                // Keep replacement metadata continuously present, including across SIGKILL.
                fs::link_owned(&destination, &backup, before)?;
                require(fs::same(&stage, after)?, "staged package file changed")?;
                fs::exchange(&stage, &destination)?;
                require(
                    fs::same(&stage, before)?,
                    format!(
                        "file replaced during publication; retained at {}",
                        stage.display()
                    ),
                )?;
                require(
                    fs::same(&destination, after)?,
                    "published package file changed",
                )?;
            }
            (Some(before), None) => {
                fs::move_noreplace(&destination, &backup)?;
                require(
                    fs::same(&backup, before)?,
                    format!(
                        "file replaced during backup; preserved at {}",
                        backup.display()
                    ),
                )?;
            }
            (None, Some(after)) => {
                require(fs::same(&stage, after)?, "staged package file changed")?;
                fs::move_noreplace(&stage, &destination)?;
                require(
                    fs::same(&destination, after)?,
                    "published package file changed",
                )?;
            }
            (None, None) => {}
        }
        if index == 0 && first_is_locator {
            journal::checkpoint("locator-published");
        }
    }
    journal::check_cancel(cancel)?;
    verify_after(entry)?;
    require(
        journal::current_database(paths)? == entry.before_database,
        "package database changed before commit",
    )?;
    let (database_file, identity) =
        fs::write_bytes(&entry.directory(), &entry.after_database, 0o644)?;
    entry.database_stage = Some(identity);
    entry.write()?;
    fs::publish(&database_file, &entry.directory().join("database.after"))?;
    journal::checkpoint("precommit");
    journal::check_cancel(cancel)?;
    require(
        journal::current_database(paths)? == entry.before_database,
        "package database changed at commit",
    )?;
    fs::replace(
        &entry.directory().join("database.after"),
        &paths.installed(),
    )?;
    journal::checkpoint("postcommit");
    Ok(())
}

fn verify_after(entry: &Journal) -> Result<()> {
    let mut final_actions = std::collections::BTreeMap::new();
    for action in &entry.actions {
        final_actions.insert(&action.path, action);
    }
    for action in final_actions.values() {
        let path = entry.prefix.join(&action.path);
        match &action.after {
            Some(after) => require(
                fs::same(&path, after)?,
                format!("installed file changed before commit: {}", path.display()),
            )?,
            None => require(
                !fs::exists(&path)?,
                format!("removed file reappeared before commit: {}", path.display()),
            )?,
        }
    }
    Ok(())
}
