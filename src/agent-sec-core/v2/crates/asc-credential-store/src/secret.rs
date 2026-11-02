use std::fmt;

use crate::CredentialStoreError;

/// Upper bound for real and fake values in bytes.
pub const MAX_SECRET_BYTES: usize = 512;

/// A real credential value.
///
/// Deliberately implements neither `Clone`, `Serialize` nor `Display`, and its
/// `Debug` output is fixed, so the value cannot leak through logs, plans or
/// generic serialization. Only the store's own persistence and the gateway
/// snapshot read it through [`Secret::expose`].
pub struct Secret(Box<str>);

impl Secret {
    /// Wraps a real value of 1..=512 visible ASCII bytes.
    ///
    /// # Errors
    /// Returns [`CredentialStoreError::Invalid`] for empty, oversized, or
    /// non-visible-ASCII input; the error does not contain the value.
    pub fn new(value: String) -> Result<Self, CredentialStoreError> {
        validate_token_value(&value, "real value")?;
        Ok(Self(value.into_boxed_str()))
    }

    /// Returns the real value; callers must not log or persist it outside the store.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Compares with `other` without an early exit on the first differing byte.
    ///
    /// Only the length can leak, and fakes deliberately share the real value's length.
    pub(crate) fn matches(&self, other: &str) -> bool {
        let (left, right) = (self.0.as_bytes(), other.as_bytes());
        if left.len() != right.len() {
            return false;
        }
        let difference = left
            .iter()
            .zip(right)
            .fold(0_u8, |acc, (a, b)| acc | (a ^ b));
        std::hint::black_box(difference) == 0
    }

    /// Copies the value for the store's copy-validate-commit updates; not exposed as `Clone`.
    pub(crate) fn duplicate(&self) -> Self {
        Self(self.0.clone())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<secret>")
    }
}

/// Header and Basic carriers can only transport visible ASCII without quoting.
pub(crate) fn validate_token_value(
    value: &str,
    field: &'static str,
) -> Result<(), CredentialStoreError> {
    if value.is_empty() || value.len() > MAX_SECRET_BYTES {
        return Err(CredentialStoreError::Invalid {
            field,
            reason: "must be 1..=512 bytes",
        });
    }
    if !value.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(CredentialStoreError::Invalid {
            field,
            reason: "must contain only visible ASCII characters",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_shows_the_value() {
        let secret = Secret::new("ghp_supersecretvalue".into()).unwrap();
        assert_eq!(format!("{secret:?}"), "<secret>");
    }

    #[test]
    fn rejects_unsafe_values_without_echoing_them() {
        for value in [
            String::new(),
            "a".repeat(513),
            "has space".into(),
            "tab\tx".into(),
            "é".into(),
        ] {
            let error = Secret::new(value.clone()).unwrap_err().to_string();
            assert!(value.is_empty() || !error.contains(&value), "{error}");
        }
        assert!(Secret::new("a".repeat(512)).is_ok());
    }
}
