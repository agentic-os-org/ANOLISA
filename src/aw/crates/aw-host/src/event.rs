//! One shared event budget and once-only step claims; scheduling belongs to the Adapter.

use crate::{
    check, identifier, positive, Error, Exchange, Failure, FailureAction, Host, Invocation, Method,
    StepOutput,
};
use aw_provider::{
    admission::{AdmittedStep, StepExecution},
    Reply, MAX_DEPTH, MAX_MESSAGE_BYTES, VERSION,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    sync::{atomic::AtomicBool, Mutex},
    time::{Duration, Instant},
};

/// A local event tied to one prepared Host and one absolute deadline.
///
/// Invoke configured steps serially or concurrently according to native Adapter
/// semantics. Every step may be claimed once; failed attempts are not retried.
/// The caller must collect all required results before dispatching a native tool.
pub struct Event<'a> {
    host: &'a Host,
    value: Value,
    native_input: Option<Vec<u8>>,
    name: String,
    id: String,
    deadline: Instant,
    cancelled: &'a AtomicBool,
    claimed: Mutex<BTreeSet<String>>,
    callback: Mutex<Option<(Value, Vec<u8>)>>,
}

impl Host {
    /// Bind a normalized event to this Host, capping its budget by the caller deadline.
    ///
    /// The budget starts on entry, including local context checks. Supply the
    /// original callback deadline to count work performed before this call. Input
    /// is a locally constructed JSON value; invocation schema validation occurs
    /// before any step process starts. Adapter and configured binding must match.
    /// Instance, session and tool IDs remain native data, not authenticated identity.
    ///
    /// # Errors
    /// Rejects expired/cancelled events, excessive input, mismatched Agent binding,
    /// disabled or unsupported events and invalid local budget/context data.
    /// Native steps require [`Self::hook_event`] with original callback input.
    pub fn event<'a>(
        &'a self,
        value: Value,
        deadline: Instant,
        cancelled: &'a AtomicBool,
    ) -> Result<Event<'a>, Error> {
        self.open(value, None, deadline, cancelled)
    }

    /// Bind the exact native callback bytes and normalized event to one shared budget.
    ///
    /// Native steps receive these bytes unchanged. Structured steps receive the
    /// normalized event and retain their regular protocol validation.
    ///
    /// # Errors
    /// Returns the same context/deadline errors as [`Self::event`], or an input limit.
    pub fn hook_event<'a>(
        &'a self,
        value: Value,
        native_input: Vec<u8>,
        deadline: Instant,
        cancelled: &'a AtomicBool,
    ) -> Result<Event<'a>, Error> {
        if native_input.len() > MAX_MESSAGE_BYTES {
            return Err(Error::Invalid("native input size limit"));
        }
        self.open(value, Some(native_input), deadline, cancelled)
    }

    fn open<'a>(
        &'a self,
        value: Value,
        native_input: Option<Vec<u8>>,
        deadline: Instant,
        cancelled: &'a AtomicBool,
    ) -> Result<Event<'a>, Error> {
        let started = Instant::now();
        check(deadline, cancelled)?;
        if value["agent"]["adapter"] != self.capabilities.adapter
            || value["agent"]["binding_id"] != self.target
        {
            return Err(Error::Invalid(
                "event Agent does not match prepared binding",
            ));
        }
        let name = value["name"]
            .as_str()
            .ok_or(Error::Invalid("missing event name"))?
            .to_owned();
        let spec = &self.configuration.as_value()["spec"];
        let configured = &spec["events"][&name];
        if configured["enabled"] != true || !matches!(name.as_str(), "tool.before" | "tool.after") {
            return Err(Error::Invalid("event was not enabled and admitted"));
        }
        if native_input.is_none()
            && self
                .steps
                .iter()
                .any(|step| step.event == name && matches!(step.execution, StepExecution::Native))
        {
            return Err(Error::Invalid(
                "native steps require original callback input",
            ));
        }
        let budget = configured
            .get("budget_ms")
            .unwrap_or(&spec["execution"]["default_event_budget_ms"]);
        let deadline = deadline.min(started + Duration::from_millis(positive(budget)?));
        // Check local Value depth before cloning or recursively encoding event data.
        let mut pending = vec![(&value, 0)];
        while let Some((value, depth)) = pending.pop() {
            if depth > MAX_DEPTH {
                return Err(Error::Invalid("event nesting limit"));
            }
            match value {
                Value::Object(values) => pending.extend(values.values().map(|v| (v, depth + 1))),
                Value::Array(values) => pending.extend(values.iter().map(|v| (v, depth + 1))),
                _ => {}
            }
        }
        if crate::encode(&value)?.len() > MAX_MESSAGE_BYTES {
            return Err(Error::Invalid("event size limit"));
        }
        check(deadline, cancelled)?;
        Ok(Event {
            host: self,
            value,
            native_input,
            name,
            id: identifier()?,
            deadline,
            cancelled,
            claimed: Mutex::new(BTreeSet::new()),
            callback: Mutex::new(None),
        })
    }
}

