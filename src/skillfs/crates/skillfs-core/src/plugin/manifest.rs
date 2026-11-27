//! Validate the closed Agent Plugins 1.0 manifest schema.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use super::containment::{Kind, OpenError, PackageRoot, ReadError};
use super::{
    DiagnosticSeverity, PluginComponent, PluginDiagnostic, json_as, json_object, read_json,
};

const SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";
const FIELDS: &[&str] = &[
    "$schema",
    "name",
    "version",
    "description",
    "author",
    "homepage",
    "repository",
    "license",
    "keywords",
    "extensions",
];

/// Portable metadata from a validated `plugin.json`.
#[derive(Debug, Deserialize, Serialize)]
pub struct PluginManifest {
    /// Canonical manifest schema identifier.
    #[serde(rename = "$schema")]
    pub schema: String,
    /// Validated portable name.
    pub name: String,
    /// Version, when supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Description, when supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional author name, email, and URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<BTreeMap<String, String>>,
    /// Homepage, when supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// Repository, when supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// License, when supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Keywords, when supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keywords: Option<Vec<String>>,
    /// Client-owned namespaces and their opaque values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<BTreeMap<String, Box<RawValue>>>,
}

pub(super) fn load(
    root: &PackageRoot,
    diagnostics: &mut Vec<PluginDiagnostic>,
) -> Option<(PluginManifest, Arc<[u8]>)> {
    use DiagnosticSeverity::{RejectPlugin, Warn};
    let mut issue = |severity, keys: &[&str], spec_ref, message| {
        diagnostics.push(PluginDiagnostic::new(
            severity,
            PluginComponent::Manifest,
            "plugin.json",
            keys,
            spec_ref,
            message,
        ));
    };

    let entry = match root.open_entry(Path::new("plugin.json"), Kind::RegularFile) {
        Ok(entry) => entry,
        Err(err) => {
            let (spec_ref, message) = match err {
                OpenError::Containment(_) => ("§4.1", "manifest fails package containment"),
                OpenError::Absent | OpenError::WrongKind => {
                    ("§5.1", "root plugin.json must be a regular file")
                }
                OpenError::Unreadable(_) => ("§5.2", "cannot read valid manifest JSON"),
            };
            issue(RejectPlugin, &[], spec_ref, message);
            return None;
        }
    };
    let (bytes, _) = match entry.read_regular(root) {
        Ok(read) => read,
        Err(error) => {
            let message = match error {
                ReadError::TooLarge => "manifest exceeds 1 MiB limit",
                ReadError::Io(_) => "cannot read valid manifest JSON",
            };
            issue(RejectPlugin, &[], "§5.2", message);
            return None;
        }
    };
    let Some(value) = read_json(&bytes) else {
        issue(RejectPlugin, &[], "§5.2", "cannot read valid manifest JSON");
        return None;
    };
    let Some(object) = json_object(value) else {
        issue(RejectPlugin, &[], "§5.2", "manifest must be an object");
        return None;
    };

    for field in object.keys() {
        if !FIELDS.contains(&field.as_str()) {
            issue(Warn, &[field], "§5.2", "unknown manifest field ignored");
        }
    }
    // §8.1 leaves extension values opaque. Preserve raw members and their
    // names for root-entry filtering without decoding extension data.
    let extensions = object.get("extensions").and_then(|value| {
        let parsed = json_as::<BTreeMap<String, Box<RawValue>>>(value);
        if parsed.is_none() {
            issue(
                Warn,
                &["extensions"],
                "§8.1",
                "non-object extensions ignored",
            );
        }
        parsed
    });

    let schema = object
        .get("$schema")
        .and_then(|value| json_as::<String>(value));
    if schema.as_deref() != Some(SCHEMA) {
        issue(
            RejectPlugin,
            &["$schema"],
            "§5.2",
            "missing or unsupported manifest schema",
        );
    }
    let name = match object.get("name") {
        None => {
            issue(
                RejectPlugin,
                &["name"],
                "§5.3",
                "missing required plugin name",
            );
            None
        }
        Some(value) => {
            let name = json_as::<String>(value).filter(|value| valid_name(value));
            if name.is_none() {
                issue(RejectPlugin, &["name"], "§5.5", "invalid plugin name");
            }
            name
        }
    };

    let mut text_field = |field| {
        let value = object.get(field)?;
        let parsed = json_as::<String>(value);
        if parsed.is_none() {
            issue(RejectPlugin, &[field], "§5.4", "expected string metadata");
        }
        parsed
    };
    let version = text_field("version");
    let description = text_field("description");
    let homepage = text_field("homepage");
    let repository = text_field("repository");
    let license = text_field("license");

    let keywords = object.get("keywords").and_then(|value| {
        let parsed = json_as::<Vec<String>>(value);
        if parsed.is_none() {
            issue(
                RejectPlugin,
                &["keywords"],
                "§5.4",
                "keywords must be a list of strings",
            );
        }
        parsed
    });
    let author = object.get("author").and_then(|value| {
        let Some(fields) = json_object(value) else {
            issue(
                RejectPlugin,
                &["author"],
                "§5.4",
                "author must be an object",
            );
            return None;
        };
        let mut author = BTreeMap::new();
        for (key, value) in fields {
            match json_as::<String>(value) {
                Some(value) if matches!(key.as_str(), "name" | "email" | "url") => {
                    author.insert(key, value);
                }
                _ => issue(
                    RejectPlugin,
                    &["author", &key],
                    "§5.4",
                    "invalid author field",
                ),
            }
        }
        Some(author)
    });
    if diagnostics.iter().any(|item| item.severity == RejectPlugin) {
        return None;
    }

    let manifest = PluginManifest {
        schema: schema?,
        name: name?,
        version,
        description,
        author,
        homepage,
        repository,
        license,
        keywords,
        extensions,
    };
    Some((manifest, bytes.into()))
}

fn valid_name(name: &str) -> bool {
    let alphanumeric = |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    let bytes = name.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes.first().is_some_and(alphanumeric)
        && bytes.last().is_some_and(alphanumeric)
        && bytes
            .iter()
            .all(|byte| alphanumeric(byte) || matches!(byte, b'.' | b'-'))
        && !name.contains("--")
        && !name.contains("..")
}
