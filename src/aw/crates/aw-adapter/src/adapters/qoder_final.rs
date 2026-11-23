//! Admit the verified legacy plaintext boundary for one foreground Qoder Query.

use crate::{
    adapter::{self, LaunchContext, LaunchInput, NativeContext},
    Result,
};
use serde_json::{json, Value};
use std::{fs, path::Path, sync::atomic::AtomicBool};

pub(super) fn profile(
    input: &LaunchInput,
    entry: &Path,
    cancelled: &AtomicBool,
) -> Result<Option<Value>> {
    let required = aw_config::requirements::project_document(&input.document, &input.target)
        .map_err(|_| "could not project native Hook requirements")?
        .iter()
        .any(|route| route.event == "security.violation" && !route.steps.is_empty());
    if !required {
        return Ok(None);
    }
    if input.options.reloadable
        || input
            .command
            .environment
            .contains_key(std::ffi::OsStr::new("NODE_OPTIONS"))
    {
        return Err(
            "Qoder final gate requires a foreground worker without loader overrides".into(),
        );
    }
    let script = r#"import {readFileSync} from 'node:fs'; import {dirname,join} from 'node:path';
import {createHash} from 'node:crypto';
const entry=process.argv.at(-1), root=dirname(entry);
const metadata=JSON.parse(readFileSync(join(root,'../package.json')));
const runtime=JSON.parse(readFileSync(join(root,'_worker/runtime-info.json')));
const hash=file=>createHash('sha256').update(readFileSync(file)).digest('hex');
console.log(JSON.stringify({sdk:metadata.version,worker:metadata.qoderCliVersion,
target:process.platform+'-'+process.arch,runtime_target:runtime.target,
sdk_sha256:hash(entry),worker_sha256:hash(join(root,'_worker/qoder-worker-runtime.obf.mjs'))}));"#;
    let output = adapter::Probe(cancelled).version_output(
        &input.command,
        &[
            "--input-type=module".into(),
            "--eval".into(),
            script.into(),
            "--".into(),
            entry.into(),
        ],
    )?;
    let actual: Value = serde_json::from_str(&output)?;
    select_profile(&actual)
}

fn select_profile(actual: &Value) -> Result<Option<Value>> {
    let profiles: Vec<Value> =
        serde_json::from_str(include_str!("../../native/qoder-sdk/final-profiles.json"))?;
    Ok(profiles.into_iter().find(|profile| {
        ["sdk", "worker", "target", "sdk_sha256", "worker_sha256"]
            .iter()
            .all(|key| profile[key] == actual[key])
            && actual["target"] == actual["runtime_target"]
    }))
}

pub(super) fn configure(context: &LaunchContext<'_>) -> Result<std::path::PathBuf> {
    fs::create_dir_all(context.files.directory().join("shared/js-runtime"))?;
    for (name, bytes) in [
        (
            "qoder-final-gate.mjs",
            include_bytes!("../../native/qoder-sdk/final-gate.mjs").as_slice(),
        ),
        (
            "claude-final-gate.mjs",
            include_bytes!("../../native/claude-sdk/final-gate.mjs").as_slice(),
        ),
        (
            "shared/transport.mjs",
            include_bytes!("../../native/shared/transport.mjs").as_slice(),
        ),
        (
            "shared/js-runtime/module-patch.mjs",
            include_bytes!("../../native/shared/js-runtime/module-patch.mjs").as_slice(),
        ),
    ] {
        context.files.write(name, bytes)?;
    }
    context.files.write(
        "qoder-final-preload.mjs",
        include_bytes!("../../native/qoder-sdk/final-preload.mjs"),
    )
}

pub(super) fn normalize(binding: &NativeContext, native: &Value) -> Result<Value> {
    if native["hook_event_name"] != "AWFinalToolResult"
        || native["tool_name"] != "Bash"
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
        return Err("Qoder final gate requires a bound Bash text result".into());
    }
    let mut payload = native.clone();
    payload["hook_event_name"] = json!("PostToolUse");
    let mut event =
        super::sdk_tools::normalize_tools(binding, "tool.after", &payload, "qoder-sdk")?;
    event["name"] = json!("security.violation");
    event["native"] = native.clone();
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_profiles_require_exact_worker_sources_and_platforms() {
        let profiles: Vec<Value> =
            serde_json::from_str(include_str!("../../native/qoder-sdk/final-profiles.json"))
                .unwrap();
        for profile in profiles {
            let mut actual = profile.clone();
            actual["runtime_target"] = actual["target"].clone();
            assert_eq!(select_profile(&actual).unwrap(), Some(profile));
            for key in [
                "sdk",
                "worker",
                "target",
                "runtime_target",
                "sdk_sha256",
                "worker_sha256",
            ] {
                let mut changed = actual.clone();
                changed[key] = json!("unknown");
                assert!(select_profile(&changed).unwrap().is_none());
            }
        }
    }

    #[test]
    fn final_tool_events_preserve_identity_and_reject_uncovered_payloads() {
        let binding = NativeContext {
            instance_id: "instance".into(),
            target: "sdk".into(),
            cwd: "/work".into(),
        };
        let native = json!({"hook_event_name":"AWFinalToolResult", "session_id":"session", "tool_use_id":"call",
            "tool_name":"Bash", "tool_input":{"command":"echo safe"}, "cwd":"/work",
            "tool_response":{"content":[{"type":"text","text":"result"}],"isError":false}});
        let event = normalize(&binding, &native).unwrap();
        assert_eq!(event["name"], "security.violation");
        assert_eq!(event["tool"]["call_id"], "call");
        assert_eq!(event["tool"]["result"], native["tool_response"]);
        for (key, value) in [
            ("cwd", json!("/other")),
            ("tool_name", json!("Task")),
            ("tool_use_id", json!("")),
            (
                "tool_response",
                json!({"content":[{"type":"image"}],"isError":false}),
            ),
            ("hook_event_name", json!("PostToolUse")),
            ("aw_source", json!({})),
        ] {
            let mut changed = native.clone();
            changed[key] = value;
            assert!(normalize(&binding, &changed).is_err());
        }
    }
}
