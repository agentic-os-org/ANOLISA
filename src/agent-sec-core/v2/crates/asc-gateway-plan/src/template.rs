//! Parse, validate and compile a preset template from its JSON source into a
//! [`Plan`].
//!
//! Embedded presets use one path from schema parsing through semantic validation,
//! canonicalization and plan generation.

use std::collections::BTreeSet;

use asc_credential_store::{Carrier, CredentialId, CredentialSpec, FakeFormat};
use serde::Deserialize;

use crate::PlanError;
use crate::plan::{CredentialDefault, DefaultEffect, Method, ResponseRewrite};

/// Hard upper bounds applied to every template source.
const MAX_TEMPLATE_BYTES: usize = 32_768;
const MAX_TEMPLATE_RULES: usize = 64;
const MAX_TEMPLATE_METHODS: usize = 6;
const MAX_TEMPLATE_PREFIXES: usize = 16;
const MAX_NAME_BYTES: usize = 63;
const MAX_BECAUSE_BYTES: usize = 256;
const MAX_HOST_BYTES: usize = 253;
const MAX_PATH_BYTES: usize = 512;
const MAX_PREFIX_BYTES: usize = 64;

/// A parsed and validated preset template, ready to compile into a [`Plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PresetTemplate {
    pub(crate) name: String,
    pub(crate) version: u32,
    pub(crate) credential_spec: CredentialSpec,
    pub(crate) credential_request_default: CredentialDefault,
    pub(crate) request_default: DefaultEffect,
    pub(crate) rules: Vec<TemplateRule>,
}

/// One validated rule inside a template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TemplateRule {
    pub(crate) host: String,
    pub(crate) path: String,
    pub(crate) methods: Vec<Method>,
    pub(crate) response_rewrite: ResponseRewrite,
    pub(crate) because: String,
}

