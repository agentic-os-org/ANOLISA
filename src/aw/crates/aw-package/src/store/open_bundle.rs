//! Verify local package contents without copying files or resolving network dependencies.

mod payload;

use crate::store::{
    database::{self, InstalledDependency, InstalledPackage, SourceEvidence},
    description::{PackageDescription, MAX_DESCRIPTION_BYTES},
    require, Result,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// Fully inspected input retaining the exact descriptor bytes used for planning.
#[derive(Clone, Debug)]
pub struct Bundle {
    /// Packages delivered by this input; dependency installation IDs are selected by the plan.
    pub packages: Vec<InstalledPackage>,
    /// Actual payload bytes verified while reading the bundle's files.
    pub(crate) payload_bytes: u64,
    pub(crate) sources: BTreeMap<String, PathBuf>,
    pub(crate) records: BTreeMap<PathBuf, Vec<u8>>,
}

/// Read `package.json` and verify every declared payload without executing code.
///
/// # Errors
/// Rejects ambiguous formats, invalid metadata, unsupported platforms, unsafe paths
/// and files whose bytes or mode differ from their description.
pub fn inspect(bundle: &Path) -> Result<Bundle> {
    database::safe_directory(bundle)?;
    let descriptor = bundle.join("package.json");
    require(
        std::fs::symlink_metadata(bundle.join("manifest.json"))
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
        "manifest.json is unsupported; local packages require package.json",
    )?;
    let bytes = database::safe_bytes(&descriptor, MAX_DESCRIPTION_BYTES)?;
    let bundle_digest = format!("{:x}", Sha256::digest(&bytes));
    let package: PackageDescription = database::decode(&bytes)?;
    package.validate()?;
    let packages = vec![InstalledPackage {
        package_id: package.package_id,
        version: package.version,
        os: package.os,
        arch: package.arch,
        files: package.files,
        dependencies: package
            .dependencies
            .into_iter()
            .map(|requirement| InstalledDependency {
                installation_id: String::new(),
                package_id: requirement.package_id,
                version: requirement.version,
            })
            .collect(),
        source: SourceEvidence {
            bundle_sha256: Some(bundle_digest),
            source_commit: package.source_commit,
        },
        provider: package.provider,
    }];
    let mut identities = BTreeSet::new();
    let mut sources = BTreeMap::new();
    let mut budget = payload::MAX_TOTAL_BYTES;
    for package in &packages {
        require(
            identities.insert(&package.package_id),
            "duplicate package ID in bundle",
        )?;
        require(
            package.os == std::env::consts::OS && package.arch == std::env::consts::ARCH,
            format!(
                "package {} does not support this operating system and architecture",
                package.package_id
            ),
        )?;
        for (name, metadata) in &package.files {
            require(
                sources
                    .insert(name.clone(), bundle.join("payload").join(name))
                    .is_none(),
                format!("overlapping bundle file ownership: {name}"),
            )?;
            payload::verify(&bundle.join("payload").join(name), metadata, &mut budget)?;
        }
    }
    for name in sources.keys() {
        for parent in Path::new(name).ancestors().skip(1) {
            require(
                !parent
                    .to_str()
                    .is_some_and(|parent| sources.contains_key(parent)),
                format!("bundle payload is another file's parent: {name}"),
            )?;
        }
    }
    require(
        database::safe_bytes(&descriptor, MAX_DESCRIPTION_BYTES)? == bytes,
        "bundle descriptor changed during verification",
    )?;
    Ok(Bundle {
        packages,
        payload_bytes: payload::MAX_TOTAL_BYTES - budget,
        sources,
        records: BTreeMap::from([(descriptor, bytes)]),
    })
}

/// Maximum verified payload bytes in one raw installation.
pub(crate) const MAX_PAYLOAD_BYTES: u64 = payload::MAX_TOTAL_BYTES;

pub(crate) use payload::verify;
