//! Transactional owned-file installation; existing components and user data are preserved.

mod report;
pub use report::{
    MutationFailure, MutationNotice, MutationReport, MutationResult, NoticeCode, RecoveryState,
};

use crate::{filesystem as paths, inspect, require, Component, Result};
use std::{
    collections::BTreeMap,
    fs::{self, File, Permissions},
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

pub(crate) fn receipts(prefix: &Path) -> Result<BTreeMap<String, Component>> {
    selected_receipts(prefix, None)
}

pub(crate) fn selected_receipts(
    prefix: &Path,
    selected: Option<&[&str]>,
) -> Result<BTreeMap<String, Component>> {
    let directory = prefix.join(".aw-packages");
    paths::absolute(&directory)?;
    let mut components = BTreeMap::new();
    if !directory.exists() {
        return Ok(components);
    }
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            if selected.is_some_and(|names| {
                !names
                    .iter()
                    .any(|name| path.file_stem().is_some_and(|stem| stem == *name))
            }) {
                continue;
            }
            paths::absolute(&path)?;
            let value: Component = serde_json::from_reader(File::open(&path)?)?;
            require(
                matches!(
                    value.component.as_str(),
                    "aw-core" | "aw-provider-sec-core" | "aw-provider-tokenless"
                ) && path
                    .file_stem()
                    .is_some_and(|stem| stem == value.component.as_str())
                    && !components.contains_key(&value.component),
                "invalid package receipt",
            )?;
            components.insert(value.component.clone(), value);
        }
    }
    Ok(components)
}

pub(crate) fn verified_files(
    prefix: &Path,
    installed: &BTreeMap<String, Component>,
) -> Result<Vec<PathBuf>> {
    let mut owned = Vec::new();
    for component in installed.values() {
        for (name, metadata) in &component.files {
            paths::relative(name)?;
            let path = prefix.join(name);
            paths::absolute(&path)?;
            require(
                path.is_file()
                    && path.metadata()?.permissions().mode() & 0o7777 == metadata.mode
                    && crate::digest(&path)? == metadata.sha256,
                format!("installed file modified or missing: {}", path.display()),
            )?;
            owned.push(path);
        }
    }
    Ok(owned)
}

struct Transaction<'a> {
    files: Vec<PathBuf>,
    directories: Vec<PathBuf>,
    prefix: Option<PathBuf>,
    // Keep ownership through Drop so rollback cannot race another installer.
    lock: Option<File>,
    committed: bool,
    report: &'a mut MutationReport,
}

impl Transaction<'_> {
    fn directory(&mut self, path: &Path) -> Result<()> {
        if path.exists() {
            return Ok(());
        }
        let parent = path
            .parent()
            .ok_or_else(|| crate::Error::Invalid("missing parent".into()))?;
        self.directory(parent)?;
        fs::create_dir(path)?;
        self.directories.push(path.into());
        fs::set_permissions(path, Permissions::from_mode(0o755))?;
        Ok(())
    }

    fn create(&mut self, path: &Path) -> Result<File> {
        self.directory(
            path.parent()
                .ok_or_else(|| crate::Error::Invalid("missing parent".into()))?,
        )?;
        let file = paths::create(path, 0o600)?;
        // Register before opening a source or writing a receipt can fail.
        self.files.push(path.into());
        Ok(file)
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if self.files.is_empty() && self.directories.is_empty() && self.prefix.is_none() {
            return;
        }
        self.report.recovery = RecoveryState::Complete;
        for path in self.files.iter().rev() {
            if let Err(error) = fs::remove_file(path) {
                self.report.notice(
                    NoticeCode::RollbackRemoveFailed,
                    path.clone(),
                    Some(error.kind()),
                );
                self.report.recovery = RecoveryState::Incomplete;
            }
        }
        for path in self.directories.iter().rev() {
            if let Err(error) = fs::remove_dir(path) {
                self.report.notice(
                    NoticeCode::RollbackRemoveFailed,
                    path.clone(),
                    Some(error.kind()),
                );
                self.report.recovery = RecoveryState::Incomplete;
            }
        }
        if let Some(prefix) = self
            .prefix
            .as_ref()
            .filter(|_| self.report.recovery == RecoveryState::Complete)
        {
            let cleanup = if self.lock.is_some() {
                fs::remove_file(prefix.join(".aw-install.lock"))
                    .and_then(|()| fs::remove_dir(prefix))
            } else {
                // Before acquiring the lock, retire only an empty owned prefix.
                fs::remove_dir(prefix)
            };
            if let Err(error) = cleanup {
                self.report.notice(
                    NoticeCode::RollbackRetireFailed,
                    prefix.clone(),
                    Some(error.kind()),
                );
                self.report.recovery = RecoveryState::Incomplete;
            }
        }
    }
}

