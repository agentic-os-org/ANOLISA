//! Tokenizer for estimating token counts.

pub use tokenless_protocol::{count_chars, estimate_tokens, estimate_tokens_from_bytes};

/// Backwards-compatible struct for existing code.
/// Prefer using the free functions `estimate_tokens` and `count_chars` directly.
pub struct Tokenizer;

impl Tokenizer {
    #[doc(hidden)]
    pub fn new() -> Self {
        Self
    }

    #[doc(hidden)]
    pub fn estimate_tokens(&self, text: &str) -> usize {
        estimate_tokens(text)
    }

    #[doc(hidden)]
    pub fn count_chars(&self, text: &str) -> usize {
        count_chars(text)
    }
}

impl Default for Tokenizer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("tests/tokenizer_tests.rs");
}
