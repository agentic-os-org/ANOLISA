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

fn sdk_trusted_directive(event: &str, stdout: &[u8]) -> Result<Option<Value>> {
    if stdout.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let parsed: Value = serde_json::from_slice(stdout).map_err(|_| rejected("sdk_step_failed"))?;
    let object = parsed
        .as_object()
        .ok_or_else(|| rejected("sdk_step_failed"))?;
    if object.len() != 1 {
        return Err(rejected("sdk_step_failed"));
    }
    if event == "tool.before" {
        if let Some(input) = object.get("aw_updated_input").filter(|v| v.is_object()) {
            return Ok(Some(json!({"type": "replace_input", "input": input})));
        }
    }
    if event == "tool.after" {
        if object.get("aw_withheld") == Some(&Value::Bool(true)) {
            return Ok(Some(json!({"type": "withhold_result"})));
        }
        if let Some(result) = object.get("aw_replacement") {
            return Ok(Some(json!({"type": "replace_result", "result": result})));
        }
    }
    Err(rejected("sdk_step_failed"))
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
            let call = super::hooks::callback_id(&normalized)?;
            self.close_hook(id, event, session, &call, deadline)?;
            return Ok(json!({"code":0,"stdout":[],"stderr":[]}));
        }
        let mut diagnostics = Vec::new();
        let mut directive = None;
        let output = if step == aw_adapter::OBSERVE_STEP {
            self.observe_hook(id, normalized, input, deadline)?;
            adapter.reply(false)
        } else {
            let handle: EventHandle =
                serde_json::from_value(self.open_hook(id.into(), normalized, input, deadline)?)
                    .map_err(|_| rejected("sdk_event_invalid"))?;
            let invoked = self.invoke_event_step(
                &handle.event_id,
                id,
                step.into(),
                Some(environment),
                deadline,
            );
            let replayed = invoked
                .as_ref()
                .is_ok_and(|value| value["stop_replay"] == true);
            let native_only = invoked.as_ref().is_ok_and(|value| {
                value["kind"] == "native" && value.get("input_replacement").is_none()
            });
            let result = invoked.and_then(|value| {
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
            if adapter_id == "sdk" && !native_only {
                if let Ok(ref out) = result {
                    if matches!(out.exit, NativeExit::Code(0)) {
                        directive = sdk_trusted_directive(event, &out.stdout)?;
                    }
                }
            }
            let early_stop_after = adapter_id == "sdk"
                && event == "tool.after"
                && (matches!(
                    result.as_ref().map(|out| &out.exit),
                    Ok(NativeExit::Code(2))
                ) || directive
                    .as_ref()
                    .is_some_and(|item| item["type"] == "withhold_result"));
            if !replayed
                && handle.steps.last().map(String::as_str) != Some(step)
                && (early_stop_after
                    || (matches!(event, "input.submit" | "tool.before" | "turn.stop")
                        && aw_adapter::callback::stops_before(
                            &adapter_id,
                            result.as_ref().ok(),
                            &on_error,
                        )))
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
        let mut reply = json!({"code":code,"stdout":output.stdout,"stderr":diagnostics});
        if let Some(directive) = directive {
            reply["directive"] = directive;
        }
        Ok(reply)
    }
}
