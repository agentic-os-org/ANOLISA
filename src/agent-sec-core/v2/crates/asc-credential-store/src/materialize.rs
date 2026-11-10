use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::Serialize;

use crate::filesystem::{DirectoryAccess, PinnedDirectory};
use crate::{CredentialId, CredentialStore, CredentialStoreError, ScopeId};

/// Schema version of the explicit-pair gateway secrets snapshot.
pub const SECRETS_SNAPSHOT_VERSION: u32 = 2;

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
    unavailable: Vec<Tombstone<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Entry<'a> {
    fake: &'a str,
    credential: &'a CredentialId,
    scope_ids: &'a BTreeSet<ScopeId>,
    real: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Tombstone<'a> {
    fake: &'a str,
    credential: &'a CredentialId,
}

impl CredentialStore {
    /// Writes the fake→real mappings of `credentials` plus every tombstone to `target`.
    ///
    /// Real values outside `credentials` never leave the store directory. Pending pairs
    /// have no live entry; shared fakes have one entry with every active Scope.
    /// Unavailable live fakes and all tombstones remain recognizable without disclosing real values.
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
            if !record.scope_ids.is_empty() {
                entries.push(Entry {
                    fake: &record.fake,
                    credential: id,
                    scope_ids: &record.scope_ids,
                    real: real.expose(),
                });
            }
        }
        let retired = self
            .records
            .iter()
            .filter(|(_, record)| record.real.is_none())
            .map(|(id, record)| Tombstone {
                fake: &record.fake,
                credential: id,
            })
            .collect();
        let unavailable = self
            .records
            .iter()
            .filter(|(id, record)| {
                record.real.is_some() && (record.scope_ids.is_empty() || !seen.contains(id))
            })
            .map(|(id, record)| Tombstone {
                fake: &record.fake,
                credential: id,
            })
            .collect();
        let snapshot = Snapshot {
            version: SECRETS_SNAPSHOT_VERSION,
            generation,
            entries,
            retired,
            unavailable,
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
