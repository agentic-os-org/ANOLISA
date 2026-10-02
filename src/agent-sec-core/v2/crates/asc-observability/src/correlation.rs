//! Correlation ID length capping, mirroring v1 `correlation_context`.

/// Maximum persisted length of a correlation ID, in characters.
pub const MAX_CORRELATION_ID_LENGTH: usize = 256;

/// Suffix appended when a correlation ID is capped.
pub const TRUNCATED_CORRELATION_ID_SUFFIX: &str = "...[truncated]";

/// Caps `value` to [`MAX_CORRELATION_ID_LENGTH`] characters.
///
/// v1 measures and slices with Python `str` semantics, i.e. by code point, not
/// by byte, so this counts `char`s to stay byte-identical on non-ASCII input.
#[must_use]
pub fn truncate_correlation_id(value: &str) -> String {
    let length = value.chars().count();
    if length <= MAX_CORRELATION_ID_LENGTH {
        return value.to_owned();
    }

    let prefix_len = MAX_CORRELATION_ID_LENGTH - TRUNCATED_CORRELATION_ID_SUFFIX.chars().count();
    let mut out: String = value.chars().take(prefix_len).collect();
    out.push_str(TRUNCATED_CORRELATION_ID_SUFFIX);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_ids_pass_through() {
        assert_eq!(truncate_correlation_id("abc"), "abc");
        let exact = "x".repeat(MAX_CORRELATION_ID_LENGTH);
        assert_eq!(truncate_correlation_id(&exact), exact);
    }

    /// Asserted against a live v1 run: 300 `x` chars capped to 256 total.
    #[test]
    fn long_ids_are_capped_to_v1_shape() {
        let capped = truncate_correlation_id(&"x".repeat(300));
        assert_eq!(capped.chars().count(), MAX_CORRELATION_ID_LENGTH);
        assert!(capped.ends_with(TRUNCATED_CORRELATION_ID_SUFFIX));
        assert_eq!(
            capped.chars().take(242).collect::<String>(),
            "x".repeat(242)
        );
    }

    #[test]
    fn capping_counts_characters_not_bytes() {
        let capped = truncate_correlation_id(&"漢".repeat(300));
        assert_eq!(capped.chars().count(), MAX_CORRELATION_ID_LENGTH);
    }
}
