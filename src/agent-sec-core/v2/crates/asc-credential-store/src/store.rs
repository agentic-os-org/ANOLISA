use std::collections::BTreeMap;
use std::path::Path;

use ring::rand::SystemRandom;
use serde::{Deserialize, Serialize};

use crate::filesystem::{DirectoryAccess, PinnedDirectory};
use crate::secret::validate_token_value;
use crate::{AgentName, CredentialId, CredentialStoreError, FakeFormat, Secret};

/// File holding all credential records inside the store directory.
pub const STORE_FILE_NAME: &str = "store.json";
/// Maximum active fakes issued to one Agent domain.
pub const MAX_ACTIVE_FAKES_PER_AGENT: usize = 64;
/// Maximum fakes across all credentials, tombstones included.
///
/// Tombstones are never pruned, so repeated remove/re-add cycles consume this
/// budget permanently; removed fakes must stay rejected rather than becoming unknown.
pub const MAX_FAKES_TOTAL: usize = 4096;

const STORE_FILE_MODE: rustix::fs::RawMode = 0o600;
const MAX_STORE_BYTES: u64 = 8 * 1024 * 1024;
const STORE_VERSION: u32 = 1;
const FAKE_ATTEMPTS: usize = 8;

/// Request location where a credential's fake may appear and be replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Carrier {
    /// The token part of an `Authorization: Bearer|token <value>` header.
    HeaderToken,
    /// The password of an `Authorization: Basic` header.
    BasicPassword,
}

/// Static description of a credential, supplied by a preset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialSpec {
    /// Store identifier.
    pub id: CredentialId,
    /// Locations the gateway inspects for this credential's fakes.
    pub carriers: Vec<Carrier>,
    /// Shape used when issuing fakes.
    pub format: FakeFormat,
}

/// Lifecycle state visible to administrators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialState {
    /// Holds a real value and can issue fakes.
    Active,
    /// Removed; its former fakes are kept only as rejection tombstones.
    Retired,
}

/// Value-free description of one credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialSummary {
    /// Store identifier.
    pub id: CredentialId,
    /// Lifecycle state.
    pub state: CredentialState,
    /// Configured carriers.
    pub carriers: Vec<Carrier>,
    /// Issued fakes, abbreviated.
    pub fakes: Vec<FakeSummary>,
}

/// Abbreviated fake: enough to recognise it, not enough to use it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FakeSummary {
    /// Agent domain the fake belongs to.
    pub agent: AgentName,
    /// Whether the fake is a tombstone of a removed credential.
    pub retired: bool,
    /// Last four characters of the fake.
    pub suffix: String,
}

/// Result of [`CredentialStore::put`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutOutcome {
    /// A new credential was stored.
    Created,
    /// The stored credential already had this value and spec; nothing was written.
    Unchanged,
    /// The value or spec changed; issued fakes are kept.
    Replaced,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Fake {
    pub(crate) agent: AgentName,
    pub(crate) value: String,
}

pub(crate) struct Record {
    pub(crate) carriers: Vec<Carrier>,
    pub(crate) format: FakeFormat,
    pub(crate) real: Option<Secret>,
    pub(crate) fakes: Vec<Fake>,
    pub(crate) retired_fakes: Vec<Fake>,
}

impl Record {
    fn duplicate(&self) -> Self {
        Self {
            carriers: self.carriers.clone(),
            format: self.format.clone(),
            real: self.real.as_ref().map(Secret::duplicate),
            fakes: self.fakes.clone(),
            retired_fakes: self.retired_fakes.clone(),
        }
    }
}

type Records = BTreeMap<CredentialId, Record>;

/// Root-only credential store backed by one atomically replaced file.
///
/// The directory must be owned by the effective user with mode 0700 and the
/// store file with mode 0600; symlinks are refused at every path component.
/// Every mutation is computed on a copy, validated, persisted, and only then
/// becomes visible, so a failed write leaves both disk and memory unchanged.
pub struct CredentialStore {
    pub(crate) directory: PinnedDirectory,
    pub(crate) records: Records,
    random: SystemRandom,
}

impl CredentialStore {
    /// Opens an existing store without creating anything.
    ///
    /// # Errors
    /// Returns [`CredentialStoreError::NotEnabled`] when the directory is
    /// absent, and path, I/O or corruption errors otherwise.
    pub fn open(directory: &Path) -> Result<Self, CredentialStoreError> {
        let pinned = PinnedDirectory::open_existing(directory, DirectoryAccess::OwnerOnly)?
            .ok_or_else(|| CredentialStoreError::NotEnabled {
                path: directory.into(),
            })?;
        Self::load(pinned)
    }

