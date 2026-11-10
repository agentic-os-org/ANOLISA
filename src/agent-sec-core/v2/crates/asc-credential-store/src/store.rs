//! Explicit real/fake pairs with atomic registration and canonical Scope membership.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use ring::rand::{SecureRandom as _, SystemRandom};
use serde::{Deserialize, Serialize};

use crate::filesystem::{DirectoryAccess, PinnedDirectory};
use crate::secret::validate_token_value;
use crate::{CredentialId, CredentialStoreError, ScopeId, Secret, validate_pair};

/// File holding all pairs inside the store directory.
pub const STORE_FILE_NAME: &str = "store.json";
/// Maximum active pairs assigned to one Scope.
pub const MAX_ACTIVE_FAKES_PER_SCOPE: usize = 64;
/// Maximum registered pairs, including permanent tombstones.
pub const MAX_FAKES_TOTAL: usize = 4096;

const STORE_FILE_MODE: rustix::fs::RawMode = 0o600;
const MAX_STORE_BYTES: u64 = 8 * 1024 * 1024;
const STORE_VERSION: u32 = 2;
const MAX_REGISTRATIONS: usize = 4096;
const MAX_REGISTRATION_PAIRS: usize = 256;

/// Caller-provided values; registration never infers authentication metadata.
pub struct PairInput {
    /// Real value, retained only while the pair is not retired.
    pub real: Secret,
    /// Explicit replacement value.
    pub fake: String,
}

impl std::fmt::Debug for PairInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("PairInput").finish_non_exhaustive()
    }
}

/// Lifecycle state visible to administrators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialState {
    /// Registered, possibly pending its first Scope assignment.
    Active,
    /// Permanently retired; only its fake tombstone remains.
    Retired,
}

/// Value-free description of one pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialSummary {
    /// Stable opaque pair identifier.
    pub id: CredentialId,
    /// Lifecycle state.
    pub state: CredentialState,
    /// Current memberships, or last memberships of a retired pair.
    pub fakes: Vec<FakeSummary>,
}

/// Abbreviated fake associated with a Scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FakeSummary {
    /// Current or last Scope membership.
    pub scope_id: ScopeId,
    /// Whether the entire pair is permanently retired.
    pub retired: bool,
    /// Last four characters; empty for short fakes to avoid exposing the whole value.
    pub suffix: String,
}

pub(crate) struct Record {
    pub(crate) real: Option<Secret>,
    pub(crate) fake: String,
    pub(crate) scope_ids: BTreeSet<ScopeId>,
    // Last memberships are display metadata only, never authorization.
    retired_scope_ids: BTreeSet<ScopeId>,
}

impl Record {
    fn duplicate(&self) -> Self {
        Self {
            real: self.real.as_ref().map(Secret::duplicate),
            fake: self.fake.clone(),
            scope_ids: self.scope_ids.clone(),
            retired_scope_ids: self.retired_scope_ids.clone(),
        }
    }

    fn retire(&mut self) {
        self.real = None;
        self.retired_scope_ids = std::mem::take(&mut self.scope_ids);
    }
}

type Records = BTreeMap<CredentialId, Record>;
type Registrations = BTreeMap<String, Vec<CredentialId>>;

/// Owner-only store backed by one atomically replaced file and one exclusive lock.
///
/// Every mutation is copied, validated, persisted, and only then published in memory.
/// Retired fakes never become available again, even under a new pair identifier.
pub struct CredentialStore {
    pub(crate) directory: PinnedDirectory,
    pub(crate) records: Records,
    registrations: Registrations,
    random: SystemRandom,
}

impl CredentialStore {
    /// Opens an existing store without creating it.
    ///
    /// # Errors
    /// Returns `NotEnabled` for an absent directory, or lock, path, I/O and schema errors.
    pub fn open(directory: &Path) -> Result<Self, CredentialStoreError> {
        let pinned = PinnedDirectory::open_existing(directory, DirectoryAccess::OwnerOnly)?
            .ok_or_else(|| CredentialStoreError::NotEnabled {
                path: directory.into(),
            })?;
        Self::load(pinned)
    }

