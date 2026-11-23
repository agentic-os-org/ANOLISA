//! Conservative shape-only fake generation; client acceptance is not guaranteed.

use ring::rand::{SecureRandom, SystemRandom};

use crate::CredentialStoreError;
use crate::secret::validate_token_value;

const MIN_RANDOMIZED_POSITIONS: usize = 16;
const GITHUB_PREFIXES: [&str; 6] = ["github_pat_", "ghp_", "gho_", "ghu_", "ghs_", "ghr_"];

/// Checks visible ASCII, equal length, and at least 80% differing positions.
///
/// Prefixes and punctuation count in the ratio; short explicit pairs are allowed.
///
/// # Errors
/// Returns value-free validation errors for invalid values, length or similarity.
pub fn validate_pair(real: &str, fake: &str) -> Result<(), CredentialStoreError> {
    validate_token_value(real, "real value")?;
    validate_token_value(fake, "fake value")?;
    if real.len() != fake.len() {
        return Err(CredentialStoreError::Invalid {
            field: "pair",
            reason: "real and fake must have equal lengths",
        });
    }
    let mismatches = real
        .bytes()
        .zip(fake.bytes())
        .filter(|(a, b)| a != b)
        .count();
    if mismatches * 5 < real.len() * 4 {
        return Err(CredentialStoreError::Invalid {
            field: "pair",
            reason: "at least 80% of all positions must differ, including prefix and punctuation",
        });
    }
    Ok(())
}

/// Generates a shape-preserving fake for a known prefix or a plain alphanumeric token.
///
/// Only `auto` is supported. Structured unknown values require an explicit caller pair.
/// This is a generation heuristic, not a claim that a client accepts the result.
///
/// # Errors
/// Rejects unknown formats, unsafe values, fewer than 16 randomized positions,
/// and shapes whose preserved bytes prevent 80% difference; may fail on randomness.
pub fn generate_fake(real: &str, format: &str) -> Result<String, CredentialStoreError> {
    if format != "auto" {
        return Err(CredentialStoreError::Invalid {
            field: "format",
            reason: "only auto is supported; provide an explicit fake for other formats",
        });
    }
    FakeFormat::with_prefixes(GITHUB_PREFIXES).generate(real, &SystemRandom::new())
}

/// Pure generation helper with explicitly known prefixes and same-class replacement.
///
/// Keeps the longest matching prefix and punctuation. Unknown structured values
/// are rejected rather than guessed; generated fakes carry no readable marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeFormat {
    prefixes: Vec<String>,
}

impl FakeFormat {
    /// Builds a helper that preserves the longest matching caller-known prefix.
    pub fn with_prefixes<I, S>(prefixes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut prefixes: Vec<String> = prefixes.into_iter().map(Into::into).collect();
        prefixes.retain(|prefix| !prefix.is_empty());
        prefixes.sort_by_key(|prefix| std::cmp::Reverse(prefix.len()));
        Self { prefixes }
    }

    fn split<'a>(&self, real: &'a str) -> (&'a str, &'a str) {
        let prefix_len = self
            .prefixes
            .iter()
            .find(|prefix| real.starts_with(prefix.as_str()))
            .map_or(0, String::len);
        real.split_at(prefix_len)
    }

    fn check(&self, real: &str) -> Result<(), CredentialStoreError> {
        validate_token_value(real, "real value")?;
        let (prefix, body) = self.split(real);
        if prefix.is_empty() && !real.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(CredentialStoreError::Invalid {
                field: "real value",
                reason: "unknown structured format; provide an explicit fake",
            });
        }
        let positions = body.bytes().filter(u8::is_ascii_alphanumeric).count();
        if positions < MIN_RANDOMIZED_POSITIONS {
            return Err(CredentialStoreError::Invalid {
                field: "real value",
                reason: "needs at least 16 letters or digits after the known prefix",
            });
        }
        if positions * 5 < real.len() * 4 {
            return Err(CredentialStoreError::Invalid {
                field: "real value",
                reason: "preserved prefix and punctuation prevent 80% difference",
            });
        }
        Ok(())
    }

    /// Generates same-class replacements, excluding the original at every random position.
    ///
    /// # Errors
    /// Returns shape, pair validation or system randomness errors.
    pub fn generate(
        &self,
        real: &str,
        random: &SystemRandom,
    ) -> Result<String, CredentialStoreError> {
        self.check(real)?;
        let (prefix, body) = self.split(real);
        let mut fake = String::with_capacity(real.len());
        fake.push_str(prefix);
        for byte in body.bytes() {
            let (base, count) = match byte {
                b'a'..=b'z' => (b'a', 26),
                b'A'..=b'Z' => (b'A', 26),
                b'0'..=b'9' => (b'0', 10),
                other => {
                    fake.push(char::from(other));
                    continue;
                }
            };
            // Sample the class minus the original, rather than hoping retries meet the ratio.
            let offset = random_below(random, count - 1)?;
            let original = byte - base;
            let replacement = base + offset + u8::from(offset >= original);
            fake.push(char::from(replacement));
        }
        validate_pair(real, &fake)?;
        Ok(fake)
    }
}

fn random_below(random: &SystemRandom, bound: u8) -> Result<u8, CredentialStoreError> {
    let limit = u8::MAX - (u8::MAX % bound);
    for _ in 0..16 {
        let mut byte = [0_u8; 1];
        random
            .fill(&mut byte)
            .map_err(|_| CredentialStoreError::Randomness)?;
        if byte[0] < limit {
            return Ok(byte[0] % bound);
        }
    }
    Err(CredentialStoreError::Randomness)
}