    /// Creates the 0700 store directory when missing and opens the store.
    ///
    /// The parent directory must already exist; it is not created.
    ///
    /// # Errors
    /// Returns path, I/O or corruption errors.
    pub fn create(directory: &Path) -> Result<Self, CredentialStoreError> {
        Self::load(PinnedDirectory::create_owner_only(directory)?)
    }

    fn load(directory: PinnedDirectory) -> Result<Self, CredentialStoreError> {
        // A single writer is assumed by copy-validate-commit; a second one would lose updates.
        directory.lock_exclusive()?;
        directory.remove_temporaries()?;
        let records =
            match directory.read_file(STORE_FILE_NAME, STORE_FILE_MODE, MAX_STORE_BYTES)? {
                Some(bytes) => decode(&bytes)?,
                None => Records::new(),
            };
        Ok(Self {
            directory,
            records,
            random: SystemRandom::new(),
        })
    }

    /// Stores or replaces the real value of a credential.
    ///
    /// Replacing keeps every issued fake, so Agents keep working after a key
    /// rotation. Storing a retired credential reactivates it without its old
    /// fakes, which stay tombstones.
    ///
    /// # Errors
    /// Returns [`CredentialStoreError::DuplicateValue`] when the value equals
    /// another credential's real value or any issued fake, and validation or
    /// persistence errors otherwise.
    pub fn put(
        &mut self,
        spec: CredentialSpec,
        real: Secret,
    ) -> Result<PutOutcome, CredentialStoreError> {
        let mut carriers = spec.carriers;
        carriers.sort_unstable();
        carriers.dedup();
        if carriers.is_empty() {
            return Err(CredentialStoreError::Invalid {
                field: "carriers",
                reason: "at least one carrier is required",
            });
        }
        spec.format.check(real.expose())?;
        let collides = self.records.iter().any(|(id, record)| {
            (*id != spec.id
                && record
                    .real
                    .as_ref()
                    .is_some_and(|other| other.matches(real.expose())))
                || record
                    .fakes
                    .iter()
                    .chain(&record.retired_fakes)
                    .any(|fake| real.matches(&fake.value))
        });
        if collides {
            return Err(CredentialStoreError::DuplicateValue(spec.id));
        }
        let mut records = self.duplicate_records();
        let outcome = match records.get_mut(&spec.id) {
            None => {
                records.insert(
                    spec.id,
                    Record {
                        carriers,
                        format: spec.format,
                        real: Some(real),
                        fakes: Vec::new(),
                        retired_fakes: Vec::new(),
                    },
                );
                PutOutcome::Created
            }
            Some(record) => {
                let unchanged = record
                    .real
                    .as_ref()
                    .is_some_and(|old| old.matches(real.expose()))
                    && record.carriers == carriers
                    && record.format == spec.format;
                if unchanged {
                    return Ok(PutOutcome::Unchanged);
                }
                record.real = Some(real);
                record.carriers = carriers;
                record.format = spec.format;
                PutOutcome::Replaced
            }
        };
        self.commit(records)?;
        Ok(outcome)
    }

    /// Returns the fake of `credential` for `agent`, issuing and persisting one when absent.
    ///
    /// Repeated calls return the same fake.
    ///
    /// # Errors
    /// Returns unknown/retired credential, capacity, fake generation or persistence errors.
    pub fn issue_fake(
        &mut self,
        credential: &CredentialId,
        agent: &AgentName,
    ) -> Result<String, CredentialStoreError> {
        let record = self
            .records
            .get(credential)
            .ok_or_else(|| CredentialStoreError::UnknownCredential(credential.clone()))?;
        let Some(real) = &record.real else {
            return Err(CredentialStoreError::Retired(credential.clone()));
        };
        if let Some(fake) = record.fakes.iter().find(|fake| fake.agent == *agent) {
            return Ok(fake.value.clone());
        }
        if self.active_fakes_of(agent) >= MAX_ACTIVE_FAKES_PER_AGENT {
            return Err(CredentialStoreError::Capacity(
                "per-agent active fake limit reached",
            ));
        }
        if self.total_fakes() >= MAX_FAKES_TOTAL {
            return Err(CredentialStoreError::Capacity("global fake limit reached"));
        }
        let mut value = None;
        for _ in 0..FAKE_ATTEMPTS {
            let candidate = record.format.generate(real.expose(), &self.random)?;
            if !self.is_managed_value(&candidate) {
                value = Some(candidate);
                break;
            }
        }
        let value = value.ok_or(CredentialStoreError::FakeGeneration)?;
        let mut records = self.duplicate_records();
        if let Some(record) = records.get_mut(credential) {
            record.fakes.push(Fake {
                agent: agent.clone(),
                value: value.clone(),
            });
        }
        self.commit(records)?;
        Ok(value)
    }

