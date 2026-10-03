//! One shared event budget and once-only step claims; scheduling belongs to the Adapter.

use crate::{
    check, identifier, positive, Error, Exchange, Failure, FailureAction, Host, Invocation, Method,
};
use aw_provider::{admission::AdmittedStep, Reply, MAX_DEPTH, MAX_MESSAGE_BYTES, VERSION};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
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
    name: String,
    id: String,
    deadline: Instant,
    cancelled: &'a AtomicBool,
    claimed: Mutex<BTreeSet<String>>,
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
    pub fn event<'a>(
        &'a self,
        value: Value,
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
            name,
            id: identifier()?,
            deadline,
            cancelled,
            claimed: Mutex::new(BTreeSet::new()),
        })
    }
}

impl Event<'_> {
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
            Method::Invoke,
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
        let request_id = record.request_id.clone();
        let Exchange { record, result } = self
            .host
            .transport(provider, self.deadline, self.cancelled)
            .exchange(record, |budget_ms| {
                self.host.protocol.bind_invocation(json!({
                    "api_version": VERSION, "method": "invoke", "request_id": request_id,
                    "operation": step.operation, "config_revision": self.host.revision,
                    "budget_ms": budget_ms, "allowed_effects": step.effects,
                    "config": provider.config, "event": self.value,
                }))
            });
        let result = result.and_then(|reply| match reply {
            Reply::Invocation(outcome) => Ok(outcome),
            _ => Err(Failure::Protocol(aw_provider::Error::Invalid(
                "unexpected invocation reply type",
            ))),
        });
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
