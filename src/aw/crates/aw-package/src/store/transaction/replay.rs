//! Replay compares the actual database with both journal snapshots before touching payloads.

use super::{
    fs,
    journal::{self, Journal},
    RecoveryReport,
};
use crate::{database::DatabasePaths, require, Error, Result};
use std::{path::Path, sync::atomic::AtomicBool};

pub(super) fn recover(paths: &DatabasePaths, cancel: &AtomicBool) -> Result<RecoveryReport> {
    journal::check_cancel(cancel)?;
    match fs::directory(&paths.root) {
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RecoveryReport::default())
        }
        Err(error) => return Err(error),
        Ok(_) => fs::owned(&paths.root)?,
    }
    let _database_lock = fs::lock(&paths.root.join("lock"), false)?;
    require(
        journal::current_database(paths)?.is_some(),
        format!(
            "incomplete database bootstrap; preserve {}",
            paths.root.display()
        ),
    )?;
    let transactions = paths.root.join("transactions");
    let entries = match fs::entries(&transactions) {
        Ok(entries) => entries,
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    let mut report = RecoveryReport::default();
    for directory in entries {
        journal::check_cancel(cancel)?;
        match classify(&directory)? {
            PendingDirectory::Retired => continue,
            PendingDirectory::Conflict => {
                report.conflicts.push(directory);
                continue;
            }
            PendingDirectory::Journal => {}
        }
        let entry = journal::read(paths, &directory)?;
        let committed = decide(paths, &entry)?;
        let prefix_exists = match fs::directory(&entry.prefix) {
            Ok(_) => true,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error),
        };
        let _prefix_lock = if prefix_exists {
            fs::owned(&entry.prefix)?;
            let actual = fs::directory_identity(&entry.prefix)?;
            if entry
                .prefix_identity
                .as_ref()
                .is_none_or(|expected| actual.dev != expected.dev || actual.ino != expected.ino)
            {
                report.conflicts.push(entry.prefix.clone());
                continue;
            }
            let lock = entry.prefix.join(".aw-install.lock");
            if let Some(expected) = &entry.lock_identity {
                let readable = committed
                    && journal::readable_lock(paths, &entry)
                        .is_some_and(|after| fs::same(&lock, &after).unwrap_or(false));
                if !fs::same(&lock, expected).unwrap_or(false) && !readable {
                    // An earlier replay may remove our new lock before an unknown file blocks
                    // rmdir. Only already-restored payloads permit a lock-free cleanup retry.
                    if entry.lock_created
                        && !committed
                        && matches!(fs::identity(&lock), Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound)
                        && restored(&entry)
                    {
                        None
                    } else {
                        report.conflicts.push(lock);
                        continue;
                    }
                } else {
                    Some(fs::lock(&lock, false)?)
                }
            } else if fs::exists(&lock).unwrap_or(true) {
                report.conflicts.push(lock);
                continue;
            } else {
                None
            }
        } else {
            None
        };
        let conflicts = finish(paths, &entry, committed)?;
        if !conflicts.is_empty() {
            report.conflicts.extend(conflicts);
            continue;
        }
        if committed {
            report.committed += 1;
        } else {
            report.rolled_back += 1;
        }
    }
    Ok(report)
}

enum PendingDirectory {
    Retired,
    Conflict,
    Journal,
}

fn classify(directory: &Path) -> Result<PendingDirectory> {
    if !directory.file_name().is_some_and(journal::generated_id) || fs::owned(directory).is_err() {
        return Ok(PendingDirectory::Conflict);
    }
    let identity = fs::directory_identity(directory)?;
    if identity.mode != 0o700 {
        return Ok(PendingDirectory::Conflict);
    }
    let entries = fs::entries(directory)?;
    if entries.is_empty() {
        // SIGKILL may interrupt mkdir→first journal or final unlink→rmdir. Empty reserved
        // directories contain no evidence to replay; retire only the observed empty inode.
        return Ok(if fs::rmdir(directory, Some(&identity))? {
            PendingDirectory::Retired
        } else {
            PendingDirectory::Conflict
        });
    }
    Ok(
        if entries.iter().any(|path| {
            path.file_name()
                .is_some_and(|name| name == "journal.json" || name == "journal.next")
        }) {
            PendingDirectory::Journal
        } else {
            PendingDirectory::Conflict
        },
    )
}

fn restored(entry: &Journal) -> bool {
    let mut first = std::collections::BTreeMap::new();
    for action in &entry.actions {
        first.entry(&action.path).or_insert(&action.before);
    }
    first.into_iter().all(
        |(path, before)| match fs::identity(&entry.prefix.join(path)) {
            Ok(identity) => before.as_ref() == Some(&identity),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                before.is_none()
            }
            Err(_) => false,
        },
    )
}

pub(super) fn decide(paths: &DatabasePaths, entry: &Journal) -> Result<bool> {
    let committed = observed(paths, entry)?;
    if committed {
        // A rename can be visible even when its directory fsync failed. Recovery
        // must establish that barrier before deleting backups or the journal.
        fs::confirm_replacement(&entry.directory(), &paths.installed())?;
    }
    Ok(committed)
}

pub(super) fn observed(paths: &DatabasePaths, entry: &Journal) -> Result<bool> {
    let current = journal::current_database(paths)?;
    if current.as_deref() == Some(entry.after_database.as_slice()) {
        return Ok(true);
    }
    require(
        current == entry.before_database,
        format!(
            "database differs from both transaction snapshots; preserve {}",
            entry.directory().display()
        ),
    )?;
    Ok(false)
}

