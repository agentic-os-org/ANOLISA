//! Resolve SDK profile IDs using the build's trusted builtin catalogue.

use crate::Error;
use aw_contracts::instrumentation::{builtin, InstrumentationProfile};
use serde_json::Value;

/// Returns the builtin profiles for a target, or an empty set for ordinary agents.
///
/// # Errors
/// Rejects an unknown or incompatible configured profile binding.
pub fn profiles(
    target: &str,
    agent: &Value,
) -> Result<Vec<&'static InstrumentationProfile>, Error> {
    let Some(ids) = agent["sdk"]["profiles"].as_array() else {
        return Ok(Vec::new());
    };
    builtin::select(
        ids.iter()
            .map(|id| id.as_str().ok_or(Error::InvalidSchema))
            .collect::<Result<Vec<_>, _>>()?,
    )
    .map_err(|reason| Error::Invariant {
        path: format!("/spec/agents/{target}/sdk/profiles"),
        reason,
    })
}
