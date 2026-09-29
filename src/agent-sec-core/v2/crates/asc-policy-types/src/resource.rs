//! Backend-independent resource sets and selectors.

use std::collections::HashSet;
use std::net::IpAddr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Validate, ValidationError};
use crate::identifiers::{Digest, ResourceSetId};

/// Resource domain used for operation compatibility checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    /// Files and directory entries.
    File,
    /// Network destinations.
    Endpoint,
    /// Executable images.
    Executable,
    /// Credentials referenced by stable product identity.
    Credential,
}

/// One named set of resources in a policy revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceSet {
    /// Policy-local resource-set identity.
    pub id: ResourceSetId,
    /// Typed selector for this set.
    pub selector: ResourceSelector,
}

#[derive(Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum ResourceSetRef<'a> {
    File {
        id: &'a ResourceSetId,
        matchers: &'a [FileMatcher],
    },
    Endpoint {
        id: &'a ResourceSetId,
        matchers: &'a [EndpointMatcher],
    },
    Executable {
        id: &'a ResourceSetId,
        matchers: &'a [ExecutableMatcher],
    },
    Credential {
        id: &'a ResourceSetId,
        matchers: &'a [CredentialMatcher],
    },
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum ResourceSetOwned {
    File {
        id: ResourceSetId,
        matchers: Vec<FileMatcher>,
    },
    Endpoint {
        id: ResourceSetId,
        matchers: Vec<EndpointMatcher>,
    },
    Executable {
        id: ResourceSetId,
        matchers: Vec<ExecutableMatcher>,
    },
    Credential {
        id: ResourceSetId,
        matchers: Vec<CredentialMatcher>,
    },
}

impl Serialize for ResourceSet {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match &self.selector {
            ResourceSelector::File { matchers } => ResourceSetRef::File {
                id: &self.id,
                matchers,
            },
            ResourceSelector::Endpoint { matchers } => ResourceSetRef::Endpoint {
                id: &self.id,
                matchers,
            },
            ResourceSelector::Executable { matchers } => ResourceSetRef::Executable {
                id: &self.id,
                matchers,
            },
            ResourceSelector::Credential { matchers } => ResourceSetRef::Credential {
                id: &self.id,
                matchers,
            },
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ResourceSet {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let (id, selector) = match ResourceSetOwned::deserialize(deserializer)? {
            ResourceSetOwned::File { id, matchers } => (id, ResourceSelector::File { matchers }),
            ResourceSetOwned::Endpoint { id, matchers } => {
                (id, ResourceSelector::Endpoint { matchers })
            }
            ResourceSetOwned::Executable { id, matchers } => {
                (id, ResourceSelector::Executable { matchers })
            }
            ResourceSetOwned::Credential { id, matchers } => {
                (id, ResourceSelector::Credential { matchers })
            }
        };
        Ok(Self { id, selector })
    }
}

impl Validate for ResourceSet {
    fn validate(&self) -> Result<(), ValidationError> {
        self.selector.validate()
    }
}

/// Closed set of V1 resource selectors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ResourceSelector {
    /// Filesystem resources.
    File {
        /// File matchers whose union forms the resource set.
        matchers: Vec<FileMatcher>,
    },
    /// Network destinations.
    Endpoint {
        /// Endpoint matchers whose union forms the resource set.
        matchers: Vec<EndpointMatcher>,
    },
    /// Executable images.
    Executable {
        /// Executable matchers whose union forms the resource set.
        matchers: Vec<ExecutableMatcher>,
    },
    /// Product-level credential identities.
    Credential {
        /// Credential matchers whose union forms the resource set.
        matchers: Vec<CredentialMatcher>,
    },
}

impl ResourceSelector {
    /// Returns the resource domain selected by this value.
    pub const fn kind(&self) -> ResourceKind {
        match self {
            Self::File { .. } => ResourceKind::File,
            Self::Endpoint { .. } => ResourceKind::Endpoint,
            Self::Executable { .. } => ResourceKind::Executable,
            Self::Credential { .. } => ResourceKind::Credential,
        }
    }

    fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::File { matchers } => validate_matchers(matchers, "matchers", Validate::validate),
            Self::Endpoint { matchers } => {
                validate_matchers(matchers, "matchers", Validate::validate)
            }
            Self::Executable { matchers } => {
                validate_matchers(matchers, "matchers", Validate::validate)
            }
            Self::Credential { matchers } => {
                validate_matchers(matchers, "matchers", Validate::validate)
            }
        }
    }
}

