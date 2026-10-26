//! Desired documents share one base; each explicit policy adds its Provider and steps together.

use super::{Policy, Settings};
use crate::{require, Result};
use serde_json::{json, Value};
use std::path::Path;

pub(super) fn document(settings: &Settings<'_>) -> Result<Value> {
    require(
        settings.qoder.is_some() || settings.openclaw.is_some(),
        "select at least one Agent entrypoint",
    )?;
    require(
        settings.node.is_some() == settings.openclaw.is_some(),
        "--node and --openclaw must be provided together",
    )?;
    let mut value = json!({"apiVersion":"aw/v1alpha1","kind":"AWConfiguration",
        "metadata":{"name":"local-agent"},"spec":{
        "daemon":{"startup":"on_demand","state_dir":settings.state,"endpoint":"auto"},
        "execution":{"guarantee":"native_hook","default_event_budget_ms":5000},
        "audit":{"enabled":true,"payload":"metadata_only"},
        "agents":{},"providers":{},"events":{}}});
    if let Some(qoder) = settings.qoder {
        value["spec"]["agents"]["qoder"] = json!({"adapter":"qoder","argv":[qoder]});
    }
    if let (Some(node), Some(openclaw)) = (settings.node, settings.openclaw) {
        value["spec"]["agents"]["openclaw"] =
            json!({"adapter":"openclaw","argv":[node,openclaw,"gateway","run"]});
    }
    match &settings.policy {
        Policy::None => {}
        Policy::Tokenless => {}
        Policy::SecCore { socket } | Policy::SecCoreTokenless { socket } => {
            let provider = settings.prefix.join("libexec/aw/providers/sec-core");
            let mut tools = json!({});
            let mut selectors = Vec::new();
            if settings.qoder.is_some() {
                selectors.push("native:qoder:Bash");
                tools["Bash"] = json!({"language":"bash","input_pointer":"/command"});
            }
            if settings.openclaw.is_some() {
                selectors.push("native:openclaw:exec");
                tools["exec"] = json!({"language":"bash","input_pointer":"/command"});
            }
            value["spec"]["providers"]["security"] = json!({
                "protocol":"aw-provider/v1alpha1",
                "transport":{"type":"stdio","location":"agent","argv":[provider.join("aw-provider-sec-core"),"--cli",provider.join("agent-sec-cli"),"--socket",socket]},
                "timeout_ms":2000,"max_output_bytes":4096,
                "config":{"version":1,"mode":"block","unmapped_tool":"error","tools":tools}});
            value["spec"]["events"] = json!({
                "tool.before":{"enabled":true,"required":true,"steps":[{"id":"scan-code","match":{"tools":selectors},"provider":"security","operation":"scan_code","effects":["observe","block"],"on_error":"block"}]},
                "tool.after":{"enabled":true,"required":true,"steps":[{"id":"observe-completion","provider":"security","operation":"observe_tool","effects":["observe"],"on_error":"report"}]}});
        }
        Policy::Command {
            argv,
            effect,
            reason_code,
        } => {
            require(
                !argv.is_empty()
                    && argv.len() <= 128
                    && Path::new(&argv[0]).is_absolute()
                    && !argv.iter().any(|arg| arg.contains('\0')),
                "command argv must start with an absolute program and contain literal arguments",
            )?;
            require(
                matches!(*effect, "block" | "observe"),
                "command effect must be block or observe",
            )?;
            require(
                !reason_code.is_empty()
                    && reason_code.len() <= 128
                    && reason_code
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-')),
                "invalid command reason code",
            )?;
            value["spec"]["providers"]["command"] = json!({
                "transport":{"type":"stdio","location":"agent","argv":argv},
                "timeout_ms":1000,"max_output_bytes":32});
            value["spec"]["events"] = json!({
                "tool.before":{"enabled":true,"required":true,"steps":[{"id":"check-command","provider":"command","effects":[effect],"reason_code":reason_code,"on_error":"block"}]}});
        }
    }
    if matches!(
        settings.policy,
        Policy::Tokenless | Policy::SecCoreTokenless { .. }
    ) {
        let provider = settings.prefix.join("libexec/aw/providers/tokenless");
        value["spec"]["providers"]["tokenless"] = json!({
            "protocol":"aw-provider/v1alpha1", "transport":{"type":"stdio","location":"agent","argv":[provider.join("aw-provider-tokenless"),"--cli",provider.join("tokenless"),"--data-dir",settings.state.join("tokenless")]},
            "timeout_ms":2000,"max_output_bytes":1048576,"config":{"version":1}});
        let mut view_argv = value["spec"]["providers"]["tokenless"]["transport"]["argv"]
            .as_array()
            .ok_or_else(|| crate::Error::Invalid("Tokenless argv must be an array".into()))?
            .clone();
        view_argv.insert(1, json!("--view"));
        value["spec"]["providers"]["tokenless"]["view"] = json!({
            "protocol":"aw-provider-view/v1alpha1","implementation":"tokenless",
            "transport":{"type":"stdio","location":"agent","argv":view_argv}});
        if value["spec"]["events"]["tool.before"].is_null() {
            value["spec"]["events"]["tool.before"] =
                json!({"enabled":true,"required":true,"steps":[]});
        }
        value["spec"]["events"]["tool.before"]["steps"]
            .as_array_mut()
            .ok_or_else(|| crate::Error::Invalid("tool.before steps must be an array".into()))?
            .push(json!({"id":"rewrite-command","provider":"tokenless","operation":"pre_tool","effects":["block"],"on_error":"report"}));
        if value["spec"]["events"]["tool.after"].is_null() {
            value["spec"]["events"]["tool.after"] =
                json!({"enabled":true,"required":true,"steps":[]});
        }
        value["spec"]["events"]["tool.after"]["steps"]
            .as_array_mut()
            .ok_or_else(|| crate::Error::Invalid("tool.after steps must be an array".into()))?
            .push(json!({"id":"compress-result","provider":"tokenless","operation":"post_tool","effects":["replace_result"],"on_error":"report"}));
    }
    Ok(value)
}
