use super::{private_state, Error, GuideStore};
use crate::insight::guidance::{self, BashEvent};
use serde::{Deserialize, Serialize};

const TTL_MS: u64 = 60 * 60 * 1000;
const MAX_RECORDS: usize = 16;
const MAX_BYTES: usize = 128 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Contexts {
    version: u32,
    entries: Vec<Context>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Context {
    id: String,
    event: BashEvent,
}

pub(crate) fn valid_context_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn fresh(event: &BashEvent, now_ms: u64) -> bool {
    now_ms
        .checked_sub(event.observed_at_ms)
        .is_some_and(|age| age <= TTL_MS)
}

pub(crate) fn load_failure_context(id: &str, now_ms: u64) -> Result<BashEvent, Error> {
    if !valid_context_id(id) {
        return Err(Error::InvalidRequest);
    }
    let root = crate::config::load::copilot_shell_cosh_dir()
        .ok_or(Error::InvalidRequest)?
        .join("guide");
    private_state::validate_directory(&root)?;
    GuideStore { root }.load_failure_context(id, now_ms)
}

impl GuideStore {
    fn read_failure_contexts(&self) -> Result<Contexts, Error> {
        let Some(bytes) =
            private_state::read_bounded(&self.root.join("failure-contexts.json"), MAX_BYTES)?
        else {
            return Ok(Contexts {
                version: 1,
                entries: Vec::new(),
            });
        };
        let records: Contexts = serde_json::from_slice(&bytes).map_err(|_| Error::Corrupt)?;
        if records.version != 1
            || records.entries.len() > MAX_RECORDS
            || records.entries.iter().any(|record| {
                !valid_context_id(&record.id) || guidance::failure_kind(&record.event).is_none()
            })
        {
            return Err(Error::Corrupt);
        }
        Ok(records)
    }

    pub(crate) fn claim_failure_context(
        &self,
        runtime: &super::RuntimeState,
        snapshot: &super::GuideSnapshot,
        request: super::ClaimRequest<'_>,
        id: &str,
        event: &BashEvent,
        now_ms: u64,
    ) -> Result<super::ClaimDecision, Error> {
        if request.kind != super::InviteKind::Failure {
            return Err(Error::InvalidRequest);
        }
        self.claim_prepared(Some(runtime), snapshot, request, now_ms, || {
            self.save_failure_context_unlocked(id, event, now_ms)
        })
    }

    #[cfg(test)]
    pub(crate) fn save_failure_context(
        &self,
        id: &str,
        event: &BashEvent,
        now_ms: u64,
    ) -> Result<(), Error> {
        let _lock = self.lock()?;
        self.save_failure_context_unlocked(id, event, now_ms)
    }

    fn save_failure_context_unlocked(
        &self,
        id: &str,
        event: &BashEvent,
        now_ms: u64,
    ) -> Result<(), Error> {
        if !valid_context_id(id) || guidance::failure_kind(event).is_none() || !fresh(event, now_ms)
        {
            return Err(Error::InvalidRequest);
        }
        let mut records = self.read_failure_contexts()?;
        records
            .entries
            .retain(|record| fresh(&record.event, now_ms));
        records
            .entries
            .sort_by_key(|record| record.event.observed_at_ms);
        if records.entries.len() == MAX_RECORDS {
            records.entries.remove(0);
        }
        records.entries.push(Context {
            id: id.to_string(),
            event: event.clone(),
        });
        let bytes = serde_json::to_vec(&records).map_err(|_| Error::Corrupt)?;
        private_state::atomic_write(&self.root.join("failure-contexts.json"), &bytes, MAX_BYTES)?;
        Ok(())
    }

    pub(crate) fn load_failure_context(&self, id: &str, now_ms: u64) -> Result<BashEvent, Error> {
        if !valid_context_id(id) {
            return Err(Error::InvalidRequest);
        }
        let _lock = self.lock()?;
        self.read_failure_contexts()?
            .entries
            .into_iter()
            .find(|record| record.id == id && fresh(&record.event, now_ms))
            .map(|record| record.event)
            .ok_or(Error::InvalidRequest)
    }
}
