//! Query the pinned CLI's public hook inventory before binding any AW instance.

use super::hook_command;
use crate::Result;
use aw_exec::{CommandSpec, Limits};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    time::{Duration, Instant},
};

pub(super) struct Invocation {
    pub exec: bool,
    pub bypass_hook_trust: bool,
}

pub(super) fn arguments(args: &[OsString]) -> Result<Invocation> {
    let args: Vec<_> = args
        .iter()
        .map(|arg| arg.to_str().ok_or("non-UTF-8 Codex argument"))
        .collect::<std::result::Result<_, _>>()?;
    let exec = args.first() == Some(&"exec");
    let mut index = usize::from(exec);
    let mut prompt = false;
    let mut bypass_hook_trust = false;
    while index < args.len() {
        let arg = args[index];
        if arg == "--" {
            break;
        }
        if !arg.starts_with('-') {
            if prompt
                || matches!(
                    arg,
                    "resume"
                        | "fork"
                        | "review"
                        | "app-server"
                        | "agents"
                        | "remote-control"
                        | "exec"
                        | "e"
                        | "login"
                        | "logout"
                        | "mcp"
                        | "plugin"
                        | "completion"
                        | "update"
                        | "doctor"
                        | "sandbox"
                        | "debug"
                        | "apply"
                        | "a"
                        | "queue"
                        | "archive"
                        | "delete"
                        | "migrate-rollouts"
                        | "unarchive"
                        | "cloud"
                        | "exec-server"
                        | "features"
                        | "help"
                )
            {
                return Err("Codex supports one local TUI or exec prompt; resume, fork and remote entrypoints are not admitted".into());
            }
            prompt = true;
        } else {
            let (name, inline) = arg
                .split_once('=')
                .map_or((arg, false), |(name, _)| (name, true));
            match name {
                "-m" | "--model" | "-s" | "--sandbox" | "-a" | "--ask-for-approval" | "--color" => {
                    if !inline { index += 1; if index >= args.len() { return Err("Codex option requires a value".into()); } }
                }
                "--ephemeral" | "--json" | "--skip-git-repo-check" if exec && !inline => {}
                "--no-alt-screen" if !exec && !inline => {}
                "--dangerously-bypass-hook-trust" if !inline => { bypass_hook_trust = true; }
                "--dangerously-bypass-approvals-and-sandbox" if !inline => {}
                _ => return Err(format!("Codex option {name} is not admitted; keep model and native Hooks in the existing Codex config").into()),
            }
        }
        index += 1;
    }
    Ok(Invocation {
        exec,
        bypass_hook_trust,
    })
}

pub(super) fn hooks(
    command: &CommandSpec,
    events: &[&str],
    bypass: bool,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let executable = std::env::current_exe()?;
    let mut expected = BTreeMap::new();
    for event in events {
        expected.insert(event, hook_command(&executable, event)?);
    }
    let mut probe = command.clone();
    probe.program = "python3".into();
    probe.args = vec![
        "-B".into(),
        "-c".into(),
        include_str!("../../../native/codex/probe.py").into(),
        command.program.clone().into_os_string(),
        serde_json::to_string(&expected)?.into(),
    ];
    let output = aw_exec::run_cooperative(
        &probe,
        &[],
        Limits {
            input_bytes: 0,
            stdout_bytes: 65536,
            stderr_bytes: 65536,
        },
        Instant::now() + Duration::from_secs(12),
        cancelled,
    )?;
    if !output.status.success() {
        return Err("Codex hook inventory probe failed; check native configuration and Python 3 availability".into());
    }
    let result: Value = serde_json::from_slice(&output.stdout)?;
    if result["enabled"] != true || result["errors"] != false {
        return Err("Codex Hooks are disabled or their configuration has errors".into());
    }
    for event in events {
        let matches = result["hooks"][*event]
            .as_array()
            .ok_or("missing Codex Hook inventory")?;
        if matches.len() != 1 {
            return Err(format!(
                "Codex {event} requires exactly one installed AW hook; run aw install first"
            )
            .into());
        }
        let hook = &matches[0];
        if hook["enabled"] != true
            || hook["async"] != false
            || hook["matcher"] != ".*"
            || hook["timeoutSec"] != json!(60)
        {
            return Err(format!("Codex {event} AW hook must be enabled, synchronous, match all tools and use timeout 60").into());
        }
        if !bypass && !matches!(hook["trustStatus"].as_str(), Some("trusted" | "managed")) {
            return Err("Codex AW hook needs native trust review: open Codex /hooks and trust the exact AW definitions".into());
        }
    }
    if bypass {
        eprintln!(
            "AW: honoring the explicitly supplied Codex hook-trust bypass for this invocation"
        );
    }
    Ok(())
}
