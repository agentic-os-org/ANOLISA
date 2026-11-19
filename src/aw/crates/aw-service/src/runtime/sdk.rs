//! Direct SDK callbacks reuse Adapter normalization and the once-only Hook ledger.

use super::Runtime;
use crate::{rejected, Availability, EventHandle, Result};
use aw_adapter::{NativeContext, NativeExit};
use serde_json::{json, Value};
use std::{sync::Arc, time::Instant};

pub(super) fn validate_availability(
    configuration: &aw_config::Configuration,
    target: &str,
    mode: Option<Availability>,
) -> Result<()> {
    if mode == Some(Availability::NativeFallback)
        && aw_config::requirements::requires_service(configuration, target)?
    {
        return Err(rejected("sdk_required_policy_conflict"));
    }
    Ok(())
}

impl Runtime {
    #[allow(clippy::too_many_arguments)] // One wire callback; all policy fields come from its Scope.
    pub(super) fn sdk_callback(
        self: &Arc<Self>,
        id: &str,
        event: &str,
        step: &str,
        native: Value,
        close: bool,
        deadline: Instant,
    ) -> Result<Value> {
        let (adapter_id, context, environment, on_error, replaces_result) = {
            let instances = self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            let instance = instances
                .get(id)
                .and_then(Option::as_ref)
                .filter(|instance| !instance.closing)
                .ok_or_else(|| rejected("scope_unavailable"))?;
            // Bridge expiry fences execution; the acknowledged owner can still drain events.
            if close {
                self.check_scope_cleanup(id)?;
            } else {
                self.check_scope_owner(id)?;
            }
            let owners = self.owners.lock().map_err(|_| rejected("state_poisoned"))?;
            if !owners
                .get(id)
                .is_some_and(|owner| owner.sdk.is_some() && owner.parent.is_some())
            {
                return Err(rejected("sdk_scope_required"));
            }
            let routes = instance
                .routes
                .as_ref()
                .ok_or_else(|| rejected("native_policy_unsupported"))?;
            let route = routes
                .event(event)
                .ok_or_else(|| rejected("sdk_event_unavailable"))?;
            let on_error = route
                .step_on_error(step)
                .ok_or_else(|| rejected("sdk_step_unavailable"))?
                .to_owned();
            (
                instance.host.capabilities().adapter.clone(),
                NativeContext {
                    target: instance.host.target().into(),
                    instance_id: id.into(),
                    cwd: instance.context.cwd.clone(),
                },
                instance.context.environment.clone(),
                on_error,
                route.replacement_step().is_some(),
            )
        };
        let adapter = aw_adapter::select(&adapter_id).map_err(|_| rejected("sdk_adapter"))?;
        let input = serde_json::to_vec(&native).map_err(|_| rejected("sdk_input_invalid"))?;
        let (normalized, native, input) = aw_adapter::callback::normalize(
            adapter,
            &adapter_id,
            &context,
            event,
            input,
            replaces_result,
        )
        .map_err(|_| rejected("sdk_input_invalid"))?;
        if close {
            let session = normalized["session_id"]
                .as_str()
                .ok_or_else(|| rejected("sdk_input_invalid"))?;
            let call = if event == "input.submit" {
                &normalized["turn_id"]
            } else {
                &normalized["tool"]["call_id"]
            };
            self.close_hook(
                id,
                event,
                session,
                call.as_str().ok_or_else(|| rejected("sdk_input_invalid"))?,
                deadline,
            )?;
            return Ok(json!({"code":0,"stdout":[],"stderr":[]}));
        }
        let mut diagnostics = Vec::new();
        let output = if step == aw_adapter::OBSERVE_STEP {
            self.observe_hook(id, normalized, input, deadline)?;
            adapter.reply(false)
        } else {
            let handle: EventHandle =
                serde_json::from_value(self.open_hook(id.into(), normalized, input, deadline)?)
                    .map_err(|_| rejected("sdk_event_invalid"))?;
            let result = self
                .invoke_event_step(
                    &handle.event_id,
                    id,
                    step.into(),
                    Some(environment),
                    deadline,
                )
                .and_then(|value| {
                    aw_adapter::callback::response(
                        adapter,
                        event,
                        &on_error,
                        value,
                        &adapter_id,
                        &native,
                        &mut diagnostics,
                    )
                    .map_err(|_| rejected("sdk_step_failed"))
                });
            if matches!(event, "input.submit" | "tool.before")
                && handle.steps.last().map(String::as_str) != Some(step)
                && aw_adapter::callback::stops_before(&adapter_id, result.as_ref().ok(), &on_error)
            {
                // Retain the original denial/error if cleanup is uncertain.
                let _ = self.close_event(&handle.event_id, id, deadline);
            }
            result?
        };
        diagnostics.extend_from_slice(&output.stderr);
        let NativeExit::Code(code) = output.exit else {
            return Err(rejected("sdk_callback_signal"));
        };
        Ok(json!({"code":code,"stdout":output.stdout,"stderr":diagnostics}))
    }
}
