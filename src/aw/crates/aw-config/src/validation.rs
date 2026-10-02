//! Cross-field constraints for desired configuration; no capability discovery.

use crate::Error;
use serde_json::Value;
use std::collections::BTreeSet;

fn require(condition: bool, path: &str, reason: &'static str) -> Result<(), Error> {
    if condition {
        Ok(())
    } else {
        Err(Error::Invariant {
            path: path.into(),
            reason,
        })
    }
}

pub(super) fn validate(document: &Value) -> Result<(), Error> {
    // Called only after schema validation; these conversions still propagate a
    // visible error if the bundled shape and semantic validator ever diverge.
    let spec = &document["spec"];
    let providers = spec["providers"].as_object().ok_or(Error::InvalidSchema)?;
    let events = spec["events"].as_object().ok_or(Error::InvalidSchema)?;
    for (name, event) in events {
        let path = format!("/spec/events/{name}");
        let enabled = event["enabled"] == true;
        require(
            enabled || event["required"] != true,
            &format!("{path}/required"),
            "a disabled event cannot be required",
        )?;
        if let Some(tools) = event["match"]["tools"].as_array() {
            require(
                tools.len() == 1 || !tools.iter().any(|tool| tool == "*"),
                &format!("{path}/match/tools"),
                "the all-tools selector cannot be combined with exact selectors",
            )?;
            for (index, tool) in tools.iter().enumerate() {
                let tool = tool.as_str().ok_or(Error::InvalidSchema)?;
                let standard = matches!(tool, "*" | "bash" | "file_read" | "file_write");
                let mut parts = tool.splitn(3, ':');
                let native = parts.next() == Some("native")
                    && matches!(
                        parts.next(),
                        Some("qwenpaw" | "qoder" | "openclaw" | "hermes")
                    )
                    && parts.next().is_some_and(|name| {
                        !name.is_empty()
                            && !name.contains('*')
                            && !name.chars().any(char::is_control)
                    });
                require(
                    standard || native,
                    &format!("{path}/match/tools/{index}"),
                    "expected all tools, a standard tool or an exact native:<adapter>:<name>",
                )?;
            }
        }
        let mut ids = BTreeSet::new();
        for (index, step) in event["steps"]
            .as_array()
            .ok_or(Error::InvalidSchema)?
            .iter()
            .enumerate()
        {
            let path = format!("{path}/steps/{index}");
            let id = step["id"].as_str().ok_or(Error::InvalidSchema)?;
            require(ids.insert(id), &format!("{path}/id"), "duplicate step ID")?;
            let provider = step["provider"].as_str().ok_or(Error::InvalidSchema)?;
            require(
                providers.contains_key(provider),
                &format!("{path}/provider"),
                "unknown Provider reference",
            )?;
            let active = enabled && step["enabled"] != false;
            for (index, effect) in step["effects"]
                .as_array()
                .ok_or(Error::InvalidSchema)?
                .iter()
                .enumerate()
            {
                let effect = effect.as_str().ok_or(Error::InvalidSchema)?;
                let path = format!("{path}/effects/{index}");
                let allowed = match name.as_str() {
                    "tool.before" => {
                        matches!(effect, "observe" | "block" | "replace_input" | "ask")
                    }
                    "tool.after" => matches!(effect, "observe" | "replace_result"),
                    "security.violation" => matches!(effect, "observe" | "block"),
                    _ => effect == "observe",
                };
                require(allowed, &path, "effect is not defined for this event")?;
                require(
                    !(active && effect == "ask"),
                    &path,
                    "ask is reserved; active approval steps are not supported in this revision",
                )?;
            }
            let on_error = step["on_error"].as_str().ok_or(Error::InvalidSchema)?;
            require(
                match on_error {
                    "report" => true,
                    "block" => matches!(name.as_str(), "tool.before" | "security.violation"),
                    "withhold_result" => name == "tool.after",
                    _ => false,
                },
                &format!("{path}/on_error"),
                "failure action is not defined for this event",
            )?;
        }
    }
    let before = &spec["events"]["tool.before"];
    let guard = &spec["events"]["security.violation"];
    let linked = before["guard"] == "security.violation";
    require(
        !linked || events.contains_key("security.violation"),
        "/spec/events/tool.before/guard",
        "guard must reference a declared security.violation event",
    )?;
    require(
        !(linked && before["enabled"] == true) || guard["enabled"] == true,
        "/spec/events/tool.before/guard",
        "an active tool.before guard cannot reference a disabled event",
    )?;
    require(
        guard["enabled"] != true || (linked && before["enabled"] == true),
        "/spec/events/security.violation/enabled",
        "security.violation runs only as the guard of an active tool.before event",
    )
}
