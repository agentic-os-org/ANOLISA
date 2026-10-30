//! Native release metadata is evidence; adapters validate the required interfaces.

use crate::Result;

/// Retain bounded native metadata independently of release ordering.
pub(crate) fn reported(value: &str, name: &str) -> Result<String> {
    if value.is_empty()
        || value.len() > 128
        || !value.as_bytes()[0].is_ascii_digit()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".+-_!".contains(&byte))
    {
        return Err(format!("{name} must report a version of 1..128 ASCII bytes, starting with a digit; got {value:?}").into());
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_native_releases_without_a_release_allowlist() {
        for value in [
            "1.0.55",
            "1.0.57",
            "2.0.0",
            "2.2.2.post2",
            "1!2.0rc1",
            "2026.10.29",
            "0.85.0-preview+build.1",
        ] {
            assert_eq!(reported(value, "SDK").unwrap(), value);
        }
    }

    #[test]
    fn rejects_missing_or_ambiguous_metadata() {
        for value in [
            "",
            "unknown",
            "SDK 1.2.3",
            "1.2.3\nother",
            "1.2.3\0",
            "１.2.3",
            &"1".repeat(129),
        ] {
            assert!(reported(value, "SDK").is_err(), "{value:?}");
        }
    }
}
