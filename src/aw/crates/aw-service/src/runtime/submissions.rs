//! Retire input details while retaining a producer high-water mark against replay.

use super::Runtime;
use crate::{rejected, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct Ledger {
    issued: u64,
    active: BTreeMap<u64, (String, bool)>,
}

impl Ledger {
    pub(super) fn claim(
        &mut self,
        session: &str,
        turn: &str,
        closing: bool,
        limit: usize,
    ) -> Result<()> {
        let sequence = turn
            .parse::<u64>()
            .map_err(|_| rejected("submission_sequence_invalid"))?;
        if sequence == 0
            || sequence.to_string() != turn
            || session.is_empty()
            || session.len() > 512
        {
            return Err(rejected("submission_identity_invalid"));
        }
        if let Some((owner, closed)) = self.active.get_mut(&sequence) {
            if owner != session || (!closing && *closed) {
                return Err(rejected("submission_closed_or_mismatched"));
            }
            *closed |= closing;
            return Ok(());
        }
        if self.issued.checked_add(1) != Some(sequence) {
            return Err(rejected("submission_sequence_replay"));
        }
        // Admission failure still consumes the sequence; recovery cannot replay its budget.
        self.issued = sequence;
        if self.active.len() >= limit {
            return Err(rejected("submission_limit"));
        }
        self.active.insert(sequence, (session.into(), closing));
        Ok(())
    }

    pub(super) fn retire(&mut self, turn: &str) {
        if let Ok(sequence) = turn.parse::<u64>() {
            self.active.remove(&sequence);
        }
    }
}

impl Runtime {
    pub(super) fn enable_submissions(&self, id: &str) -> Result<Value> {
        self.check_scope_owner(id)?;
        let instances = self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        let instance = instances
            .get(id)
            .and_then(Option::as_ref)
            .filter(|instance| !instance.closing)
            .ok_or_else(|| rejected("unknown_instance"))?;
        let mut state = instance
            .invocations
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        if state.submissions.is_some() || state.legacy_submissions {
            return Err(rejected("submission_protocol_requires_unused_binding"));
        }
        state.submissions = Some(Ledger::default());
        Ok(json!({"enabled":true}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_submissions_are_bounded_and_finish_out_of_order_without_replay() {
        let mut ledger = Ledger::default();
        ledger.claim("one", "1", false, 2).unwrap();
        ledger.claim("two", "2", false, 2).unwrap();
        assert!(ledger.claim("three", "3", false, 2).is_err());
        ledger.claim("two", "2", true, 2).unwrap();
        assert!(ledger.claim("two", "2", false, 2).is_err());
        ledger.retire("2");
        assert!(ledger.claim("three", "3", false, 2).is_err());
        ledger.claim("four", "4", false, 2).unwrap();
        ledger.claim("one", "1", false, 2).unwrap();
        assert!(ledger.claim("two", "2", true, 2).is_err());
        assert!(ledger.claim("wrong", "1", true, 2).is_err());
        assert_eq!(ledger.active.len(), 2);
        ledger.claim("one", "1", true, 2).unwrap();
        ledger.retire("1");
        ledger.claim("four", "4", true, 2).unwrap();
        ledger.retire("4");
        assert!(ledger.active.is_empty());
        assert!(ledger.claim("four", "4", false, 2).is_err());
    }
}
