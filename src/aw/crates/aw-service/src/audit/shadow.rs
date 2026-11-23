//! Private, best-effort observation chains keyed by the associated event.

use aw_core::{journal::FileJournal, ports::Journal as _};
use serde_json::{json, Value};
use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};

pub(crate) struct Journal {
    directory: PathBuf,
    journal: Mutex<Option<FileJournal>>,
    failed: AtomicBool,
}

impl Journal {
    pub(super) fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            journal: Mutex::new(None),
            failed: AtomicBool::new(false),
        }
    }

    pub(crate) fn write(&self, key: &str, records: Vec<Value>) {
        if records.is_empty() || self.failed.load(Ordering::Acquire) {
            return;
        }
        // An optional record may be dropped under contention. No caller waits
        // for an observation write to make progress on the primary journal.
        let Ok(mut journal) = self.journal.try_lock() else {
            return;
        };
        if self.failed.load(Ordering::Acquire) {
            return;
        }
        let result = (|| -> crate::Result<()> {
            if journal.is_none() {
                super::check_directory(&self.directory)?;
                *journal = Some(
                    FileJournal::new(&self.directory)
                        .map_err(|_| crate::rejected("shadow_audit_unavailable"))?,
                );
            }
            let Some(journal) = journal.as_mut() else {
                return Err(crate::rejected("shadow_audit_unavailable"));
            };
            let unavailable = |_| crate::rejected("shadow_audit_unavailable");
            let result = (|| {
                journal
                    .claim(key, &json!({"kind":"shadow","event_id":key}))
                    .map_err(unavailable)?;
                for mut record in records {
                    record["createdAt"] = json!(crate::runtime::facts::timestamp()?);
                    journal.append(key, &record).map_err(unavailable)?;
                }
                journal
                    .append(key, &json!({"phase":"closed","status":"observed"}))
                    .map_err(unavailable)?;
                Ok(())
            })();
            journal.release(key);
            result
        })();
        if result.is_err() {
            self.failed.store(true, Ordering::Release);
        }
    }

    pub(super) fn read(&self, key: &str, max_bytes: u64) -> Option<Vec<Value>> {
        // Polling an absent record leaves its writer free to create the chain.
        let metadata = fs::symlink_metadata(self.directory.join(format!("{key}.jsonl"))).ok()?;
        if !metadata.is_file() || metadata.len() > max_bytes {
            return None;
        }
        let journal = self.journal.try_lock().ok()?;
        // This also lets a new service generation read a previously closed chain.
        match journal.as_ref() {
            Some(journal) => journal.read(key).ok(),
            None => {
                super::check_directory(&self.directory).ok()?;
                FileJournal::new(&self.directory).ok()?.read(key).ok()
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}

pub(crate) fn unassessed<'a>(
    steps: impl Iterator<Item = &'a aw_core::admission::AdmittedStep>,
    reason: &str,
) -> Option<Value> {
    let steps: Vec<_> = steps.map(|step| &step.step_id).collect();
    (!steps.is_empty())
        .then(|| json!({"phase":"shadow","status":"not_evaluated","reason":reason,"steps":steps}))
}
