//! Retain one pre-tool rewrite until all fail-closed checks have completed.

use super::Runtime;
use crate::{rejected, Result};
use aw_host::{Event, Invocation, StepOutput};
use serde_json::{json, Value};
use std::sync::Mutex;

#[derive(Default)]
pub(super) struct State {
    input: Option<Value>,
    denied: bool,
}

impl Runtime {
    pub(super) fn adopt_input(
        &self,
        instance: &str,
        event: &Event<'_>,
        original: &Value,
        invocation: &mut Invocation,
        state: &Mutex<State>,
        last: bool,
    ) -> Result<Option<Value>> {
        if original["name"] != "tool.before" || !event.steps().next().is_some_and(|step| {
            matches!(&step.execution, aw_core::admission::StepExecution::Provider { effects, .. } if effects.iter().any(|effect| effect == "replace_input"))
        }) {
            return Ok(None);
        }
        let mut state = state
            .lock()
            .map_err(|_| rejected("input_replacement_state"))?;
        let adapter = original["agent"]["adapter"]
            .as_str()
            .ok_or_else(|| rejected("input_replacement_adapter"))?;
        match &invocation.result {
            Ok(StepOutput::Provider(outcome)) => {
                state.denied |= outcome.requests_block();
                if let Some(effect) = outcome.as_value()["effects"]
                    .as_array()
                    .and_then(|effects| {
                        effects
                            .iter()
                            .find(|effect| effect["type"] == "replace_input")
                    })
                {
                    let input = effect["input"].clone();
                    let native = aw_adapter::input::native(adapter, &original["native"], &input)
                        .map_err(|_| rejected("input_replacement_native"))?;
                    event
                        .input_view(input.clone(), native)
                        .map_err(|_| rejected("input_replacement_invalid"))?;
                    state.input = Some(input);
                }
            }
            Ok(StepOutput::Native(output)) => {
                use std::os::unix::process::ExitStatusExt;
                let value = json!({"status":"ok","kind":"native","native":{"stdout":output.stdout,"stderr":output.stderr,"exit_code":output.status.code(),"signal":output.status.signal()}});
                let response = aw_adapter::callback::response(
                    aw_adapter::select(adapter)
                        .map_err(|_| rejected("input_replacement_adapter"))?,
                    "tool.before",
                    "block",
                    value,
                    adapter,
                    &original["native"],
                    &mut std::io::sink(),
                );
                let directive = serde_json::from_slice::<Value>(&output.stdout).ok();
                let mutation = directive.as_ref().is_some_and(|value| {
                    value.get("aw_updated_input").is_some()
                        || value.get("params").is_some()
                        || value["action"] == "modify"
                        || value["decision"] == "modify"
                        || value["hookSpecificOutput"].get("updatedInput").is_some()
                });
                let native_failed =
                    mutation || (!output.status.success() && output.status.code() != Some(2));
                match response.and_then(|reply| {
                    aw_adapter::reload::adopt_step(adapter, "tool.before", "block", reply)
                }) {
                    Ok(reply) if !native_failed => {
                        state.denied |= aw_adapter::reload::denied(&reply)
                            || aw_adapter::callback::stops_before(adapter, Some(&reply), "block");
                    }
                    _ => {
                        state.denied = true;
                        invocation.result = Err(aw_host::Failure::Protocol(
                            aw_provider::Error::Invalid("invalid native pre-tool check"),
                        ));
                        invocation.failure_action = Some(aw_host::FailureAction::Block);
                    }
                }
            }
            Err(_) => state.denied = true,
            _ => {}
        }
        if !last || state.denied {
            return Ok(None);
        }
        let input = state.input.clone();
        if let Some(input) = &input {
            let ledger = self.invocation_ledger(instance)?;
            let mut state = ledger.lock().map_err(|_| rejected("state_poisoned"))?;
            if let Some(ledger) = state.ledger.as_mut() {
                ledger.rewrite(original, input)?;
            }
        }
        Ok(input)
    }
}
