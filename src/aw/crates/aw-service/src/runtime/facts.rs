//! Capture public metadata at runtime boundaries without optional source queries.

use super::Runtime;
use crate::{
    public_projection::{digest, registry},
    rejected, Result, RunOutcome,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Default)]
pub(super) struct NativeFacts {
    // None reserves an identity whose durable write has not completed, including failures.
    pub(super) claims: BTreeMap<(String, String, String), Option<String>>,
    pub(super) overflowed: bool,
}

pub(crate) fn timestamp() -> Result<String> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| rejected("audit_clock_invalid"))?;
    let seconds =
        libc::time_t::try_from(duration.as_secs()).map_err(|_| rejected("audit_clock_invalid"))?;
    let mut output = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: both pointers reference valid storage; gmtime_r initializes output on success.
    if unsafe { libc::gmtime_r(&seconds, output.as_mut_ptr()) }.is_null() {
        return Err(rejected("audit_clock_invalid"));
    }
    // SAFETY: gmtime_r returned a non-null pointer after initializing output.
    let time = unsafe { output.assume_init() };
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        time.tm_year + 1900,
        time.tm_mon + 1,
        time.tm_mday,
        time.tm_hour,
        time.tm_min,
        time.tm_sec,
        duration.subsec_millis()
    ))
}

fn native_id(value: &Value) -> Value {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))
        .map_or(Value::Null, |s| json!(s))
}

impl Runtime {
    fn fact(
        &self,
        key: &str,
        host: &aw_host::Host,
        instance: &str,
        name: &str,
        details: Value,
        native: Option<(&Value, &Value)>,
    ) -> Result<Option<Value>> {
        let (event, association) = native.unwrap_or((&Value::Null, &Value::Null));
        let session = native_id(&event["session_id"]);
        let turn = native_id(&event["turn_id"]);
        let call = native_id(&event["tool"]["call_id"]);
        let verified = association["status"] == "verified";
        let fields: Vec<_> = [("sessionId", &session), ("toolCallId", &call)]
            .into_iter()
            .filter(|(_, value)| verified && value.is_string())
            .map(|(field, _)| field)
            .collect();
        let associated = session.is_string() || turn.is_string() || call.is_string();
        let native_refs =
            aw_adapter::association::native_references(&host.capabilities().adapter, event);
        let gaps = if !associated {
            json!(["aw.instance_has_no_request"])
        } else if fields.is_empty() {
            json!(["aw.native_identity_unverified"])
        } else if turn.is_null() {
            json!(["aw.native_turn_unavailable"])
        } else {
            json!([])
        };
        let created = timestamp()?;
        let object = json!({"apiVersion":"runtime.aw/v1alpha1","kind":"ExecutionEvent",
            "metadata":{"uid":key,"createdAt":created,"producer":{"producerId":"aw-service","scopeId":self.audit.source_id,"generation":self.identity.generation}},
            "data":{"name":name,"subject":{"adapterId":host.capabilities().adapter,"bindingId":host.target(),"instanceId":instance,
                "configRevision":{"algorithm":"sha256","value":host.revision()},"sessionId":session,"turnId":turn,"toolCallId":call},
                "nativeRefs":native_refs,
                "association":{"status":if !fields.is_empty() {"verified"} else if associated {"unverified"} else {"unassociated"},
                    "basis":if !fields.is_empty() {"native_callback"} else {"none"},"verifiedFields":fields,
                    "gaps":gaps},
                "times":{"occurredAt":null,"receivedAt":created},"details":details}});
        // Older manual events may omit identities required by the public contract.
        // Preserve their private record and expose a projection gap instead of inventing IDs.
        if registry()?.validate_object(&object).is_err() {
            return Ok(None);
        }
        Ok(Some(object))
    }

    pub(super) fn hook_fact(
        &self,
        key: &str,
        host: &aw_host::Host,
        instance: &str,
        event: &Value,
        association: &Value,
    ) -> Result<Option<Value>> {
        let Some(name) = event["name"].as_str() else {
            return Ok(None);
        };
        let details = if name == "input.submit" {
            json!({"inputDigest":digest(&event["input"])?})
        } else {
            let tool = &event["tool"];
            let mut details = json!({"name":tool["name"],"nativeName":tool.get("native_name").unwrap_or(&tool["name"]),"inputDigest":digest(&tool["input"])?});
            if matches!(name, "tool.after" | "security.violation") {
                let Some(status) =
                    aw_adapter::completion::result_status(&host.capabilities().adapter, event)
                else {
                    return Ok(None);
                };
                details["resultDigest"] = digest(&tool["result"])?;
                details["resultStatus"] = json!(status);
            }
            details
        };
        self.fact(
            key,
            host,
            instance,
            name,
            details,
            Some((event, association)),
        )
    }