    /// Creates the final 0700 directory when missing; its parent must already exist.
    ///
    /// # Errors
    /// Returns lock, path, I/O and schema errors; unsupported stores are not migrated.
    pub fn create(directory: &Path) -> Result<Self, CredentialStoreError> {
        Self::load(PinnedDirectory::create_owner_only(directory)?)
    }

    fn load(directory: PinnedDirectory) -> Result<Self, CredentialStoreError> {
        directory.lock_exclusive()?;
        let (records, registrations) =
            match directory.read_file(STORE_FILE_NAME, STORE_FILE_MODE, MAX_STORE_BYTES)? {
                Some(bytes) => decode(&bytes)?,
                None => (Records::new(), Registrations::new()),
            };
        // Do not touch files in an unsupported or corrupt store.
        directory.remove_temporaries()?;
        Ok(Self {
            directory,
            records,
            registrations,
            random: SystemRandom::new(),
        })
    }

    /// Registers an entire batch atomically, returning identifiers in input order.
    ///
    /// Identical live pairs reuse their stable random identifier across input files.
    /// A real may have multiple fakes, but a fake has exactly one pair owner forever.
    ///
    /// # Errors
    /// Returns pair validation, collision, retirement, capacity or persistence errors.
    pub fn register_pairs(
        &mut self,
        inputs: Vec<PairInput>,
    ) -> Result<Vec<CredentialId>, CredentialStoreError> {
        let mut records = self.duplicate_records();
        let ids = self.prepare_pairs(&mut records, inputs)?;
        if records.len() != self.records.len() {
            self.commit(records, self.registrations.clone())?;
        }
        Ok(ids)
    }

    /// Looks up ordered pair identifiers without rereading the input token files.
    ///
    /// `key` is the caller's lowercase SHA-256 hex digest of the complete Cedar
    /// source and schema, never a secret digest or a public credential identity.
    /// A receipt records registration only; it grants no Scope membership.
    ///
    /// # Errors
    /// Rejects malformed keys and receipts referring to missing or permanently retired pairs.
    pub fn lookup_registration(
        &self,
        key: &str,
    ) -> Result<Option<Vec<CredentialId>>, CredentialStoreError> {
        validate_registration_key(key)?;
        let Some(ids) = self.registrations.get(key) else {
            return Ok(None);
        };
        for id in ids {
            let record = self
                .records
                .get(id)
                .ok_or_else(|| CredentialStoreError::UnknownCredential(id.clone()))?;
            if record.real.is_none() {
                return Err(CredentialStoreError::Retired(id.clone()));
            }
        }
        Ok(Some(ids.clone()))
    }

    /// Atomically registers pairs and a source receipt, preserving input order.
    ///
    /// Stores only the source/schema key described by [`Self::lookup_registration`]
    /// and 1..=256 pair identifiers. Repeated keys must supply exactly the same
    /// ordered real/fake pairs and do not rewrite the store.
    ///
    /// # Errors
    /// Returns validation, collision, retirement, receipt mismatch, capacity or I/O errors.
    pub fn register_policy_pairs(
        &mut self,
        key: &str,
        inputs: Vec<PairInput>,
    ) -> Result<Vec<CredentialId>, CredentialStoreError> {
        let registered = self.lookup_registration(key)?;
        if !(1..=MAX_REGISTRATION_PAIRS).contains(&inputs.len()) {
            return Err(CredentialStoreError::Invalid {
                field: "registration pairs",
                reason: "must contain 1..=256 pairs",
            });
        }
        if registered
            .as_ref()
            .is_some_and(|ids| ids.len() != inputs.len())
        {
            return Err(CredentialStoreError::Invalid {
                field: "registration pairs",
                reason: "batch length differs from the source receipt",
            });
        }
        if registered.is_none() && self.registrations.len() >= MAX_REGISTRATIONS {
            return Err(CredentialStoreError::Capacity(
                "source registration limit reached",
            ));
        }
        let mut records = self.duplicate_records();
        let ids = self.prepare_pairs(&mut records, inputs)?;
        if let Some(registered) = registered {
            if ids != registered {
                return Err(CredentialStoreError::Invalid {
                    field: "registration pairs",
                    reason: "ordered pairs differ from the source receipt",
                });
            }
            return Ok(ids);
        }
        let mut registrations = self.registrations.clone();
        registrations.insert(key.to_owned(), ids.clone());
        self.commit(records, registrations)?;
        Ok(ids)
    }

