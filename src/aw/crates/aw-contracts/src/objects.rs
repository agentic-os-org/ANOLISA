//! Public metadata objects and immutable references for runtime/audit consumers.
//!
//! Validation checks supplied facts and relationships. Producers authenticate
//! identities and capture native boundaries; consumers authorize reads and retain
//! replay state. No runtime hooks, storage or source clients are installed here.

use crate::{canonical, require, Error, Registry};
use serde_json::{json, Value};

const ASSOCIATED_FIELDS: &[&str] = &["sessionId", "turnId", "toolCallId"];

impl Registry {
    /// Validates a registered object and its field-level semantic invariants.
    ///
    /// Decode untrusted bytes with [`canonical::parse`] before calling this
    /// method so duplicate keys and numeric spellings cannot be discarded.
    /// `verified` and `complete` remain producer assertions, not authentication.
    ///
    /// # Errors
    /// Rejects unknown kinds/versions, malformed fields, contradictory association
    /// or coverage, and audit identities that do not bind their source position.
    pub fn validate_object(&self, object: &Value) -> Result<(), Error> {
        let version = object["apiVersion"]
            .as_str()
            .ok_or(Error::InvalidDocument)?;
        let kind = object["kind"].as_str().ok_or(Error::InvalidDocument)?;
        self.validate(self.object_schema(version, kind)?, object)?;
        let data = &object["data"];
        match kind {
            "ExecutionEvent" => {
                subject_for_event(&data["name"], &data["subject"])?;
                association(data)?;
            }
            "AuditRecord" => {
                subject_for_event(&data["eventName"], &data["subject"])?;
                require(
                    object["metadata"]["producer"] == data["eventRef"]["producer"],
                    "audit event producer changed",
                )?;
                let uid = audit_uid(
                    &object["metadata"]["producer"],
                    data["chain"]["id"].as_str().ok_or(Error::InvalidDocument)?,
                    data["chain"]["sequence"]
                        .as_u64()
                        .ok_or(Error::InvalidDocument)?,
                )?;
                require(object["metadata"]["uid"] == uid, "audit identity changed")?;
                coverage(&data["coverage"], &data["subject"])?;
                if data["kind"] == "provider.completed" {
                    provider_result(data)?;
                }
            }
            _ => return Err(Error::UnsupportedSchema("object invariants".into())),
        }
        Ok(())
    }
}

fn subject_for_event(name: &Value, subject: &Value) -> Result<(), Error> {
    match name.as_str().ok_or(Error::InvalidDocument)? {
        "instance.started" | "instance.ended" => require(
            ASSOCIATED_FIELDS
                .iter()
                .all(|field| subject[field].is_null()),
            "instance event has request identities",
        ),
        "input.submit" | "turn.ended" => require(
            subject["sessionId"].is_string()
                && subject["turnId"].is_string()
                && subject["toolCallId"].is_null(),
            "turn event identity mismatch",
        ),
        "tool.before" | "tool.after" | "security.violation" => require(
            subject["toolCallId"].is_string(),
            "tool event identity missing",
        ),
        _ => Err(Error::UnsupportedSchema("event name".into())),
    }
}

fn association(data: &Value) -> Result<(), Error> {
    let association = &data["association"];
    let subject = &data["subject"];
    for field in association["verifiedFields"]
        .as_array()
        .ok_or(Error::InvalidDocument)?
    {
        require(
            subject[field.as_str().ok_or(Error::InvalidDocument)?].is_string(),
            "verified association field missing",
        )?;
    }
    if association["status"] == "unassociated" {
        require(
            ASSOCIATED_FIELDS
                .iter()
                .all(|field| subject[field].is_null()),
            "unassociated event has request identities",
        )?;
    }
    for field in ASSOCIATED_FIELDS {
        require(
            data["nativeRefs"][field].is_null() || subject[field].is_string(),
            "native identity has no normalized subject",
        )?;
    }
    if data["name"] == "instance.started" || data["name"] == "instance.ended" {
        require(
            data["nativeRefs"]["modelCallId"].is_null(),
            "instance event has model identity",
        )?;
    }
    Ok(())
}