pub(super) fn finish(
    paths: &DatabasePaths,
    entry: &Journal,
    committed: bool,
) -> Result<Vec<std::path::PathBuf>> {
    let mut conflicts = Vec::new();
    if committed {
        if let (Some(before), Some(after)) =
            (&entry.lock_identity, journal::readable_lock(paths, entry))
        {
            let lock = entry.prefix.join(".aw-install.lock");
            if !fs::make_readable(&lock, before, &after)? {
                return Ok(vec![lock]);
            }
            journal::checkpoint("lock-readable");
        }
    }
    for (index, action) in entry.actions.iter().enumerate().rev() {
        // Closing the historical writer is undone only after every other rollback is safe.
        if !committed
            && index == 0
            && action.path == ".aw-packages/aw-core.json"
            && !conflicts.is_empty()
        {
            continue;
        }
        let destination = entry.prefix.join(&action.path);
        let backup = entry.workspace().join(&action.backup);
        let stage = entry.workspace().join(&action.stage);
        if !action.active {
            remove_if_owned(&stage, action.after.as_ref(), &mut conflicts)?;
            continue;
        }
        if committed {
            remove_known(
                &backup,
                action.before.as_ref(),
                action.after.as_ref(),
                &mut conflicts,
            )?;
        } else if let Some(before) = &action.before {
            restore(
                &destination,
                &backup,
                before,
                action.after.as_ref(),
                &mut conflicts,
            )?;
        } else {
            remove_if_owned(&destination, action.after.as_ref(), &mut conflicts)?;
        }
        remove_known(
            &stage,
            action.after.as_ref(),
            action.before.as_ref(),
            &mut conflicts,
        )?;
    }
    let database_stage = entry.directory().join("database.after");
    remove_if_owned(
        &database_stage,
        entry.database_stage.as_ref(),
        &mut conflicts,
    )?;
    if !conflicts.is_empty() {
        return Ok(conflicts);
    }
    let workspace = entry.workspace();
    match fs::directory(&workspace) {
        Ok(_) if !fs::rmdir(&workspace, entry.workspace_identity.as_ref())? => {
            conflicts.push(workspace)
        }
        Ok(_) => {}
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => conflicts.push(workspace),
    }
    if !conflicts.is_empty() {
        return Ok(conflicts);
    }
    if !committed {
        let mut directories = entry.directories.iter().collect::<Vec<_>>();
        directories.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));
        for (name, identity) in directories {
            let _removed = fs::rmdir(&entry.prefix.join(name), Some(identity))?;
        }
        if entry.lock_created {
            remove_if_owned(
                &entry.prefix.join(".aw-install.lock"),
                entry.lock_identity.as_ref(),
                &mut conflicts,
            )?;
        }
        if entry.prefix_created && !fs::rmdir(&entry.prefix, entry.prefix_identity.as_ref())? {
            conflicts.push(entry.prefix.clone());
        }
    }
    if !conflicts.is_empty() {
        return Ok(conflicts);
    }
    let directory = entry.directory();
    let extras = fs::entries(&directory)?
        .into_iter()
        .filter(|path| path != &entry.path())
        .collect::<Vec<_>>();
    if !extras.is_empty() {
        return Ok(extras);
    }
    let directory_identity = fs::directory_identity(&directory)?;
    let journal_identity = fs::identity(&entry.path())?;
    fs::unlink(&entry.path(), &journal_identity)?;
    journal::checkpoint("journal-file-removed");
    if !fs::rmdir(&directory, Some(&directory_identity))? {
        conflicts.push(directory);
    }
    Ok(conflicts)
}

fn remove_if_owned(
    path: &Path,
    identity: Option<&fs::Identity>,
    conflicts: &mut Vec<std::path::PathBuf>,
) -> Result<()> {
    remove_known(path, identity, None, conflicts)
}

fn remove_known(
    path: &Path,
    first: Option<&fs::Identity>,
    second: Option<&fs::Identity>,
    conflicts: &mut Vec<std::path::PathBuf>,
) -> Result<()> {
    match fs::identity(path) {
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(actual) if first == Some(&actual) || second == Some(&actual) => {
            fs::unlink(path, &actual)
        }
        _ => {
            conflicts.push(path.to_path_buf());
            Ok(())
        }
    }
}

fn restore(
    destination: &Path,
    backup: &Path,
    before: &fs::Identity,
    after: Option<&fs::Identity>,
    conflicts: &mut Vec<std::path::PathBuf>,
) -> Result<()> {
    let current = match fs::identity(destination) {
        Ok(identity) => Some(identity),
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => {
            conflicts.push(destination.into());
            return Ok(());
        }
    };
    if current.as_ref() == Some(before) {
        return remove_known(backup, Some(before), after, conflicts);
    }
    if current.is_some() && current.as_ref() != after {
        conflicts.push(destination.into());
        return Ok(());
    }
    if !fs::same(backup, before).unwrap_or(false) {
        conflicts.push(backup.into());
        return Ok(());
    }
    if current.is_none() {
        fs::move_noreplace(backup, destination)?;
    } else {
        fs::exchange(backup, destination)?;
        // A concurrent replacement is retained in backup instead of being overwritten or deleted.
        remove_known(backup, after, None, conflicts)?;
    }
    if !fs::same(destination, before).unwrap_or(false) {
        conflicts.push(destination.into());
    }
    Ok(())
}
