use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

/// A bounded non-empty identifier carried across process boundaries.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ResourceId(String);

impl ResourceId {
    /// Builds an identifier accepted by the policy control plane.
    ///
    /// # Errors
    /// Returns an error for empty, oversized, or unsafe identifiers.
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        if value.is_empty() || value.len() > 128 {
            return Err(IdentifierError);
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
        {
            return Err(IdentifierError);
        }
        Ok(Self(value))
    }

    /// Returns the wire representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ResourceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ResourceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Invalid identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("identifier must be 1..=128 ASCII letters, digits, '.', ':', '_' or '-'")]
pub struct IdentifierError;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_id_accepts_only_the_bounded_wire_alphabet() {
        let valid = ResourceId::new("policy:v2_example-1.0").unwrap();
        assert_eq!(valid.as_str(), "policy:v2_example-1.0");

        assert!(ResourceId::new("").is_err());
        assert!(ResourceId::new("contains/slash").is_err());
        assert!(ResourceId::new("contains space").is_err());
        assert!(ResourceId::new("非ascii").is_err());
        assert!(ResourceId::new("a".repeat(129)).is_err());
    }

    #[test]
    fn resource_id_deserialization_enforces_constructor_validation() {
        let parsed: ResourceId = serde_json::from_str(r#""binding:valid-1""#).unwrap();
        assert_eq!(parsed.as_str(), "binding:valid-1");

        assert!(serde_json::from_str::<ResourceId>(r#""../invalid""#).is_err());
        assert!(serde_json::from_str::<ResourceId>(r#""""#).is_err());
    }
}