fn validate_matchers<T, F>(
    matchers: &[T],
    path: &str,
    mut validate: F,
) -> Result<(), ValidationError>
where
    T: Eq + std::hash::Hash,
    F: FnMut(&T) -> Result<(), ValidationError>,
{
    if matchers.is_empty() {
        return Err(ValidationError::new(path, "must not be empty"));
    }
    let mut seen = HashSet::with_capacity(matchers.len());
    for (index, matcher) in matchers.iter().enumerate() {
        validate(matcher).map_err(|error| {
            ValidationError::new(format!("{path}[{index}].{}", error.path), error.message)
        })?;
        if !seen.insert(matcher) {
            return Err(ValidationError::new(
                format!("{path}[{index}]"),
                "duplicate matcher",
            ));
        }
    }
    Ok(())
}

/// One filesystem selector and its object-resolution semantics.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileMatcher {
    /// Canonical path matcher.
    pub path: PathMatcher,
    /// Required namespace/object resolution.
    pub resolution: FileResolution,
}

impl Validate for FileMatcher {
    fn validate(&self) -> Result<(), ValidationError> {
        self.path
            .validate()
            .map_err(|error| ValidationError::new(format!("path.{}", error.path), error.message))
    }
}

/// Canonical V1 path matcher.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PathMatcher {
    /// Match exactly one absolute path.
    Exact {
        /// Absolute normalized path.
        path: String,
    },
    /// Match one absolute path and its segment-delimited descendants.
    Prefix {
        /// Absolute normalized prefix.
        path: String,
    },
    /// Match an absolute path using the bounded V1 glob grammar.
    Glob {
        /// Absolute normalized glob pattern.
        pattern: String,
    },
}

impl Validate for PathMatcher {
    fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Exact { path } | Self::Prefix { path } => {
                validate_absolute_path(path, "path")?;
                if path.contains(['*', '?']) {
                    return Err(ValidationError::new(
                        "path",
                        "exact and prefix paths cannot contain wildcard syntax",
                    ));
                }
                Ok(())
            }
            Self::Glob { pattern } => validate_glob(pattern),
        }
    }
}

/// Filesystem identity against which a matcher is evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum FileResolution {
    /// Match the named directory entry.
    PathEntry,
    /// Match the object reached after final-component resolution.
    FinalObject {
        /// Whether a final symlink must be followed.
        follow_final_symlink: bool,
        /// Whether all hardlinks to the same object must be matched.
        match_hardlink_identity: bool,
    },
}

/// V1 endpoint matcher.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EndpointMatcher {
    /// Canonical lowercase host or `*.` suffix pattern.
    Host {
        /// Host pattern.
        pattern: String,
        /// Strictly ascending, non-zero destination ports.
        ports: Vec<u16>,
    },
    /// Canonical IP network.
    Cidr {
        /// CIDR in canonical network form.
        cidr: String,
        /// Strictly ascending, non-zero destination ports.
        ports: Vec<u16>,
    },
}

impl Validate for EndpointMatcher {
    fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Host { pattern, ports } => {
                validate_host_pattern(pattern)?;
                validate_ports(ports)
            }
            Self::Cidr { cidr, ports } => {
                validate_cidr(cidr)?;
                validate_ports(ports)
            }
        }
    }
}

/// V1 executable matcher.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ExecutableMatcher {
    /// Match an executable by an exact or bounded-glob absolute path.
    Path {
        /// Absolute normalized path pattern.
        pattern: String,
    },
    /// Match exact executable content.
    Digest {
        /// SHA-256 content digest.
        sha256: Digest,
    },
}

impl Validate for ExecutableMatcher {
    fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Path { pattern } => validate_glob(pattern),
            Self::Digest { .. } => Ok(()),
        }
    }
}

/// V1 product-level credential matcher.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum CredentialMatcher {
    /// Match one stable credential alias.
    Alias {
        /// Product-defined alias.
        value: String,
    },
    /// Match one stable credential class.
    Class {
        /// Product-defined class.
        value: String,
    },
}

impl Validate for CredentialMatcher {
    fn validate(&self) -> Result<(), ValidationError> {
        let value = match self {
            Self::Alias { value } | Self::Class { value } => value,
        };
        if value.trim().is_empty() || value.len() > 256 || value.contains('\0') {
            return Err(ValidationError::new(
                "value",
                "credential matcher must be a non-empty value of at most 256 bytes",
            ));
        }
        Ok(())
    }
}

fn validate_absolute_path(value: &str, path: &str) -> Result<(), ValidationError> {
    if !value.starts_with('/') || value.contains('\0') || value.contains('~') || value.contains('$')
    {
        return Err(ValidationError::new(
            path,
            "path must be absolute and contain no NUL, home expansion, or environment variables",
        ));
    }
    if value.len() > 4_096 {
        return Err(ValidationError::new(path, "path exceeds 4096 bytes"));
    }
    if value.len() > 1 && value.ends_with('/') {
        return Err(ValidationError::new(
            path,
            "path must not have a trailing separator",
        ));
    }
    if value.len() > 1 && value.contains("//") {
        return Err(ValidationError::new(
            path,
            "path contains repeated separators",
        ));
    }
    if value
        .split('/')
        .any(|segment| matches!(segment, "." | ".."))
    {
        return Err(ValidationError::new(path, "path contains a dot segment"));
    }
    Ok(())
}

