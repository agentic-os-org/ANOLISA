//! Source identity for paid causal analyses; not part of the HTTP response.

use agentsight_atif::AtifTrajectory;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Grounding reads preceding evidence as well as the selected round. The
/// pipeline also uses the session's last timestamp as its reference clock.
pub(super) fn fingerprint(
    trajectory: &AtifTrajectory,
    round: std::ops::Range<usize>,
) -> Result<String, serde_json::Error> {
    let mut value = serde_json::to_value((
        &trajectory.session_id,
        round.start,
        &trajectory.steps[..round.end],
        trajectory
            .steps
            .last()
            .and_then(|step| step.timestamp.as_deref()),
    ))?;
    // Step.extra and tool arguments may contain maps whose iteration order
    // changes on reload. Equal input must still reuse its paid analysis.
    value.sort_all_objects();
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&value)?)))
}

/// Old rows have no source identity. Keep them readable when the trajectory
/// was purged; when a source exists, recompute once to establish its identity.
pub(super) fn restored_case(
    mut value: Value,
    source: Option<&str>,
    available: bool,
) -> Option<Value> {
    let metadata = value.as_object_mut()?.remove("_agentsight_causal_source");
    if let Some(metadata) = metadata {
        if metadata.get("version").and_then(Value::as_u64) != Some(1) {
            return None;
        }
        if available && (source.is_none() || metadata.get("hash").and_then(Value::as_str) != source)
        {
            return None;
        }
    } else if available {
        return None;
    }
    value.is_object().then_some(value)
}

pub(super) fn persisted_case(
    case: &super::CausalCase,
    source: Option<&str>,
) -> Result<String, serde_json::Error> {
    let mut value = serde_json::to_value(case)?;
    // Keep the existing case fields at the root so older binaries can still
    // display the case after a rollback. This reader strips private metadata.
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "_agentsight_causal_source".into(),
            serde_json::json!({"version":1,"hash":source}),
        );
    }
    serde_json::to_string(&value)
}
