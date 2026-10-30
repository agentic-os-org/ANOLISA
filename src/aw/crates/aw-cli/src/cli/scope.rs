//! Private SDK task handles stay in memory; no per-request binding files survive a worker.

use super::{
    adapter::{self, HookBinding},
    hook, Arguments, Exit, Result,
};
use aw_service::{Binding, Client, LifecyclePhase, Operation};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Open {
    pid: u32,
    cwd: PathBuf,
    environment: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reservation {
    pid: u32,
    key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ready {
    pid: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    binding: Binding,
    parent_id: String,
    cwd: PathBuf,
    pid: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Invocation {
    scope: Scope,
    native_input: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    scope: Scope,
    sequence: u64,
    call_id: String,
    tool_name: Option<String>,
    input: Option<serde_json::Value>,
}

pub(super) fn run(args: &Arguments) -> Result<Exit> {
    let started = Instant::now();
    args.check(
        &[
            "--adapter",
            "--binding",
            "--action",
            "--event",
            "--step",
            "--on-error",
        ],
        false,
    )?;
    let mut root = HookBinding::read(args.required("--binding")?)?;
    if root.adapter != args.required("--adapter")?
        || !matches!(
            root.adapter.as_str(),
            "pi" | "claude-sdk" | "qoder-sdk" | "qwenpaw"
        )
        || root.reloadable
    {
        return Err("unsupported SDK scope binding".into());
    }
    let action = args.required("--action")?;
    // SDK factories exec AW directly; forwarded handles cannot change task owner.
    // SAFETY: getppid only reads the live parent identity.
    let owner_pid = unsafe { libc::getppid() } as u32;
    if action != "invoke"
        && ["--event", "--step", "--on-error"]
            .iter()
            .any(|flag| args.flags.contains_key(*flag))
    {
        return Err("Hook route options require scope invoke".into());
    }
    let resident_policy = root.resident_policy_reload;
    let deadline = if action == "invoke" && !resident_policy {
        hook::deadline_at(args, &root, started)?
    } else if action == "invoke" {
        // Policy discovery shares the native call's transport allowance.
        started + hook::TRANSPORT_ALLOWANCE
    } else {
        Instant::now() + Duration::from_secs(if action == "open" { 30 } else { 5 })
    };
    let input = hook::read_input(deadline)?;
    let client = Client::connect(&root.socket, deadline)?;
    if client.identity() != &root.binding.identity {
        return Err("stale SDK host binding".into());
    }
    if action == "ready" {
        let ready: Ready = serde_json::from_slice(&input)?;
        if !resident_policy || ready.pid != owner_pid {
            return Err("invalid resident worker readiness".into());
        }
        client.call(
            Operation::BridgeLifecycle {
                instance_id: root.binding.instance_id,
                pid: owner_pid,
                phase: LifecyclePhase::Ready,
                key: String::new(),
            },
            deadline,
        )?;
        return Ok(Exit::Code(0));
    }
    if matches!(action, "reserve" | "release") {
        let reservation: Reservation = serde_json::from_slice(&input)?;
        if !resident_policy || reservation.pid != owner_pid || reservation.key.is_empty() {
            return Err("invalid resident policy reservation".into());
        }
        if action == "reserve" {
            client.call(
                Operation::BridgeLifecycle {
                    instance_id: root.binding.instance_id.clone(),
                    pid: owner_pid,
                    phase: LifecyclePhase::Ready,
                    key: String::new(),
                },
                deadline,
            )?;
        }
        client.call(
            Operation::BridgeLifecycle {
                instance_id: root.binding.instance_id,
                pid: owner_pid,
                phase: if action == "reserve" {
                    LifecyclePhase::RequestStart
                } else {
                    LifecyclePhase::RequestEnd
                },
                key: reservation.key,
            },
            deadline,
        )?;
        return Ok(Exit::Code(0));
    }
    if action == "open" {
        let value: Open = serde_json::from_slice(&input)?;
        if value.pid != owner_pid {
            return Err("SDK scope worker PID mismatch".into());
        }
        if !value.cwd.is_absolute() {
            return Err("SDK task cwd must be absolute".into());
        }
        let cwd = value.cwd.canonicalize()?;
        let binding: Binding = serde_json::from_value(
            client.call(
                Operation::OpenScope {
                    parent_id: root.binding.instance_id.clone(),
                    pid: value.pid,
                    cwd: cwd.to_str().ok_or("SDK task cwd must be UTF-8")?.into(),
                    environment: value
                        .environment
                        .into_iter()
                        .map(|(key, value)| (key.into(), value.into()))
                        .collect(),
                },
                deadline,
            )?,
        )?;
        let scope = Scope {
            binding,
            parent_id: root.binding.instance_id,
            cwd,
            pid: value.pid,
        };
        // A lost stdout response remains unacknowledged and expires in the service.
        serde_json::to_writer(std::io::stdout().lock(), &scope)?;
        std::io::stdout().flush()?;
        return Ok(Exit::Code(0));
    }
    let mut registration = None;
    let (scope, native) = if matches!(action, "register" | "finish") {
        let value: Registration = serde_json::from_slice(&input)?;
        registration = Some((value.sequence, value.call_id, value.tool_name, value.input));
        (value.scope, None)
    } else if action == "invoke" {
        let value: Invocation = serde_json::from_slice(&input)?;
        (value.scope, Some(value.native_input.into_bytes()))
    } else {
        (serde_json::from_slice::<Scope>(&input)?, None)
    };
    if scope.pid != owner_pid {
        return Err("SDK scope was inherited or forwarded to another worker".into());
    }
    if scope.parent_id != root.binding.instance_id
        || scope.binding.instance_id == scope.parent_id
        || scope.binding.identity != root.binding.identity
        || scope.binding.target != root.binding.target
        || !scope.cwd.is_absolute()
    {
        return Err("SDK task handle does not belong to this host".into());
    }
    match action {
        "policy" => {
            let snapshot = client.call(
                Operation::ScopePolicy {
                    instance_id: scope.binding.instance_id,
                },
                deadline,
            )?;
            serde_json::to_writer(std::io::stdout().lock(), &snapshot)?;
        }
        "enable" => {
            let result = client.call(
                Operation::EnableInvocations {
                    instance_id: scope.binding.instance_id,
                },
                deadline,
            )?;
            serde_json::to_writer(std::io::stdout().lock(), &result)?;
        }
        "register" | "finish" => {
            let (sequence, call_id, tool_name, input) = registration.ok_or("missing invocation")?;
            let operation = if action == "register" {
                Operation::RegisterInvocation {
                    instance_id: scope.binding.instance_id,
                    sequence,
                    call_id,
                    tool_name: tool_name.ok_or("missing tool name")?,
                    input: input.ok_or("missing tool input")?,
                }
            } else {
                if tool_name.is_some() || input.is_some() {
                    return Err("unexpected finish input".into());
                }
                Operation::FinishInvocation {
                    instance_id: scope.binding.instance_id,
                    sequence,
                    call_id,
                }
            };
            client.call(operation, deadline)?;
        }
        "acknowledge" => {
            client.call(
                Operation::AcknowledgeScope {
                    instance_id: scope.binding.instance_id,
                },
                deadline,
            )?;
        }
        "close" => {
            match client.call(
                Operation::ReleaseInstance {
                    instance_id: scope.binding.instance_id,
                },
                deadline,
            ) {
                // Expired handoffs may already have been reclaimed by maintenance.
                Err(aw_service::Error::Rejected(code)) if code == "unknown_instance" => {}
                result => {
                    result?;
                }
            }
        }
        "invoke" => {
            // Read routes from the immutable child Host, never the startup binding
            // or a caller-supplied route snapshot after an idle policy update.
            let deadline = if resident_policy {
                let snapshot = client.call(
                    Operation::ScopePolicy {
                        instance_id: scope.binding.instance_id.clone(),
                    },
                    deadline,
                )?;
                root.events = serde_json::from_value(snapshot["events"].clone())?;
                hook::deadline_at(args, &root, started)?
            } else {
                deadline
            };
            root.binding = scope.binding;
            root.cwd = scope.cwd;
            return hook::invoke_bound(
                args,
                adapter::select(&root.adapter)?,
                root,
                native.ok_or("missing native input")?,
                deadline,
            );
        }
        _ => return Err("unsupported SDK scope action".into()),
    }
    Ok(Exit::Code(0))
}
