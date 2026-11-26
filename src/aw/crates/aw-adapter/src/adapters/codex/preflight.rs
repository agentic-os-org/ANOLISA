//! Query the CLI's public hook inventory before binding any AW instance.

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
    pub entrypoint: Entrypoint,
    pub bypass_hook_trust: bool,
}

#[derive(PartialEq, Eq)]
pub(super) enum Entrypoint {
    Tui,
    Exec,
    AppServer,
    Gateway(String),
}

impl Entrypoint {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Tui => "tui",
            Self::Exec => "exec",
            Self::AppServer => "app-server",
            Self::Gateway(_) => "app-server-gateway",
        }
    }
}

pub(super) fn arguments(args: &[OsString]) -> Result<Invocation> {
    let args: Vec<_> = args
        .iter()
        .map(|arg| arg.to_str().ok_or("non-UTF-8 Codex argument"))
        .collect::<std::result::Result<_, _>>()?;
    if let ["app-server", options @ ..] = args.as_slice() {
        return Ok(Invocation {
            entrypoint: app_server_options(options)?,
            bypass_hook_trust: false,
        });
    }
    let exec = args.first() == Some(&"exec");
    let mut index = usize::from(exec);
    let mut bypass_hook_trust = false;
    while index < args.len() {
        let arg = args[index];
        if arg == "--" {
            if args.len() - index > 2 {
                return Err("Codex accepts one prompt after --".into());
            }
            break;
        }
        if !arg.starts_with('-') {
            return Err("Codex prompt must follow --; unsupported subcommand".into());
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
        entrypoint: if exec {
            Entrypoint::Exec
        } else {
            Entrypoint::Tui
        },
        bypass_hook_trust,
    })
}

// Keep the gateway address dynamic while all permission and Hook settings stay in
// the reviewed native configuration. Pass the same argv to inventory and launch.
fn app_server_options(mut args: &[&str]) -> Result<Entrypoint> {
    args = match args {
        ["--listen", "stdio://", rest @ ..] | ["--listen=stdio://", rest @ ..] => rest,
        _ => args,
    };
    if args.is_empty() {
        return Ok(Entrypoint::AppServer);
    }
    if args.len() != 12 {
        return Err("Codex app-server requires local stdio and six gateway overrides".into());
    }
    let mut settings = BTreeMap::new();
    for pair in args.chunks_exact(2) {
        let (key, value) = pair[1]
            .split_once('=')
            .ok_or("invalid Codex local stdio gateway override")?;
        if pair[0] != "-c" || settings.insert(key, value).is_some() {
            return Err("invalid or duplicate Codex local stdio gateway override".into());
        }
    }
    let provider = settings
        .remove("model_provider")
        .and_then(quoted)
        .filter(|id| {
            id.len() <= 64
                && id.starts_with(|c: char| c.is_ascii_lowercase())
                && id.bytes().all(|c| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-')
                })
        })
        .ok_or("invalid Codex local stdio gateway provider ID")?;
    let prefix = format!("model_providers.{provider}.");
    for (key, value) in settings {
        let admitted = match key.strip_prefix(&prefix) {
            Some("name") => gateway_name(value),
            Some("env_key") => quoted(value).is_some_and(gateway_env_key),
            Some("wire_api") => value == r#""responses""#,
            Some("supports_websockets") => value == "false",
            Some("base_url") => value
                .strip_prefix(r#""http://127.0.0.1:"#)
                .and_then(|url| url.strip_suffix(r#"/v1""#))
                .is_some_and(|port| {
                    port.parse::<u16>()
                        .is_ok_and(|n| n > 0 && n.to_string() == port)
                }),
            _ => false,
        };
        if !admitted {
            return Err("invalid Codex local stdio gateway override".into());
        }
    }
    Ok(Entrypoint::Gateway(provider.into()))
}

fn quoted(value: &str) -> Option<&str> {
    value.strip_prefix('"')?.strip_suffix('"')
}

fn gateway_name(value: &str) -> bool {
    let Ok(name) = serde_json::from_str::<String>(value) else {
        return false;
    };
    // Canonical string literals keep the SDK, JSON and native TOML escape rules aligned.
    !name.is_empty()
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '"' | '\''))
        && serde_json::to_string(&name).is_ok_and(|literal| literal == value)
}

fn gateway_env_key(key: &str) -> bool {
    const RESERVED: &[&str] = &[
        "HOME",
        "PATH",
        "SHELL",
        "USER",
        "LOGNAME",
        "PWD",
        "OLDPWD",
        "TMPDIR",
        "ENV",
        "IFS",
        "SHELLOPTS",
        "BASHOPTS",
        "LANG",
        "TERM",
        "SHLVL",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
    ];
    const PREFIXES: &[&str] = &[
        "AW_", "BASH_", "CODEX_", "DYLD_", "GIT_", "LC_", "LD_", "NODE_", "PYTHON", "SSH_", "SSL_",
        "XDG_",
    ];
    key.len() <= 64
        && key.starts_with(|c: char| c.is_ascii_uppercase())
        && key
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
        && !RESERVED.contains(&key)
        && !PREFIXES.iter().any(|prefix| key.starts_with(prefix))
}

pub(super) fn hooks(
    command: &CommandSpec,
    events: &[&str],
    bypass: bool,
    require_final_input: bool,
    replacement: bool,
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
    if command.args.first().is_some_and(|arg| arg == "app-server") {
        probe.args.extend_from_slice(&command.args);
    } else {
        probe.args.push("app-server".into());
    }
    crate::adapter::isolate_python_environment(&mut probe)?;
    let (output, bytes) = crate::adapter::Probe(cancelled).run_python(
        probe,
        &[],
        Limits {
            input_bytes: 0,
            stdout_bytes: 65536,
            stderr_bytes: 65536,
        },
        Instant::now() + Duration::from_secs(12),
        aw_exec::CleanupMode::Cooperative,
    )?;
    if !output.status.success() {
        return Err("Codex hook inventory probe failed; check native configuration and Python 3 availability".into());
    }
    let result: Value = serde_json::from_slice(&bytes)?;
    if result["enabled"] != true || result["errors"] != false {
        return Err("Codex Hooks are disabled or their configuration has errors".into());
    }
    if require_final_input && result["other_pre_tool_hooks"].as_u64() != Some(0) {
        return Err("required Codex tool.before cannot share PreToolUse with native Hooks that may rewrite checked input".into());
    }
    if replacement
        && (result["code_mode"] != false || result["other_post_tool_hooks"].as_u64() != Some(0))
    {
        return Err(
            "Codex result replacement requires code mode disabled and no other PostToolUse Hooks"
                .into(),
        );
    }
    for event in events {
        let matches = result["hooks"][*event]
            .as_array()
            .ok_or("missing Codex Hook inventory")?;
        if matches.len() != 1 {
            return Err(format!(
                "Codex {event} requires exactly one installed AW hook; run aw connect first"
            )
            .into());
        }
        let hook = &matches[0];
        if hook["enabled"] != true
            || hook["async"] != false
            || hook["matcher"]
                != if matches!(*event, "input.submit" | "turn.stop") {
                    Value::Null
                } else {
                    json!(".*")
                }
            || hook["timeoutSec"] != json!(60)
        {
            return Err(format!("Codex {event} AW hook must be enabled, synchronous, match all tools and use timeout 60").into());
        }
        if !bypass && !matches!(hook["trustStatus"].as_str(), Some("trusted" | "managed")) {
            return Err("Codex AW hook needs native trust review: open Codex /hooks and trust the exact AW definitions".into());
        }
    }
    Ok(())
}
