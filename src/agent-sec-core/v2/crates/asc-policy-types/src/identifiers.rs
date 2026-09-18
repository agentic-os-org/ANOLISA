//! Strong identifiers, revisions, and content digests.

use std::fmt;

pub use asc_foundation_types::{ResourceId, Revision};
use serde::{Deserialize, Deserializer, Serialize};

fn validate_identifier(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("must not be empty".to_owned());
    }
    if value.len() > 256 {
        return Err("must not exceed 256 bytes".to_owned());
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "-_.:".contains(character))
    {
        return Err("contains unsupported characters".to_owned());
    }
    Ok(())
}

fn validate_profile_identifier(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("must not be empty".to_owned());
    }
    if value.len() > 256 {
        return Err("must not exceed 256 bytes".to_owned());
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "-_.:/".contains(character))
    {
        return Err("contains unsupported profile identifier characters".to_owned());
    }
    Ok(())
}

macro_rules! identifier {
    ($name:ident, $doc:literal) => {
        identifier!($name, $doc, validate_identifier);
    };
    ($name:ident, $doc:literal, $validator:path) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates a validated identifier.
            ///
            /// # Errors
            /// Returns an error when the value is empty, too long, or contains
            /// a character outside the closed wire alphabet.
            pub fn new(value: impl Into<String>) -> Result<Self, String> {
                Self::try_from(value.into())
            }

            /// Returns the wire value.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = String;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                $validator(&value)?;
                Ok(Self(value))
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::try_from(value).map_err(serde::de::Error::custom)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

identifier!(PolicyId, "Stable policy identifier.");
identifier!(
    ProfileId,
    "Immutable Canonical Policy IR profile identifier.",
    validate_profile_identifier
);
identifier!(RuleId, "Stable rule identifier within one policy revision.");
identifier!(
    ResourceSetId,
    "Stable resource-set identifier within one policy revision."
);
identifier!(Label, "Backend-independent sensitive-data label.");

/// Lowercase SHA-256 digest encoded as `sha256:<64 hex characters>`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Digest(String);

impl Digest {
    /// Parses and validates a SHA-256 digest.
    ///
    /// # Errors
    /// Returns an error unless the value is `sha256:` followed by exactly 64
    /// lowercase hexadecimal characters.
    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        Self::try_from(value.into())
    }

    /// Returns the wire value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Digest {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let Some(hex) = value.strip_prefix("sha256:") else {
            return Err("digest must use the sha256 algorithm prefix".to_owned());
        };
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(
                "digest must contain exactly 64 lowercase hexadecimal characters".to_owned(),
            );
        }
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_from(value).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_and_profile_ids_keep_distinct_wire_alphabets() {
        assert!(PolicyId::new("policy:v2-1").is_ok());
        assert!(PolicyId::new("policy/v2").is_err());
        assert!(ProfileId::new("agentseccore-canonical-ir/v1alpha1-demo1").is_ok());
        assert!(RuleId::new("").is_err());
        assert!(ResourceSetId::new("a".repeat(257)).is_err());
        assert!(Label::new("contains space").is_err());
    }

    #[test]
    fn revision_wire_values_are_positive_and_bounded() {
        assert!(Revision::new(0).is_err());
        let maximum = Revision::new(u32::MAX).unwrap();
        assert_eq!(maximum.get(), u32::MAX);
        assert!(maximum.checked_next().is_err());

        assert!(serde_json::from_str::<Revision>("0").is_err());
        assert!(serde_json::from_str::<Revision>("4294967296").is_err());
    }

    #[test]
    fn shared_foundation_types_keep_one_rust_and_wire_contract() {
        let foundation_id = asc_foundation_types::ResourceId::new("resource:shared-1").unwrap();
        let policy_id: ResourceId = foundation_id;
        let id_wire = serde_json::to_string(&policy_id).unwrap();
        let foundation_id: asc_foundation_types::ResourceId =
            serde_json::from_str(&id_wire).unwrap();
        assert_eq!(foundation_id.as_str(), "resource:shared-1");
        assert!(ResourceId::new("a".repeat(129)).is_err());

        let foundation_revision = asc_foundation_types::Revision::new(7).unwrap();
        let policy_revision: Revision = foundation_revision;
        let revision_wire = serde_json::to_string(&policy_revision).unwrap();
        let foundation_revision: asc_foundation_types::Revision =
            serde_json::from_str(&revision_wire).unwrap();
        assert_eq!(foundation_revision.get(), 7);
    }

    #[test]
    fn digest_requires_a_lowercase_sha256_wire_value() {
        let valid = format!("sha256:{}", "a".repeat(64));
        assert_eq!(Digest::new(&valid).unwrap().as_str(), valid);
        assert!(Digest::new("a".repeat(64)).is_err());
        assert!(Digest::new(format!("sha256:{}", "A".repeat(64))).is_err());
        assert!(Digest::new(format!("sha256:{}", "a".repeat(63))).is_err());
    }
}
