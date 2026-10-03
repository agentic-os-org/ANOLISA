//! BLAKE3-derived stash keys.
//!
//! A key is the first 24 hex characters (12 bytes / 96 bits) of a BLAKE3 hash
//! of the payload. 96 bits makes a deliberate collision astronomically
//! unlikely (2^48 birthday bound), so a key is treated as a unique handle for
//! its payload. Key length is aligned with Headroom's CCR for cross-tool
//! comparability.

/// Compute a 24-hex-char stash key for `payload`.
pub fn compute_key(payload: &[u8]) -> String {
    blake3::hash(payload).to_hex().as_str()[..24].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_24_hex_chars() {
        let key = compute_key(b"hello world");
        assert_eq!(key.len(), 24);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn same_payload_same_key() {
        assert_eq!(compute_key(b"payload-a"), compute_key(b"payload-a"));
    }

    #[test]
    fn different_payload_different_key() {
        assert_ne!(compute_key(b"payload-a"), compute_key(b"payload-b"));
    }

    #[test]
    fn empty_payload_is_stable() {
        let key = compute_key(b"");
        assert_eq!(key.len(), 24);
        assert_eq!(key, compute_key(b""));
    }

    #[test]
    fn cjk_payload_key_stable() {
        let key1 = compute_key("你好世界".as_bytes());
        let key2 = compute_key("你好世界".as_bytes());
        assert_eq!(key1, key2);
        assert_ne!(key1, compute_key("你好世人".as_bytes()));
    }

    #[test]
    fn no_collisions_across_many_payloads() {
        let mut seen = std::collections::HashSet::new();
        for i in 0..100_000u64 {
            let payload = format!("collision-probe-{i}");
            let key = compute_key(payload.as_bytes());
            assert!(seen.insert(key), "collision at i={i}");
        }
    }
}
