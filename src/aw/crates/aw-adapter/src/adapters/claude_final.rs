//! Exact bundled-runtime admission and the Claude final tool-result dialect.

use crate::{
    adapter::{self, LaunchContext, LaunchInput, NativeContext},
    Result,
};
use aw_core::admission::{AdmittedStep, StepExecution};
use serde_json::{json, Value};
use std::{fs, sync::atomic::AtomicBool};

pub(super) fn profile(
    input: &LaunchInput,
    version: &str,
    cli: &str,
    cancelled: &AtomicBool,
) -> Result<Option<Value>> {
    if input.document["spec"]["events"]["security.violation"]["enabled"] != true {
        return Ok(None);
    }
    let script = "import claude_agent_sdk, hashlib, json, platform; from pathlib import Path; root = Path(claude_agent_sdk.__file__).parent; print(json.dumps({'platform': platform.machine(), 'sha256': {name: hashlib.sha256((root / name).read_bytes()).hexdigest() for name in ['_bundled/claude', '_internal/query.py', '_internal/transport/subprocess_cli.py', 'types.py']}}))";
    let output =
        adapter::Probe(cancelled).version_output(&input.command, &["-c".into(), script.into()])?;
    let actual: Value = serde_json::from_str(&output)?;
    select_profile(version, cli, &actual)
}

fn select_profile(version: &str, cli: &str, actual: &Value) -> Result<Option<Value>> {
    let profiles: Vec<Value> =
        serde_json::from_str(include_str!("../../native/claude-sdk/final-profiles.json"))?;
    Ok(profiles.into_iter().find(|profile| {
        profile["sdk"] == version
            && profile["cli"] == cli
            && profile["platform"] == actual["platform"]
            && profile["sha256"] == actual["sha256"]
    }))
}

pub(super) fn validate(steps: &[AdmittedStep]) -> Result<()> {
    for step in steps
        .iter()
        .filter(|step| step.event == "security.violation")
    {
        if step.on_error != "block" || matches!(step.execution, StepExecution::Native) {
            return Err(
                "Claude final gate requires a structured policy with on_error: block".into(),
            );
        }
    }
    Ok(())
}

pub(super) fn configure(context: &LaunchContext<'_>) -> Result<()> {
    fs::create_dir_all(context.files.directory().join("shared/js-runtime"))?;
    for (name, content) in [
        (
            "aw_final.py",
            include_bytes!("../../native/claude-sdk/aw_final.py").as_slice(),
        ),
        (
            "claude-final-preload.mjs",
            include_bytes!("../../native/claude-sdk/final-preload.mjs").as_slice(),
        ),
        (
            "claude-final-gate.mjs",
            include_bytes!("../../native/claude-sdk/final-gate.mjs").as_slice(),
        ),
        (
            "shared/js-runtime/fetch-gate.mjs",
            include_bytes!("../../native/shared/js-runtime/fetch-gate.mjs").as_slice(),
        ),
    ] {
        context.files.write(name, content)?;
    }
    Ok(())
}

pub(super) fn normalize(binding: &NativeContext, native: &Value) -> Result<Value> {
    if native["hook_event_name"] != "AWFinalToolResult"
        || native.get("aw_source").is_some()
        || !native["tool_response"]["isError"].is_boolean()
        || !native["tool_response"]["content"]
            .as_array()
            .is_some_and(|items| {
                items
                    .iter()
                    .all(|item| item["type"] == "text" && item["text"].is_string())
            })
    {
        return Err("Claude final gate requires text content and error status".into());
    }
    let mut payload = native.clone();
    payload["hook_event_name"] = json!("PostToolUse");
    let mut event =
        super::sdk_tools::normalize_tools(binding, "tool.after", &payload, "claude-sdk")?;
    event["name"] = json!("security.violation");
    event["native"] = native.clone();
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_requires_exact_sdk_cli_and_source_fingerprints() {
        let profiles: Vec<Value> =
            serde_json::from_str(include_str!("../../native/claude-sdk/final-profiles.json"))
                .unwrap();
        for profile in profiles {
            let sdk = profile["sdk"].as_str().unwrap();
            let cli = profile["cli"].as_str().unwrap();
            assert_eq!(
                select_profile(sdk, cli, &profile).unwrap(),
                Some(profile.clone())
            );
            assert!(select_profile("0.0.0", cli, &profile).unwrap().is_none());
            assert!(select_profile(sdk, "unknown", &profile).unwrap().is_none());
            for field in ["platform", "sha256"] {
                let mut changed = profile.clone();
                changed[field] = json!("changed");
                assert!(select_profile(sdk, cli, &changed).unwrap().is_none());
            }
        }
    }

    #[test]
    fn final_events_require_bound_text_result_identity() {
        let binding = NativeContext {
            instance_id: "instance".into(),
            target: "target".into(),
            cwd: "/work".into(),
        };
        let native = json!({"hook_event_name":"AWFinalToolResult", "session_id":"session",
            "tool_use_id":"call", "tool_name":"Bash", "tool_input":{"command":"echo"},
            "cwd":"/work", "tool_response":{"content":[{"type":"text","text":"final"}],"isError":false}});
        let event = normalize(&binding, &native).unwrap();
        assert_eq!(event["name"], "security.violation");
        assert_eq!(event["tool"]["result"], native["tool_response"]);
        for (key, value) in [
            ("cwd", json!("/other")),
            ("tool_use_id", json!("")),
            ("hook_event_name", json!("PostToolUse")),
            ("tool_response", json!({"content":[]})),
        ] {
            let mut invalid = native.clone();
            invalid[key] = value;
            assert!(normalize(&binding, &invalid).is_err());
        }
    }
}
