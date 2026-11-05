//! Minimal user submission contract, separate from native tool identities.

use crate::{NativeContext, Result};
use serde_json::{json, Value};

pub(super) fn normalize(
    binding: &NativeContext,
    native: &Value,
    adapter: &str,
    text: &str,
) -> Result<Value> {
    let session = identity(&native["session_id"])?;
    let turn = identity(&native["aw_turn_id"])?;
    let text = native[text].as_str().ok_or("submission requires text")?;
    Ok(json!({"name":"input.submit",
        "agent":{"adapter":adapter,"binding_id":binding.target,"instance_id":binding.instance_id},
        "session_id":session,"turn_id":turn,"input":{"text":text},"native":native}))
}

fn identity(value: &Value) -> Result<&str> {
    value
        .as_str()
        .filter(|value| !value.trim().is_empty() && value.len() <= 512)
        .ok_or_else(|| "submission requires bounded session and turn identities".into())
}