    pub(super) fn fact_gap(&self, instance: &str, reason: &str) -> Result<()> {
        let key = self.identifier()?;
        self.audit.claim(
            &key,
            json!({"kind":"runtime_fact","publicProjectionGap":reason,"instance_id":instance}),
        )?;
        let result = self
            .audit
            .append(&key, json!({"phase":"closed","status":"incomplete"}));
        self.audit.release(&key);
        result.map(|_| ())
    }

    pub(super) fn standalone_fact(&self, event: Value) -> Result<()> {
        let key = event["metadata"]["uid"]
            .as_str()
            .ok_or_else(|| rejected("public_event_invalid"))?;
        self.audit
            .claim(key, json!({"kind":"runtime_fact","executionEvent":event}))?;
        let result = self
            .audit
            .append(key, json!({"phase":"closed","status":"observed"}));
        self.audit.release(key);
        result.map(|_| ())
    }

    pub(in crate::runtime) fn report_run(
        &self,
        instance_id: &str,
        outcome: RunOutcome,
    ) -> Result<Value> {
        let observing = {
            let mut instances = self
                .instances
                .lock()
                .map_err(|_| rejected("state_poisoned"))?;
            let instance = instances
                .get_mut(instance_id)
                .and_then(Option::as_mut)
                .filter(|instance| !instance.closing)
                .ok_or_else(|| rejected("unknown_instance"))?;
            if let (Some(RunOutcome::Started { pid: previous }), RunOutcome::Started { pid }) =
                (&instance.run, &outcome)
            {
                // Native readiness can be retried after a lost reply, including
                // when optional observation is disabled and has no run snapshot.
                let owners = self.owners.lock().map_err(|_| rejected("state_poisoned"))?;
                if let Some(owner) = owners
                    .get(instance_id)
                    .filter(|owner| owner.independent && owner.native && owner.parent.is_none())
                {
                    if previous == pid
                        && owner.process.metadata()["pid"] == *pid
                        && owner.acknowledge_by.is_none()
                        && !owner.cancelled.load(std::sync::atomic::Ordering::Acquire)
                    {
                        owner.process.check()?;
                        if !self.audit.healthy() {
                            return Err(rejected("audit_unavailable"));
                        }
                        return Ok(json!({"recorded":true}));
                    }
                }
            }
            let valid = match &outcome {
                RunOutcome::Started { pid } => instance.run.is_none() && *pid > 0,
                RunOutcome::Exited { code, signal } => {
                    matches!(instance.run, Some(RunOutcome::Started { .. }))
                        && matches!((code, signal), (Some(0..=255), None) | (None, Some(1..=64)))
                }
                RunOutcome::Failed => {
                    instance.run.is_none()
                        || matches!(instance.run, Some(RunOutcome::Started { .. }))
                }
            };
            if !valid {
                return Err(rejected("run_transition_invalid"));
            }
            let (name, details) = match &outcome {
                RunOutcome::Started { pid } => ("instance.started", json!({"pid":pid})),
                RunOutcome::Exited { code, signal } => (
                    "instance.ended",
                    json!({"outcome":"exited","exitCode":code,"signal":signal,"reasonCode":null}),
                ),
                RunOutcome::Failed => (
                    "instance.ended",
                    json!({"outcome":"failed","exitCode":null,"signal":null,"reasonCode":"aw.launch_failed"}),
                ),
            };
            let key = self.identifier()?;
            let event = self
                .fact(&key, &instance.host, instance_id, name, details, None)?
                .ok_or_else(|| rejected("public_event_invalid"))?;
            self.standalone_fact(event)?;
            instance.run = Some(outcome.clone());
            instance.observation.is_some()
        };
        if observing {
            self.report_observed_run(instance_id, outcome)?;
        }
        Ok(json!({"recorded":true}))
    }
}

#[cfg(test)]
mod tests;
