//! Normalize existing Preview records without migrating or checking payload bytes.

use super::{
    paths, Installation, InstalledDependency, InstalledPackage, Manager, Registration, Scope,
    SourceEvidence, MAX_PACKAGES,
};
use crate::{description, require, Component, Result};
use std::{collections::BTreeSet, fs::File, os::unix::fs::MetadataExt, path::Path};

pub(super) fn installation(prefix: &Path) -> Result<Option<(Installation, File)>> {
    let directory = prefix.join(".aw-packages");
    let _directory = match paths::directory(&directory) {
        Err(error) if paths::missing(&error) => return Ok(None),
        result => result?,
    };
    let lock = paths::lock(&prefix.join(".aw-install.lock"))?;
    let mut entries = paths::entries(&directory)?;
    entries.sort();
    let mut packages = Vec::new();
    let mut owned = BTreeSet::new();
    let installation_id = format!("legacy:{}", prefix.display());
    for path in entries {
        require(
            packages.len() < MAX_PACKAGES,
            "too many legacy package records",
        )?;
        require(
            path.extension()
                .is_some_and(|extension| extension == "json"),
            format!(
                "unknown entry in legacy package records: {}",
                path.display()
            ),
        )?;
        let value: Component =
            super::decode(&paths::bytes(&path, description::MAX_DESCRIPTION_BYTES)?)?;
        require(
            path.file_stem()
                .is_some_and(|stem| stem == value.component.as_str()),
            "legacy record name does not match package ID",
        )?;
        validate(&value)?;
        for name in value.files.keys() {
            require(
                owned.insert(name.clone()),
                "overlapping legacy file ownership",
            )?;
        }
        let provider = if value.component == "aw-core" {
            None
        } else {
            Some(description::ProviderDeclaration {
                protocol: value.provider_protocol.clone(),
            })
        };
        packages.push(InstalledPackage {
            package_id: value.component,
            version: value.version,
            os: value.os,
            arch: value.arch,
            files: value.files,
            dependencies: value
                .requires
                .into_iter()
                .map(|(package_id, version)| InstalledDependency {
                    installation_id: installation_id.clone(),
                    package_id,
                    version,
                })
                .collect(),
            source: SourceEvidence {
                bundle_sha256: None,
                source_commit: Some(value.source_commit),
            },
            provider,
        });
    }
    if packages.is_empty() {
        return Ok(None);
    }
    for name in &owned {
        for ancestor in Path::new(name).ancestors().skip(1) {
            require(
                !ancestor.to_str().is_some_and(|name| owned.contains(name)),
                "legacy payload is another file's parent",
            )?;
        }
    }
    Ok(Some((
        Installation {
            installation_id,
            prefix: prefix.to_path_buf(),
            owner_uid: paths::directory(prefix)?.metadata()?.uid(),
            scope: Scope::Unknown,
            manager: Manager::Raw,
            registration: Registration::LegacyUnregistered,
            packages,
        },
        lock,
    )))
}

pub(crate) fn validate(value: &Component) -> Result<()> {
    require(
        value.format == 1,
        "unsupported legacy package record format",
    )?;
    // Installation accepted versions independently of the builder's Preview naming rule.
    description::package_fields(
        &value.component,
        "0.0.0",
        &value.os,
        &value.arch,
        &value.files,
        None,
    )?;
    require(
        !value.version.is_empty()
            && value.version.len() <= 128
            && value.source_commit.len() <= 1024,
        "invalid legacy package version or source evidence",
    )?;
    require(
        value.provider_protocol == "aw-provider/v1alpha1",
        "unsupported legacy Provider protocol",
    )?;
    let required: &[&str] = match value.component.as_str() {
        "aw-core" => &["bin/aw"],
        "aw-provider-sec-core" => &[
            "libexec/aw/providers/sec-core/aw-provider-sec-core",
            "libexec/aw/providers/sec-core/agent-sec-cli",
            "libexec/aw/providers/sec-core/agent-sec-daemon",
            "libexec/aw/providers/sec-core/agent-sec-daemon-health",
        ],
        "aw-provider-tokenless" => &[
            "libexec/aw/providers/tokenless/aw-provider-tokenless",
            "libexec/aw/providers/tokenless/tokenless",
            "libexec/aw/providers/tokenless/rtk",
        ],
        _ => return Err(crate::Error::Invalid("unknown legacy package ID".into())),
    };
    require(
        required.iter().all(|path| {
            value
                .files
                .get(*path)
                .is_some_and(|file| file.mode == 0o755)
        }),
        "incomplete legacy package file list",
    )?;
    if value.component == "aw-core" {
        if let Some(file) = value.files.get("bin/aw-package") {
            require(file.mode == 0o755, "invalid legacy package CLI file mode")?;
        }
    }
    for (name, version) in &value.requires {
        require(
            matches!(
                name.as_str(),
                "aw-core" | "aw-provider-sec-core" | "aw-provider-tokenless"
            ) && !version.is_empty()
                && version.len() <= 128,
            "invalid legacy package dependency",
        )?;
    }
    if value.component != "aw-core" {
        require(
            value.requires.get("aw-core") == Some(&value.version),
            "legacy Provider requires its matching core",
        )?;
    }
    Ok(())
}
