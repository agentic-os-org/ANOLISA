//! Canonical extension identifiers and capability security fingerprints.

use std::fmt;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Maximum byte length of package names and capability local IDs.
pub const MAX_ID_BYTES: usize = 64;

/// Supported extension capability namespaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CapabilityKind {
    /// Agent skill contribution.
    Skill,
    /// Runtime hook contribution.
    Hook,
    /// MCP server contribution.
    Mcp,
    /// Context file contribution.
    Context,
    /// Agent definition contribution.
    Agent,
}

impl fmt::Display for CapabilityKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Skill => "skill",
            Self::Hook => "hook",
            Self::Mcp => "mcp",
            Self::Context => "context",
            Self::Agent => "agent",
        };
        formatter.write_str(value)
    }
}

/// Typed canonical capability identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CapabilityId {
    /// Owning extension package name.
    pub extension: String,
    /// Capability namespace.
    pub kind: CapabilityKind,
    /// Extension-local capability name.
    pub local: String,
}

impl CapabilityId {
    /// Validates and builds a canonical capability identity.
    pub fn new(
        extension: impl Into<String>,
        kind: CapabilityKind,
        local: impl Into<String>,
    ) -> Result<Self, IdentityError> {
        let extension = extension.into();
        let local = local.into();
        validate_package_name(&extension)?;
        validate_local_id(&local)?;
        Ok(Self {
            extension,
            kind,
            local,
        })
    }

    /// Returns the persistent protocol representation.
    pub fn canonical(&self) -> String {
        format!("{}/{}/{}", self.extension, self.kind, self.local)
    }
}

/// Validation error for a package, local capability, or setting identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityError {
    code: &'static str,
    message: String,
}

impl IdentityError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Returns the stable diagnostic code.
    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for IdentityError {}

/// Validates a canonical lowercase ASCII package name.
pub fn validate_package_name(value: &str) -> Result<(), IdentityError> {
    validate_dotted_id(value, "extension_name")
}

/// Validates a canonical lowercase ASCII capability local ID.
pub fn validate_local_id(value: &str) -> Result<(), IdentityError> {
    validate_dotted_id(value, "extension_capability_id")
}

fn validate_dotted_id(value: &str, prefix: &'static str) -> Result<(), IdentityError> {
    if value.is_empty() {
        return Err(IdentityError::new(
            if prefix == "extension_name" {
                "extension_name_invalid"
            } else {
                "extension_capability_id_invalid"
            },
            "extension identity cannot be empty",
        ));
    }
    if value.len() > MAX_ID_BYTES {
        return Err(IdentityError::new(
            if prefix == "extension_name" {
                "extension_name_too_long"
            } else {
                "extension_capability_id_too_long"
            },
            format!("extension identity exceeds {MAX_ID_BYTES} bytes: {value}"),
        ));
    }
    if value.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Err(IdentityError::new(
            if prefix == "extension_name" {
                "extension_name_not_canonical"
            } else {
                "extension_capability_id_not_canonical"
            },
            format!("extension identity must already be lowercase ASCII: {value}"),
        ));
    }
    let bytes = value.as_bytes();
    let valid_edge = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    let valid_inner = |byte: u8| valid_edge(byte) || matches!(byte, b'.' | b'_' | b'-');
    if !valid_edge(bytes[0])
        || !valid_edge(bytes[bytes.len() - 1])
        || bytes.iter().copied().any(|byte| !valid_inner(byte))
    {
        return Err(IdentityError::new(
            if prefix == "extension_name" {
                "extension_name_invalid"
            } else {
                "extension_capability_id_invalid"
            },
            format!("invalid extension identity: {value}"),
        ));
    }
    Ok(())
}

/// Validates a camelCase ASCII extension setting key.
pub fn validate_setting_key(value: &str) -> Result<(), IdentityError> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || !value.as_bytes()[0].is_ascii_lowercase()
        || value.bytes().any(|byte| !byte.is_ascii_alphanumeric())
    {
        return Err(IdentityError::new(
            "extension_setting_key_invalid",
            format!("invalid extension setting key: {value}"),
        ));
    }
    Ok(())
}

/// Hashes a validated capability security projection as canonical JSON.
pub fn fingerprint_projection(mut projection: Value) -> Result<String, serde_json::Error> {
    canonicalize_value(&mut projection);
    let bytes = serde_json::to_vec(&projection)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(hex::encode(hasher.finalize()))
}

fn canonicalize_value(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for value in values.iter_mut() {
                canonicalize_value(value);
            }
        }
        Value::Object(map) => {
            let mut entries = std::mem::take(map).into_iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
            for (_, value) in &mut entries {
                canonicalize_value(value);
            }
            map.extend(entries);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_spec_identity_vectors() {
        assert!(validate_package_name("example.ops").is_ok());
        assert!(validate_package_name("a").is_ok());
        assert_eq!(
            validate_package_name("Example.Ops").unwrap_err().code(),
            "extension_name_not_canonical"
        );
        for invalid in [".example", "example.", "example/ops"] {
            assert_eq!(
                validate_package_name(invalid).unwrap_err().code(),
                "extension_name_invalid"
            );
        }
        assert_eq!(
            validate_package_name(&"a".repeat(65)).unwrap_err().code(),
            "extension_name_too_long"
        );
        assert!(validate_setting_key("inventoryToken").is_ok());
        assert_eq!(
            validate_setting_key("InventoryToken").unwrap_err().code(),
            "extension_setting_key_invalid"
        );
        assert_eq!(
            validate_setting_key("inventory-token").unwrap_err().code(),
            "extension_setting_key_invalid"
        );
    }

    #[test]
    fn builds_canonical_capability_id() {
        let id = CapabilityId::new("example.ops", CapabilityKind::Hook, "guard").unwrap();
        assert_eq!(id.canonical(), "example.ops/hook/guard");
    }

    #[test]
    fn fingerprint_matches_normative_vector() {
        let projection = json!({
            "settings": [],
            "policyVersion": 1,
            "extension": "example.ops",
            "hostExecutables": [],
            "capabilities": [{
                "type": "command",
                "matcher": "shell",
                "kind": "hook",
                "id": "example.ops/hook/guard",
                "event": "PreToolUse",
                "command": "hooks/guard"
            }]
        });
        assert_eq!(
            fingerprint_projection(projection).unwrap(),
            "f678fe77434f8ed6a87de660a42db17c06aa29411280150fd92f2c29f8012b13"
        );
    }

    #[test]
    fn fingerprint_ignores_nested_object_insertion_order() {
        let first: Value =
            serde_json::from_str(r#"{"z":{"b":2,"a":1},"a":[{"d":4,"c":3}]}"#).unwrap();
        let second: Value =
            serde_json::from_str(r#"{"a":[{"c":3,"d":4}],"z":{"a":1,"b":2}}"#).unwrap();

        assert_eq!(
            fingerprint_projection(first).unwrap(),
            fingerprint_projection(second).unwrap()
        );
    }
}