/// Wire format for a preset template JSON document; strict `deny_unknown_fields`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawTemplate {
    name: String,
    version: u32,
    credential: RawCredential,
    credential_request_default: CredentialDefault,
    request_default: DefaultEffect,
    rules: Vec<RawRule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawCredential {
    id: CredentialId,
    carriers: Vec<Carrier>,
    fake_prefixes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawRule {
    host: String,
    path: String,
    method: Vec<Method>,
    response_rewrite: ResponseRewrite,
    because: String,
}

/// Parses and validates a preset template from JSON source.
///
/// `source_name` identifies the origin for error messages (e.g. `"builtin:github"`).
///
/// # Errors
/// Returns [`PlanError`] for malformed JSON, unknown fields, violated bounds,
/// duplicate/conflicting rules, or credential inconsistencies.
pub(crate) fn parse_template(source: &str, source_name: &str) -> Result<PresetTemplate, PlanError> {
    if source.len() > MAX_TEMPLATE_BYTES {
        return Err(PlanError::Invalid {
            field: "template",
            reason: "exceeds maximum size",
        });
    }

    let raw: RawTemplate = serde_json::from_str(source)
        .map_err(|error| PlanError::Decode(format!("{source_name}: {error}")))?;

    validate_header(&raw)?;
    let credential_spec = validate_credential(&raw.credential)?;
    let rules = validate_rules(&raw.rules)?;

    Ok(PresetTemplate {
        name: raw.name,
        version: raw.version,
        credential_spec,
        credential_request_default: raw.credential_request_default,
        request_default: raw.request_default,
        rules,
    })
}

fn invalid(field: &'static str, reason: &'static str) -> Result<(), PlanError> {
    Err(PlanError::Invalid { field, reason })
}

fn validate_header(raw: &RawTemplate) -> Result<(), PlanError> {
    if raw.name.is_empty() || raw.name.len() > MAX_NAME_BYTES {
        return invalid("name", "must be 1..=63 bytes");
    }
    if !raw
        .name
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || raw.name.starts_with('-')
        || raw.name.ends_with('-')
    {
        return invalid("name", "must be a lowercase DNS label");
    }
    if raw.version == 0 {
        return invalid("version", "must be >= 1");
    }
    Ok(())
}

fn validate_credential(cred: &RawCredential) -> Result<CredentialSpec, PlanError> {
    if cred.carriers.is_empty() {
        return Err(PlanError::Invalid {
            field: "credential.carriers",
            reason: "must be non-empty",
        });
    }
    {
        let mut sorted = cred.carriers.clone();
        sorted.sort();
        sorted.dedup();
        if sorted.len() != cred.carriers.len() {
            return Err(PlanError::Invalid {
                field: "credential.carriers",
                reason: "must be unique",
            });
        }
    }
    if cred.fake_prefixes.is_empty() || cred.fake_prefixes.len() > MAX_TEMPLATE_PREFIXES {
        return Err(PlanError::Invalid {
            field: "credential.fakePrefixes",
            reason: "must be 1..=16 entries",
        });
    }
    for prefix in &cred.fake_prefixes {
        if prefix.is_empty() || prefix.len() > MAX_PREFIX_BYTES {
            return Err(PlanError::Invalid {
                field: "credential.fakePrefixes",
                reason: "each prefix must be 1..=64 bytes",
            });
        }
    }
    {
        let unique: BTreeSet<&str> = cred.fake_prefixes.iter().map(String::as_str).collect();
        if unique.len() != cred.fake_prefixes.len() {
            return Err(PlanError::Invalid {
                field: "credential.fakePrefixes",
                reason: "must be unique",
            });
        }
    }

    Ok(CredentialSpec {
        id: cred.id.clone(),
        carriers: {
            let mut carriers = cred.carriers.clone();
            carriers.sort();
            carriers
        },
        format: FakeFormat::with_prefixes(cred.fake_prefixes.iter().map(String::as_str)),
    })
}

fn validate_rules(raw_rules: &[RawRule]) -> Result<Vec<TemplateRule>, PlanError> {
    if raw_rules.is_empty() || raw_rules.len() > MAX_TEMPLATE_RULES {
        return Err(PlanError::Invalid {
            field: "rules",
            reason: "must be 1..=64 entries",
        });
    }

    let mut validated = Vec::with_capacity(raw_rules.len());
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();

    for (index, rule) in raw_rules.iter().enumerate() {
        validate_rule(index, rule, &mut seen)?;

        let mut methods = rule.method.clone();
        methods.sort();

        validated.push(TemplateRule {
            host: rule.host.clone(),
            path: rule.path.clone(),
            methods,
            response_rewrite: rule.response_rewrite,
            because: rule.because.clone(),
        });
    }
    Ok(validated)
}

fn validate_rule(
    index: usize,
    rule: &RawRule,
    seen: &mut BTreeSet<(String, String)>,
) -> Result<(), PlanError> {
    let rule_invalid = |field: &'static str, reason: &'static str| {
        Err(PlanError::InvalidRule {
            index,
            field,
            reason,
        })
    };

    if rule.host.is_empty() || rule.host.len() > MAX_HOST_BYTES {
        return rule_invalid("host", "must be 1..=253 bytes");
    }
    if !is_exact_host(&rule.host) {
        return rule_invalid("host", "must be an exact lowercase DNS name");
    }
    if rule.path.is_empty() || rule.path.len() > MAX_PATH_BYTES {
        return rule_invalid("path", "must be 1..=512 bytes");
    }
    if !is_path_pattern(&rule.path) {
        return rule_invalid("path", "must be an absolute path pattern");
    }
    if rule.method.is_empty() || rule.method.len() > MAX_TEMPLATE_METHODS {
        return rule_invalid("method", "must be 1..=6 entries");
    }
    {
        let mut sorted = rule.method.clone();
        sorted.sort();
        sorted.dedup();
        if sorted.len() != rule.method.len() {
            return rule_invalid("method", "must be unique");
        }
    }
    if rule.because.is_empty()
        || rule.because.len() > MAX_BECAUSE_BYTES
        || rule.because.chars().any(char::is_control)
    {
        return rule_invalid(
            "because",
            "must be 1..=256 bytes without control characters",
        );
    }
    let key = (rule.host.clone(), rule.path.clone());
    if !seen.insert(key) {
        return rule_invalid("host+path", "duplicate rule for same host and path");
    }
    Ok(())
}

/// Validates a DNS hostname (same rules as `plan.rs`).
fn is_exact_host(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

/// Validates a path pattern (same rules as `plan.rs`).
fn is_path_pattern(path: &str) -> bool {
    let Some(rest) = path.strip_prefix('/') else {
        return false;
    };
    rest.split('/').all(|segment| {
        matches!(segment, "*" | "**")
            || (!segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
                }))
    })
}