fn validate_glob(pattern: &str) -> Result<(), ValidationError> {
    validate_absolute_path(pattern, "pattern")?;
    if pattern.contains(['[', ']', '{', '}', '\\']) {
        return Err(ValidationError::new(
            "pattern",
            "glob supports only *, ?, and whole-segment **",
        ));
    }
    for segment in pattern.split('/') {
        if segment.contains("**") && segment != "**" {
            return Err(ValidationError::new(
                "pattern",
                "** must occupy a complete path segment",
            ));
        }
    }
    Ok(())
}

fn validate_ports(ports: &[u16]) -> Result<(), ValidationError> {
    if ports.is_empty() {
        return Err(ValidationError::new(
            "ports",
            "V1 endpoint matchers require at least one port",
        ));
    }
    for (index, port) in ports.iter().copied().enumerate() {
        if port == 0 {
            return Err(ValidationError::new(
                format!("ports[{index}]"),
                "port must be greater than zero",
            ));
        }
        if index > 0 && port == ports[index - 1] {
            return Err(ValidationError::new(
                format!("ports[{index}]"),
                "duplicate port",
            ));
        }
        if index > 0 && port < ports[index - 1] {
            return Err(ValidationError::new(
                format!("ports[{index}]"),
                "ports must be in strictly ascending canonical order",
            ));
        }
    }
    Ok(())
}

fn validate_host_pattern(pattern: &str) -> Result<(), ValidationError> {
    let host = pattern.strip_prefix("*.").unwrap_or(pattern);
    if host.is_empty()
        || host.len() > 253
        || host.ends_with('.')
        || host.bytes().any(|byte| byte.is_ascii_uppercase())
        || host.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                || label.starts_with('-')
                || label.ends_with('-')
        })
    {
        return Err(ValidationError::new(
            "pattern",
            "host must be a canonical lowercase DNS name or *. suffix pattern",
        ));
    }
    Ok(())
}

