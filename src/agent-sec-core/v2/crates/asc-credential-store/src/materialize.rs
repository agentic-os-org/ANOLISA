use std::path::PathBuf;

use serde::Serialize;

use crate::filesystem::{DirectoryAccess, PinnedDirectory};
use crate::store::Carrier;
use crate::{AgentName, CredentialId, CredentialStore, CredentialStoreError};

/// Schema version of the gateway secrets snapshot.
pub const SECRETS_SNAPSHOT_VERSION: u32 = 1;

const SNAPSHOT_FILE_MODE: rustix::fs::RawMode = 0o640;

/// Destination of a gateway secrets snapshot, normally on tmpfs under `/run`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretsTarget {
    /// Existing directory owned by the effective user without group/other write access.
    pub directory: PathBuf,
    /// Plain file name inside `directory`.
    pub file_name: String,
    /// Group granted read access (mode 0640), normally the gateway's group.
    pub group: Option<u32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot<'a> {
    version: u32,
    generation: u64,
    entries: Vec<Entry<'a>>,
    retired: Vec<Tombstone<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Entry<'a> {
    fake: &'a str,
    credential: &'a CredentialId,
    agent: &'a AgentName,
    real: &'a str,
    carriers: &'a [Carrier],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Tombstone<'a> {
    fake: &'a str,
    credential: &'a CredentialId,
    agent: &'a AgentName,
}

impl CredentialStore {
    /// Writes the fake→real mappings of `credentials` plus every tombstone to `target`.
    ///
    /// Credentials outside `credentials` never leave the store directory.
    /// Tombstones are always included so removed fakes stay rejected instead
    /// of degrading to "no credential". The file is replaced atomically.
    ///
    /// # Errors
    /// Returns unknown/retired credential, invalid target, path or I/O errors.
    pub fn materialize(
        &self,
        target: &SecretsTarget,
        credentials: &[CredentialId],
        generation: u64,
    ) -> Result<(), CredentialStoreError> {
        let valid_name = !target.file_name.is_empty()
            && !target.file_name.starts_with('.')
            && target
                .file_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
        if !valid_name {
            return Err(CredentialStoreError::Invalid {
                field: "snapshot file name",
                reason: "must be a plain file name",
            });
        }
        let mut seen = std::collections::HashSet::new();
        if !credentials.iter().all(|id| seen.insert(id)) {
            return Err(CredentialStoreError::Invalid {
                field: "credentials",
                reason: "must not repeat a credential",
            });
        }
        let mut entries = Vec::new();
        for id in credentials {
            let record = self
                .records
                .get(id)
                .ok_or_else(|| CredentialStoreError::UnknownCredential(id.clone()))?;
            let real = record
                .real
                .as_ref()
                .ok_or_else(|| CredentialStoreError::Retired(id.clone()))?;
            entries.extend(record.fakes.iter().map(|fake| Entry {
                fake: &fake.value,
                credential: id,
                agent: &fake.agent,
                real: real.expose(),
                carriers: &record.carriers,
            }));
        }
        let retired = self
            .records
            .iter()
            .flat_map(|(id, record)| {
                record.retired_fakes.iter().map(move |fake| Tombstone {
                    fake: &fake.value,
                    credential: id,
                    agent: &fake.agent,
                })
            })
            .collect();
        let snapshot = Snapshot {
            version: SECRETS_SNAPSHOT_VERSION,
            generation,
            entries,
            retired,
        };
        let bytes = serde_json::to_vec(&snapshot)
            .map_err(|_| CredentialStoreError::Corrupt("cannot encode secrets snapshot".into()))?;
        let directory =
            PinnedDirectory::open_existing(&target.directory, DirectoryAccess::OwnerWritable)?
                .ok_or_else(|| CredentialStoreError::UnsafePath {
                    path: target.directory.clone(),
                    reason: "snapshot directory does not exist",
                })?;
        if directory.same_directory(&self.directory)? {
            return Err(CredentialStoreError::UnsafePath {
                path: target.directory.clone(),
                reason: "snapshot directory must differ from the store directory",
            });
        }
        directory.remove_temporaries()?;
        directory
            .write_atomic(&target.file_name, &bytes, SNAPSHOT_FILE_MODE, target.group)
            .map_err(|failure| failure.error)
    }
}
