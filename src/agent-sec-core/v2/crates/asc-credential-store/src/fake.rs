use ring::rand::{SecureRandom, SystemRandom};

use crate::CredentialStoreError;

/// Fewer randomized positions would make a fake guessable from its real value's shape.
const MIN_RANDOMIZED_POSITIONS: usize = 16;

/// Shape rules for format-preserving fakes.
///
/// A fake keeps the longest matching known prefix and the real value's length;
/// every ASCII letter or digit after the prefix is replaced by a random
/// character of the same class, and punctuation is kept. Client-side format
/// checks therefore accept the fake, while the fake carries no readable marker:
/// the gateway identifies it only by exact registry lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeFormat {
    prefixes: Vec<String>,
}

impl FakeFormat {
    /// Builds a format that preserves any of `prefixes` found at the start of the real value.
    pub fn with_prefixes<I, S>(prefixes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut prefixes: Vec<String> = prefixes.into_iter().map(Into::into).collect();
        prefixes.sort_by_key(|prefix| std::cmp::Reverse(prefix.len()));
        Self { prefixes }
    }

    pub(crate) fn prefixes(&self) -> &[String] {
        &self.prefixes
    }

    /// Checks that fakes for `real` would carry enough randomness.
    pub(crate) fn check(&self, real: &str) -> Result<(), CredentialStoreError> {
        let (_, body) = self.split(real);
        if body.bytes().filter(u8::is_ascii_alphanumeric).count() < MIN_RANDOMIZED_POSITIONS {
            return Err(CredentialStoreError::Invalid {
                field: "real value",
                reason: "needs at least 16 letters or digits after the known prefix",
            });
        }
        Ok(())
    }

    fn split<'a>(&self, real: &'a str) -> (&'a str, &'a str) {
        let prefix_len = self
            .prefixes
            .iter()
            .find(|prefix| real.starts_with(prefix.as_str()))
            .map_or(0, String::len);
        real.split_at(prefix_len)
    }

    pub(crate) fn generate(
        &self,
        real: &str,
        random: &SystemRandom,
    ) -> Result<String, CredentialStoreError> {
        self.check(real)?;
        let (prefix, body) = self.split(real);
        let mut fake = String::with_capacity(real.len());
        fake.push_str(prefix);
        for byte in body.bytes() {
            let replacement = match byte {
                b'a'..=b'z' => b'a' + random_below(random, 26)?,
                b'A'..=b'Z' => b'A' + random_below(random, 26)?,
                b'0'..=b'9' => b'0' + random_below(random, 10)?,
                other => other,
            };
            fake.push(char::from(replacement));
        }
        Ok(fake)
    }
}

/// Uniform draw by rejection sampling, so no character is favoured.
fn random_below(random: &SystemRandom, bound: u8) -> Result<u8, CredentialStoreError> {
    let limit = u8::MAX - (u8::MAX % bound);
    loop {
        let mut byte = [0_u8; 1];
        random
            .fill(&mut byte)
            .map_err(|_| CredentialStoreError::Randomness)?;
        if byte[0] < limit {
            return Ok(byte[0] % bound);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github() -> FakeFormat {
        FakeFormat::with_prefixes(["ghp_", "github_pat_", "gho_"])
    }

    #[test]
    fn fake_preserves_prefix_length_and_character_classes() {
        let real = "github_pat_11ABCDEFG0abcdefghij_KLMNOPQRSTUVWXYZ0123456789abcdef";
        let fake = github().generate(real, &SystemRandom::new()).unwrap();
        assert_ne!(fake, real);
        assert_eq!(fake.len(), real.len());
        assert!(fake.starts_with("github_pat_"));
        for (fake_byte, real_byte) in fake.bytes().zip(real.bytes()) {
            let same_class = (fake_byte.is_ascii_lowercase() && real_byte.is_ascii_lowercase())
                || (fake_byte.is_ascii_uppercase() && real_byte.is_ascii_uppercase())
                || (fake_byte.is_ascii_digit() && real_byte.is_ascii_digit())
                || fake_byte == real_byte;
            assert!(same_class);
        }
    }

    #[test]
    fn unknown_prefix_randomizes_the_whole_value() {
        let real = "abcdefghijklmnopqrstuvwxyz";
        let fake = github().generate(real, &SystemRandom::new()).unwrap();
        assert_eq!(fake.len(), real.len());
        assert_ne!(fake, real);
    }

    #[test]
    fn low_entropy_values_are_rejected() {
        assert!(
            github()
                .generate("ghp_short-value", &SystemRandom::new())
                .is_err()
        );
    }
}
