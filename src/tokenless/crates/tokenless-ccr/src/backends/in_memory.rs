//! In-process stash backend.
//!
//! Suitable for tests and single-process CLI runs only. The tokenless hooks
//! fork+exec a fresh process per call, so an in-memory store loses its
//! contents between calls — use [`sqlite::SqliteStore`](crate::SqliteStore)
//! for the production hook path.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::key::compute_key;
use crate::store::{MAX_GENERATION, StashError, StashStore, StashWrite};

/// Default time-to-live for an entry: 5 minutes.
const DEFAULT_TTL: Duration = Duration::from_secs(5 * 60);

/// Default maximum number of live entries before FIFO eviction.
const DEFAULT_CAPACITY: usize = 1000;

struct Entry {
    payload: String,
    inserted_at: Instant,
    generation: u64,
}

struct Inner {
    map: HashMap<String, Entry>,
    /// Store-wide ownership high-water mark. Lifecycle cleanup never changes it.
    last_generation: u64,
    /// Insertion order for FIFO eviction, front = oldest. A re-stash refreshes
    /// the entry by moving its key to the back so a refreshed entry survives
    /// eviction — matching `SqliteStore`'s `expires_at`-ordered eviction.
    order: VecDeque<String>,
    ttl: Duration,
    capacity: usize,
}

/// An in-memory stash with TTL and FIFO eviction.
pub struct InMemoryStore {
    inner: Mutex<Inner>,
}

