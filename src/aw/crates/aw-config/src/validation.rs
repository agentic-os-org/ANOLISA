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
    let mut adapters = BTreeSet::new();
    for (name, agent) in spec["agents"].as_object().ok_or(Error::InvalidSchema)? {
        adapters.insert(agent["adapter"].as_str().ok_or(Error::InvalidSchema)?);
        require(
            agent.get("qoder_sdk").is_none() || agent["adapter"] == "qoder-sdk",
            &format!("/spec/agents/{name}/qoder_sdk"),
            "Qoder SDK grants require the Qoder SDK Adapter",
        )?;
        require(
            agent.get("qoder").is_none() || agent["adapter"] == "qoder",
            &format!("/spec/agents/{name}/qoder"),
            "Qoder settings require the Qoder Adapter",
        )?;
    }
    let providers = spec["providers"].as_object().ok_or(Error::InvalidSchema)?;
    let events = spec["events"].as_object().ok_or(Error::InvalidSchema)?;
    for (name, provider) in providers {
        if let Some(view) = provider.get("view") {
            require(
                view["transport"]["argv"][0]
                    .as_str()
                    .is_some_and(|path| std::path::Path::new(path).is_absolute()),
                &format!("/spec/providers/{name}/view/transport/argv/0"),
                "view entrypoint must be an absolute executable path",
            )?;
        }
        let protocol = provider["protocol"]
            .as_str()
            .unwrap_or(crate::HOOK_PROTOCOL);
        require(
            !matches!(protocol, "native-hook/v1alpha1" | crate::HOOK_PROTOCOL)
                || provider.get("config").is_none()
                || provider["config"].as_object().is_some_and(|config| config.is_empty()),
            &format!("/spec/providers/{name}/config"),
            "command hooks have no Provider private configuration; use explicit argv and environment",
        )?;
    }
    for (name, event) in events {
        let path = format!("/spec/events/{name}");
        let enabled = event["enabled"] == true;
        require(
            enabled || event["required"] != true,
            &format!("{path}/required"),
            "a disabled event cannot be required",
        )?;
        validate_tools(event, &path, &adapters)?;
        let mut ids = BTreeSet::new();
        for (index, step) in event["steps"]
            .as_array()
            .ok_or(Error::InvalidSchema)?
            .iter()
            .enumerate()
        {
            let path = format!("{path}/steps/{index}");
            validate_tools(step, &path, &adapters)?;
            let id = step["id"].as_str().ok_or(Error::InvalidSchema)?;
            require(ids.insert(id), &format!("{path}/id"), "duplicate step ID")?;
            let provider = step["provider"].as_str().ok_or(Error::InvalidSchema)?;
            require(
                providers.contains_key(provider),
                &format!("{path}/provider"),
                "unknown Provider reference",
            )?;
            let native = step.get("native").is_some();
            let protocol = providers[provider]["protocol"]
                .as_str()
                .unwrap_or(crate::HOOK_PROTOCOL);
            require(
                native == (protocol == "native-hook/v1alpha1"),
                &format!("{path}/provider"),
                "step shape must match the referenced Provider protocol",
            )?;
            require(
                protocol != "aw-provider/v1alpha1" || step.get("operation").is_some(),
                &format!("{path}/operation"),
                "structured Providers require an explicit operation",
            )?;
            require(
                protocol != crate::HOOK_PROTOCOL
                    || step.get("operation").is_none()
                    || step["operation"] == "invoke",
                &format!("{path}/operation"),
                "boolean hooks support only the default invoke operation",
            )?;
            require(
                step.get("reason_code").is_none() || protocol == crate::HOOK_PROTOCOL,
                &format!("{path}/reason_code"),
                "configured reason codes belong only to boolean hooks",
            )?;
            require(
                !native || matches!(name.as_str(), "tool.before" | "tool.after"),
                &path,
                "native hooks are defined only for tool.before and tool.after",
            )?;
            require(
                !native || matches!(step["on_error"].as_str(), Some("report" | "block")),
                &format!("{path}/on_error"),
                "native hooks support report or block failure actions",
            )?;
            let active = enabled && step["enabled"] != false;
            for (index, effect) in step
                .get("effects")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
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

fn validate_tools(value: &Value, path: &str, adapters: &BTreeSet<&str>) -> Result<(), Error> {
    if let Some(tools) = value["match"]["tools"].as_array() {
        require(
            tools.len() == 1 || !tools.iter().any(|tool| tool == "*"),
            &format!("{path}/match/tools"),
            "the all-tools selector cannot be combined with exact selectors",
        )?;
        for (index, tool) in tools.iter().enumerate() {
            let tool = tool.as_str().ok_or(Error::InvalidSchema)?;
            let standard = matches!(tool, "*" | "bash" | "file_read" | "file_write");
            let mut parts = tool.splitn(3, ':');
            let prefix = parts.next();
            let adapter = parts.next();
            let native = prefix == Some("native")
                && matches!(
                    adapter,
                    Some(
                        "pi" | "qwenpaw"
                            | "qoder"
                            | "openclaw"
                            | "hermes"
                            | "claude-sdk"
                            | "qoder-sdk"
                            | "agentscope"
                    )
                )
                && parts.next().is_some_and(|name| {
                    !name.is_empty() && !name.contains('*') && !name.chars().any(char::is_control)
                });
            require(
                standard || native,
                &format!("{path}/match/tools/{index}"),
                "expected all tools, a standard tool or an exact native:<adapter>:<name>",
            )?;
            require(
                !native || adapter.is_some_and(|adapter| adapters.contains(adapter)),
                &format!("{path}/match/tools/{index}"),
                "native tool selector requires an Adapter declared in spec.agents",
            )?;
        }
    }
    Ok(())
}
