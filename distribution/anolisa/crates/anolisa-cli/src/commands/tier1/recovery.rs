//! Locked recovery gate shared by lifecycle mutation executors.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anolisa_core::facts::{JournalEvidence, JournalInventory};
use anolisa_core::lock::InstallLock;
use anolisa_core::transaction::Transaction;

use crate::response::CliError;

/// Validated journal inventory tied to the install lock for the same state
/// root.
///
/// Executors construct this after taking the lock and reloading state, then
/// use [`Self::begin`] before any lifecycle side effect. Holding the lock for
/// the gate's lifetime makes the inventory authoritative until the new
/// journal is persisted.
pub(crate) struct LockedJournalGate<'lock> {
    _lock: &'lock InstallLock,
    inventory: JournalInventory,
    journal_dir: PathBuf,
    claimed_subjects: BTreeSet<String>,
}

impl<'lock> LockedJournalGate<'lock> {
    /// Validate the journal directory protected by `lock`.
    pub(crate) fn load(
        lock: &'lock InstallLock,
        evidence: JournalEvidence<'_>,
        command: &str,
    ) -> Result<Self, CliError> {
        let journal_dir = evidence.journal_dir();
        if lock.path().parent() != journal_dir.parent() {
            return Err(CliError::Runtime {
                command: command.to_string(),
                reason: format!(
                    "internal: install lock {} does not protect journal directory {}",
                    lock.path().display(),
                    journal_dir.display()
                ),
            });
        }
        let inventory = JournalInventory::load(evidence).map_err(|err| CliError::Runtime {
            command: command.to_string(),
            reason: err.to_string(),
        })?;
        Ok(Self {
            _lock: lock,
            inventory,
            journal_dir: journal_dir.to_path_buf(),
            claimed_subjects: BTreeSet::new(),
        })
    }

    /// Validated journal snapshot loaded under the lock.
    ///
    /// The snapshot predates any journal this gate began, so executors can
    /// check earlier recovery claims without seeing their own journal.
    pub(crate) fn inventory(&self) -> &JournalInventory {
        &self.inventory
    }

    /// Pending journal attributed to `subject`, including an unattributed
    /// legacy journal whose scope is unknown.
    pub(crate) fn pending_path(&self, subject: &str) -> Option<&Path> {
        self.inventory
            .blocking_for(subject)
            .map(|entry| entry.path())
    }

    /// Refuse to start a second recovery chain for `subject`.
    pub(crate) fn ensure_clear(&self, subject: &str, command: &str) -> Result<(), CliError> {
        if let Some(path) = self.pending_path(subject) {
            return Err(pending_operation_error(command, subject, path));
        }
        if self.claimed_subjects.contains(subject) {
            return Err(CliError::Runtime {
                command: command.to_string(),
                reason: format!(
                    "internal: lifecycle executor attempted to create two journals for component '{subject}' under one install lock"
                ),
            });
        }
        Ok(())
    }

    /// Begin a subject journal only after the locked inventory proves no
    /// earlier recovery chain is pending.
    pub(crate) fn begin(
        &mut self,
        operation: &str,
        subject: &str,
        state_path: PathBuf,
        command: &str,
    ) -> Result<Transaction, CliError> {
        self.ensure_clear(subject, command)?;
        let transaction = Transaction::begin_with_subject(
            operation,
            Some(subject),
            state_path,
            &self.journal_dir,
        )
        .map_err(|err| CliError::Runtime {
            command: command.to_string(),
            reason: format!("failed to begin operation journal: {err}"),
        })?;
        self.claimed_subjects.insert(subject.to_string());
        Ok(transaction)
    }
}