impl Event<'_> {
    /// Select a later native completion view without renewing claims or budget.
    ///
    /// The Adapter must bind both views to the same original result. Only native
    /// completion metadata, its result view and its projection may differ.
    ///
    /// # Errors
    /// Rejects non-native/after events, changed identity or input, invalid bounds,
    /// an expired budget or a poisoned callback lock.
    pub fn completion_view(&self, value: Value, input: Vec<u8>) -> Result<(), Error> {
        if self.name != "tool.after" || self.native_input.is_none() {
            return Err(Error::Invalid(
                "completion view requires a native after event",
            ));
        }
        let candidate = self
            .host
            .hook_event(value, input, self.deadline, self.cancelled)?;
        let mut bound = candidate.value.clone();
        bound["native"] = self.value["native"].clone();
        for field in ["result", "result_projection"] {
            if let Some(original) = self.value["tool"].get(field) {
                bound["tool"][field] = original.clone();
            } else if let Some(tool) = bound["tool"].as_object_mut() {
                tool.remove(field);
            }
        }
        if bound != self.value {
            return Err(Error::Invalid(
                "completion view changed event identity or input",
            ));
        }
        *self
            .callback
            .lock()
            .map_err(|_| Error::Invalid("callback view poisoned"))? = Some((
            candidate.value,
            candidate
                .native_input
                .ok_or(Error::Invalid("missing native completion input"))?,
        ));
        Ok(())
    }

    /// Process-local correlation ID shared by all attempts within this event.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Absolute event deadline shared by serial and parallel calls.
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Steps admitted for this event, retaining their configured order.
    pub fn steps(&self) -> impl Iterator<Item = &AdmittedStep> {
        self.host
            .steps
            .iter()
            .filter(|step| step.event == self.name)
    }

    /// Execute one retained step, preserving failure details and its `on_error` action.
    ///
    /// No implicit shell, scheduling, retry or native action is performed. An empty
    /// effect list adds no restriction and never grants native permission. Cleanup
    /// failures remain transport failures even when `on_error` requests blocking.
    ///
    /// # Errors
    /// Returns a selection error for unknown/already claimed steps. All call failures,
    /// including pre-spawn cancellation/deadline, are recorded inside the invocation.
    pub fn invoke(&self, step_id: &str) -> Result<Invocation, Error> {
        self.invoke_with_environment(step_id, None)
    }

    /// Execute a step with the complete environment observed by its native callback.
    ///
    /// Only native-hook steps use this snapshot, replacing their bound environment
    /// without restoring variables removed by the Agent. Structured Providers keep
    /// their prepared context. Environment bytes are not event data or call metadata.
    /// This method shares the original event deadline and once-only step claims.
    ///
    /// # Errors
    /// Returns the same selection errors as [`Self::invoke`]. Invalid native
    /// environments produce a recorded [`Failure::NativeEnvironment`] and consume
    /// the step claim, preserving its configured failure action.
    pub fn invoke_with_native_environment(
        &self,
        step_id: &str,
        environment: &BTreeMap<OsString, OsString>,
    ) -> Result<Invocation, Error> {
        self.invoke_with_environment(step_id, Some(environment))
    }

    fn invoke_with_environment(
        &self,
        step_id: &str,
        environment: Option<&BTreeMap<OsString, OsString>>,
    ) -> Result<Invocation, Error> {
        let step = self
            .steps()
            .find(|step| step.step_id == step_id)
            .ok_or(Error::Invalid("step was not admitted for this event"))?;
        let provider = self
            .host
            .providers
            .get(&step.provider)
            .ok_or(Error::Invalid("prepared Provider is missing"))?;
        let record = self.host.record(
            &step.provider,
            if matches!(step.execution, StepExecution::Native) {
                Method::NativeHook
            } else {
                Method::Invoke
            },
            Some(&self.id),
            Some(&step.step_id),
        )?;
        if !self
            .claimed
            .lock()
            .map_err(|_| Error::Invalid("event step claim poisoned"))?
            .insert(step.step_id.clone())
        {
            return Err(Error::Invalid("step was already claimed for this event"));
        }
        let transport = self.host.transport(provider, self.deadline, self.cancelled);
        let callback = self
            .callback
            .lock()
            .map_err(|_| Error::Invalid("callback view poisoned"))?
            .clone();
        let (record, result) = match &step.execution {
            StepExecution::Native => {
                let input = callback
                    .as_ref()
                    .map(|(_, input)| input)
                    .or(self.native_input.as_ref())
                    .ok_or(Error::Invalid("native callback input is missing"))?;
                transport.native(record, input, environment)
            }
            StepExecution::Provider { effects, .. } | StepExecution::Hook { effects, .. } => {
                let request_id = record.request_id.clone();
                let operation = if let StepExecution::Provider { operation, .. } = &step.execution {
                    operation.as_str()
                } else {
                    "invoke"
                };
                // Bind one immutable event across callbacks; projections belong
                // only to the final replacement Provider, not earlier observers.
                let mut event = callback
                    .as_ref()
                    .map(|(value, _)| value)
                    .unwrap_or(&self.value)
                    .clone();
                if !step.execution.replaces_result() {
                    if let Some(tool) = event["tool"].as_object_mut() {
                        tool.remove("result_projection");
                    }
                }
                let build = |budget_ms| {
                    let value = json!({
                        "api_version": VERSION, "method": "invoke", "request_id": request_id,
                        "operation": operation, "config_revision": self.host.revision,
                        "budget_ms": budget_ms, "allowed_effects": effects,
                        "config": provider.config, "event": event,
                    });
                    if matches!(step.execution, StepExecution::Hook { .. }) {
                        self.host.protocol.bind_hook_invocation(value)
                    } else {
                        self.host.protocol.bind_invocation(value)
                    }
                };
                let Exchange { record, result } =
                    if let StepExecution::Hook { reason_code, .. } = &step.execution {
                        transport.hook(record, build, reason_code.as_deref())
                    } else {
                        transport.exchange(record, build)
                    };
                let result = result.and_then(|reply| match reply {
                    Reply::Invocation(outcome) => Ok(StepOutput::Provider(outcome)),
                    _ => Err(Failure::Protocol(aw_provider::Error::Invalid(
                        "unexpected invocation reply type",
                    ))),
                });
                (record, result)
            }
        };
        let failure_action = result.is_err().then_some(if step.on_error == "block" {
            FailureAction::Block
        } else {
            FailureAction::Report
        });
        Ok(Invocation {
            record,
            result,
            failure_action,
        })
    }
}
