//! Versioned write-ahead records; durable database bytes decide which side won a crash.

mod snapshots;

use super::{
    capacity,
    fs::{self, Identity},
};
use crate::{
    database::{self, DatabasePaths, MAX_DATABASE_BYTES},
    require, Error, Result,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub(super) const MAX_JOURNAL_BYTES: u64 = 96 * 1024 * 1024;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Action {
    pub path: String,
    pub before: Option<Identity>,
    pub after: Option<Identity>,
    pub stage: String,
    pub backup: String,
    pub active: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Journal {
    pub schema_version: u32,
    pub transaction_id: String,
    pub database_root: PathBuf,
    pub prefix: PathBuf,
    pub installation_id: String,
    #[serde(with = "snapshots::optional")]
    pub before_database: Option<Vec<u8>>,
    #[serde(with = "snapshots")]
    pub after_database: Vec<u8>,
    pub prefix_created: bool,
    pub prefix_identity: Option<Identity>,
    pub workspace_identity: Option<Identity>,
    pub lock_created: bool,
    pub lock_identity: Option<Identity>,
    pub directories: BTreeMap<String, Identity>,
    pub actions: Vec<Action>,
    pub database_stage: Option<Identity>,
}

impl Journal {
    pub fn bytes(&self) -> Result<Vec<u8>> {
        let bytes = serde_json::to_vec(self)?;
        require(
            bytes.len() as u64 <= MAX_JOURNAL_BYTES,
            "transaction journal exceeds limit",
        )?;
        Ok(bytes)
    }

    pub fn directory(&self) -> PathBuf {
        self.database_root
            .join("transactions")
            .join(&self.transaction_id)
    }
    pub fn workspace(&self) -> PathBuf {
        self.prefix.join(format!(".aw-txn-{}", self.transaction_id))
    }
    pub fn path(&self) -> PathBuf {
        self.directory().join("journal.json")
    }

    pub fn write(&self) -> Result<()> {
        let bytes = self.bytes()?;
        let next = self.directory().join("journal.next");
        require(
            !fs::exists(&next)?,
            "unfinished journal replacement needs recovery",
        )?;
        let (file, _) = fs::write_bytes(&self.directory(), &bytes, 0o600)?;
        fs::publish(&file, &next)?;
        fs::replace(&next, &self.path())
    }

    pub fn validate(&self, paths: &DatabasePaths, directory: &Path) -> Result<()> {
        require(
            self.schema_version == 1
                && self.database_root == paths.root
                && self.directory() == directory,
            "unsupported or misplaced transaction journal",
        )?;
        crate::description::identifier(&self.transaction_id)?;
        crate::description::identifier(&self.installation_id)?;
        fs::absolute(&self.prefix)?;
        require(
            !self.prefix.starts_with(&paths.root) && !paths.root.starts_with(&self.prefix),
            "package prefix overlaps database",
        )?;
        require(
            self.before_database
                .as_ref()
                .is_none_or(|bytes| bytes.len() as u64 <= MAX_DATABASE_BYTES)
                && self.after_database.len() as u64 <= MAX_DATABASE_BYTES
                && self.actions.len() <= capacity::MAX_ACTIONS
                && self.directories.len() <= capacity::MAX_DIRECTORIES,
            "transaction journal exceeds record limits",
        )?;
        let after: database::InstalledDatabase = database::decode(&self.after_database)?;
        database::validate_database(&after, paths.scope())?;
        let before_generation = match &self.before_database {
            Some(bytes) => {
                let before: database::InstalledDatabase = database::decode(bytes)?;
                database::validate_database(&before, paths.scope())?;
                before.generation
            }
            None => 0,
        };
        require(
            before_generation.checked_add(1) == Some(after.generation),
            "invalid transaction database generation",
        )?;
        let mut names = std::collections::BTreeSet::new();
        let mut paths_seen = std::collections::BTreeSet::new();
        for action in &self.actions {
            fs::relative(Path::new(&action.path))?;
            require(
                paths_seen.insert(&action.path) || action.path == ".aw-packages/aw-core.json",
                "duplicate transaction action path",
            )?;
            if !action.path.starts_with(".aw-packages/") {
                crate::filesystem::relative(&action.path)?;
            } else {
                require(
                    matches!(
                        action.path.as_str(),
                        ".aw-packages/aw-core.json"
                            | ".aw-packages/aw-provider-sec-core.json"
                            | ".aw-packages/aw-provider-tokenless.json"
                    ),
                    "unsupported historical record action",
                )?;
            }
            for name in [&action.stage, &action.backup] {
                require(
                    !name.contains('/') && name.starts_with("file-") && names.insert(name),
                    "invalid transaction workspace filename",
                )?;
            }
            for identity in action.before.iter().chain(action.after.iter()) {
                crate::description::sha256(&identity.sha256)?;
                require(
                    identity.ino != 0 && matches!(identity.mode, 0o600 | 0o644 | 0o755),
                    "invalid transaction file identity",
                )?;
            }
        }
        for name in self.directories.keys() {
            fs::relative(Path::new(name))?;
        }
        Ok(())
    }
}

pub(super) fn readable_lock(paths: &DatabasePaths, entry: &Journal) -> Option<Identity> {
    let before = entry.lock_identity.as_ref()?;
    if paths.scope() != database::Scope::System
        || before.mode != 0o600
        || before.sha256 != fs::bytes_payload(b"", 0o600).sha256
    {
        return None;
    }
    let mut after = before.clone();
    after.mode = 0o644;
    Some(after)
}

pub(super) fn new_id() -> Result<String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| Error::Invalid(format!("clock precedes Unix epoch: {error}")))?;
    Ok(format!(
        "txn-{:x}-{:x}-{:x}",
        now.as_nanos(),
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

pub(super) fn generated_id(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let mut parts = name.split('-');
    if parts.next() != Some("txn") {
        return false;
    }
    for limit in [32, 8, 16] {
        let Some(value) = parts.next() else {
            return false;
        };
        if value.is_empty()
            || value.len() > limit
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return false;
        }
    }
    parts.next().is_none()
}

pub(super) fn read(paths: &DatabasePaths, directory: &Path) -> Result<Journal> {
    fs::owned(directory)?;
    let next = directory.join("journal.next");
    let selected = if fs::exists(&next)? {
        next.clone()
    } else {
        directory.join("journal.json")
    };
    let bytes = fs::read(&selected, MAX_JOURNAL_BYTES)?;
    let journal: Journal = database::decode(&bytes)?;
    journal.validate(paths, directory)?;
    if selected == next {
        fs::replace(&next, &journal.path())?;
    }
    Ok(journal)
}

pub(super) fn current_database(paths: &DatabasePaths) -> Result<Option<Vec<u8>>> {
    fs::read_optional(&paths.installed(), MAX_DATABASE_BYTES)
}

pub(super) fn check_cancel(cancel: &std::sync::atomic::AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

pub(super) fn checkpoint(_phase: &str) {
    #[cfg(test)]
    if std::env::var("AW_PACKAGE_TEST_KILL_AT").ok().as_deref() == Some(_phase) {
        // SAFETY: the test explicitly requests killing its own isolated child process.
        unsafe {
            libc::kill(libc::getpid(), libc::SIGKILL);
        }
    }
}
