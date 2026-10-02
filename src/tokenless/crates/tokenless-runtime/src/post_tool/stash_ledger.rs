//! Transaction ledger for tentative PostTool Stash writes.

use tokenless_ccr::{RecoveryMethod, StashStore, StashWrite, recovery_hashes};

/// Tracks the generations one PostTool run may safely roll back.
#[derive(Default)]
pub(super) struct StashLedger {
    keys: Vec<String>,
    owned: Vec<(String, u64)>,
    live_writes: usize,
    errors: usize,
}

impl StashLedger {
    pub(super) fn record(&mut self, write: StashWrite) {
        if !self.keys.contains(&write.key) {
            self.keys.push(write.key.clone());
        }
        if write.created {
            self.live_writes += 1;
            self.owned.push((write.key, write.generation));
            return;
        }
        let Some(index) = self.owned.iter().position(|(key, _)| *key == write.key) else {
            self.live_writes += 1;
            return;
        };
        if write.previous_generation == Some(self.owned[index].1) {
            self.owned[index].1 = write.generation;
        } else {
            self.owned.swap_remove(index);
        }
    }

    pub(super) fn rollback(&mut self, stash: Option<&dyn StashStore>) {
        self.keys.clear();
        for (key, generation) in std::mem::take(&mut self.owned) {
            self.delete_owned(stash, &key, generation);
        }
    }

    pub(super) fn commit(
        &mut self,
        output: &str,
        stash: Option<&dyn StashStore>,
        recovery: &RecoveryMethod,
    ) -> Vec<String> {
        let visible = recovery_hashes(output, recovery)
            .into_iter()
            .map(str::to_ascii_lowercase)
            .collect::<std::collections::HashSet<_>>();
        let (kept, orphaned): (Vec<_>, Vec<_>) = std::mem::take(&mut self.keys)
            .into_iter()
            .partition(|key| visible.contains(key));
        for key in orphaned {
            if let Some(index) = self.owned.iter().position(|(owned, _)| *owned == key) {
                let (_, generation) = self.owned.swap_remove(index);
                self.delete_owned(stash, &key, generation);
            }
        }
        self.owned.clear();
        kept
    }

    pub(super) fn live_writes(&self) -> usize {
        self.live_writes
    }

    pub(super) fn errors(&self) -> usize {
        self.errors
    }

    fn delete_owned(&mut self, stash: Option<&dyn StashStore>, key: &str, generation: u64) {
        let Some(stash) = stash else {
            return;
        };
        match stash.delete(key, generation) {
            Ok(true) => self.live_writes = self.live_writes.saturating_sub(1),
            Ok(false) => {}
            Err(_) => self.errors += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use tokenless_ccr::{InMemoryStore, StashStore};

    use super::*;

    #[test]
    fn rollback_deletes_only_rows_created_by_this_run() {
        let store = InMemoryStore::new();
        let first = store.stash("created").unwrap();
        let existing = store.stash("existing").unwrap();
        let refreshed = store.stash("existing").unwrap();
        assert!(!refreshed.created);
        let first_key = first.key.clone();
        let existing_key = existing.key.clone();

        let mut ledger = StashLedger::default();
        ledger.record(first);
        // The original create belongs to another already-emitted run.
        ledger.record(refreshed);
        ledger.rollback(Some(&store));

        assert!(store.retrieve(&first_key).unwrap().is_none());
        assert_eq!(
            store.retrieve(&existing_key).unwrap().as_deref(),
            Some("existing")
        );
        assert_eq!(store.len(), 1);
        assert_eq!(ledger.live_writes(), 1);
        assert!(existing.created);
    }

    #[test]
    fn commit_removes_created_rows_without_visible_markers() {
        let store = InMemoryStore::new();
        let write = store.stash("payload").unwrap();
        let mut ledger = StashLedger::default();
        ledger.record(write);
        assert!(
            ledger
                .commit("no marker", Some(&store), &RecoveryMethod::Shell)
                .is_empty()
        );
        assert_eq!(store.len(), 0);
        assert_eq!(ledger.live_writes(), 0);
    }

    #[test]
    fn commit_keeps_only_complete_visible_references_for_the_actual_method() {
        for method in [
            RecoveryMethod::Shell,
            RecoveryMethod::tool("tenant_retrieve").unwrap(),
        ] {
            let store = InMemoryStore::new();
            let kept = store.stash("原文\n").unwrap();
            let orphan = store.stash("discarded candidate").unwrap();
            let key = kept.key.clone();
            let orphan_key = orphan.key.clone();
            let mut ledger = StashLedger::default();
            ledger.record(kept);
            ledger.record(orphan);
            let output = serde_json::json!({
                "text": tokenless_ccr::recovery_instruction(&key, &method),
                "unrelated_hash": orphan_key,
            })
            .to_string();
            assert_eq!(
                ledger.commit(&output, Some(&store), &method),
                std::slice::from_ref(&key)
            );
            assert_eq!(ledger.live_writes(), 1);
            assert_eq!(store.len(), 1);
            assert_eq!(store.retrieve(&key).unwrap().as_deref(), Some("原文\n"));
            assert!(store.retrieve(&orphan_key).unwrap().is_none());
        }
    }

    #[test]
    fn another_tools_reference_does_not_keep_a_tentative_write() {
        let store = InMemoryStore::new();
        let write = store.stash("payload").unwrap();
        let output = tokenless_ccr::recovery_instruction(
            &write.key,
            &RecoveryMethod::tool("other").unwrap(),
        );
        let mut ledger = StashLedger::default();
        ledger.record(write);
        assert!(
            ledger
                .commit(
                    &output,
                    Some(&store),
                    &RecoveryMethod::tool("current").unwrap()
                )
                .is_empty()
        );
        assert_eq!(store.len(), 0);
    }
}
