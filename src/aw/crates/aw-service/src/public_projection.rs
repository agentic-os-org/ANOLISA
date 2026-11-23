//! Replay public metadata from validated, retained journal positions.
//!
//! Callers authenticate storage and validate journal envelopes before projection.
//! Private records and legacy chains remain distinguishable from public facts.

use crate::{rejected, Error};
use aw_contracts::{canonical, objects, Registry};
use serde_json::{json, Value};
use std::{path::Path, sync::OnceLock};

type Result<T> = std::result::Result<T, Error>;

pub(crate) fn registry() -> Result<&'static Registry> {
    static REGISTRY: OnceLock<std::result::Result<Registry, String>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| Registry::new().map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|_| rejected("public_contract_unavailable"))
}

/// Reads the retained source scope from a trusted service audit directory.
///
/// # Errors
/// Rejects missing, malformed, linked, foreign-owned or non-private descriptors.
/// A directory FD path may be supplied to bind this read to an already opened directory.
pub fn source_identity(directory: &Path) -> Result<String> {
    crate::audit::source::read(directory)
}

/// Extracts the initial public event; `None` identifies a legacy/private chain.
///
/// # Errors
/// Rejects a malformed initial envelope or an invalid retained public object.
pub fn execution_event(initial: &Value) -> Result<Option<Value>> {
    if initial["sequence"] != 0 || !initial["record"]["plan"].is_object() {
        return Err(rejected("public_initial_invalid"));
    }
    let Some(event) = initial["record"]["plan"].get("executionEvent") else {
        return Ok(None);
    };
    registry()?
        .validate_object(event)
        .map_err(|_| rejected("public_event_invalid"))?;
    if event["kind"] != "ExecutionEvent" || event["metadata"]["uid"] != initial["event_key"] {
        return Err(rejected("public_event_invalid"));
    }
    Ok(Some(event.clone()))
}

/// Projects one verified envelope using its retained initial event.
///
/// `None` identifies private intermediate metadata, such as step dispatch.
/// Provider completion records describe policy output; they do not attest native adoption.
///
/// # Errors
/// Rejects inconsistent source identity, malformed public metadata or invalid objects.
pub fn project_record(envelope: &Value, event: &Value) -> Result<Option<Value>> {
    let registry = registry()?;
    registry
        .validate_object(event)
        .map_err(|_| rejected("public_event_invalid"))?;
    let sequence = envelope["sequence"]
        .as_u64()
        .ok_or_else(|| rejected("public_record_invalid"))?;
    let key = envelope["event_key"]
        .as_str()
        .ok_or_else(|| rejected("public_record_invalid"))?;
    if event["metadata"]["uid"] != key {
        return Err(rejected("public_event_mismatch"));
    }
    let record = &envelope["record"];
    let (kind, details) = if sequence == 0 {
        if execution_event(envelope)?.as_ref() != Some(event) {
            return Err(rejected("public_event_mismatch"));
        }
        ("event.observed", json!({}))
    } else if record["phase"] == "completed" && record["kind"] == "provider" {
        let call = &record["call"];
        let failed = record["status"] != "ok";
        (
            "provider.completed",
            json!({"providerId":call["provider"],"stepId":call["step_id"],"requestId":call["request_id"],
            "status":if failed {"failed"} else {"ok"},"policyBlock":if failed {Value::Null} else {record["policy_block"].clone()},
            "failureAction":if failed {record["failure_action"].clone()} else {Value::Null},
            "failureCode":if failed {json!(format!("aw.{}",record["failure"].as_str().ok_or_else(|| rejected("public_record_invalid"))?))} else {Value::Null}}),
        )
    } else if record["phase"] == "closed" {
        let details = match record["status"].as_str() {
            Some("observed") => json!({"outcome":"completed","reasonCode":null}),
            Some("worker_failed") => json!({"outcome":"failed","reasonCode":"aw.worker_failed"}),
            Some("cancelled" | "stopped" | "expired") => {
                json!({"outcome":"cancelled","reasonCode":format!("aw.{}",record["status"].as_str().unwrap_or("cancelled"))})
            }
            _ => json!({"outcome":"incomplete","reasonCode":"aw.native_adoption_unconfirmed"}),
        };
        ("event.closed", details)
    } else {
        return Ok(None);
    };
    let created = if sequence == 0 {
        &event["metadata"]["createdAt"]
    } else {
        &record["createdAt"]
    };
    let producer = &event["metadata"]["producer"];
    let name = &event["data"]["name"];
    let subject = &event["data"]["subject"];
    let level = if subject["toolCallId"].is_string() {
        "tool"
    } else if subject["turnId"].is_string() {
        "turn"
    } else {
        "instance"
    };
    let value = json!({"apiVersion":"audit.aw/v1alpha1","kind":"AuditRecord",
        "metadata":{"uid":objects::audit_uid(producer,key,sequence).map_err(|_| rejected("public_record_invalid"))?,"createdAt":created,"producer":producer},
        "data":{"kind":kind,"eventRef":objects::reference(registry,event).map_err(|_| rejected("public_event_invalid"))?,"eventName":name,"subject":subject,
        "chain":{"id":key,"sequence":sequence,"sourceDigest":{"algorithm":"sha256","value":envelope["digest"]}},
        "coverage":{"status":"unknown","scope":{"level":level,"events":[name]},"interval":{"start":null,"end":null},"observedAt":created,"gaps":["aw.coverage_unmeasured"],"truncated":false},"details":details}});
    objects::validate_audit_event(registry, &value, event)
        .map_err(|_| rejected("public_record_invalid"))?;
    Ok(Some(value))
}

pub(crate) fn digest(value: &Value) -> Result<Value> {
    Ok(
        json!({"algorithm":"sha256","value":canonical::digest(&serde_json::to_vec(value).map_err(|_| rejected("public_digest_invalid"))?)}),
    )
}