/// Install a verified bundle into a new prefix or add its matching Provider.
/// Never overwrites existing files; failures roll back only this invocation's writes.
pub fn install(bundle: &Path, prefix: &Path) -> Result<()> {
    install_cancellable(bundle, prefix, &AtomicBool::new(false))
}

/// Install with cooperative cancellation, rolling back before committing receipts.
/// The caller owns signal handling; cancellation never interrupts rollback.
pub fn install_cancellable(bundle: &Path, prefix: &Path, cancel: &AtomicBool) -> Result<()> {
    legacy(install_reported(bundle, prefix, cancel))
}

/// Install a Preview bundle and return observed commit and recovery facts.
/// This API never prints diagnostics; the caller owns presentation and cancellation.
///
/// # Errors
/// Retains the primary validation, I/O or cancellation error alongside cleanup notices.
pub fn install_reported(bundle: &Path, prefix: &Path, cancel: &AtomicBool) -> MutationResult {
    install_reported_using(bundle, prefix, cancel, paths::lock, |source, output| {
        let mut source = File::open(source)?;
        let mut buffer = [0; 65536];
        loop {
            check_cancel(cancel)?;
            let count = source.read(&mut buffer)?;
            if count == 0 {
                return Ok(());
            }
            output.write_all(&buffer[..count])?;
        }
    })
}

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(crate::Error::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
fn install_using(
    bundle: &Path,
    prefix: &Path,
    cancel: &AtomicBool,
    lock: impl FnOnce(&Path) -> Result<File>,
    copy: impl FnMut(&Path, &mut File) -> Result<()>,
) -> Result<()> {
    legacy(install_reported_using(bundle, prefix, cancel, lock, copy))
}

fn install_reported_using(
    bundle: &Path,
    prefix: &Path,
    cancel: &AtomicBool,
    lock: impl FnOnce(&Path) -> Result<File>,
    copy: impl FnMut(&Path, &mut File) -> Result<()>,
) -> MutationResult {
    let mut report = MutationReport::default();
    let result = install_into(bundle, prefix, cancel, lock, copy, &mut report);
    report.finish(result)
}

fn install_into(
    bundle: &Path,
    prefix: &Path,
    cancel: &AtomicBool,
    lock: impl FnOnce(&Path) -> Result<File>,
    mut copy: impl FnMut(&Path, &mut File) -> Result<()>,
    report: &mut MutationReport,
) -> Result<()> {
    check_cancel(cancel)?;
    let manifest = inspect(bundle)?;
    check_cancel(cancel)?;
    paths::parent(prefix)?;
    let created_prefix = !prefix.exists();
    let mut transaction = Transaction {
        files: vec![],
        directories: vec![],
        prefix: None,
        lock: None,
        committed: false,
        report,
    };
    if created_prefix {
        fs::create_dir(prefix)?;
        transaction.prefix = Some(prefix.into());
        fs::set_permissions(prefix, Permissions::from_mode(0o755))?;
    }
    paths::owned_directory(prefix)?;
    // A concurrent first installer must not take the creator's lock before its
    // initial receipt directory exists. Otherwise both processes can fail.
    require(
        created_prefix || prefix.join(".aw-packages").exists(),
        "refusing a prefix without AW package receipts",
    )?;
    // Enroll lock ownership before the permission change can fail.
    // Configuration readers need no write access to a root-owned prefix.
    transaction
        .lock
        .insert(lock(prefix)?)
        .set_permissions(Permissions::from_mode(0o644))?;
    let installed = receipts(prefix)?;
    require(
        created_prefix || !installed.is_empty(),
        "refusing a prefix without AW package receipts",
    )?;
    verified_files(prefix, &installed)?;
    let mut available = installed.clone();
    for component in &manifest.components {
        require(
            !available.contains_key(&component.component),
            "component already installed; use a new prefix",
        )?;
        available.insert(component.component.clone(), component.clone());
    }
    for component in &manifest.components {
        for (name, version) in &component.requires {
            require(
                available.get(name).is_some_and(|required| {
                    required.version == *version
                        && required.source_commit == component.source_commit
                        && required.arch == component.arch
                }),
                format!("requires matching {name} from the same Preview build"),
            )?;
        }
        for name in component.files.keys() {
            let target = prefix.join(name);
            paths::absolute(&target)?;
            require(
                !target.exists(),
                format!("refusing to overwrite: {}", target.display()),
            )?;
        }
    }
    for component in &manifest.components {
        for (name, metadata) in &component.files {
            check_cancel(cancel)?;
            let target = prefix.join(name);
            let mut output = transaction.create(&target)?;
            copy(&bundle.join("payload").join(name), &mut output)?;
            check_cancel(cancel)?;
            require(
                crate::digest(&target)? == metadata.sha256,
                format!("installed payload checksum mismatch: {name}"),
            )?;
            output.set_permissions(Permissions::from_mode(metadata.mode))?;
        }
        let target = prefix
            .join(".aw-packages")
            .join(format!("{}.json", component.component));
        let mut output = transaction.create(&target)?;
        output.write_all(&serde_json::to_vec_pretty(component)?)?;
        output.set_permissions(Permissions::from_mode(0o644))?;
    }
    check_cancel(cancel)?;
    transaction.committed = true;
    transaction.report.committed = true;
    Ok(())
}

/// Remove unchanged package-owned files after preflighting the entire installation.
/// Preserves the prefix, lock, unknown files and all external configuration/state.
pub fn uninstall(prefix: &Path) -> Result<()> {
    uninstall_cancellable(prefix, &AtomicBool::new(false))
}

/// Uninstall with cooperative cancellation, restoring removed files before returning.
/// The caller owns signal handling; cancellation never interrupts restoration.
pub fn uninstall_cancellable(prefix: &Path, cancel: &AtomicBool) -> Result<()> {
    legacy(uninstall_reported(prefix, cancel))
}

/// Remove one Preview installation and report commit, recovery and backup cleanup.
/// This API never prints diagnostics and does not add crash recovery or runtime checks.
///
/// # Errors
/// Preserves the primary failure even when restoring tracked files also fails.
pub fn uninstall_reported(prefix: &Path, cancel: &AtomicBool) -> MutationResult {
    uninstall_reported_using(
        prefix,
        cancel,
        |path| fs::remove_file(path),
        |path| fs::remove_dir_all(path),
    )
}

#[cfg(test)]
fn uninstall_using(
    prefix: &Path,
    cancel: &AtomicBool,
    remove: impl FnMut(&Path) -> std::io::Result<()>,
    cleanup: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<()> {
    legacy(uninstall_reported_using(prefix, cancel, remove, cleanup))
}

fn uninstall_reported_using(
    prefix: &Path,
    cancel: &AtomicBool,
    remove: impl FnMut(&Path) -> std::io::Result<()>,
    cleanup: impl FnOnce(&Path) -> std::io::Result<()>,
) -> MutationResult {
    let mut report = MutationReport::default();
    let result = uninstall_from(prefix, cancel, remove, cleanup, &mut report);
    report.finish(result)
}

fn uninstall_from(
    prefix: &Path,
    cancel: &AtomicBool,
    mut remove: impl FnMut(&Path) -> std::io::Result<()>,
    cleanup: impl FnOnce(&Path) -> std::io::Result<()>,
    report: &mut MutationReport,
) -> Result<()> {
    check_cancel(cancel)?;
    paths::owned_directory(prefix)?;
    let _lock = paths::lock(prefix)?;
    let installed = receipts(prefix)?;
    require(!installed.is_empty(), "no AW package receipts")?;
    let mut owned = verified_files(prefix, &installed)?;
    for name in installed.keys() {
        owned.push(prefix.join(".aw-packages").join(format!("{name}.json")));
    }
    // Hard links preserve bytes and modes without copying large executables.
    // Back up receipts too, so a late deletion failure restores the whole prefix.
    let backup = tempfile::Builder::new()
        .prefix("uninstall-")
        .tempdir_in(prefix)?;
    let mut removed = 0;
    let result = (|| -> Result<()> {
        for (index, path) in owned.iter().enumerate() {
            check_cancel(cancel)?;
            fs::hard_link(path, backup.path().join(index.to_string()))?;
        }
        for path in &owned {
            check_cancel(cancel)?;
            remove(path)?;
            removed += 1;
        }
        check_cancel(cancel)
    })();
    if let Err(error) = result {
        // Backup creation itself owns temporary resources, even before any removal.
        report.recovery = RecoveryState::Complete;
        for (previous, path) in owned[..removed].iter().enumerate() {
            if let Err(restore) = fs::hard_link(backup.path().join(previous.to_string()), path) {
                report.notice(
                    NoticeCode::RollbackRestoreFailed,
                    path.clone(),
                    Some(restore.kind()),
                );
                report.recovery = RecoveryState::Incomplete;
            }
        }
        if report.recovery == RecoveryState::Incomplete {
            report.notice(NoticeCode::BackupRetained, backup.keep(), None);
        } else {
            let location = backup.path().to_path_buf();
            if let Err(cleanup) = backup.close() {
                report.notice(
                    NoticeCode::BackupCleanupFailed,
                    location,
                    Some(cleanup.kind()),
                );
                report.recovery = RecoveryState::Incomplete;
            }
        }
        return Err(error);
    }
    // Payload/receipt removal has committed; partial backup cleanup cannot undo it.
    report.committed = true;
    let backup = backup.keep();
    if let Err(error) = cleanup(&backup) {
        report.notice(NoticeCode::BackupCleanupFailed, backup, Some(error.kind()));
    }
    Ok(())
}

fn legacy(result: MutationResult) -> Result<()> {
    let report = match &result {
        Ok(report) => report,
        Err(failure) => &failure.report,
    };
    for notice in &report.notices {
        let path = notice.path.display();
        let kind = notice
            .io_kind
            .map(|kind| format!("{kind:?}"))
            .unwrap_or_default();
        match notice.code {
            NoticeCode::RollbackRemoveFailed => {
                eprintln!("aw: rollback could not remove {path}: {kind}");
            }
            NoticeCode::RollbackRestoreFailed => {
                eprintln!("aw: rollback could not restore {path}: {kind}");
            }
            NoticeCode::RollbackRetireFailed => {
                eprintln!("aw: rollback could not retire {path}: {kind}");
            }
            NoticeCode::BackupRetained => {
                eprintln!("aw: retained uninstall backup at {path}");
            }
            NoticeCode::BackupCleanupFailed if report.committed => {
                eprintln!("aw: uninstall completed; backup cleanup failed at {path}: {kind}; remove this backup manually");
            }
            NoticeCode::BackupCleanupFailed => {
                eprintln!("aw: backup cleanup failed at {path}: {kind}; retained resources may require manual cleanup");
            }
        }
    }
    result.map(|_| ()).map_err(|failure| failure.error)
}

#[cfg(test)]
mod tests;
