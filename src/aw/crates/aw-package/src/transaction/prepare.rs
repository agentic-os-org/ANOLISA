//! Establish locks and record all intended paths before the first prefix change.

use super::{
    fs,
    journal::{self, Action, Journal},
    ChangeSet,
};
use crate::{
    database::{self, DatabasePaths, Scope},
    require, Error, Result,
};
use aw_exec::PackageLock;
use std::{collections::BTreeMap, path::Path};

pub(super) struct Prepared {
    pub journal: Journal,
    pub _database_lock: PackageLock,
    pub prefix_lock: Option<PackageLock>,
}

pub(super) fn begin(paths: &DatabasePaths, changes: &ChangeSet) -> Result<Prepared> {
    validate(paths, changes)?;
    let initialized = initialize(paths, changes)?;
    let database_lock = fs::lock(&paths.root.join("lock"), false)?;
    let transactions = paths.root.join("transactions");
    match fs::entries(&transactions) {
        Ok(entries) if !entries.is_empty() => return Err(Error::NeedsRecovery(transactions)),
        Ok(_) => {}
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::mkdir(&transactions, transactions_mode(paths))?
        }
        Err(error) => return Err(error),
    }
    let before_database = journal::current_database(paths)?;
    require(
        before_database == changes.database_before
            || (initialized
                && changes.database_before.is_none()
                && before_database == Some(empty_database()?)),
        "package database changed after planning; create a fresh plan",
    )?;
    let prefix_created = match fs::directory(&changes.prefix) {
        Ok(_) => {
            fs::owned(&changes.prefix)?;
            false
        }
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::owned(
                changes
                    .prefix
                    .parent()
                    .ok_or_else(|| Error::Invalid("prefix has no parent".into()))?,
            )?;
            true
        }
        Err(error) => return Err(error),
    };
    let lock_created = !fs::exists(&changes.prefix.join(".aw-install.lock"))?;
    let prefix_lock = if lock_created {
        None
    } else {
        Some(fs::lock(&changes.prefix.join(".aw-install.lock"), false)?)
    };
    if paths.scope() == Scope::System {
        if let Some(lock) = &prefix_lock {
            use std::os::unix::fs::MetadataExt;
            let metadata = lock.metadata()?;
            require(
                matches!(metadata.mode() & 0o7777, 0o600 | 0o644),
                "system installation lock must have mode 0600 or 0644",
            )?;
            require(
                metadata.mode() & 0o7777 != 0o600 || metadata.len() == 0,
                "refusing to expose nonempty private installation lock",
            )?;
        }
    }
    require(
        prefix_lock.is_some()
            || (changes.verify.is_empty()
                && changes.legacy.is_empty()
                && changes.marker_before.is_none()),
        "existing installation is missing its prefix lock",
    )?;
    recheck(changes)?;
    super::capacity::check_bytes(paths, changes)?;
    let id = journal::new_id()?;
    let mut entry = Journal {
        schema_version: 1,
        transaction_id: id,
        database_root: paths.root.clone(),
        prefix: changes.prefix.clone(),
        installation_id: changes.installation_id.clone(),
        before_database,
        after_database: serde_json::to_vec(&changes.database_after)?,
        prefix_created,
        prefix_identity: if prefix_created {
            None
        } else {
            Some(fs::directory_identity(&changes.prefix)?)
        },
        workspace_identity: None,
        lock_created,
        lock_identity: if lock_created {
            None
        } else {
            Some(fs::identity(&changes.prefix.join(".aw-install.lock"))?)
        },
        directories: BTreeMap::new(),
        actions: Vec::new(),
        database_stage: None,
    };
    let gate = gate_bytes(paths, changes)?;
    if gate.is_some() && gate != changes.marker_before {
        push(
            &mut entry,
            ".aw-packages/aw-core.json",
            changes
                .marker_before
                .as_ref()
                .map(|_| fs::identity(&changes.prefix.join(".aw-packages/aw-core.json")))
                .transpose()?,
        )?;
    }
    for name in changes
        .legacy
        .keys()
        .filter(|name| name.as_str() != "aw-core.json")
    {
        push(
            &mut entry,
            &format!(".aw-packages/{name}"),
            Some(fs::identity(
                &changes.prefix.join(".aw-packages").join(name),
            )?),
        )?;
    }
    for name in changes.removes.keys() {
        push(
            &mut entry,
            name,
            Some(fs::identity(&changes.prefix.join(name))?),
        )?;
    }
    for name in changes.writes.keys() {
        require(
            !changes.removes.contains_key(name),
            "file is both removed and written",
        )?;
        let before = changes
            .verify
            .get(name)
            .map(|_| fs::identity(&changes.prefix.join(name)))
            .transpose()?;
        require(
            before.is_some() || !fs::exists(&changes.prefix.join(name))?,
            format!("unowned file conflicts with installation: {name}"),
        )?;
        push(&mut entry, name, before)?;
    }
    if changes.marker_after.is_none() && changes.marker_before.is_some() {
        let before = if gate != changes.marker_before {
            None
        } else {
            Some(fs::identity(
                &changes.prefix.join(".aw-packages/aw-core.json"),
            )?)
        };
        push(&mut entry, ".aw-packages/aw-core.json", before)?;
    }
    entry.validate(paths, &entry.directory())?;
    drop(entry.bytes()?);
    fs::mkdir(&entry.directory(), 0o700)?;
    journal::checkpoint("journal-directory-created");
    entry.write()?;
    Ok(Prepared {
        journal: entry,
        _database_lock: database_lock,
        prefix_lock,
    })
}

