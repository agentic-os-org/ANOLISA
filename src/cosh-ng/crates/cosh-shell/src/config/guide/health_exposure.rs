//! Bash exposure events paired with v1 claims and a v2 compatibility mirror.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::super::private_state;
use super::{Error, MAX_BYTES, MAX_CLAIMS};

const FILE: &str = "health-exposures-v3.json";
const LEGACY_FILE: &str = "health-exposures-v2.json";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HealthExposures {
    pub(super) version: u32,
    pub(super) entries: Vec<HealthExposureEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(super) struct HealthExposureEntry {
    pub(super) problem: String,
    pub(super) shown_at_ms: u64,
    pub(super) evidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) event: Option<ExposureEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) severity: Option<u8>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(super) struct ExposureEvent {
    pub(super) domain: String,
    pub(super) identity: String,
}

pub(super) fn valid_event_domain(domain: &str) -> bool {
    !domain.is_empty()
        && domain.len() <= 32
        && domain
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

impl Default for HealthExposures {
    fn default() -> Self {
        Self {
            version: 3,
            entries: Vec::new(),
        }
    }
}

pub(super) fn read(root: &Path) -> Result<HealthExposures, Error> {
    let mut merged = read_file(root, LEGACY_FILE, 2)?.unwrap_or_default();
    let current = read_file(root, FILE, 3)?;
    merged.version = 3;
    for entry in &mut merged.entries {
        entry.event = current.as_ref().and_then(|current| {
            current
                .entries
                .iter()
                .find(|previous| {
                    entry.problem == previous.problem
                        && entry.shown_at_ms == previous.shown_at_ms
                        && entry.identity == previous.identity
                        && entry.evidence == previous.evidence
                        && entry.severity == previous.severity
                })
                .and_then(|previous| previous.event.clone())
        });
    }
    Ok(merged)
}

fn read_file(root: &Path, file: &str, version: u32) -> Result<Option<HealthExposures>, Error> {
    let Some(bytes) = private_state::read_bounded(&root.join(file), MAX_BYTES)? else {
        return Ok(None);
    };
    let exposures: HealthExposures = serde_json::from_slice(&bytes).map_err(|_| Error::Corrupt)?;
    if exposures.version != version {
        return Err(Error::UnsupportedVersion);
    }
    if exposures.entries.len() > MAX_CLAIMS
        || exposures.entries.iter().any(|entry| {
            !super::valid_digest(&entry.problem)
                || !super::valid_digest(&entry.evidence)
                || entry
                    .identity
                    .as_ref()
                    .is_some_and(|id| !super::valid_digest(id))
                || entry.event.as_ref().is_some_and(|event| {
                    !valid_event_domain(&event.domain) || !super::valid_digest(&event.identity)
                })
        })
    {
        return Err(Error::Corrupt);
    }
    Ok(Some(exposures))
}

pub(super) fn write(root: &Path, exposures: &HealthExposures) -> Result<(), Error> {
    let legacy = HealthExposures {
        version: 2,
        entries: exposures
            .entries
            .iter()
            .cloned()
            .map(|mut entry| {
                entry.event = None;
                entry
            })
            .collect(),
    };
    let legacy_bytes = serde_json::to_vec(&legacy).map_err(|_| Error::Corrupt)?;
    private_state::atomic_write(&root.join(LEGACY_FILE), &legacy_bytes, MAX_BYTES)?;
    let bytes = serde_json::to_vec(exposures).map_err(|_| Error::Corrupt)?;
    private_state::atomic_write(&root.join(FILE), &bytes, MAX_BYTES).map_err(Error::from)
}

pub(super) fn paired<'a>(
    exposures: &'a HealthExposures,
    claim: &super::ClaimEntry,
) -> Option<&'a HealthExposureEntry> {
    exposures.entries.iter().find(|entry| {
        entry.problem == claim.problem
            && entry.shown_at_ms == claim.shown_at_ms
            && entry.identity.as_deref() == Some(super::claim_identity(claim).as_str())
    })
}
