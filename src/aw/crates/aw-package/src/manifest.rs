//! Exact component provenance and payload integrity checks.

use crate::{
    filesystem::{absolute, relative},
    require, Result,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::Path,
};

/// One package's component set.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Manifest schema version.
    pub format: u8,
    /// Components shipped together, with disjoint owned payloads.
    pub components: Vec<Component>,
}

/// Immutable component receipt, shared between split and combined packages.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Component {
    /// Receipt schema version.
    pub format: u8,
    /// Component identity.
    pub component: String,
    /// Preview version.
    pub version: String,
    /// Full source commit SHA.
    pub source_commit: String,
    /// Required operating system.
    pub os: String,
    /// Native CPU architecture.
    pub arch: String,
    /// Provider wire protocol.
    pub provider_protocol: String,
    /// Exact component version dependencies from the same build.
    pub requires: BTreeMap<String, String>,
    /// Relative owned paths and their integrity metadata.
    pub files: BTreeMap<String, Payload>,
}

/// Integrity metadata for an owned regular file.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Payload {
    /// Lowercase SHA-256 digest.
    pub sha256: String,
    /// Unix permission bits, restricted to executable or read-only data modes.
    pub mode: u32,
}

/// Stream a file's SHA-256 without loading the payload into memory.
pub fn digest(path: &Path) -> Result<String> {
    let mut source = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Validate every component and payload before modifying an installation.
pub fn inspect(bundle: &Path) -> Result<Manifest> {
    let manifest: Manifest = serde_json::from_reader(File::open(bundle.join("manifest.json"))?)?;
    require(
        manifest.format == 1 && !manifest.components.is_empty(),
        "unsupported package manifest",
    )?;
    let mut identities = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for component in &manifest.components {
        require(
            matches!(
                component.component.as_str(),
                "aw-core" | "aw-provider-sec-core"
            ) && identities.insert(&component.component),
            "unknown or duplicate component",
        )?;
        require(
            component.format == 1
                && component.os == "linux"
                && std::env::consts::OS == "linux"
                && component.arch == std::env::consts::ARCH
                && component.provider_protocol == "aw-provider/v1alpha1",
            "incompatible platform or Provider protocol",
        )?;
        let required = if component.component == "aw-core" {
            vec!["bin/aw", "bin/aw-package"]
        } else {
            require(
                component.requires.get("aw-core") == Some(&component.version),
                "aw-provider-sec-core requires matching aw-core version in its manifest",
            )?;
            vec![
                "libexec/aw/providers/sec-core/aw-provider-sec-core",
                "libexec/aw/providers/sec-core/agent-sec-cli",
                "libexec/aw/providers/sec-core/agent-sec-daemon",
            ]
        };
        require(
            required
                .iter()
                .all(|name| component.files.contains_key(*name)),
            "incomplete component payload",
        )?;
        for (name, metadata) in &component.files {
            relative(name)?;
            let path = bundle.join("payload").join(name);
            absolute(&path)?;
            require(
                paths.insert(name) && path.metadata()?.is_file(),
                "duplicate or nonregular payload",
            )?;
            require(
                matches!(metadata.mode, 0o644 | 0o755)
                    && (!required.contains(&name.as_str()) || metadata.mode == 0o755)
                    && digest(&path)? == metadata.sha256,
                format!("payload checksum or mode mismatch: {name}"),
            )?;
        }
    }
    Ok(manifest)
}
