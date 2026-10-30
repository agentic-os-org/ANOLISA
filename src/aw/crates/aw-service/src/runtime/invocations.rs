//! Producer-issued sequences permit exact replay rejection after detail reclamation.

use super::{hooks, Runtime};
use crate::{rejected, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Instant,
};

#[derive(Default)]
pub(super) struct State {
    pub(super) ledger: Option<Ledger>,
    pub(super) legacy_used: bool,
}

#[derive(Default)]
pub(super) struct Ledger {
    issued: u64,
    pub(super) active: BTreeMap<String, Invocation>,
}

pub(super) struct Invocation {
    sequence: u64,
    digest: [u8; 32],
    session: Option<String>,
    closing: bool,
}

fn digest(tool: &str, input: &Value) -> Result<[u8; 32]> {
    fn numbers(value: &Value) -> Result<Value> {
        // Managed Pi/Claude producers dispatch through JavaScript Number.
        // Compare binary64 values while preserving the original Provider input.
        match value {
            Value::Number(number) => {
                let number = number
                    .as_f64()
                    .filter(|number| number.is_finite())
                    .ok_or_else(|| rejected("invocation_input_invalid"))?;
                Ok(json!(if number == 0.0 { 0.0 } else { number }))
            }
            Value::Array(items) => items
                .iter()
                .map(numbers)
                .collect::<Result<Vec<_>>>()
                .map(Value::Array),
            Value::Object(items) => items
                .iter()
                .map(|(key, value)| Ok((key.clone(), numbers(value)?)))
                .collect::<Result<serde_json::Map<_, _>>>()
                .map(Value::Object),
            _ => Ok(value.clone()),
        }
    }
    Ok(Sha256::digest(
        serde_json::to_vec(&json!([tool, numbers(input)?]))
            .map_err(|_| rejected("invocation_input_invalid"))?,
    )
    .into())
}

#[cfg(test)]
mod tests;

impl Ledger {
    pub(super) fn check_close(&self, session: &str, call: &str) -> Result<()> {
        if !self
            .active
            .get(call)
            .is_some_and(|entry| entry.session.as_deref() == Some(session))
        {
            return Err(rejected("invocation_retired_or_unknown"));
        }
        Ok(())
    }

    fn register(
        &mut self,
        sequence: u64,
        call: &str,
        tool: &str,
        input: &Value,
        limit: usize,
    ) -> Result<()> {
        if self.issued.checked_add(1) != Some(sequence) || self.active.contains_key(call) {
            return Err(rejected("invocation_sequence_replay"));
        }
        // Even failed admission consumes the producer's sequence. Retrying it
        // must never mint a fresh execution budget after resource recovery.
        self.issued = sequence;
        if self.active.len() >= limit {
            return Err(rejected("invocation_limit"));
        }
        self.active.insert(
            call.into(),
            Invocation {
                sequence,
                digest: digest(tool, input)?,
                session: None,
                closing: false,
            },
        );
        Ok(())
    }

    pub(super) fn check(&mut self, value: &Value) -> Result<()> {
        let call = value["tool"]["call_id"]
            .as_str()
            .ok_or_else(|| rejected("invocation_identity"))?;
        let invocation = self
            .active
            .get_mut(call)
            .filter(|entry| !entry.closing)
            .ok_or_else(|| rejected("invocation_retired_or_unknown"))?;
        let session = value["session_id"]
            .as_str()
            .ok_or_else(|| rejected("invocation_identity"))?;
        let tool = value["tool"]["native_name"]
            .as_str()
            .ok_or_else(|| rejected("invocation_identity"))?;
        if invocation.digest != digest(tool, &value["tool"]["input"])?
            || invocation
                .session
                .as_ref()
                .is_some_and(|expected| expected != session)
        {
            return Err(rejected("invocation_input_mismatch"));
        }
        invocation.session = Some(session.into());
        Ok(())
    }
}

impl Runtime {
    fn invocation_ledger(&self, id: &str) -> Result<Arc<Mutex<State>>> {
        self.check_scope_owner(id)?;
        let instances = self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?;
        let instance = instances
            .get(id)
            .and_then(Option::as_ref)
            .filter(|item| !item.closing)
            .ok_or_else(|| rejected("unknown_instance"))?;
        Ok(Arc::clone(&instance.invocations))
    }