fn validate_cidr(cidr: &str) -> Result<(), ValidationError> {
    let Some((address, prefix)) = cidr.split_once('/') else {
        return Err(ValidationError::new("cidr", "CIDR prefix is required"));
    };
    if prefix.contains('/') {
        return Err(ValidationError::new(
            "cidr",
            "CIDR contains multiple prefixes",
        ));
    }
    let address = address
        .parse::<IpAddr>()
        .map_err(|_| ValidationError::new("cidr", "invalid IP address"))?;
    let prefix = prefix
        .parse::<u8>()
        .map_err(|_| ValidationError::new("cidr", "invalid prefix length"))?;
    if cidr != format!("{address}/{prefix}") {
        return Err(ValidationError::new(
            "cidr",
            "CIDR must use canonical address and prefix spelling",
        ));
    }
    let canonical = match address {
        IpAddr::V4(address) if prefix <= 32 => {
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            u32::from(address) & mask == u32::from(address)
        }
        IpAddr::V6(address) if prefix <= 128 => {
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            u128::from(address) & mask == u128::from(address)
        }
        _ => false,
    };
    if !canonical {
        return Err(ValidationError::new(
            "cidr",
            "CIDR must use its canonical network address and prefix",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_set(matchers: Vec<FileMatcher>) -> ResourceSet {
        ResourceSet {
            id: ResourceSetId::new("protected-files").unwrap(),
            selector: ResourceSelector::File { matchers },
        }
    }

    #[test]
    fn file_selectors_require_normalized_absolute_paths() {
        let valid = file_set(vec![FileMatcher {
            path: PathMatcher::Glob {
                pattern: "/workspace/**".to_owned(),
            },
            resolution: FileResolution::PathEntry,
        }]);
        assert!(valid.validate().is_ok());

        for path in [
            "relative/path",
            "/workspace/../secret",
            "/workspace//secret",
        ] {
            let invalid = file_set(vec![FileMatcher {
                path: PathMatcher::Exact {
                    path: path.to_owned(),
                },
                resolution: FileResolution::PathEntry,
            }]);
            assert!(invalid.validate().is_err(), "accepted invalid path {path}");
        }
    }

    #[test]
    fn path_matchers_reject_trailing_separators_except_for_root() {
        for matcher in [
            PathMatcher::Exact {
                path: "/vault".to_owned(),
            },
            PathMatcher::Prefix {
                path: "/vault".to_owned(),
            },
            PathMatcher::Glob {
                pattern: "/vault".to_owned(),
            },
        ] {
            assert!(matcher.validate().is_ok());
        }

        for matcher in [
            PathMatcher::Exact {
                path: "/vault/".to_owned(),
            },
            PathMatcher::Prefix {
                path: "/vault/".to_owned(),
            },
            PathMatcher::Glob {
                pattern: "/vault/".to_owned(),
            },
        ] {
            assert!(matcher.validate().is_err());
        }

        for matcher in [
            PathMatcher::Exact {
                path: "/".to_owned(),
            },
            PathMatcher::Prefix {
                path: "/".to_owned(),
            },
            PathMatcher::Glob {
                pattern: "/".to_owned(),
            },
        ] {
            assert!(matcher.validate().is_ok());
        }
    }

    #[test]
    fn resource_sets_reject_empty_and_duplicate_matchers() {
        assert!(file_set(Vec::new()).validate().is_err());

        let matcher = FileMatcher {
            path: PathMatcher::Exact {
                path: "/workspace/secret".to_owned(),
            },
            resolution: FileResolution::FinalObject {
                follow_final_symlink: true,
                match_hardlink_identity: true,
            },
        };
        assert!(file_set(vec![matcher.clone(), matcher]).validate().is_err());
    }

    #[test]
    fn endpoint_selectors_require_canonical_hosts_networks_and_ports() {
        let valid = ResourceSet {
            id: ResourceSetId::new("trusted-endpoints").unwrap(),
            selector: ResourceSelector::Endpoint {
                matchers: vec![EndpointMatcher::Host {
                    pattern: "*.example.com".to_owned(),
                    ports: vec![443],
                }],
            },
        };
        assert!(valid.validate().is_ok());

        for matcher in [
            EndpointMatcher::Host {
                pattern: "Example.com".to_owned(),
                ports: vec![443],
            },
            EndpointMatcher::Cidr {
                cidr: "10.0.0.1/24".to_owned(),
                ports: vec![443],
            },
            EndpointMatcher::Host {
                pattern: "example.com".to_owned(),
                ports: vec![0],
            },
        ] {
            let invalid = ResourceSet {
                id: ResourceSetId::new("invalid-endpoint").unwrap(),
                selector: ResourceSelector::Endpoint {
                    matchers: vec![matcher],
                },
            };
            assert!(invalid.validate().is_err());
        }
    }

    #[test]
    fn endpoint_selectors_reject_permuted_and_duplicate_ports() {
        let permuted = ResourceSet {
            id: ResourceSetId::new("permuted-ports").unwrap(),
            selector: ResourceSelector::Endpoint {
                matchers: vec![
                    EndpointMatcher::Host {
                        pattern: "example.com".to_owned(),
                        ports: vec![80, 443],
                    },
                    EndpointMatcher::Host {
                        pattern: "example.com".to_owned(),
                        ports: vec![443, 80],
                    },
                ],
            },
        };
        let error = permuted.validate().unwrap_err();
        assert_eq!(error.path, "matchers[1].ports[1]");
        assert_eq!(
            error.message,
            "ports must be in strictly ascending canonical order"
        );

        let duplicate = ResourceSet {
            id: ResourceSetId::new("duplicate-ports").unwrap(),
            selector: ResourceSelector::Endpoint {
                matchers: vec![EndpointMatcher::Host {
                    pattern: "example.com".to_owned(),
                    ports: vec![443, 443],
                }],
            },
        };
        let error = duplicate.validate().unwrap_err();
        assert_eq!(error.path, "matchers[0].ports[1]");
        assert_eq!(error.message, "duplicate port");
    }

    #[test]
    fn cidr_rejects_equivalent_noncanonical_spellings() {
        for cidr in ["10.0.0.0/8", "2001:db8::/32"] {
            let canonical = ResourceSet {
                id: ResourceSetId::new("canonical-network").unwrap(),
                selector: ResourceSelector::Endpoint {
                    matchers: vec![EndpointMatcher::Cidr {
                        cidr: cidr.to_owned(),
                        ports: vec![443],
                    }],
                },
            };
            assert!(
                canonical.validate().is_ok(),
                "rejected canonical CIDR {cidr}"
            );
        }

        for cidr in [
            "2001:0DB8::/32",
            "2001:0db8:0000::/32",
            "2001:db8::/032",
            "10.0.0.0/08",
        ] {
            let noncanonical = ResourceSet {
                id: ResourceSetId::new("noncanonical-network").unwrap(),
                selector: ResourceSelector::Endpoint {
                    matchers: vec![EndpointMatcher::Cidr {
                        cidr: cidr.to_owned(),
                        ports: vec![443],
                    }],
                },
            };
            assert!(
                noncanonical.validate().is_err(),
                "accepted noncanonical CIDR {cidr}"
            );
        }
    }
}
