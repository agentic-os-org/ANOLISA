//! Preserve lifecycle reasons and correlate scopes with service-owned identities.

use super::{Instance, Runtime};
use crate::{rejected, EventHandle, Result};
use aw_contracts::canonical;
use serde_json::{json, Value};

pub(super) fn verify(instance: &Instance, id: &str, value: &Value, input: &[u8]) -> Result<()> {
    let capabilities = instance.host.capabilities();
    let name = value["name"]
        .as_str()
        .ok_or_else(|| rejected("event_invalid"))?;
    let native = crate::ipc::decode(input).map_err(|_| rejected("native_event_invalid"))?;
    let context = aw_adapter::NativeContext {
        target: instance.host.target().into(),
        instance_id: id.into(),
        cwd: instance.context.cwd.clone(),
    };
    if !capabilities.events.contains_key(name)
        || aw_adapter::select(&capabilities.adapter)
            .and_then(|adapter| adapter.normalize(&context, name, &native))
            .map_err(|_| rejected("native_event_invalid"))?
            != *value
    {
        return Err(rejected("native_lifecycle_mismatch"));
    }
    Ok(())
}

impl Runtime {
    pub(super) fn reopen_stop(&self, key: &str, value: &Value) -> Result<Value> {
        if !replays_stop(value) {
            return Err(rejected("native_event_closed"));
        }
        let journal = self.audit.read(key)?;
        let plan = &journal["records"][0]["record"]["plan"];
        if *plan
            .get("stop_input_digest")
            .ok_or_else(|| rejected("native_event_closed"))?
            != fingerprint(value)?
        {
            return Err(rejected("native_correlation_mismatch"));
        }
        if journal["terminal"] != true {
            return Err(rejected("native_event_closed"));
        }
        Ok(plan["stop_handle"].clone())
    }

    pub(super) fn record_stop_reply(
        &self,
        key: &str,
        event: &aw_host::Event<'_>,
        original: &Value,
        step: &str,
        record: &mut Value,
        result: &Value,
    ) -> Result<()> {
        if !replays_stop(original) {
            return Ok(());
        }
        let on_error = &event
            .steps()
            .find(|candidate| candidate.step_id == step)
            .ok_or_else(|| rejected("step_unknown_or_claimed"))?
            .on_error;
        let Ok(native) = aw_adapter::callback::response(
            aw_adapter::select("claude-sdk").map_err(|_| rejected("sdk_adapter"))?,
            "turn.stop",
            on_error,
            result.clone(),
            "claude-sdk",
            &original["native"],
            &mut std::io::sink(),
        ) else {
            return Ok(());
        };
        let (code, reason_digest) = match native.exit {
            aw_adapter::NativeExit::Code(code @ (0 | 2)) if native.stdout.is_empty() => {
                (code, None)
            }
            aw_adapter::NativeExit::Code(2)
                if serde_json::from_slice::<String>(&native.stdout)
                    .is_ok_and(|reason| !reason.is_empty()) =>
            {
                (
                    2,
                    Some(self.audit.store_stop_reason(key, step, &native.stdout)?),
                )
            }
            _ => return Ok(()),
        };
        record["stop_reply"] = json!({"code":code,"reason_digest":reason_digest});
        Ok(())
    }

