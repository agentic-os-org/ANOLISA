//! Render explicitly supplied data without coupling AW to distributor presets.

use super::{Policy, Settings};
use crate::{require, Error, Result};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeMap, os::unix::ffi::OsStrExt, path::Path};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Template {
    format: u32,
    #[serde(default)]
    packages: Vec<String>,
    #[serde(default)]
    constraints: Vec<Constraint>,
    #[serde(default)]
    file_hashes: BTreeMap<String, FileHash>,
    providers: Value,
    events: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileHash {
    package: String,
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Constraint {
    kind: String,
    value: String,
}

fn parse(definition: &Value) -> Result<Template> {
    require(
        serde_json::to_vec(definition)?.len() <= aw_config::MAX_DOCUMENT_BYTES,
        "template exceeds the configuration size limit",
    )?;
    let template: Template = serde_json::from_value(definition.clone())?;
    require(
        template.format == 1,
        "unsupported configuration template format",
    )?;
    require(
        template.providers.is_object() && template.events.is_object(),
        "template providers and events must be objects",
    )?;
    let mut unique = std::collections::BTreeSet::new();
    for package in &template.packages {
        require(
            !package.is_empty()
                && package.len() <= 128
                && package
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
                && unique.insert(package),
            "template package requirements must be distinct package IDs",
        )?;
    }
    for (name, source) in &template.file_hashes {
        require(
            !matches!(name.as_str(), "PREFIX" | "STATE")
                && !name.is_empty()
                && name
                    .bytes()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_'),
            "template hash variable must use a nonreserved name",
        )?;
        require(
            template.packages.contains(&source.package)
                && !source.path.is_empty()
                && Path::new(&source.path)
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_))),
            "template file hashes must name a relative file in a required package",
        )?;
    }
    Ok(template)
}

pub(super) fn required_packages(policy: &Policy<'_>) -> Result<Vec<String>> {
    match policy {
        Policy::Template { definition, .. } => Ok(parse(definition)?.packages),
        _ => Ok(Vec::new()),
    }
}

pub(super) fn verify_files(
    policy: &Policy<'_>,
    packages: &BTreeMap<String, crate::database::InstalledPackage>,
) -> Result<()> {
    if let Policy::Template { definition, .. } = policy {
        for source in parse(definition)?.file_hashes.values() {
            require(
                packages
                    .get(&source.package)
                    .is_some_and(|package| package.files.contains_key(&source.path)),
                format!(
                    "required template file is absent from package {}: {}",
                    source.package, source.path
                ),
            )?;
        }
    }
    Ok(())
}

fn substitute(text: &str, variables: &BTreeMap<String, String>) -> Result<String> {
    let mut result = String::new();
    let mut remaining = text;
    while let Some((literal, tail)) = remaining.split_once("${") {
        result.push_str(literal);
        let (name, tail) = tail
            .split_once('}')
            .ok_or_else(|| Error::Invalid("unterminated template placeholder".into()))?;
        result.push_str(
            variables
                .get(name)
                .ok_or_else(|| Error::Invalid(format!("missing template variable: {name}")))?,
        );
        remaining = tail;
    }
    result.push_str(remaining);
    require(
        result.len() <= aw_config::MAX_DOCUMENT_BYTES,
        "rendered template value is too large",
    )?;
    Ok(result)
}

fn render_value(value: &mut Value, variables: &BTreeMap<String, String>) -> Result<()> {
    match value {
        Value::String(text) => *text = substitute(text, variables)?,
        Value::Array(values) => {
            for value in values {
                render_value(value, variables)?;
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                render_value(value, variables)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn render(
    settings: &Settings<'_>,
    packages: Option<&BTreeMap<String, crate::database::InstalledPackage>>,
) -> Result<Value> {
    let Policy::Template {
        definition,
        variables,
    } = &settings.policy
    else {
        return Err(Error::Invalid(
            "an explicit configuration template is required".into(),
        ));
    };
    let template = parse(definition)?;
    for (name, value) in variables {
        require(
            !matches!(name.as_str(), "PREFIX" | "STATE")
                && !name.is_empty()
                && name.len() <= 128
                && name
                    .bytes()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
                && !value.contains('\0'),
            format!("invalid or reserved template variable: {name}"),
        )?;
    }
    let mut variables = variables.clone();
    variables.insert("PREFIX".into(), settings.prefix.display().to_string());
    variables.insert("STATE".into(), settings.state.display().to_string());
    if let Some(packages) = packages {
        for (name, source) in template.file_hashes {
            let payload = packages
                .get(&source.package)
                .and_then(|package| package.files.get(&source.path))
                .ok_or_else(|| {
                    Error::Invalid(format!(
                        "missing template hash source: {}:{}",
                        source.package, source.path
                    ))
                })?;
            variables.insert(name, payload.sha256.clone());
        }
    }
    for constraint in template.constraints {
        let rendered = substitute(&constraint.value, &variables)?;
        let path = Path::new(&rendered);
        match constraint.kind.as_str() {
            "unix_socket" => require(path.is_absolute() && path.as_os_str().as_bytes().len() < 108,
                format!("template unix_socket path must be absolute and shorter than 108 bytes: {rendered}"))?,
            "search_path" => require(path.is_absolute() && std::env::join_paths([path]).is_ok(),
                format!("template search_path cannot be represented in PATH: {rendered}"))?,
            _ => return Err(Error::Invalid(format!("unknown template constraint: {}", constraint.kind))),
        }
    }
    let mut value = serde_json::json!({"providers":template.providers,"events":template.events});
    render_value(&mut value, &variables)?;
    Ok(value)
}