    /// Checks one proposed Scope without activating pairs or changing retirement state.
    /// # Errors
    /// Rejects missing or retired pairs, duplicate real values and excess active pairs.
    pub fn validate_scope_pairs(&self, ids: &[CredentialId]) -> Result<(), CredentialStoreError> {
        let mut seen = BTreeSet::new();
        let mut reals: Vec<&Secret> = Vec::new();
        for id in ids {
            let record = self
                .records
                .get(id)
                .ok_or_else(|| CredentialStoreError::UnknownCredential(id.clone()))?;
            let real = record
                .real
                .as_ref()
                .ok_or_else(|| CredentialStoreError::Retired(id.clone()))?;
            if !seen.insert(id) {
                continue;
            }
            if reals.iter().any(|other| other.matches(real.expose())) {
                return Err(CredentialStoreError::Conflict(id.clone()));
            }
            reals.push(real);
        }
        if seen.len() > MAX_ACTIVE_FAKES_PER_SCOPE {
            return Err(CredentialStoreError::Capacity(
                "per-scope active pair limit reached",
            ));
        }
        Ok(())
    }

    /// Replaces all active Scope memberships with the caller's canonical assignments.
    ///
    /// Omitted memberships are revoked. Losing the last membership retires a pair;
    /// a pending pair with no previous membership remains registered without activation.
    ///
    /// # Errors
    /// Returns unknown/retired pair, repeated Scope, ambiguous real, capacity or I/O errors.
    pub fn sync_scopes(
        &mut self,
        assignments: &[(ScopeId, Vec<CredentialId>)],
    ) -> Result<(), CredentialStoreError> {
        let mut memberships: BTreeMap<CredentialId, BTreeSet<ScopeId>> = BTreeMap::new();
        let mut scopes = BTreeSet::new();
        for (scope, ids) in assignments {
            if !scopes.insert(scope) {
                return Err(CredentialStoreError::Invalid {
                    field: "scope assignments",
                    reason: "must not repeat a Scope",
                });
            }
            self.validate_scope_pairs(ids)?;
            for id in ids {
                memberships
                    .entry(id.clone())
                    .or_default()
                    .insert(scope.clone());
            }
        }
        let mut records = self.duplicate_records();
        let mut changed = false;
        for (id, record) in &mut records {
            if record.real.is_none() {
                continue;
            }
            let scopes = memberships.remove(id).unwrap_or_default();
            if scopes == record.scope_ids {
                continue;
            }
            if scopes.is_empty() && !record.scope_ids.is_empty() {
                record.retire();
            } else {
                record.scope_ids = scopes;
            }
            changed = true;
        }
        if changed {
            self.commit(records, self.registrations.clone())?;
        }
        Ok(())
    }

    /// Returns a fake only for an active pair's current Scope membership.
    pub fn fake(&self, credential: &CredentialId, scope_id: &ScopeId) -> Option<&str> {
        let record = self.records.get(credential)?;
        (record.real.is_some() && record.scope_ids.contains(scope_id))
            .then_some(record.fake.as_str())
    }