    /// Returns the already issued fake of `credential` for `agent`.
    pub fn fake(&self, credential: &CredentialId, agent: &AgentName) -> Option<&str> {
        self.records
            .get(credential)?
            .fakes
            .iter()
            .find(|fake| fake.agent == *agent)
            .map(|fake| fake.value.as_str())
    }

    /// Retires a credential: drops its real value and turns its fakes into tombstones.
    ///
    /// Removing an already retired credential is a no-op.
    ///
    /// # Errors
    /// Returns [`CredentialStoreError::UnknownCredential`] or persistence errors.
    pub fn remove(&mut self, credential: &CredentialId) -> Result<(), CredentialStoreError> {
        let record = self
            .records
            .get(credential)
            .ok_or_else(|| CredentialStoreError::UnknownCredential(credential.clone()))?;
        if record.real.is_none() {
            return Ok(());
        }
        let mut records = self.duplicate_records();
        if let Some(record) = records.get_mut(credential) {
            record.real = None;
            let fakes = std::mem::take(&mut record.fakes);
            record.retired_fakes.extend(fakes);
        }
        self.commit(records)
    }

    /// Turns the fake of `credential` held by `agent` into a tombstone.
    ///
    /// Other Agent domains keep using the credential. Retiring an absent fake
    /// is a no-op; a later [`Self::issue_fake`] issues a new value.
    ///
    /// # Errors
    /// Returns [`CredentialStoreError::UnknownCredential`] or persistence errors.
    pub fn retire_fake(
        &mut self,
        credential: &CredentialId,
        agent: &AgentName,
    ) -> Result<(), CredentialStoreError> {
        let record = self
            .records
            .get(credential)
            .ok_or_else(|| CredentialStoreError::UnknownCredential(credential.clone()))?;
        if !record.fakes.iter().any(|fake| fake.agent == *agent) {
            return Ok(());
        }
        let mut records = self.duplicate_records();
        if let Some(record) = records.get_mut(credential) {
            let (retired, kept) = std::mem::take(&mut record.fakes)
                .into_iter()
                .partition(|fake| fake.agent == *agent);
            record.fakes = kept;
            record.retired_fakes.extend::<Vec<Fake>>(retired);
        }
        self.commit(records)
    }

    /// Lists credentials without real values and with abbreviated fakes.
    pub fn list(&self) -> Vec<CredentialSummary> {
        self.records
            .iter()
            .map(|(id, record)| CredentialSummary {
                id: id.clone(),
                state: if record.real.is_some() {
                    CredentialState::Active
                } else {
                    CredentialState::Retired
                },
                carriers: record.carriers.clone(),
                fakes: record
                    .fakes
                    .iter()
                    .map(|fake| (fake, false))
                    .chain(record.retired_fakes.iter().map(|fake| (fake, true)))
                    .map(|(fake, retired)| FakeSummary {
                        agent: fake.agent.clone(),
                        retired,
                        suffix: suffix(&fake.value),
                    })
                    .collect(),
            })
            .collect()
    }

    fn duplicate_records(&self) -> Records {
        self.records
            .iter()
            .map(|(id, record)| (id.clone(), record.duplicate()))
            .collect()
    }

    fn active_fakes_of(&self, agent: &AgentName) -> usize {
        self.records
            .values()
            .flat_map(|record| &record.fakes)
            .filter(|fake| fake.agent == *agent)
            .count()
    }

    fn total_fakes(&self) -> usize {
        self.records
            .values()
            .map(|record| record.fakes.len() + record.retired_fakes.len())
            .sum()
    }

    fn is_managed_value(&self, candidate: &str) -> bool {
        self.records.values().any(|record| {
            record
                .real
                .as_ref()
                .is_some_and(|real| real.matches(candidate))
                || record
                    .fakes
                    .iter()
                    .chain(&record.retired_fakes)
                    .any(|fake| fake.value == candidate)
        })
    }

    fn commit(&mut self, records: Records) -> Result<(), CredentialStoreError> {
        validate(&records)?;
        let bytes = encode(&records)?;
        match self
            .directory
            .write_atomic(STORE_FILE_NAME, &bytes, STORE_FILE_MODE, None)
        {
            Ok(()) => {
                self.records = records;
                Ok(())
            }
            Err(failure) => {
                // Once renamed, the disk holds the new state; memory must follow it, or a
                // removed credential would keep being materialized until restart.
                if failure.replaced {
                    self.records = records;
                }
                Err(failure.error)
            }
        }
    }
}