pub(crate) fn pending_operation_error(command: &str, subject: &str, path: &Path) -> CliError {
    CliError::Runtime {
        command: command.to_string(),
        reason: format!(
            "component '{subject}' has a pending operation journal at {}; run `anolisa repair {subject}` before retrying",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::tier1::rpm_install;
    use anolisa_core::state::OperationRecord;
    use anolisa_platform::fs_layout::FsLayout;

    fn journal_count(journal_dir: &Path) -> usize {
        std::fs::read_dir(journal_dir)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .ends_with(".journal.toml")
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    #[test]
    fn pending_created_after_preflight_blocks_a_second_locked_journal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_path = tmp.path().join("installed.toml");
        let journal_dir = tmp.path().join("journal");
        let lock_path = tmp.path().join("anolisa.lock");
        std::fs::write(&state_path, b"state-before-race").expect("seed state");

        // Models invocation A crashing after invocation B's preflight but
        // before B acquires the shared state-root lock.
        let pending = Transaction::begin_with_subject(
            "install",
            Some("cosh"),
            state_path.clone(),
            &journal_dir,
        )
        .expect("pending journal");
        let pending_path = pending.journal_path.clone();
        drop(pending);
        let journals_before = journal_count(&journal_dir);
        let state_before = std::fs::read(&state_path).expect("read state");

        let lock = InstallLock::acquire(&lock_path).expect("install lock");
        let evidence = JournalEvidence::new(&journal_dir, &[]);
        let mut gate =
            LockedJournalGate::load(&lock, evidence, "install cosh").expect("trusted inventory");
        let err = gate
            .begin("install", "cosh", state_path.clone(), "install cosh")
            .expect_err("pending recovery must block a second journal");

        assert!(err.reason().contains("anolisa repair cosh"));
        assert!(
            err.reason()
                .contains(pending_path.to_string_lossy().as_ref())
        );
        assert_eq!(journal_count(&journal_dir), journals_before);
        assert_eq!(
            std::fs::read(&state_path).expect("read state"),
            state_before
        );
    }

    #[test]
    fn committed_legacy_journal_allows_an_unrelated_locked_begin() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let pending =
            rpm_install::begin_fresh_install(&layout, "cosh", "copilot-shell", "install cosh")
                .expect("begin legacy journal");
        let operation_id = pending.transaction.operation_id.clone();
        drop(pending);
        let operations = vec![OperationRecord {
            id: operation_id,
            command: "install cosh".to_string(),
            status: "ok".to_string(),
            started_at: "2026-07-21T00:00:00Z".to_string(),
            finished_at: Some("2026-07-21T00:00:01Z".to_string()),
            parent_operation_id: None,
        }];
        let journal_dir = rpm_install::journal_dir(&layout);
        let evidence = JournalEvidence::new(&journal_dir, &operations);
        let lock = InstallLock::acquire(&layout.lock_file).expect("install lock");
        let mut gate = LockedJournalGate::load(&lock, evidence, "install unrelated")
            .expect("committed legacy journal must not block the locked gate");

        let journal = gate
            .begin(
                "install",
                "unrelated",
                layout.state_dir.join("installed.toml"),
                "install unrelated",
            )
            .expect("begin unrelated journal");

        assert_eq!(journal.subject.as_deref(), Some("unrelated"));
    }

    #[test]
    fn untrusted_locked_inventory_blocks_every_subject() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let journal_dir = tmp.path().join("journal");
        let lock_path = tmp.path().join("anolisa.lock");
        std::fs::create_dir_all(&journal_dir).expect("journal dir");
        let path = journal_dir.join("broken.journal.toml");
        std::fs::write(&path, "invalid = [").expect("corrupt journal");
        let journals_before = journal_count(&journal_dir);

        let lock = InstallLock::acquire(&lock_path).expect("install lock");
        let evidence = JournalEvidence::new(&journal_dir, &[]);
        let err = LockedJournalGate::load(&lock, evidence, "update cosh")
            .err()
            .expect("untrusted inventory must fail closed");

        assert!(err.reason().contains(path.to_string_lossy().as_ref()));
        assert_eq!(journal_count(&journal_dir), journals_before);
    }

    #[test]
    fn one_locked_batch_gate_claims_each_subject_only_once() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_path = tmp.path().join("installed.toml");
        let journal_dir = tmp.path().join("journal");
        let lock_path = tmp.path().join("anolisa.lock");
        let lock = InstallLock::acquire(&lock_path).expect("install lock");
        let evidence = JournalEvidence::new(&journal_dir, &[]);
        let mut gate =
            LockedJournalGate::load(&lock, evidence, "install --all").expect("trusted inventory");

        let first = gate
            .begin(
                "install",
                "component-a",
                state_path.clone(),
                "install --all",
            )
            .expect("first member journal");
        let second = gate
            .begin(
                "install",
                "component-b",
                state_path.clone(),
                "install --all",
            )
            .expect("second member journal");
        let err = gate
            .begin("install", "component-a", state_path, "install --all")
            .expect_err("duplicate member journal must fail");

        assert_ne!(first.journal_path, second.journal_path);
        assert!(err.reason().contains("two journals"));
        assert_eq!(journal_count(&journal_dir), 2);
    }
}
