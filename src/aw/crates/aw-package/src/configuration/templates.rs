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
        Policy::SecCore { socket } => {
            let provider = settings.prefix.join("libexec/aw/providers/sec-core");
            let mut tools = json!({});
            if settings.qoder.is_some() {
                tools["Bash"] = json!({"language":"bash","input_pointer":"/command"});
            }
            if settings.openclaw.is_some() {
                tools["exec"] = json!({"language":"bash","input_pointer":"/command"});
            }
            value["spec"]["providers"]["security"] = json!({
                "protocol":"aw-provider/v1alpha1",
                "transport":{"type":"stdio","location":"agent","argv":[provider.join("aw-provider-sec-core"),"--cli",provider.join("agent-sec-cli"),"--socket",socket]},
                "timeout_ms":2000,"max_output_bytes":4096,
                "config":{"version":1,"mode":"block","tools":tools}});
            value["spec"]["events"] = json!({
                "tool.before":{"enabled":true,"required":true,"steps":[{"id":"scan-code","provider":"security","operation":"scan_code","effects":["observe","block"],"on_error":"block"}]},
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
                "protocol":"aw-provider/v1alpha1",
                "transport":{"type":"stdio","location":"agent","argv":[settings.prefix.join("bin/aw"),"policy"]},
                "timeout_ms":2000,"max_output_bytes":4096,
                "config":{"version":1,"argv":argv,"timeout_ms":1000,"on_true":{"type":effect,"reason_code":reason_code}}});
            value["spec"]["events"] = json!({
                "tool.before":{"enabled":true,"required":true,"steps":[{"id":"check-command","provider":"command","operation":"check","effects":[effect],"on_error":"block"}]}});
        }
    }
    Ok(value)
}