    /// Permanently retires a pair globally; repeated removal is a no-op.
    ///
    /// # Errors
    /// Returns unknown pair or persistence errors.
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
            record.retire();
        }
        self.commit(records, self.registrations.clone())
    }

    /// Lists pair state without real values or complete fake values.
    pub fn list(&self) -> Vec<CredentialSummary> {
        self.records
            .iter()
            .map(|(id, record)| {
                let retired = record.real.is_none();
                let scopes = if retired {
                    &record.retired_scope_ids
                } else {
                    &record.scope_ids
                };
                CredentialSummary {
                    id: id.clone(),
                    state: if retired {
                        CredentialState::Retired
                    } else {
                        CredentialState::Active
                    },
                    fakes: scopes
                        .iter()
                        .map(|scope_id| FakeSummary {
                            scope_id: scope_id.clone(),
                            retired,
                            suffix: if record.fake.len() > 4 {
                                record.fake[record.fake.len() - 4..].to_owned()
                            } else {
                                String::new()
                            },
                        })
                        .collect(),
                }
            })
            .collect()
    }

    fn prepare_pairs(
        &self,
        records: &mut Records,
        inputs: Vec<PairInput>,
    ) -> Result<Vec<CredentialId>, CredentialStoreError> {
        let mut ids = Vec::with_capacity(inputs.len());
        for input in inputs {
            validate_pair(input.real.expose(), &input.fake)?;
            if let Some((id, record)) = records.iter().find(|(_, r)| r.fake == input.fake) {
                let real = record
                    .real
                    .as_ref()
                    .ok_or_else(|| CredentialStoreError::Retired(id.clone()))?;
                if !real.matches(input.real.expose()) {
                    return Err(CredentialStoreError::DuplicateValue(id.clone()));
                }
                ids.push(id.clone());
                continue;
            }
            for (id, record) in records.iter() {
                if input.real.matches(&record.fake)
                    || record
                        .real
                        .as_ref()
                        .is_some_and(|real| real.matches(&input.fake))
                {
                    return Err(CredentialStoreError::DuplicateValue(id.clone()));
                }
            }
            if records.len() >= MAX_FAKES_TOTAL {
                return Err(CredentialStoreError::Capacity("global pair limit reached"));
            }
            let id = self.new_id(records)?;
            records.insert(
                id.clone(),
                Record {
                    real: Some(input.real),
                    fake: input.fake,
                    scope_ids: BTreeSet::new(),
                    retired_scope_ids: BTreeSet::new(),
                },
            );
            ids.push(id);
        }
        Ok(ids)
    }

    fn new_id(&self, records: &Records) -> Result<CredentialId, CredentialStoreError> {
        for _ in 0..8 {
            let mut bytes = [0_u8; 16];
            self.random
                .fill(&mut bytes)
                .map_err(|_| CredentialStoreError::Randomness)?;
            let mut value = String::from("pair-");
            for byte in bytes {
                let _ = write!(value, "{byte:02x}");
            }
            let id = CredentialId::new(value)?;
            if !records.contains_key(&id) {
                return Ok(id);
            }
        }
        Err(CredentialStoreError::Randomness)
    }

    fn duplicate_records(&self) -> Records {
        self.records
            .iter()
            .map(|(id, record)| (id.clone(), record.duplicate()))
            .collect()
    }

    fn commit(
        &mut self,
        records: Records,
        registrations: Registrations,
    ) -> Result<(), CredentialStoreError> {
        validate(&records, &registrations)?;
        let bytes = encode(&records, &registrations)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_STORE_BYTES {
            return Err(CredentialStoreError::Capacity("store byte limit reached"));
        }
        let result = self
            .directory
            .write_atomic(STORE_FILE_NAME, &bytes, STORE_FILE_MODE, None);
        // After rename, both pairs and receipts follow disk even if directory fsync fails.
        if result.is_ok() || result.as_ref().is_err_and(|failure| failure.replaced) {
            self.records = records;
            self.registrations = registrations;
        }
        result.map_err(|failure| failure.error)
    }
}

