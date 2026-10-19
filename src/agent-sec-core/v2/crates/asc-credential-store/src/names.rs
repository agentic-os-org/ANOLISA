use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::CredentialStoreError;

macro_rules! dns_label_name {
    ($(#[$meta:meta])* $name:ident, $field:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Validates a lowercase DNS label (1..=63 bytes of `a-z`, `0-9`, `-`,
            /// not starting or ending with `-`).
            ///
            /// # Errors
            /// Returns [`CredentialStoreError::Invalid`] for any other value.
            pub fn new(value: impl Into<String>) -> Result<Self, CredentialStoreError> {
                let value = value.into();
                if is_dns_label(&value) {
                    Ok(Self(value))
                } else {
                    Err(CredentialStoreError::Invalid {
                        field: $field,
                        reason: "must be a lowercase DNS label",
                    })
                }
            }

            /// Returns the wire representation.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }
    };
}

dns_label_name!(
    /// Identifier of a managed credential, such as the `github` preset.
    CredentialId,
    "credential id"
);

dns_label_name!(
    /// Name of an Agent domain, the subject a fake is issued to.
    AgentName,
    "agent name"
);

fn is_dns_label(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=63).contains(&bytes.len())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        && bytes.first() != Some(&b'-')
        && bytes.last() != Some(&b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_accept_only_lowercase_dns_labels() {
        for value in ["github", "dev-agent", "a", "a1", &"a".repeat(63)] {
            assert!(AgentName::new(value).is_ok(), "{value}");
        }
        for value in ["", "-a", "a-", "Dev", "a_b", "a.b", "../x", &"a".repeat(64)] {
            assert!(AgentName::new(value).is_err(), "{value}");
        }
    }

    #[test]
    fn deserialization_enforces_validation() {
        assert!(serde_json::from_str::<CredentialId>(r#""github""#).is_ok());
        assert!(serde_json::from_str::<CredentialId>(r#""Git/hub""#).is_err());
    }
}
