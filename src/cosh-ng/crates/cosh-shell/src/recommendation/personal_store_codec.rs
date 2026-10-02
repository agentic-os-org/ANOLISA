use std::fs;
use std::io::Read;
use std::path::Path;

use crate::recommendation::personal_model::{RecommendationState, RECOMMENDATION_SCHEMA_VERSION};

use super::personal_store::{
    open_owner_file, StoreError, ANALYZER_GUARD_BYTES, CURRENT_FILE, MAX_STATE_BYTES,
};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AnalyzerGuardHeader {
    pub(crate) version: u8,
    pub(crate) store_epoch: String,
    pub(crate) generation: u64,
    pub(crate) lease: Option<AnalyzerGuardLease>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AnalyzerGuardLease {
    pub(crate) owner_session_id: String,
    pub(crate) lease_nonce: String,
    pub(crate) owner_pid: u32,
    pub(crate) owner_start_identity: String,
    pub(crate) core_leader_pid: Option<u32>,
    pub(crate) core_leader_start_identity: Option<String>,
    pub(crate) core_process_group_id: Option<u32>,
}

pub(super) fn serialize_state(state: &RecommendationState) -> Result<Vec<u8>, StoreError> {
    let bytes = serde_json::to_vec(state).map_err(|_| StoreError::CorruptState)?;
    if bytes.len() > MAX_STATE_BYTES {
        Err(StoreError::StateTooLarge)
    } else {
        Ok(bytes)
    }
}

pub(super) fn analyzer_guard_header(state: &RecommendationState) -> AnalyzerGuardHeader {
    AnalyzerGuardHeader {
        version: 1,
        store_epoch: state.store_epoch.clone(),
        generation: state.generation,
        lease: state
            .scheduler
            .lease
            .as_ref()
            .map(|lease| AnalyzerGuardLease {
                owner_session_id: lease.owner_session_id.clone(),
                lease_nonce: lease.lease_nonce.clone(),
                owner_pid: lease.owner_pid,
                owner_start_identity: lease.owner_start_identity.clone(),
                core_leader_pid: lease.core_leader_pid,
                core_leader_start_identity: lease.core_leader_start_identity.clone(),
                core_process_group_id: lease.core_process_group_id,
            }),
    }
}

pub(super) fn serialize_analyzer_guard(
    header: &AnalyzerGuardHeader,
) -> Result<Vec<u8>, StoreError> {
    let encoded = serde_json::to_vec(header).map_err(|_| StoreError::CorruptState)?;
    if encoded.len() > ANALYZER_GUARD_BYTES {
        return Err(StoreError::StateTooLarge);
    }
    let mut fixed = vec![b' '; ANALYZER_GUARD_BYTES];
    fixed[..encoded.len()].copy_from_slice(&encoded);
    Ok(fixed)
}

pub(crate) fn read_analyzer_guard(root: &Path) -> Result<AnalyzerGuardHeader, StoreError> {
    let path = root.join(CURRENT_FILE);
    let mut file = open_owner_file(&path, false)?;
    if file.metadata()?.len() <= ANALYZER_GUARD_BYTES as u64 {
        return Err(StoreError::CorruptState);
    }
    let mut bytes = vec![0u8; ANALYZER_GUARD_BYTES];
    file.read_exact(&mut bytes)?;
    parse_analyzer_guard(&bytes)
}

fn parse_analyzer_guard(bytes: &[u8]) -> Result<AnalyzerGuardHeader, StoreError> {
    let end = bytes
        .iter()
        .rposition(|byte| *byte != b' ')
        .map(|index| index + 1)
        .ok_or(StoreError::CorruptState)?;
    serde_json::from_slice(&bytes[..end]).map_err(|_| StoreError::CorruptState)
}

pub(super) fn read_state_file(path: &Path) -> Result<Option<RecommendationState>, StoreError> {
    if fs::symlink_metadata(path).is_err() {
        return Ok(None);
    }
    let mut file = open_owner_file(path, false)?;
    let metadata = file.metadata()?;
    if metadata.len() <= ANALYZER_GUARD_BYTES as u64 {
        return Err(StoreError::CorruptState);
    }
    if metadata.len() > (MAX_STATE_BYTES + ANALYZER_GUARD_BYTES) as u64 {
        return Err(StoreError::StateTooLarge);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)?;
    let header = parse_analyzer_guard(&bytes[..ANALYZER_GUARD_BYTES])?;
    let state: RecommendationState = serde_json::from_slice(&bytes[ANALYZER_GUARD_BYTES..])
        .map_err(|_| StoreError::CorruptState)?;
    if state.schema_version != RECOMMENDATION_SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchema);
    }
    if header != analyzer_guard_header(&state) {
        return Err(StoreError::CorruptState);
    }
    Ok(Some(state))
}