impl std::fmt::Debug for CredentialStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CredentialStore")
            .field("directory", &self.directory.path())
            .field("pairs", &self.records.len())
            .finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedStore {
    version: u32,
    credentials: Vec<PersistedCredential>,
    #[serde(default)]
    registrations: Registrations,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PersistedCredential {
    id: CredentialId,
    real: Option<String>,
    fake: String,
    scope_ids: BTreeSet<ScopeId>,
    retired_scope_ids: BTreeSet<ScopeId>,
}

fn encode(
    records: &Records,
    registrations: &Registrations,
) -> Result<Vec<u8>, CredentialStoreError> {
    let persisted = PersistedStore {
        version: STORE_VERSION,
        registrations: registrations.clone(),
        credentials: records
            .iter()
            .map(|(id, record)| PersistedCredential {
                id: id.clone(),
                real: record.real.as_ref().map(|real| real.expose().to_owned()),
                fake: record.fake.clone(),
                scope_ids: record.scope_ids.clone(),
                retired_scope_ids: record.retired_scope_ids.clone(),
            })
            .collect(),
    };
    serde_json::to_vec_pretty(&persisted)
        .map_err(|_| CredentialStoreError::Corrupt("cannot encode store".into()))
}

fn decode(bytes: &[u8]) -> Result<(Records, Registrations), CredentialStoreError> {
    #[derive(Deserialize)]
    struct Version {
        version: u32,
    }
    // Inspect the version before interpreting record fields; v1 has no migration path.
    let version: Version = serde_json::from_slice(bytes).map_err(|error| parse_error(&error))?;
    if version.version != STORE_VERSION {
        return Err(CredentialStoreError::Corrupt(format!(
            "unsupported store version {}",
            version.version
        )));
    }
    let persisted: PersistedStore =
        serde_json::from_slice(bytes).map_err(|error| parse_error(&error))?;
    let mut records = Records::new();
    for credential in persisted.credentials {
        let real = credential
            .real
            .map(Secret::new)
            .transpose()
            .map_err(|_| CredentialStoreError::Corrupt("invalid real value".into()))?;
        let record = Record {
            real,
            fake: credential.fake,
            scope_ids: credential.scope_ids,
            retired_scope_ids: credential.retired_scope_ids,
        };
        if records.insert(credential.id, record).is_some() {
            return Err(CredentialStoreError::Corrupt(
                "duplicate pair identifier".into(),
            ));
        }
    }
    validate(&records, &persisted.registrations)?;
    Ok((records, persisted.registrations))
}

fn validate_registration_key(key: &str) -> Result<(), CredentialStoreError> {
    if key.len() != 64
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(CredentialStoreError::Invalid {
            field: "registration key",
            reason: "must be 64 lowercase hexadecimal characters",
        });
    }
    Ok(())
}

fn parse_error(error: &serde_json::Error) -> CredentialStoreError {
    // Parser messages can echo values; expose only their position.
    CredentialStoreError::Corrupt(format!(
        "invalid store file at line {} column {}",
        error.line(),
        error.column()
    ))
}

fn validate(records: &Records, registrations: &Registrations) -> Result<(), CredentialStoreError> {
    let corrupt = |reason: &str| CredentialStoreError::Corrupt(reason.into());
    if records.len() > MAX_FAKES_TOTAL {
        return Err(corrupt("global pair limit exceeded"));
    }
    if registrations.len() > MAX_REGISTRATIONS {
        return Err(corrupt("source registration limit exceeded"));
    }
    for (key, ids) in registrations {
        validate_registration_key(key).map_err(|_| corrupt("invalid registration key"))?;
        if !(1..=MAX_REGISTRATION_PAIRS).contains(&ids.len()) {
            return Err(corrupt("registration must contain 1..=256 pairs"));
        }
        if ids.iter().any(|id| !records.contains_key(id)) {
            return Err(corrupt("registration references an unknown pair"));
        }
    }
    let mut fakes = BTreeSet::new();
    let mut per_scope: BTreeMap<&ScopeId, Vec<&Secret>> = BTreeMap::new();
    for record in records.values() {
        validate_token_value(&record.fake, "fake value")
            .map_err(|_| corrupt("invalid fake value"))?;
        if !fakes.insert(record.fake.as_str()) {
            return Err(corrupt("fake value is not unique"));
        }
        match &record.real {
            Some(real) => {
                validate_pair(real.expose(), &record.fake)
                    .map_err(|_| corrupt("invalid real/fake pair"))?;
                if !record.retired_scope_ids.is_empty() {
                    return Err(corrupt("live pair has retired memberships"));
                }
                for scope in &record.scope_ids {
                    let reals = per_scope.entry(scope).or_default();
                    if reals.iter().any(|other| other.matches(real.expose())) {
                        return Err(corrupt("Scope has multiple fakes for one real"));
                    }
                    reals.push(real);
                }
            }
            None if !record.scope_ids.is_empty() => {
                return Err(corrupt("retired pair has active memberships"));
            }
            None => {}
        }
    }
    if per_scope
        .values()
        .any(|reals| reals.len() > MAX_ACTIVE_FAKES_PER_SCOPE)
    {
        return Err(corrupt("per-scope active pair limit exceeded"));
    }
    for real in records.values().filter_map(|record| record.real.as_ref()) {
        if fakes.iter().any(|fake| real.matches(fake)) {
            return Err(corrupt("real value collides with a managed fake"));
        }
    }
    Ok(())
}
