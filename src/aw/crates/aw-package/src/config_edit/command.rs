//! Boolean command convenience inputs compile to the existing Provider/step schema.

use crate::{require, Result};
use serde_json::{json, Value};
use std::path::Path;

/// Literal boolean script and event-step options for offline composition.
pub struct CommandHook<'a> {
    /// Event-scoped Hook ID.
    pub id: &'a str,
    /// Configuration-wide Provider name; existing definitions must match.
    pub provider: &'a str,
    /// Absolute program followed by literal arguments; no shell is inserted.
    pub argv: &'a [String],
    /// Effect requested on JSON true: observe or block.
    pub effect: &'a str,
    /// Failure action, validated against the selected event.
    pub on_error: &'a str,
    /// Optional machine-readable match reason.
    pub reason_code: Option<&'a str>,
}

impl CommandHook<'_> {
    pub(super) fn definitions(&self) -> Result<(Value, Value)> {
        require(
            self.argv
                .first()
                .is_some_and(|arg| Path::new(arg).is_absolute()),
            "command argv must start with an absolute program",
        )?;
        require(
            matches!(self.effect, "block" | "observe"),
            "command effect must be block or observe",
        )?;
        let provider = json!({
            "transport":{"type":"stdio","location":"agent","argv":self.argv},
            "timeout_ms":1000,"max_output_bytes":32
        });
        let mut hook = json!({"id":self.id,"provider":self.provider,
            "effects":[self.effect],"on_error":self.on_error});
        if let Some(reason) = self.reason_code {
            hook["reason_code"] = json!(reason);
        }
        Ok((provider, hook))
    }
}
