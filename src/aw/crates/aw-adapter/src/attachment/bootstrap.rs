//! Persistent Qoder startup groups reuse exclusive attachment's settings transaction.

use crate::Result;
use serde_json::{json, Value};
use std::path::Path;

/// Generate exec-form startup callbacks without a pre-existing runtime binding.
///
/// # Errors
/// Reject invalid native Hook settings.
pub fn groups(executable: &Path, descriptor: &Path, sequential: Option<bool>) -> Result<Value> {
    let (mut groups, _) =
        crate::qoder::reload::generate(json!({}), executable, descriptor, sequential)?;
    for (event, entries) in groups["hooks"]
        .as_object_mut()
        .ok_or("missing Hook groups")?
    {
        for group in entries.as_array_mut().ok_or("invalid Hook entries")? {
            for hook in group["hooks"].as_array_mut().ok_or("invalid Hooks")? {
                hook["args"] = json!(["qoder-bootstrap", "--descriptor", descriptor]);
                if event == "SessionStart" {
                    hook["timeout"] = json!(45);
                }
                if event == "SessionEnd" {
                    hook["timeout"] = json!(15);
                }
            }
        }
    }
    Ok(groups)
}

/// Merge only the supplied startup groups, preserving native settings and callbacks.
///
/// # Errors
/// Reject unsafe files, duplicate groups, disabled Hooks and concurrent settings writes.
pub fn install(settings: &Path, groups: &Value) -> Result<()> {
    if settings.try_exists()? {
        super::files::update(settings, |value| super::merge(value, groups))
    } else {
        let mut value = json!({});
        super::merge(&mut value, groups)?;
        super::files::create(settings, &serde_json::to_vec(&value)?)
    }
}

/// Remove exact startup groups, retaining user edits and rejecting modified owned Hooks.
///
/// # Errors
/// Reject unsafe files, changed owned groups or concurrent settings writes.
pub fn remove(settings: &Path, groups: &Value) -> Result<()> {
    super::Registration::remove_qoder_groups(settings, groups)
}
