//! Pin explicit execution consumers while keeping native settings isolated.

use crate::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileGrant {
    path: String,
    sha256: String,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Policy {
    pub sites: Vec<String>,
    pub command_wrappers: Vec<FileGrant>,
    pub runtime_preloads: Vec<FileGrant>,
    pub declarative_plugins: bool,
}

impl Policy {
    pub fn read(agent: &serde_json::Value) -> Result<Self> {
        let mut policy: Self = agent
            .get("qoder_sdk")
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()?
            .unwrap_or_default();
        if policy.sites.is_empty() {
            policy.sites.push("global".into());
        }
        for grant in policy
            .command_wrappers
            .iter()
            .chain(&policy.runtime_preloads)
        {
            let path = Path::new(&grant.path);
            if !path.is_absolute() || fs::symlink_metadata(path)?.file_type().is_symlink() {
                return Err("Qoder SDK asset must be an absolute regular file".into());
            }
            let metadata = fs::metadata(path)?;
            if !metadata.is_file() || metadata.len() > 16 * 1024 * 1024 {
                return Err("Qoder SDK asset must be a regular file below 16 MiB".into());
            }
            let actual = format!("{:x}", Sha256::digest(fs::read(path)?));
            if actual != grant.sha256 {
                return Err("Qoder SDK asset checksum differs from its configured grant".into());
            }
        }
        Ok(policy)
    }

    pub fn wrappers(&self) -> Vec<&str> {
        self.command_wrappers
            .iter()
            .map(|grant| grant.path.as_str())
            .collect()
    }
}
