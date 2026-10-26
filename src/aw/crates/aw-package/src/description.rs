//! Independent local package descriptions; reading them never installs or executes payloads.

use crate::{database, require, Payload, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// Largest accepted package description, independent of payload size.
pub const MAX_DESCRIPTION_BYTES: u64 = 1024 * 1024;

/// Optional Provider metadata, absent for ordinary tools and libraries.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderDeclaration {
    /// Wire contract implemented by the package; this is not a launch configuration.
    pub protocol: String,
}

/// Exact local requirement declared by a producer before installation.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackageRequirement {
    /// Name of the package required in the selected installation.
    pub package_id: String,
    /// Exact version; repository resolution is outside local package management.
    pub version: String,
}

/// Producer-supplied description, separate from committed installation records.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackageDescription {
    /// Description format, currently one.
    pub schema_version: u32,
    /// Independent package name, unrestricted to AW's bundled component names.
    pub package_id: String,
    /// Independent SemVer version; no Preview suffix or shared build is required.
    pub version: String,
    /// Required operating system.
    pub os: String,
    /// Required CPU architecture.
    pub arch: String,
    /// Owned paths relative to the installation prefix.
    pub files: BTreeMap<String, Payload>,
    /// Exact requirements checked against local package inputs or installed packages.
    pub dependencies: Vec<PackageRequirement>,
    /// Optional source commit supplied by the producer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_commit: Option<String>,
    /// Optional wire metadata; ordinary tools do not declare a Provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderDeclaration>,
}

impl PackageDescription {
    /// Read and validate one bounded regular JSON file without inspecting payload files.
    ///
    /// # Errors
    /// Rejects symbolic links, nonregular files, unknown fields and invalid package metadata.
    pub fn read(path: &Path) -> Result<Self> {
        let value: Self =
            database::decode(&database::read_description(path, MAX_DESCRIPTION_BYTES)?)?;
        value.validate()?;
        Ok(value)
    }

    /// Validate producer metadata without requiring AW build provenance or Provider support.
    ///
    /// # Errors
    /// Rejects unsupported formats, ambiguous ownership and invalid exact requirements.
    pub fn validate(&self) -> Result<()> {
        require(
            self.schema_version == 1,
            "unsupported package description schema_version",
        )?;
        package_fields(
            &self.package_id,
            &self.version,
            &self.os,
            &self.arch,
            &self.files,
            self.provider.as_ref(),
        )?;
        if let Some(commit) = &self.source_commit {
            commit_sha(commit)?;
        }
        let mut dependencies = BTreeSet::new();
        for dependency in &self.dependencies {
            identifier(&dependency.package_id)?;
            version(&dependency.version)?;
            require(
                dependency.package_id != self.package_id
                    && dependencies.insert(&dependency.package_id),
                "duplicate or self package requirement",
            )?;
        }
        require(
            self.dependencies.len() <= database::MAX_PACKAGES,
            "too many package requirements",
        )
    }
}

pub(crate) fn identifier(value: &str) -> Result<()> {
    require(
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || b"._-".contains(&ch))
            && value.as_bytes()[0].is_ascii_alphanumeric(),
        format!("invalid package identifier: {value}"),
    )
}

pub(crate) fn version(value: &str) -> Result<()> {
    let valid_numeric = |value: &str| {
        !value.is_empty()
            && value.bytes().all(|ch| ch.is_ascii_digit())
            && (value.len() == 1 || !value.starts_with('0'))
    };
    let (version, build) = value
        .split_once('+')
        .map_or((value, None), |(version, build)| (version, Some(build)));
    let (release, prerelease) = version
        .split_once('-')
        .map_or((version, None), |(version, pre)| (version, Some(pre)));
    let suffix = |suffix: &str, numeric: bool| {
        suffix.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == b'-')
                && (!numeric || !part.bytes().all(|ch| ch.is_ascii_digit()) || valid_numeric(part))
        })
    };
    require(
        value.len() <= 128
            && release.split('.').count() == 3
            && release.split('.').all(valid_numeric)
            && prerelease.is_none_or(|value| suffix(value, true))
            && build.is_none_or(|value| suffix(value, false)),
        format!("invalid exact package version: {value}"),
    )
}

pub(crate) fn sha256(value: &str) -> Result<()> {
    require(
        value.len() == 64
            && value
                .bytes()
                .all(|ch| ch.is_ascii_digit() || (b'a'..=b'f').contains(&ch)),
        "expected lowercase SHA-256",
    )
}

pub(crate) fn commit_sha(value: &str) -> Result<()> {
    require(
        value.len() == 40
            && value
                .bytes()
                .all(|ch| ch.is_ascii_digit() || (b'a'..=b'f').contains(&ch)),
        "expected full lowercase source commit",
    )
}

pub(crate) fn package_fields(
    id: &str,
    value: &str,
    os: &str,
    arch: &str,
    files: &BTreeMap<String, Payload>,
    provider: Option<&ProviderDeclaration>,
) -> Result<()> {
    identifier(id)?;
    version(value)?;
    require(os == "linux", "package operating system must be linux")?;
    identifier(arch)?;
    require(
        !files.is_empty() && files.len() <= 16384,
        "invalid package file count",
    )?;
    let mut paths = BTreeSet::new();
    for (name, payload) in files {
        crate::filesystem::relative(name)?;
        require(
            !name.contains('\\') && !name.contains('\0'),
            format!("invalid package file path: {name}"),
        )?;
        sha256(&payload.sha256)?;
        require(
            matches!(payload.mode, 0o644 | 0o755),
            format!("unsupported package file mode: {name}"),
        )?;
        let path = Path::new(name);
        for ancestor in path
            .ancestors()
            .skip(1)
            .filter(|path| !path.as_os_str().is_empty())
        {
            require(
                !files.contains_key(ancestor.to_str().unwrap_or("")),
                format!("package file is another payload's parent: {name}"),
            )?;
        }
        require(paths.insert(name), "duplicate package file path")?;
    }
    if let Some(provider) = provider {
        require(
            provider.protocol == "aw-provider/v1alpha1",
            "unsupported package Provider protocol",
        )?;
    }
    Ok(())
}
