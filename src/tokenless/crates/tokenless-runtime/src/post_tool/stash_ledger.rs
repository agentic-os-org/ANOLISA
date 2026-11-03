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
    /// Register one tentative write.
    ///
    /// The ownership chain and its rollback/commit consequences:
    /// - `created` rows are owned by this run: rollback and commit-time
    ///   orphan cleanup may delete them (at their generation).
    /// - A refresh whose `previous_generation` matches the generation this
    ///   ledger last recorded for the key is an in-session refresh:
    ///   ownership follows to the new generation, so rollback deletes the
    ///   live row.
    /// - A refresh with any other `previous_generation` means a foreign
    ///   writer refreshed in between (and may have emitted a marker):
    ///   ownership is dropped, so neither rollback nor orphan cleanup can
    ///   delete the row.
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

    /// Finish the run against the output that reached the model.
    ///
    /// Orphan-cleanup contract: recorded keys whose retrieval marker is
    /// visible in `output` for the caller's actual recovery method (matched
    /// case-insensitively, like `retrieve`) are committed and returned. Every
    /// other recorded key is an orphan — its marker never reached the model,
    /// so `tokenless retrieve` can no longer reach the row — and a row still
    /// owned by this run (see [`Self::record`]) is deleted at its recorded
    /// generation. A foreign-refreshed row is not owned and survives, because
    /// the run that refreshed it may still hold a marker for it.
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
    fn in_session_refresh_keeps_ownership_and_rolls_back_the_refreshed_generation() {
        // create (g1) then an in-session refresh (g2, prev=g1): ownership
        // follows, so rollback deletes the LIVE g2 row.
        let store = InMemoryStore::new();
        let created = store.stash("payload").unwrap();
        assert!(created.created);
        let refreshed = store.stash("payload").unwrap();
        assert!(!refreshed.created);
        let key = created.key.clone();

        let mut ledger = StashLedger::default();
        ledger.record(created);
        ledger.record(refreshed);
        ledger.rollback(Some(&store));

        assert!(
            store.retrieve(&key).unwrap().is_none(),
            "the refreshed live row must be rolled back"
        );
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn foreign_refresh_drops_ownership_and_rollback_keeps_the_live_row() {
        // create g1 recorded by us; a FOREIGN writer refreshes to g2 (not
        // recorded); our next stash is g3 with prev=g2 — ownership is
        // dropped, rollback must keep the live row.
        let store = InMemoryStore::new();
        let created = store.stash("payload").unwrap();
        let key = created.key.clone();
        let mut ledger = StashLedger::default();
        ledger.record(created);

        // Foreign refresh, unseen by the ledger.
        let _foreign = store.stash("payload").unwrap();
        // Our next write observes the foreign generation.
        let ours = store.stash("payload").unwrap();
        assert!(!ours.created);
        assert_eq!(ours.previous_generation, Some(_foreign.generation));

        ledger.record(ours);
        ledger.rollback(Some(&store));

        assert!(
            store.retrieve(&key).unwrap().is_some(),
            "a foreign-refreshed row must survive our rollback"
        );
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn commit_keeps_a_foreign_refreshed_row_when_its_marker_is_visible() {
        // Same foreign-refresh setup, but the output contains the marker:
        // commit must keep the row (the foreign run may still reference it).
        let store = InMemoryStore::new();
        let created = store.stash("payload").unwrap();
        let key = created.key.clone();
        let mut ledger = StashLedger::default();
        ledger.record(created);
        let _foreign = store.stash("payload").unwrap();
        let ours = store.stash("payload").unwrap();
        ledger.record(ours);

        let output = serde_json::json!({
            "text": tokenless_ccr::recovery_instruction(&key, &RecoveryMethod::Shell),
        })
        .to_string();
        let kept = ledger.commit(&output, Some(&store), &RecoveryMethod::Shell);
        assert_eq!(kept, vec![key.clone()]);
        assert!(store.retrieve(&key).unwrap().is_some());
    }

    #[test]
    fn commit_normalizes_uppercase_markers_to_lowercase_keys() {
        // LLMs quote markers back in random case; retrieve() normalizes and
        // commit() must too (recovery_hashes lowercases before matching).
        let store = InMemoryStore::new();
        let write = store.stash("payload").unwrap();
        let key = write.key.clone();
        let mut ledger = StashLedger::default();
        ledger.record(write);

        let upper_key = key.to_ascii_uppercase();
        let output = tokenless_ccr::recovery_instruction(&upper_key, &RecoveryMethod::Shell);
        let kept = ledger.commit(&output, Some(&store), &RecoveryMethod::Shell);
        assert_eq!(kept, vec![key.clone()]);
        assert!(store.retrieve(&key).unwrap().is_some());
    }

    #[test]
    fn record_deduplicates_keys_across_create_and_refresh() {
        // create + in-session refresh recorded; commit reports the key once.
        let store = InMemoryStore::new();
        let created = store.stash("payload").unwrap();
        let refreshed = store.stash("payload").unwrap();
        let key = created.key.clone();
        let mut ledger = StashLedger::default();
        ledger.record(created);
        ledger.record(refreshed);

        let output = serde_json::json!({
            "text": tokenless_ccr::recovery_instruction(&key, &RecoveryMethod::Shell),
        })
        .to_string();
        let kept = ledger.commit(&output, Some(&store), &RecoveryMethod::Shell);
        assert_eq!(kept.len(), 1, "the key may appear once: {kept:?}");
        assert_eq!(kept[0], key);
        assert_eq!(store.len(), 1);
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
