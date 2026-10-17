//! Project bounded source records as data and freeze their exact content references.

use super::{contract, now_ms, Seed};
use crate::{agentsight, rejected, Result, VERSION};
use aw_contracts::canonical;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::time::Instant;

pub(super) fn content(value: &Value) -> Result<Value> {
    let text = serde_json::to_string(value).map_err(|_| rejected("context_encoding"))?;
    Ok(json!({"encoding":"json-utf8", "digest":canonical::digest(text.as_bytes()), "text":text}))
}

fn record(
    subject: &Value,
    source: Value,
    association: Value,
    body: &Value,
    cutoff: u64,
) -> Result<Value> {
    let identity = json!([source, body["digest"]]);
    Ok(json!({
        "schema_version":"aw.evidence/v1", "evidence_id":canonical::document_digest(&identity).map_err(contract)?,
        "subject":subject, "source":source, "association":association,
        "times":{"occurred_at":null,"received_at_ms":cutoff,"cutoff_at_ms":cutoff},
        "kind":"trajectory.step", "content_ref":body["digest"],
        "coverage":{"status":"unknown", "gaps":["coverage_unknown"]},
        "trust":"source_reported", "derivation":"source_history", "execution_ref":null,
    }))
}

fn add_content(snapshot: &mut Value, body: Value) -> Result<()> {
    let contents = snapshot["contents"]
        .as_array_mut()
        .ok_or_else(|| rejected("context_encoding"))?;
    if !contents
        .iter()
        .any(|existing| existing["digest"] == body["digest"])
    {
        contents.push(body);
    }
    Ok(())
}

fn finish(mut value: Value, gaps: &BTreeSet<&str>, max_bytes: usize) -> Result<Value> {
    value["gaps"] = json!(gaps);
    value
        .as_object_mut()
        .ok_or_else(|| rejected("context_encoding"))?
        .remove("context_revision");
    value["context_revision"] = json!(canonical::document_digest(&value).map_err(contract)?);
    if canonical::bytes(&value).map_err(contract)?.len() > max_bytes {
        return Err(rejected("context_byte_limit"));
    }
    Ok(value)
}

pub(super) fn snapshot(
    seed: &Seed,
    query: Option<agentsight::QueryResult>,
    status: &str,
    gap: &'static str,
    deadline: Instant,
) -> Result<Value> {
    if Instant::now() >= deadline {
        return Err(rejected("context_cancelled_or_expired"));
    }
    let cutoff = now_ms()?;
    let max_items = seed.settings.as_ref().map_or(32, |s| s.max_items);
    let max_bytes = seed.settings.as_ref().map_or(262144, |s| s.max_bytes);
    let verified = seed.association["status"] == "verified";
    let mut native = record(
        &seed.subject,
        json!({
            "component":"aw", "scope":seed.subject["generation"],
            "instance_id":seed.subject["instance_id"], "version":VERSION,
            "record_ref":seed.subject["event_id"], "revision":seed.candidate["digest"],
        }),
        seed.association.clone(),
        &seed.candidate,
        cutoff,
    )?;
    native["kind"] = json!(if seed.after {
        "native.tool_result"
    } else {
        "native.callback"
    });
    native["trust"] = json!(if verified {
        "trusted_adapter"
    } else {
        "unverified_native"
    });
    native["derivation"] = json!("native_report");
    native["times"]["received_at_ms"] = json!(seed.received_at_ms);
    let executions = if verified && seed.after {
        let execution = canonical::document_digest(&json!([
            seed.subject["generation"],
            seed.subject["binding_id"],
            seed.subject["instance_id"],
            seed.association["session_id"],
            seed.association["tool_call_id"],
        ]))
        .map_err(contract)?;
        native["execution_ref"] = json!(execution);
        vec![execution]
    } else {
        vec![]
    };
    let mut gaps = BTreeSet::from([gap, "coverage_unknown", "process_identity_unavailable"]);
    if !verified {
        gaps.insert("native_identity_unverified");
    }
    let mut snapshot = json!({
        "schema_version":"aw.evidence-context/v1", "subject":seed.subject,
        "candidate_revision":seed.candidate["digest"], "constraints_ref":seed.constraints["digest"],
        "projection_version":"aw.evidence-projection/v1", "redaction_version":"selected-fields-no-redaction/v1",
        "original_deadline_ns":seed.deadline_ns, "cutoff_at_ms":cutoff,
        "limits":{"max_items":max_items,"max_bytes":max_bytes,
            "query_bytes":seed.settings.as_ref().map_or(1048576, |s| s.query_bytes),
            "query_timeout_ms":seed.settings.as_ref().map_or(1000, |s| s.query_timeout_ms)},
        "source_status":status, "source_revision":null, "gaps":[],
        "evidence":[native], "contents":[seed.candidate, seed.constraints], "execution_refs":executions,
    });
    let Some(query) = query else {
        return finish(snapshot, &gaps, max_bytes);
    };
    let agentsight::Outcome::Available { trajectory, .. } = query.outcome else {
        return finish(snapshot, &gaps, max_bytes);
    };
    let settings = seed
        .settings
        .as_ref()
        .ok_or_else(|| rejected("context_source"))?;
    let revision = content(&trajectory)?["digest"].clone();
    snapshot["source_revision"] = revision.clone();
    gaps.extend(["source_instance_unknown", "source_time_unverified"]);
    if trajectory["subagent_trajectories"]
        .as_array()
        .is_some_and(|items| !items.is_empty())
    {
        gaps.insert("subagent_scope_excluded");
    }
    let steps = trajectory["steps"]
        .as_array()
        .ok_or_else(|| rejected("context_source"))?;
    if steps.len() >= max_items {
        gaps.insert("item_limit");
    }
    let mut association = seed.association.clone();
    association["basis"] = json!("native_session");
    // A session match corroborates the subject, not individual model-generated calls.
    association["tool_call_id"] = Value::Null;
    let scope = canonical::document_digest(&json!([settings.source_id, settings.endpoint]))
        .map_err(contract)?;
    let selected = steps.len().saturating_sub(max_items - 1);
    for (index, step) in steps.iter().enumerate().skip(selected) {
        if Instant::now() >= deadline {
            return Err(rejected("context_cancelled_or_expired"));
        }
        let body = content(step)?;
        let mut evidence = record(
            &seed.subject,
            json!({
                "component":"agentsight", "scope":scope, "instance_id":null,
                "version":query.source.version,
                "record_ref":format!("trajectory:{}#/steps/{index}", query.session_id),
                "revision":revision,
            }),
            association.clone(),
            &body,
            cutoff,
        )?;
        if let Some(timestamp) = step["timestamp"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 128)
        {
            evidence["times"]["occurred_at"] = json!(timestamp);
        }
        evidence["coverage"]["gaps"] = json!([
            "coverage_unknown",
            "source_instance_unknown",
            "source_time_unverified"
        ]);
        if step["is_copied_context"] == true {
            evidence["derivation"] = json!("model_echo");
        }
        let mut next = snapshot.clone();
        next["evidence"]
            .as_array_mut()
            .ok_or_else(|| rejected("context_encoding"))?
            .push(evidence);
        add_content(&mut next, body)?;
        // Reserve the gap marker before accepting history so truncation itself
        // never makes an otherwise bounded result overflow.
        let mut sized_gaps = gaps.clone();
        sized_gaps.insert("content_byte_limit");
        if finish(next.clone(), &sized_gaps, max_bytes).is_ok() {
            snapshot = next;
        } else {
            gaps.insert("content_byte_limit");
        }
    }
    finish(snapshot, &gaps, max_bytes)
}