    pub(super) fn enable_invocations(&self, id: &str) -> Result<Value> {
        let state = self.invocation_ledger(id)?;
        let mut state = state.lock().map_err(|_| rejected("state_poisoned"))?;
        if state.ledger.is_some() {
            return Err(rejected("invocation_protocol_already_enabled"));
        }
        if state.legacy_used {
            return Err(rejected("invocation_protocol_requires_unused_binding"));
        }
        state.ledger = Some(Ledger::default());
        Ok(json!({"invocations_per_binding":self.limits.invocations_per_binding}))
    }

    pub(super) fn register_invocation(
        &self,
        id: &str,
        sequence: u64,
        call: &str,
        tool: &str,
        input: Value,
    ) -> Result<Value> {
        if call.is_empty()
            || call.len() > 512
            || tool.is_empty()
            || tool.len() > 512
            || serde_json::to_vec(&input)
                .map_err(|_| rejected("invocation_input_invalid"))?
                .len()
                > aw_provider::MAX_MESSAGE_BYTES
        {
            return Err(rejected("invocation_input_invalid"));
        }
        let ledger = self.invocation_ledger(id)?;
        let mut ledger = ledger.lock().map_err(|_| rejected("state_poisoned"))?;
        let ledger = ledger
            .ledger
            .as_mut()
            .ok_or_else(|| rejected("invocation_protocol_required"))?;
        if let Err(error) = ledger.register(
            sequence,
            call,
            tool,
            &input,
            self.limits.invocations_per_binding,
        ) {
            if matches!(&error, crate::Error::Rejected(code) if code == "invocation_limit") {
                return Err(self.overload("invocation_limit"));
            }
            return Err(error);
        }
        self.resources
            .peak_invocations
            .fetch_max(ledger.active.len(), std::sync::atomic::Ordering::Relaxed);
        Ok(json!({"registered":true}))
    }

    pub(super) fn finish_invocation(
        &self,
        id: &str,
        sequence: u64,
        call: &str,
        deadline: Instant,
    ) -> Result<Value> {
        let ledger = self.invocation_ledger(id)?;
        let (hooks, opening) = {
            let instances = self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            let instance = instances
                .get(id)
                .and_then(Option::as_ref)
                .ok_or_else(|| rejected("unknown_instance"))?;
            (Arc::clone(&instance.hooks), Arc::clone(&instance.hook_open))
        };
        // Closing excludes new opens before cancelling and waiting for workers.
        // Do not hold the opening lock while waiting for Provider completion.
        let session = {
            let _opening = hooks::lock_until(&opening, deadline, "invocation_close_timeout")?;
            let mut ledger = ledger.lock().map_err(|_| rejected("state_poisoned"))?;
            let ledger = ledger
                .ledger
                .as_mut()
                .ok_or_else(|| rejected("invocation_protocol_required"))?;
            let entry = ledger
                .active
                .get_mut(call)
                .filter(|entry| entry.sequence == sequence)
                .ok_or_else(|| rejected("invocation_retired_or_unknown"))?;
            entry.closing = true;
            entry.session.clone()
        };
        if let Some(session) = &session {
            for phase in ["tool.before", "tool.after"] {
                self.close_hook(id, phase, session, call, deadline)?;
            }
        }
        // Use the same ledger -> hooks lock order as close_hook. Retirement
        // removes both phases and the identity atomically against late closes.
        let mut ledger = ledger.lock().map_err(|_| rejected("state_poisoned"))?;
        let ledger = ledger
            .ledger
            .as_mut()
            .ok_or_else(|| rejected("invocation_protocol_required"))?;
        let mut hooks = hooks.lock().map_err(|_| rejected("state_poisoned"))?;
        if let Some(session) = session {
            for phase in ["tool.before", "tool.after"] {
                hooks.remove(&hooks::correlation(phase, &session, call)?);
            }
        }
        ledger.active.remove(call);
        Ok(json!({"retired":true}))
    }
}
