//! Native completion phases produce comparison data; the service owns leases and budgets.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Native completion rejection, mapped by the service to its existing wire codes.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Unsupported completion phase or callback seam.
    #[error("native_completion_seam_invalid")]
    InvalidSeam,
    /// Normalized event lacks required fields.
    #[error("event_invalid")]
    InvalidEvent,
    /// Completion differs from the admitted callback.
    #[error("native_correlation_mismatch")]
    Mismatch,
    /// Normalized event cannot be encoded.
    #[error("event_encoding")]
    Encoding,
    /// Original native input is missing or invalid.
    #[error("native_event_invalid")]
    InvalidNative,
}

impl Error {
    /// Stable service rejection code, retained across the source migration.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidSeam => "native_completion_seam_invalid",
            Self::InvalidEvent => "event_invalid",
            Self::Mismatch => "native_correlation_mismatch",
            Self::Encoding => "event_encoding",
            Self::InvalidNative => "native_event_invalid",
        }
    }
}

type Result<T> = std::result::Result<T, Error>;

/// Canonical native comparison for one event's callback phases.
pub struct Comparison {
    /// Normalized callback snapshot used for correlation.
    pub value: Value,
    /// Native bytes used for correlation.
    pub input: Vec<u8>,
    /// Whether this callback records the final observed result.
    pub observed: bool,
    /// Identity of the observed completion for replay checks.
    pub completion_digest: Option<[u8; 32]>,
}

/// Interpret native phases only when a result-replacement step was admitted.
pub fn normalize(
    value: &mut Value,
    native_input: &[u8],
    replacement_after: bool,
) -> Result<Comparison> {
    let hermes_after =
        replacement_after && value["name"] == "tool.after" && value["agent"]["adapter"] == "hermes";
    let mut comparison = value.clone();
    let mut compared_input = native_input.to_vec();
    let completion = value["native"]
        .as_object_mut()
        .and_then(|native| native.remove("aw_completion"));
    let mut observed = false;
    let mut completion_digest = None;
    if let Some(completion) = completion {
        if !replacement_after
            || value["agent"]["adapter"] != "openclaw"
            || value["name"] != "tool.after"
        {
            return Err(Error::InvalidSeam);
        }
        comparison = completion["snapshot"].clone();
        if comparison["native"]["hook"] != "after_tool_call"
            || value["native"]["hook"] != "after_tool_call"
        {
            return Err(Error::InvalidSeam);
        }
        match completion["phase"].as_str() {
            Some("replacement") if *value == comparison => {}
            Some("observation") => {
                observed = true;
                let mut bound = value.clone();
                bound["native"] = comparison["native"].clone();
                for field in ["result", "result_projection"] {
                    bound["tool"]
                        .as_object_mut()
                        .ok_or(Error::InvalidEvent)?
                        .remove(field);
                    if let Some(original) = comparison["tool"].get(field) {
                        bound["tool"][field] = original.clone();
                    }
                }
                if bound != comparison {
                    return Err(Error::Mismatch);
                }
                let mut digest = Sha256::new();
                digest.update(serde_json::to_vec(&value).map_err(|_| Error::Encoding)?);
                digest.update(native_input);
                completion_digest = Some(digest.finalize().into());
            }
            _ => return Err(Error::InvalidSeam),
        }
        compared_input = completion["input"]
            .as_str()
            .ok_or(Error::InvalidNative)?
            .as_bytes()
            .to_vec();
        if serde_json::from_slice::<Value>(&compared_input).map_err(|_| Error::InvalidNative)?
            != comparison["native"]
        {
            return Err(Error::Mismatch);
        }
    }
    if hermes_after {
        // Hermes observes completion before its model-bound transform seam.
        // Only the seam name and observer-only trace differ; retain all other
        // fields in the binding and the first callback's exact native bytes.
        comparison["native"] = hermes_completion(comparison["native"].take())?;
        let native: Value =
            serde_json::from_slice(native_input).map_err(|_| Error::InvalidNative)?;
        compared_input =
            serde_json::to_vec(&hermes_completion(native)?).map_err(|_| Error::Encoding)?;
    }
    Ok(Comparison {
        value: comparison,
        input: compared_input,
        observed,
        completion_digest,
    })
}

fn hermes_completion(mut native: Value) -> Result<Value> {
    if !matches!(
        native["hook_event_name"].as_str(),
        Some("post_tool_call" | "transform_tool_result")
    ) {
        return Err(Error::InvalidSeam);
    }
    if let Some(fields) = native.as_object_mut() {
        fields.insert("hook_event_name".into(), json!("post_tool_call"));
        if let Some(extra) = fields.get_mut("extra").and_then(Value::as_object_mut) {
            extra.remove("middleware_trace");
        }
    }
    Ok(native)
}

/// Whether the native runtime requires ordered execution for result replacement.
pub fn ordered_after(adapter: &str, event: &Value, replacement: bool) -> bool {
    adapter == "qoder" && event["name"] == "tool.after" && replacement
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hermes_completion_only_normalizes_the_two_final_native_seams() {
        let original = json!({"hook_event_name":"post_tool_call","extra":{"result":"original","middleware_trace":["observer-only"]}});
        let final_seam =
            json!({"hook_event_name":"transform_tool_result","extra":{"result":"original"}});
        assert_eq!(
            hermes_completion(original).unwrap(),
            hermes_completion(final_seam.clone()).unwrap()
        );
        let mut changed = final_seam.clone();
        changed["extra"]["result"] = json!("changed");
        assert_ne!(
            hermes_completion(final_seam).unwrap(),
            hermes_completion(changed).unwrap()
        );
        for native in [
            json!(null),
            json!("opaque"),
            json!({"hook_event_name":"pre_tool_call"}),
        ] {
            assert!(hermes_completion(native).is_err());
        }
    }
}