    pub(super) fn replay_stop_step(
        &self,
        key: &str,
        id: &str,
        step: &str,
    ) -> Result<Option<Value>> {
        let supports_stop = self
            .instances
            .lock()
            .map_err(|_| rejected("state_poisoned"))?
            .get(id)
            .and_then(Option::as_ref)
            .is_some_and(|instance| {
                !instance.closing
                    && instance.host.capabilities().adapter == "claude-sdk"
                    && instance
                        .host
                        .capabilities()
                        .events
                        .contains_key("turn.stop")
            });
        if !supports_stop {
            return Ok(None);
        }
        let journal = match self.audit.read(key) {
            Ok(journal) => journal,
            Err(crate::Error::Rejected(code))
                if matches!(code.as_str(), "unknown_audit_key" | "invalid_audit_key") =>
            {
                return Ok(None)
            }
            Err(error) => return Err(error),
        };
        let plan = &journal["records"][0]["record"]["plan"];
        let Some(handle) = plan.get("stop_handle") else {
            return Ok(None);
        };
        if journal["terminal"] != true
            || handle["event_id"] != key
            || handle["instance_id"] != id
            || plan["identity"]["generation"] != self.identity.generation
        {
            return Ok(None);
        }
        let records = journal["records"]
            .as_array()
            .ok_or_else(|| rejected("invalid_stop_receipt"))?;
        let Some(reply) = records
            .iter()
            .map(|envelope| &envelope["record"])
            .find_map(|record| {
                (record["phase"] == "completed" && record["call"]["step_id"] == step)
                    .then(|| record.get("stop_reply"))
                    .flatten()
            })
        else {
            return Ok(None);
        };
        let (code, stdout) = match (reply["code"].as_i64(), &reply["reason_digest"]) {
            (Some(code @ (0 | 2)), Value::Null) => (code, Vec::new()),
            (Some(2), Value::String(digest)) => {
                (2, self.audit.read_stop_reason(key, step, digest)?)
            }
            _ => return Err(rejected("invalid_stop_receipt")),
        };
        self.check_execution_admission(id, None)?;
        Ok(Some(
            json!({"kind":"native","status":"ok","stop_replay":true,"native":{
                "exit_code":code,"stdout":stdout,"stderr":[],"signal":null
            }}),
        ))
    }

    pub(super) fn lifecycle_key(&self, id: &str, correlation: &str) -> Result<String> {
        super::hooks::correlation(&self.identity.generation, id, correlation)
    }

    pub(super) fn close_lifecycle_receipt(&self, id: &str, key: &str) -> Result<()> {
        let plan = json!({"kind":"lifecycle_callback_retirement",
            "identity":self.identity,"instance_id":id});
        match self.audit.claim_lifecycle(key, plan) {
            Ok(_) => {
                let result = self
                    .audit
                    .append(key, json!({"phase":"closed","status":"cancelled"}));
                self.audit.release(key);
                result.map(|_| ())
            }
            Err(crate::Error::Rejected(code)) if code == "native_event_closed" => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub(super) fn lifecycle_metadata(
        &self,
        instance: &Instance,
        id: &str,
        value: &Value,
    ) -> Result<Value> {
        let name = &value["name"];
        let mut metadata = json!({"name":name, "session_id":value["session_id"]});
        if *name == "turn.stop" {
            metadata["turn_id"] = value["turn_id"].clone();
            if let Some(attempt) = value["native"].get("attempt_id") {
                metadata["attempt_id"] = attempt.clone();
            }
        }
        if let Some(epoch) = value["native"].get("session_epoch") {
            metadata["session_epoch"] = epoch.clone();
        }
        if *name != "session.start" {
            metadata["reason"] = value["native"]["reason"].clone();
        }
        let owners = self.owners.lock().map_err(|_| rejected("state_poisoned"))?;
        if let Some(owner) = owners.get(id) {
            metadata["process"] = owner.process.metadata();
            if let Some(parent) = &owner.parent {
                metadata["task_id"] = json!(id);
                metadata["parent_instance_id"] = json!(parent);
            }
        } else if let Some(crate::RunOutcome::Started { pid }) = instance.run {
            metadata["process"] = json!({"pid":pid});
        }
        Ok(metadata)
    }
}

pub(super) fn replays_stop(value: &Value) -> bool {
    value["name"] == "turn.stop"
        && value["agent"]["adapter"] == "claude-sdk"
        && value["native"]["delivery_id"].is_string()
}

fn fingerprint(value: &Value) -> Result<String> {
    serde_json::to_vec(value)
        .map(|bytes| canonical::digest(&bytes))
        .map_err(|_| rejected("event_encoding"))
}

pub(super) fn mark_stop_plan(
    candidate: &Value,
    handle: &EventHandle,
    plan: &mut Value,
) -> Result<()> {
    if replays_stop(candidate) {
        plan["stop_input_digest"] = json!(fingerprint(candidate)?);
        plan["stop_handle"] =
            serde_json::to_value(handle).map_err(|_| rejected("event_encoding"))?;
    }
    Ok(())
}