fn empty_database() -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&database::InstalledDatabase {
        schema_version: 1,
        generation: 0,
        installations: Vec::new(),
    })?)
}

pub(super) fn initialize(paths: &DatabasePaths, changes: &ChangeSet) -> Result<bool> {
    match fs::directory(&paths.root) {
        Ok(_) => {
            fs::owned(&paths.root)?;
            return Ok(false);
        }
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    require(
        changes.database_before.is_none(),
        "planned database disappeared",
    )?;
    let mode = if paths.scope() == Scope::System {
        0o755
    } else {
        0o700
    };
    let parent = paths
        .root
        .parent()
        .ok_or_else(|| Error::Invalid("database has no parent".into()))?;
    fs::ensure_tree(parent, mode)?;
    // Publish a complete empty database atomically. A crash cannot expose root+lock without data.
    let stage = parent.join(format!(".aw-database-init-{}", journal::new_id()?));
    fs::mkdir(&stage, mode)?;
    let _lock = fs::lock(&stage.join("lock"), true)?;
    fs::mkdir(&stage.join("transactions"), transactions_mode(paths))?;
    let (file, _) = fs::write_bytes(&stage, &empty_database()?, 0o644)?;
    fs::publish(&file, &stage.join("installed.json"))?;
    fs::directory(&stage)?.sync_all()?;
    fs::move_noreplace(&stage, &paths.root)?;
    Ok(true)
}

fn transactions_mode(paths: &DatabasePaths) -> u32 {
    if paths.scope() == Scope::System {
        0o755
    } else {
        0o700
    }
}

fn push(entry: &mut Journal, path: &str, before: Option<fs::Identity>) -> Result<()> {
    fs::relative(Path::new(path))?;
    let index = entry.actions.len();
    entry.actions.push(Action {
        path: path.into(),
        before,
        after: None,
        stage: format!("file-{index}-new"),
        backup: format!("file-{index}-old"),
        active: false,
    });
    Ok(())
}

fn validate(paths: &DatabasePaths, changes: &ChangeSet) -> Result<()> {
    fs::absolute(&paths.root)?;
    fs::absolute(&changes.prefix)?;
    require(
        !changes.prefix.starts_with(&paths.root) && !paths.root.starts_with(&changes.prefix),
        "package prefix overlaps database",
    )?;
    if paths.scope() == Scope::System {
        // SAFETY: geteuid only observes the effective identity.
        require(
            unsafe { libc::geteuid() } == 0,
            "system package writes require root and explicit system scope",
        )?;
    }
    crate::description::identifier(&changes.installation_id)?;
    database::validate_database(&changes.database_after, paths.scope())?;
    let generation = match &changes.database_before {
        Some(bytes) => {
            let value: database::InstalledDatabase = database::decode(bytes)?;
            database::validate_database(&value, paths.scope())?;
            value.generation
        }
        None => 0,
    };
    require(
        generation.checked_add(1) == Some(changes.database_after.generation),
        "planned database generation does not advance exactly once",
    )?;
    for (name, payload) in changes.verify.iter().chain(changes.removes.iter()) {
        crate::filesystem::relative(name)?;
        crate::description::sha256(&payload.sha256)?;
    }
    for name in changes.writes.keys() {
        crate::filesystem::relative(name)?;
    }
    Ok(())
}

pub(super) fn recheck(changes: &ChangeSet) -> Result<()> {
    for (source, expected) in &changes.source_records {
        require(
            fs::read(source, crate::description::MAX_DESCRIPTION_BYTES)? == *expected,
            format!(
                "bundle description changed after planning: {}",
                source.display()
            ),
        )?;
    }
    for source in changes.writes.values() {
        fs::verify(&source.source, &source.metadata)?;
    }
    for (name, payload) in changes.verify.iter().chain(changes.removes.iter()) {
        fs::verify(&changes.prefix.join(name), payload)?;
    }
    let directory = changes.prefix.join(".aw-packages");
    let entries = match fs::entries(&directory) {
        Ok(entries) => entries,
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    let expected_count = if changes.legacy.is_empty() {
        usize::from(changes.marker_before.is_some())
    } else {
        changes.legacy.len()
    };
    require(
        entries.len() == expected_count,
        "installation records changed after planning",
    )?;
    require(
        fs::read_optional(
            &directory.join("aw-core.json"),
            crate::description::MAX_DESCRIPTION_BYTES,
        )? == changes.marker_before,
        "prefix registration changed after planning",
    )?;
    for (name, bytes) in &changes.legacy {
        require(
            matches!(
                name.as_str(),
                "aw-core.json" | "aw-provider-sec-core.json" | "aw-provider-tokenless.json"
            ),
            "unsupported historical record filename",
        )?;
        require(
            fs::read(
                &directory.join(name),
                crate::description::MAX_DESCRIPTION_BYTES,
            )? == *bytes,
            format!("older installation record changed: {name}"),
        )?;
    }
    Ok(())
}

pub(super) fn gate_bytes(paths: &DatabasePaths, changes: &ChangeSet) -> Result<Option<Vec<u8>>> {
    if !changes.legacy.is_empty() {
        Ok(Some(serde_json::to_vec(
            &database::RegistrationPointer::new(paths, changes.installation_id.clone())?,
        )?))
    } else {
        Ok(changes.marker_after.clone())
    }
}

pub(super) fn parents(entry: &mut Journal, relative: &Path) -> Result<()> {
    let mut paths = relative
        .ancestors()
        .skip(1)
        .filter(|path| !path.as_os_str().is_empty())
        .collect::<Vec<_>>();
    paths.reverse();
    for path in paths {
        let destination = entry.prefix.join(path);
        match fs::directory(&destination) {
            Ok(_) => fs::owned(&destination)?,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = path
                    .to_str()
                    .ok_or_else(|| Error::Invalid("non-UTF8 package parent".into()))?;
                // Empty parents may disappear after planning; keep every persisted journal valid.
                require(
                    entry.directories.contains_key(name)
                        || entry.directories.len() < super::capacity::MAX_DIRECTORIES,
                    "transaction directory capacity exceeded after planning",
                )?;
                // The full action path in the existing journal authorizes only empty-parent creation.
                fs::mkdir(&destination, 0o755)?;
                let identity = fs::directory_identity(&destination)?;
                entry.directories.insert(name.into(), identity);
                entry.write()?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub(super) fn verify_bytes(path: &Path, bytes: &[u8]) -> Result<fs::Identity> {
    fs::verify(path, &fs::bytes_payload(bytes, 0o644))
}

pub(super) fn stage_source(
    entry: &mut Journal,
    index: usize,
    source: &super::SourceFile,
) -> Result<()> {
    let mut held = fs::regular(&source.source)?;
    let before = fs::verify(&source.source, &source.metadata)?;
    let (file, identity) = fs::anonymous(&entry.workspace(), source.metadata.mode, |file| {
        let count = std::io::copy(
            &mut std::io::Read::take(&mut held, fs::MAX_FILE_BYTES + 1),
            file,
        )?;
        require(
            count <= fs::MAX_FILE_BYTES,
            "bundle source grew while staging",
        )
    })?;
    require(
        identity.sha256 == source.metadata.sha256 && fs::same(&source.source, &before)?,
        "bundle source changed while staging",
    )?;
    entry.actions[index].after = Some(identity);
    entry.write()?;
    fs::publish(&file, &entry.workspace().join(&entry.actions[index].stage))
}

pub(super) fn stage_bytes(entry: &mut Journal, index: usize, bytes: &[u8]) -> Result<()> {
    let (file, identity) = fs::write_bytes(&entry.workspace(), bytes, 0o644)?;
    entry.actions[index].after = Some(identity);
    entry.write()?;
    fs::publish(&file, &entry.workspace().join(&entry.actions[index].stage))
}
