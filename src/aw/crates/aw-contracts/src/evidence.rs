//! Validate immutable analysis inputs without turning source claims into authority.

use crate::{canonical, require, Error, Registry};
use serde_json::Value;
use std::collections::BTreeSet;

/// Validate a context against the current service subject and candidate revision.
/// Content strings carry arbitrary native JSON outside the canonical metadata domain.
/// Hashes detect changed input; they do not authenticate a caller or a source.
///
/// # Errors
/// Rejects malformed metadata, stale bindings, changed content, unresolved references,
/// duplicate evidence IDs, or a context revision that does not bind the exact input.
pub fn validate_context(
    registry: &Registry,
    context: &Value,
    subject: &Value,
    candidate_revision: &str,
) -> Result<(), Error> {
    registry.validate("evidence-context-v1", context)?;
    require(&context["subject"] == subject, "context subject changed")?;
    require(
        context["candidate_revision"] == candidate_revision,
        "context candidate changed",
    )?;
    let mut unsigned = context.clone();
    unsigned
        .as_object_mut()
        .ok_or(Error::InvalidDocument)?
        .remove("context_revision");
    require(
        context["context_revision"] == canonical::document_digest(&unsigned)?,
        "context revision mismatch",
    )?;
    let mut contents = BTreeSet::new();
    for content in context["contents"]
        .as_array()
        .ok_or(Error::InvalidDocument)?
    {
        let text = content["text"].as_str().ok_or(Error::InvalidDocument)?;
        let digest = content["digest"].as_str().ok_or(Error::InvalidDocument)?;
        require(
            canonical::digest(text.as_bytes()) == digest,
            "content changed",
        )?;
        require(contents.insert(digest), "duplicate content")?;
    }
    require(
        contents.contains(candidate_revision),
        "candidate content missing",
    )?;
    require(
        context["constraints_ref"]
            .as_str()
            .is_some_and(|id| contents.contains(id)),
        "constraints content missing",
    )?;
    require(
        context["original_deadline_ns"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .is_some_and(|n| n > 0),
        "invalid original deadline",
    )?;
    require(
        canonical::bytes(context)?.len() as u64
            <= context["limits"]["max_bytes"]
                .as_u64()
                .ok_or(Error::InvalidDocument)?,
        "context byte limit",
    )?;
    require(
        context["evidence"]
            .as_array()
            .ok_or(Error::InvalidDocument)?
            .len() as u64
            <= context["limits"]["max_items"]
                .as_u64()
                .ok_or(Error::InvalidDocument)?,
        "context item limit",
    )?;
    let mut evidence_ids = BTreeSet::new();
    let mut executions = BTreeSet::new();
    for evidence in context["evidence"]
        .as_array()
        .ok_or(Error::InvalidDocument)?
    {
        registry.validate("evidence-v1", evidence)?;
        require(&evidence["subject"] == subject, "evidence subject changed")?;
        let id = evidence["evidence_id"]
            .as_str()
            .ok_or(Error::InvalidDocument)?;
        require(evidence_ids.insert(id), "duplicate evidence")?;
        let content = evidence["content_ref"]
            .as_str()
            .ok_or(Error::InvalidDocument)?;
        require(contents.contains(content), "evidence content missing")?;
        require(
            id == canonical::document_digest(&serde_json::json!([evidence["source"], content]))?,
            "evidence identity changed",
        )?;
        if let Some(execution) = evidence["execution_ref"].as_str() {
            // The schema permits execution assertions only for verified native results.
            let expected = canonical::document_digest(&serde_json::json!([
                subject["generation"],
                subject["binding_id"],
                subject["instance_id"],
                evidence["association"]["session_id"],
                evidence["association"]["tool_call_id"],
            ]))?;
            require(execution == expected, "execution identity changed")?;
            executions.insert(execution);
        }
    }
    let refs = context["execution_refs"]
        .as_array()
        .ok_or(Error::InvalidDocument)?
        .iter()
        .map(|value| value.as_str().ok_or(Error::InvalidDocument))
        .collect::<Result<BTreeSet<_>, _>>()?;
    require(refs == executions, "unverified execution reference")?;
    Ok(())
}
