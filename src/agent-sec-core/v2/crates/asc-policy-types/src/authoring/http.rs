//! Bounded HTTPS credential targets for authored policies.

use serde::{Deserialize, Serialize};

use super::Resource;
use crate::ValidationError;

/// Schemes permitted to carry managed credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpScheme {
    /// Verified upstream TLS is required.
    Https,
}

/// HTTP methods expressible by credential policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    /// Retrieve a representation.
    Get,
    /// Retrieve response headers.
    Head,
    /// Submit a request body.
    Post,
    /// Replace a resource.
    Put,
    /// Modify a resource.
    Patch,
    /// Remove a resource.
    Delete,
}

pub(super) fn validate(target: &Resource, prefix: &str) -> Result<(), ValidationError> {
    let Resource::HttpRequest {
        credential_id,
        host,
        path,
        methods,
        ..
    } = target
    else {
        return Err(ValidationError::new(
            format!("{prefix}.target"),
            "credential rules require an http_request target",
        ));
    };
    let invalid =
        |field: &str, reason| ValidationError::new(format!("{prefix}.target.{field}"), reason);
    // Keep this grammar aligned with CredentialId without depending on the store.
    if !(1..=63).contains(&credential_id.len())
        || credential_id.starts_with('-')
        || credential_id.ends_with('-')
        || !credential_id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(invalid("credentialId", "must be a lowercase DNS label"));
    }
    let labels: Vec<&str> = host.split('.').collect();
    if host.len() > 253
        || labels.len() < 2
        || !labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
    {
        return Err(invalid("host", "must be an exact lowercase DNS name"));
    }
    if path.len() > 512
        || !path.strip_prefix('/').is_some_and(|rest| {
            rest.split('/').all(|segment| {
                matches!(segment, "*" | "**")
                    || (!segment.is_empty()
                        && !matches!(segment, "." | "..")
                        && segment.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric()
                                || matches!(byte, b'-' | b'.' | b'_' | b'~')
                        }))
            })
        })
    {
        return Err(invalid("path", "must be an absolute path pattern"));
    }
    let mut unique = methods.clone();
    unique.sort_unstable();
    unique.dedup();
    if methods.is_empty() || methods.len() > 6 || unique.len() != methods.len() {
        return Err(invalid("methods", "requires 1..6 unique methods"));
    }
    Ok(())
}