fn coverage(coverage: &Value, subject: &Value) -> Result<(), Error> {
    let interval = &coverage["interval"];
    if let (Some(start), Some(end)) = (interval["start"].as_str(), interval["end"].as_str()) {
        // The schema fixes all times to UTC milliseconds, so ordering is lexical.
        require(start <= end, "coverage interval reversed")?;
    }
    match coverage["scope"]["level"].as_str() {
        Some("turn") => require(subject["turnId"].is_string(), "coverage turn missing"),
        Some("tool") => require(subject["toolCallId"].is_string(), "coverage tool missing"),
        Some("instance") => Ok(()),
        _ => Err(Error::InvalidDocument),
    }
}

fn provider_result(data: &Value) -> Result<(), Error> {
    let name = data["eventName"].as_str().ok_or(Error::InvalidDocument)?;
    require(
        matches!(
            name,
            "input.submit" | "tool.before" | "tool.after" | "security.violation"
        ),
        "provider result has no policy boundary",
    )?;
    let details = &data["details"];
    if name == "tool.after" {
        require(
            details["policyBlock"] != true && details["failureAction"] != "block",
            "post-tool result claims execution prevention",
        )?;
    } else {
        require(
            details["failureAction"] != "withhold_result",
            "event does not support output withholding",
        )?;
    }
    Ok(())
}

/// Derives a replay-stable audit UID from the producer and source chain position.
///
/// The digest of the record is deliberately excluded: changing content at the
/// same position must remain an identity conflict. The producer generation is
/// the retained source generation, never the generation of a reader after restart.
/// Validate the complete record before publishing the resulting identity.
///
/// # Errors
/// Rejects inputs outside the bounded canonical metadata domain.
pub fn audit_uid(producer: &Value, chain_id: &str, sequence: u64) -> Result<String, Error> {
    canonical::document_digest(&json!([
        "audit.aw/v1alpha1",
        "AuditRecord",
        producer,
        chain_id,
        sequence
    ]))
}

/// Builds an immutable reference containing the full producer identity and digest.
///
/// # Errors
/// Rejects an invalid object. Hashes bind bytes and do not authorize access.
pub fn reference(registry: &Registry, object: &Value) -> Result<Value, Error> {
    registry.validate_object(object)?;
    Ok(json!({
        "apiVersion": object["apiVersion"], "kind": object["kind"],
        "uid": object["metadata"]["uid"], "producer": object["metadata"]["producer"],
        "digest": {"algorithm": "sha256", "value": canonical::document_digest(object)?}
    }))
}

/// Checks the complete reference against a validated immutable target.
///
/// # Errors
/// Rejects invalid targets and changed versions, identities, digests or fields.
pub fn validate_reference(
    registry: &Registry,
    expected: &Value,
    object: &Value,
) -> Result<(), Error> {
    require(
        &reference(registry, object)? == expected,
        "object reference changed",
    )
}

/// Binds an audit record to its event, including the copied query metadata.
///
/// Consumers can validate a standalone record with [`Registry::validate_object`]
/// while its event is unavailable; only this check establishes the relationship.
///
/// # Errors
/// Rejects invalid objects, unresolved references and mismatched event subjects/names.
pub fn validate_audit_event(
    registry: &Registry,
    record: &Value,
    event: &Value,
) -> Result<(), Error> {
    registry.validate_object(record)?;
    require(record["kind"] == "AuditRecord", "expected audit record")?;
    validate_reference(registry, &record["data"]["eventRef"], event)?;
    require(
        record["data"]["subject"] == event["data"]["subject"]
            && record["data"]["eventName"] == event["data"]["name"],
        "audit event metadata changed",
    )
}