impl InMemoryStore {
    /// Create a store with the default TTL (5 min) and capacity (1000).
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_TTL, DEFAULT_CAPACITY)
    }

    /// Create a store with a custom TTL and capacity.
    pub fn with_limits(ttl: Duration, capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                map: HashMap::new(),
                last_generation: 0,
                order: VecDeque::new(),
                ttl,
                capacity,
            }),
        }
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl StashStore for InMemoryStore {
    fn stash(&self, payload: &str) -> Result<StashWrite, StashError> {
        let key = compute_key(payload.as_bytes());
        let mut inner = self
            .inner
            .lock()
            .map_err(|e| StashError::Backend(format!("in_memory mutex poisoned: {e}")))?;
        let now = Instant::now();
        let previous_generation = match inner.map.get(&key) {
            Some(e) if now.duration_since(e.inserted_at) < inner.ttl => Some(e.generation),
            _ => None,
        };
        let had_live = previous_generation.is_some();
        let generation = inner
            .last_generation
            .checked_add(1)
            .filter(|generation| *generation <= MAX_GENERATION)
            .ok_or_else(|| StashError::Backend("stash generation exhausted".to_string()))?;
        inner.last_generation = generation;
        // Re-stash refreshes: move the key to the back of the FIFO order so a
        // refreshed entry is evicted last, matching SqliteStore's
        // expires_at-ordered eviction.
        if !inner.map.contains_key(&key) {
            inner.order.push_back(key.clone());
        } else {
            inner.order.retain(|k| k != &key);
            inner.order.push_back(key.clone());
        }
        inner.map.insert(
            key.clone(),
            Entry {
                payload: payload.to_string(),
                inserted_at: now,
                generation,
            },
        );
        // Evict oldest entries until we are within capacity. Evicting after
        // re-inserting means we allow capacity entries to coexist with the
        // newcomer before trimming back to the limit.
        while inner.order.len() > inner.capacity {
            if let Some(old) = inner.order.pop_front() {
                inner.map.remove(&old);
            }
        }
        Ok(StashWrite {
            key,
            created: !had_live,
            generation,
            previous_generation,
        })
    }

    fn retrieve(&self, hash: &str) -> Result<Option<String>, StashError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|e| StashError::Backend(format!("in_memory mutex poisoned: {e}")))?;
        // Keys are stored as lowercase BLAKE3 hex; accept a marker the LLM may
        // have uppercased by lowercasing the lookup (case-insensitive retrieve).
        let key = hash.to_ascii_lowercase();
        let now = Instant::now();
        match inner.map.get(&key) {
            Some(entry) if now.duration_since(entry.inserted_at) < inner.ttl => {
                Ok(Some(entry.payload.clone()))
            }
            Some(_) => {
                // Expired: drop and report absent.
                inner.map.remove(&key);
                inner.order.retain(|k| k != &key);
                Ok(None)
            }
            None => Ok(None),
        }
    }

    fn len(&self) -> usize {
        let now = Instant::now();
        let inner = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return 0,
        };
        // Count only live entries without mutating.
        let ttl = inner.ttl;
        inner
            .map
            .iter()
            .filter(|(_, e)| now.duration_since(e.inserted_at) < ttl)
            .count()
    }

    fn evict_expired(&self) -> Result<usize, StashError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|e| StashError::Backend(format!("in_memory mutex poisoned: {e}")))?;
        let now = Instant::now();
        let ttl = inner.ttl;
        let expired: Vec<String> = inner
            .map
            .iter()
            .filter(|(_, e)| now.duration_since(e.inserted_at) >= ttl)
            .map(|(k, _)| k.clone())
            .collect();
        let count = expired.len();
        for k in &expired {
            inner.map.remove(k);
        }
        if count > 0 {
            inner.order.retain(|k| !expired.contains(k));
        }
        Ok(count)
    }

    fn delete(&self, hash: &str, generation: u64) -> Result<bool, StashError> {
        let key = hash.to_ascii_lowercase();
        let mut inner = self
            .inner
            .lock()
            .map_err(|e| StashError::Backend(format!("in_memory mutex poisoned: {e}")))?;
        let now = Instant::now();
        let ttl = inner.ttl;
        let matches = inner
            .map
            .get(&key)
            .map(|e| now.duration_since(e.inserted_at) < ttl && e.generation == generation)
            .unwrap_or(false);
        if !matches {
            // Absent, expired, or adopted by a later stash (generation moved).
            return Ok(false);
        }
        inner.map.remove(&key);
        inner.order.retain(|k| k != &key);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn retrieve_is_case_insensitive() {
        // An LLM may quote a marker back with uppercased hex; keys are stored
        // lowercase, so retrieve must normalize the lookup.
        let store = InMemoryStore::new();
        let key = store.stash("payload").unwrap().key;
        assert_eq!(key, key.to_ascii_lowercase());
        let upper = key.to_uppercase();
        assert_ne!(upper, key);
        assert_eq!(store.retrieve(&upper).unwrap(), Some("payload".to_string()));
    }

    #[test]
    fn re_stash_refreshes_fifo_position() {
        // A re-stashed (refreshed) entry should survive FIFO eviction, matching
        // SqliteStore's expires_at-ordered eviction.
        let store = InMemoryStore::with_limits(Duration::from_secs(60), 3);
        let k0 = store.stash("0").unwrap().key;
        let _ = store.stash("1").unwrap();
        let _ = store.stash("2").unwrap();
        // Re-stash k0 to refresh it (moves to back); then stash a new key to
        // trigger eviction. k0 must survive (refreshed), k1 (oldest) evicted.
        let _ = store.stash("0").unwrap();
        let _ = store.stash("3").unwrap();
        assert!(store.retrieve(&k0).unwrap().is_some());
    }

    #[test]
    fn stash_and_retrieve_round_trip() {
        let store = InMemoryStore::new();
        let key = store.stash("some payload").unwrap().key;
        assert_eq!(
            store.retrieve(&key).unwrap(),
            Some("some payload".to_string())
        );
    }

    #[test]
    fn retrieve_missing_returns_none() {
        let store = InMemoryStore::new();
        assert_eq!(store.retrieve("000000000000000000000000").unwrap(), None);
    }

    #[test]
    fn re_stash_is_idempotent_and_refreshes() {
        let store = InMemoryStore::with_limits(Duration::from_millis(50), 100);
        let k1 = store.stash("payload").unwrap().key;
        let k2 = store.stash("payload").unwrap().key;
        assert_eq!(k1, k2);
        // After the original TTL would have expired, the refreshed entry is
        // still live.
        std::thread::sleep(Duration::from_millis(30));
        let _ = store.stash("payload").unwrap();
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(store.retrieve(&k1).unwrap(), Some("payload".to_string()));
    }

    #[test]
    fn expired_entry_not_retrievable() {
        let store = InMemoryStore::with_limits(Duration::from_millis(20), 100);
        let key = store.stash("ephemeral").unwrap().key;
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(store.retrieve(&key).unwrap(), None);
    }

    #[test]
    fn evict_expired_reports_count() {
        let store = InMemoryStore::with_limits(Duration::from_millis(20), 100);
        let _ = store.stash("a").unwrap();
        let _ = store.stash("b").unwrap();
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(store.evict_expired().unwrap(), 2);
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn fifo_eviction_when_over_capacity() {
        let store = InMemoryStore::with_limits(Duration::from_secs(60), 3);
        let k0 = store.stash("0").unwrap().key;
        let _ = store.stash("1").unwrap();
        let _ = store.stash("2").unwrap();
        let _ = store.stash("3").unwrap(); // evicts "0"
        assert_eq!(store.retrieve(&k0).unwrap(), None);
        assert!(
            store
                .retrieve(&store.stash("1").unwrap().key)
                .unwrap()
                .is_some()
        );
        assert!(store.len() <= 3);
    }

    #[test]
    fn delete_live_entry_returns_true() {
        let store = InMemoryStore::new();
        let write = store.stash("payload").unwrap();
        assert!(store.delete(&write.key, write.generation).unwrap());
        assert_eq!(store.retrieve(&write.key).unwrap(), None);
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn delete_missing_returns_false() {
        let store = InMemoryStore::new();
        assert!(!store.delete("000000000000000000000000", 1).unwrap());
    }

    #[test]
    fn delete_expired_returns_false_without_removing_live_contract() {
        // Trait: Ok(false) when absent or already expired. Must not report a
        // successful live delete for a row retrieve() would already hide.
        let store = InMemoryStore::with_limits(Duration::from_millis(20), 100);
        let write = store.stash("ephemeral").unwrap();
        std::thread::sleep(Duration::from_millis(30));
        assert!(!store.delete(&write.key, write.generation).unwrap());
        assert_eq!(store.retrieve(&write.key).unwrap(), None);
    }

    #[test]
    fn delete_stale_generation_after_refresh_returns_false() {
        // After a later refresh of the same payload, delete with the create
        // generation must be a no-op so the live row stays retrievable.
        let store = InMemoryStore::new();
        let first = store.stash("payload").unwrap();
        assert!(first.created);
        let second = store.stash("payload").unwrap();
        assert!(!second.created);
        assert_ne!(first.generation, second.generation);
        assert_eq!(first.previous_generation, None);
        assert_eq!(second.previous_generation, Some(first.generation));
        assert!(!store.delete(&first.key, first.generation).unwrap());
        assert_eq!(
            store.retrieve(&second.key).unwrap(),
            Some("payload".to_string())
        );
        assert!(store.delete(&second.key, second.generation).unwrap());
    }

    #[test]
    fn lazy_expiry_recreate_never_reuses_generation() {
        let store = InMemoryStore::with_limits(Duration::from_secs(60), 100);
        let first = store.stash("payload").unwrap();
        {
            let mut inner = store.inner.lock().unwrap();
            let expired_at = Instant::now() - inner.ttl - Duration::from_secs(1);
            inner.map.get_mut(&first.key).unwrap().inserted_at = expired_at;
        }
        assert_eq!(store.retrieve(&first.key).unwrap(), None);
        let second = store.stash("payload").unwrap();
        assert!(second.generation > first.generation);
        assert!(!store.delete(&first.key, first.generation).unwrap());
        assert_eq!(store.retrieve(&second.key).unwrap(), Some("payload".into()));
    }

    #[test]
    fn evict_expired_recreate_never_reuses_generation() {
        let store = InMemoryStore::with_limits(Duration::from_secs(60), 100);
        let first = store.stash("payload").unwrap();
        {
            let mut inner = store.inner.lock().unwrap();
            let expired_at = Instant::now() - inner.ttl - Duration::from_secs(1);
            inner.map.get_mut(&first.key).unwrap().inserted_at = expired_at;
        }
        assert_eq!(store.evict_expired().unwrap(), 1);
        let second = store.stash("payload").unwrap();
        assert!(second.generation > first.generation);
        assert!(!store.delete(&first.key, first.generation).unwrap());
    }

    #[test]
    fn capacity_eviction_recreate_never_reuses_generation() {
        let store = InMemoryStore::with_limits(Duration::from_secs(60), 1);
        let first = store.stash("payload").unwrap();
        let _other = store.stash("other").unwrap();
        assert_eq!(store.retrieve(&first.key).unwrap(), None);
        let second = store.stash("payload").unwrap();
        assert!(second.generation > first.generation);
        assert!(!store.delete(&first.key, first.generation).unwrap());
    }

    #[test]
    fn delete_recreate_never_reuses_generation() {
        let store = InMemoryStore::new();
        let first = store.stash("payload").unwrap();
        assert!(store.delete(&first.key, first.generation).unwrap());
        let second = store.stash("payload").unwrap();
        assert!(second.generation > first.generation);
        assert!(!store.delete(&first.key, first.generation).unwrap());
        assert_eq!(store.retrieve(&second.key).unwrap(), Some("payload".into()));
    }

    #[test]
    fn generation_exhaustion_does_not_mutate_stash() {
        let store = InMemoryStore::new();
        let existing = store.stash("existing").unwrap();
        {
            let mut inner = store.inner.lock().unwrap();
            inner.last_generation = MAX_GENERATION;
        }
        let before = store.retrieve(&existing.key).unwrap();
        let error = store.stash("new").unwrap_err();
        assert!(error.to_string().contains("generation exhausted"));
        assert_eq!(store.retrieve(&existing.key).unwrap(), before);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn concurrent_stash_no_deadlock() {
        let store = std::sync::Arc::new(InMemoryStore::new());
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let s = store.clone();
                thread::spawn(move || {
                    for j in 0..100 {
                        let _ = s.stash(&format!("p-{i}-{j}")).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert!(store.len() <= 1000);
    }
}
