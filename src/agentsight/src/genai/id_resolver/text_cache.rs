//! Reuse exact user-text hash prefixes with a fixed payload budget and entry limit.

use lru::LruCache;
use sha2::Sha256;
use std::mem::size_of;
use std::num::NonZeroUsize;

const MAX_BYTES: usize = 2 * 1024 * 1024;
const MIN_TEXT_BYTES: usize = 4096;
const MAX_ENTRIES: usize = 64;

type Key = (String, i32, bool);

struct Entry {
    text: Box<str>,
    prefix: Sha256,
    bytes: usize,
}

/// Cache the latest session/conversation text per process without retaining requests.
pub(super) struct TextPrefixCache {
    entries: LruCache<Key, Entry>,
    bytes: usize,
}

impl TextPrefixCache {
    /// Bound table metadata independently of the retained text budget.
    pub(super) fn new() -> Self {
        Self {
            entries: LruCache::new(NonZeroUsize::new(MAX_ENTRIES).unwrap_or(NonZeroUsize::MIN)),
            bytes: 0,
        }
    }

    /// Avoid cache overhead for short text and allocation for oversized text.
    pub(super) fn eligible(agent: &str, text: &str) -> bool {
        text.len() >= MIN_TEXT_BYTES && Self::charge(agent, text) <= MAX_BYTES
    }

    fn charge(agent: &str, text: &str) -> usize {
        size_of::<(Key, Entry)>()
            .saturating_add(agent.len())
            .saturating_add(text.len())
    }

    /// Compare complete text bytes so a reused process slot cannot change identity.
    pub(super) fn get(
        &mut self,
        agent: &str,
        pid: i32,
        session: bool,
        text: &str,
    ) -> Option<Sha256> {
        let entry = self.entries.get(&(agent.to_owned(), pid, session))?;
        (entry.text.as_ref() == text).then(|| entry.prefix.clone())
    }

    /// Replace or evict only cached work; anchor lifetime is managed separately.
    pub(super) fn insert(
        &mut self,
        agent: &str,
        pid: i32,
        session: bool,
        text: &str,
        prefix: Sha256,
    ) {
        if !Self::eligible(agent, text) {
            return;
        }
        let key = (agent.to_owned(), pid, session);
        let bytes = Self::charge(agent, text);
        if let Some(old) = self.entries.pop(&key) {
            self.bytes -= old.bytes;
        }
        while self.bytes + bytes > MAX_BYTES || self.entries.len() == MAX_ENTRIES {
            let Some((_, old)) = self.entries.pop_lru() else {
                break;
            };
            self.bytes -= old.bytes;
        }
        self.entries.put(
            key,
            Entry {
                text: text.into(),
                prefix,
                bytes,
            },
        );
        self.bytes += bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::genai::id_resolver::{finish_key, key_prefix};

    #[test]
    fn exact_text_and_process_slots_control_prefix_reuse() {
        let mut cache = TextPrefixCache::new();
        let text = "雪|\"\n\0".repeat(1024);
        cache.insert("agent", 1, true, &text, key_prefix("agent", 1, &text));
        let hit = cache.get("agent", 1, true, &text).unwrap();
        assert_eq!(
            finish_key(hit, 3),
            finish_key(key_prefix("agent", 1, &text), 3)
        );
        assert!(cache.get("agent", 1, true, &(text.clone() + "x")).is_none());
        assert!(cache.get("agent", 2, true, &text).is_none());
        assert!(cache.get("other", 1, true, &text).is_none());
        assert!(cache.get("agent", 1, false, &text).is_none());
    }

    #[test]
    fn text_cache_evicts_within_both_limits_and_skips_oversized_text() {
        let mut cache = TextPrefixCache::new();
        let text = "x".repeat(600 * 1024);
        for pid in 0..8 {
            cache.insert("agent", pid, true, &text, key_prefix("agent", pid, &text));
            assert!(cache.bytes <= MAX_BYTES);
            assert!(cache.entries.len() <= MAX_ENTRIES);
        }
        assert!(cache.get("agent", 0, true, &text).is_none());
        assert!(cache.get("agent", 7, true, &text).is_some());
        let oversized = "y".repeat(MAX_BYTES);
        let before = cache.bytes;
        cache.insert(
            "agent",
            9,
            true,
            &oversized,
            key_prefix("agent", 9, &oversized),
        );
        assert_eq!(cache.bytes, before);
        let short = "s".repeat(MIN_TEXT_BYTES);
        for pid in 10..90 {
            cache.insert(
                "agent",
                pid,
                false,
                &short,
                key_prefix("agent", pid, &short),
            );
        }
        assert_eq!(cache.entries.len(), MAX_ENTRIES);
        assert!(cache.bytes <= MAX_BYTES);
    }
}