impl std::fmt::Debug for CredentialStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CredentialStore")
            .field("directory", &self.directory.path())
            .field("credentials", &self.records.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

fn suffix(value: &str) -> String {
    let start = value.len().saturating_sub(4);
    value.get(start..).unwrap_or_default().to_owned()
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedStore {
    version: u32,
    credentials: Vec<PersistedCredential>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PersistedCredential {
    id: CredentialId,
    carriers: Vec<Carrier>,
    fake_prefixes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    real: Option<String>,
    fakes: Vec<Fake>,
    retired_fakes: Vec<Fake>,
}

fn encode(records: &Records) -> Result<Vec<u8>, CredentialStoreError> {
    let persisted = PersistedStore {
        version: STORE_VERSION,
        credentials: records
            .iter()
            .map(|(id, record)| PersistedCredential {
                id: id.clone(),
                carriers: record.carriers.clone(),
                fake_prefixes: record.format.prefixes().to_vec(),
                real: record.real.as_ref().map(|real| real.expose().to_owned()),
                fakes: record.fakes.clone(),
                retired_fakes: record.retired_fakes.clone(),
            })
            .collect(),
    };
    serde_json::to_vec_pretty(&persisted)
        .map_err(|_| CredentialStoreError::Corrupt("cannot encode store".into()))
}

fn decode(bytes: &[u8]) -> Result<Records, CredentialStoreError> {
    // Parser messages can quote input fragments, so report only the position.
    let persisted: PersistedStore = serde_json::from_slice(bytes).map_err(|error| {
        CredentialStoreError::Corrupt(format!(
            "invalid store file at line {} column {}",
            error.line(),
            error.column()
        ))
    })?;
    if persisted.version != STORE_VERSION {
        return Err(CredentialStoreError::Corrupt(format!(
            "unsupported store version {}",
            persisted.version
        )));
    }
    let mut records = Records::new();
    for credential in persisted.credentials {
        let real = credential
            .real
            .map(Secret::new)
            .transpose()
            .map_err(|_| CredentialStoreError::Corrupt("invalid real value".into()))?;
        let record = Record {
            carriers: credential.carriers,
            format: FakeFormat::with_prefixes(credential.fake_prefixes),
            real,
            fakes: credential.fakes,
            retired_fakes: credential.retired_fakes,
        };
        if records.insert(credential.id.clone(), record).is_some() {
            return Err(CredentialStoreError::Corrupt(format!(
                "duplicate credential {}",
                credential.id
            )));
        }
    }
    validate(&records)?;
    Ok(records)
}

/// Checks the invariants the gateway relies on for deterministic fake/real mapping.
fn validate(records: &Records) -> Result<(), CredentialStoreError> {
    let corrupt = |reason: &str| Err(CredentialStoreError::Corrupt(reason.into()));
    let mut fake_values = std::collections::HashSet::new();
    let mut per_agent: BTreeMap<&AgentName, usize> = BTreeMap::new();
    for record in records.values() {
        if record.carriers.is_empty() {
            return corrupt("credential without carriers");
        }
        if record.real.is_none() && !record.fakes.is_empty() {
            return corrupt("retired credential has active fakes");
        }
        let mut agents = std::collections::HashSet::new();
        for fake in &record.fakes {
            if !agents.insert(&fake.agent) {
                return corrupt("credential has two active fakes for one agent");
            }
            *per_agent.entry(&fake.agent).or_default() += 1;
        }
        for fake in record.fakes.iter().chain(&record.retired_fakes) {
            if validate_token_value(&fake.value, "fake value").is_err() {
                return corrupt("invalid fake value");
            }
            if !fake_values.insert(fake.value.as_str()) {
                return corrupt("fake value is not unique");
            }
        }
    }
    if fake_values.len() > MAX_FAKES_TOTAL {
        return corrupt("global fake limit exceeded");
    }
    if per_agent
        .values()
        .any(|count| *count > MAX_ACTIVE_FAKES_PER_AGENT)
    {
        return corrupt("per-agent fake limit exceeded");
    }
    let reals: Vec<&Secret> = records
        .values()
        .filter_map(|record| record.real.as_ref())
        .collect();
    for (index, real) in reals.iter().enumerate() {
        if reals[index + 1..]
            .iter()
            .any(|other| other.matches(real.expose()))
            || fake_values.iter().any(|fake| real.matches(fake))
        {
            return corrupt("real value collides with another managed value");
        }
    }
    Ok(())
}
