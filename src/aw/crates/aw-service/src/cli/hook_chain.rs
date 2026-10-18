//! Own ordered callback execution, private bindings and event closure for native bridges.

use super::{adapter, hook, run, Arguments, Exit, Result};
use aw_adapter::{Adapter, HookOutput, NativeExit};
use aw_exec::{CommandSpec, Limits};
use aw_service::{Client, Operation};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    binding: PathBuf,
    chains: BTreeMap<String, Chain>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Chain {
    budget_ms: u64,
    steps: Vec<Step>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Step {
    id: String,
    on_error: String,
}

pub(super) fn callback(args: &Arguments, adapter_id: &str, config_env: &str) -> Result<Exit> {
    args.check(&["--event"], false)?;
    let event = args.required("--event")?;
    if !matches!(event, "tool.before" | "tool.after") {
        return Err("unsupported hook chain event".into());
    }
    let native = aw_adapter::select(adapter_id)?;
    let Some(config) = std::env::var_os(config_env) else {
        // Installed hooks remain dormant for sessions launched without AW.
        return hook::emit(native.reply(false));
    };
    run::install_signals()?;
    let output = match invoke(Path::new(&config), event, adapter_id, native) {
        Ok(output) => output,
        Err(error) => {
            eprintln!("AW {adapter_id} hook failed: {error}");
            native.reply(event == "tool.before")
        }
    };
    hook::emit(output)
}

fn invoke(
    config: &Path,
    event: &str,
    adapter_id: &str,
    native: &dyn Adapter,
) -> Result<HookOutput> {
    let config: Registration = serde_json::from_slice(&adapter::read_private_file(config)?)?;
    let binding = adapter::read_binding(&config.binding)?;
    if binding.adapter != adapter_id {
        return Err("hook chain binding has another adapter".into());
    }
    let Some(route) = binding.events.get(event) else {
        return Ok(native.reply(false));
    };
    let chain = config.chains.get(event).ok_or("hook chain is absent")?;
    if chain.budget_ms != route.budget_ms || !(1..=55000).contains(&route.budget_ms) {
        return Err("hook chain budget differs from binding".into());
    }
    let mut seen = BTreeSet::new();
    for step in &chain.steps {
        if !seen.insert(&step.id) || route.steps.get(&step.id) != Some(&step.on_error) {
            return Err("hook chain step differs from binding".into());
        }
    }
    if seen.len() != route.steps.len() {
        return Err("hook chain is incomplete".into());
    }
    let deadline = Instant::now() + Duration::from_millis(route.budget_ms + 2000);
    let input = hook::read_input(deadline)?;
    let (normalized, _, input) = aw_adapter::callback::normalize(
        native,
        adapter_id,
        &binding.native_context(),
        event,
        input,
        false,
    )?;
    let result = (|| {
        for step in &chain.steps {
            let mut args: Vec<_> = ["hook", "--adapter", adapter_id, "--binding"]
                .into_iter()
                .map(Into::into)
                .collect();
            args.push(config.binding.clone().into_os_string());
            args.extend(
                [
                    "--event",
                    event,
                    "--step",
                    &step.id,
                    "--on-error",
                    &step.on_error,
                ]
                .into_iter()
                .map(Into::into),
            );
            let command = CommandSpec {
                program: std::env::current_exe()?,
                args,
                cwd: binding.cwd.clone(),
                environment: std::env::vars_os().collect(),
            };
            let output = aw_exec::run(
                &command,
                &input,
                Limits {
                    input_bytes: 1024 * 1024,
                    stdout_bytes: 1024 * 1024,
                    stderr_bytes: 65536,
                },
                deadline,
                &run::CANCELLED,
            );
            match output {
                Ok(output) => {
                    let reply = HookOutput {
                        exit: match output.status.code() {
                            Some(code) => NativeExit::Code(code),
                            None => {
                                NativeExit::Signal(output.status.signal().ok_or("missing signal")?)
                            }
                        },
                        stdout: output.stdout,
                        stderr: output.stderr,
                    };
                    // Native veto interpretation belongs to the selected Adapter.
                    if event == "tool.before"
                        && aw_adapter::callback::stops_before(adapter_id, Some(&reply), "report")
                    {
                        return Ok(reply);
                    }
                    std::io::stderr().write_all(&reply.stderr)?;
                    if output.status.success() {
                        continue;
                    }
                    eprintln!(
                        "AW step {} failed at callback transport: nonzero exit",
                        step.id
                    );
                }
                Err(error) => {
                    eprintln!("AW step {} failed at callback transport: {error}", step.id)
                }
            }
            if event == "tool.before" && step.on_error == "block" {
                let mut reply = native.reply(true);
                aw_adapter::block_reason(adapter_id, &mut reply, "AW policy callback unavailable")?;
                return Ok(reply);
            }
        }
        Ok(native.reply(false))
    })();
    // Close skipped steps even when a callback transport timed out before replying.
    let closed: Result<()> = (|| {
        let deadline = Instant::now() + Duration::from_secs(1);
        let client = Client::connect(&binding.socket, deadline)?;
        if client.identity() != &binding.binding.identity {
            return Err("stale hook chain binding".into());
        }
        client.call(
            Operation::CloseHookEvent {
                instance_id: binding.binding.instance_id.clone(),
                event: event.into(),
                session_id: normalized["session_id"]
                    .as_str()
                    .ok_or("missing session")?
                    .into(),
                call_id: normalized["tool"]["call_id"]
                    .as_str()
                    .ok_or("missing call")?
                    .into(),
            },
            deadline,
        )?;
        Ok(())
    })();
    if let Err(error) = closed {
        eprintln!("AW {adapter_id} event cleanup failed: {error}");
    }
    result
}
