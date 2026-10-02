//! Stash store trait and error types.
//!
//! The trait is deliberately dependency-free so compressors can hold an
//! `Option<Arc<dyn StashStore>>` without pulling in any backend crate.

/// Result of a successful [`StashStore::stash`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashWrite {
    /// Content-addressed BLAKE3 key (24 lowercase hex chars).
    pub key: String,
    /// `true` only when no live entry existed for this key before this call.
    pub created: bool,
    /// Store-wide ownership token, never reused after expiry, deletion, or
    /// eviction. Rollback must pass this to [`StashStore::delete`] so a later
    /// refresh by another compressor/process is not removed.
    pub generation: u64,
    /// Live generation immediately before this write, if a live row existed.
    /// `None` when this call created the row (`created == true`).
    ///
    /// Compressors compare this to the generation they last recorded for the
    /// key: equality means the ownership chain is unbroken (in-session
    /// refresh); inequality means another writer refreshed in between, and
    /// rollback must not re-adopt this generation.
    pub previous_generation: Option<u64>,
}

/// SQLite stores generations as signed INTEGER values. Keeping the same
/// ceiling in both backends makes exhaustion behavior and the ownership-token
/// contract independent of the storage representation.
pub(crate) const MAX_GENERATION: u64 = i64::MAX as u64;

/// A reversible stash of compressed-out payloads.
///
/// `stash` stores a payload and returns its BLAKE3-derived key; the caller
/// injects a recovery instruction into the compressed output so the LLM
/// can request the original back via `retrieve`. Keeping `stash` responsible
/// for key derivation (rather than accepting a caller-supplied hash) removes a
/// injection footgun: callers cannot mismatch a marker from its payload.
pub trait StashStore: Send + Sync {
    /// Stash `payload`, returning the key, whether this call created a live
    /// row, the new ownership token, and the previous live generation.
    ///
    /// Re-stashing the same payload is idempotent (same key) and refreshes the
    /// entry's expiry, allocating a new store-wide ownership token.
    /// `created` is `true` only when no live entry existed for that key before
    /// this call. `previous_generation` is the live token just before this
    /// write (`None` on create). Callers that roll back discarded compress
    /// output must delete only writes whose ownership chain is unbroken:
    /// re-adopt the new token only when `previous_generation` still matches
    /// the token this session last recorded. A later refresh by another
    /// compressor stays live because that mismatch drops the key from the
    /// rollback list (and a stale `delete(hash, generation)` is a no-op).
    fn stash(&self, payload: &str) -> Result<StashWrite, StashError>;

    /// Retrieve a stashed payload by key. Returns `Ok(None)` if the key is
    /// absent or the entry has expired.
    fn retrieve(&self, hash: &str) -> Result<Option<String>, StashError>;

    /// Number of live (non-expired) entries. For observability/stats only.
    fn len(&self) -> usize;

    /// Whether the store holds no live entries.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drop all expired entries and return how many were removed.
    fn evict_expired(&self) -> Result<usize, StashError>;

    /// Delete a live entry by hash **and generation**. Returns `Ok(true)` when
    /// a matching live row was removed, `Ok(false)` when the key was absent,
    /// already expired, or the generation no longer matches (another writer
    /// refreshed/adopted the row). Used to roll back stash writes whose
    /// markers never reach the LLM (e.g. CLI discards a no-savings result).
    fn delete(&self, hash: &str, generation: u64) -> Result<bool, StashError>;
}

/// Errors a stash backend can surface. Kept minimal: backends map their
/// concrete error types into `Backend` with a human-readable message so the
/// trait stays free of backend-specific dependencies.
#[derive(Debug, thiserror::Error)]
pub enum StashError {
    /// A backend-specific failure (DB error, IO error, etc.).
    #[error("stash backend error: {0}")]
    Backend(String),
}

#[cfg(feature = "sqlite")]
impl From<rusqlite::Error> for StashError {
    fn from(e: rusqlite::Error) -> Self {
        StashError::Backend(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("tests/store_tests.rs");
}
