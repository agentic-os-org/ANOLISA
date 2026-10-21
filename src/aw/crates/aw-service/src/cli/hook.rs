//! Bounded callback transport dispatches only dialect mapping to native adapters.

use super::{
    adapter::{self, Adapter, HookBinding, HookOutput},
    Arguments, Exit, Result,
};
use aw_service::{Client, EventHandle, Operation};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    time::{Duration, Instant},
};

pub(super) fn callback(args: &Arguments) -> Exit {
    let selected = args
        .required("--adapter")
        .and_then(|name| adapter::select(name).map_err(Into::into));
    let adapter = match selected {
        Ok(adapter) => adapter,
        Err(error) => {
            eprintln!("aw hook failed: {error}");
            return Exit::Code(1);
        }
    };
    match invoke(args, adapter) {
        Ok(exit) => exit,
        Err(error) => {
            eprintln!("aw hook failed: {error}");
            if args.flags.contains_key("--close-event") {
                return Exit::Code(1);
            }
            let blocked = args
                .flags
                .get("--event")
                .is_some_and(|v| v == "tool.before")
                && args.flags.get("--on-error").is_some_and(|v| v == "block");
            match emit(adapter.reply(blocked)) {
                Ok(exit) => exit,
                Err(error) => {
                    eprintln!("aw hook response failed: {error}");
                    Exit::Code(1)
                }
            }
        }
    }
}

fn invoke(args: &Arguments, adapter: &dyn Adapter) -> Result<Exit> {
    if args
        .flags
        .get("--step")
        .is_some_and(|step| step == aw_adapter::reload::DISPATCH)
    {
        return emit(super::dispatch::invoke(args, adapter)?);
    }
    args.check(
        &[
            "--adapter",
            "--binding",
            "--event",
            "--step",
            "--on-error",
            "--close-event",
        ],
        false,
    )?;
    let close = match args.flags.get("--close-event").map(String::as_str) {
        None => false,
        Some("true") if aw_adapter::callback::supports_event_close(args.required("--adapter")?) => {
            true
        }
        _ => return Err("event cleanup requires the AgentScope bridge".into()),
    };
    let event = args.required("--event")?;
    let step = args.required("--step")?;
    let on_error = args.required("--on-error")?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(args.required("--binding")?)?;
    let metadata = file.metadata()?;
    // SAFETY: geteuid has no arguments and does not mutate process state.
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o7777 != 0o600
        || metadata.len() > 1024 * 1024
    {
        return Err("unsafe or oversized hook binding".into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err("hook binding exceeds byte limit".into());
    }
    let binding: HookBinding = serde_json::from_slice(&bytes)?;
    if binding.adapter != args.required("--adapter")? {
        return Err("callback adapter does not match the binding".into());
    }
    let route = binding
        .events
        .get(event)
        .ok_or("event is absent from binding")?;
    if route.steps.get(step).map(String::as_str) != Some(on_error)
        || !(1..=55000).contains(&route.budget_ms)
    {
        return Err("step or budget does not match binding".into());
    }
    let deadline =
        Instant::now() + Duration::from_millis(if close { 2000 } else { route.budget_ms + 2000 });
    let input = read_input(deadline)?;
    let (normalized, value, input) = aw_adapter::callback::normalize(
        adapter,
        &binding.adapter,
        &binding.native_context(),
        event,
        input,
        route.replacement_step.is_some(),
    )?;
    let client = Client::connect(&binding.socket, deadline)?;
    if client.identity() != &binding.binding.identity {
        return Err("stale hook binding".into());
    }
    if close {
        client.call(
            Operation::CloseHookEvent {
                instance_id: binding.binding.instance_id,
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
        return Ok(Exit::Code(0));
    }
    if step == adapter::OBSERVE_STEP {
        client.call(
            Operation::ObserveHookEvent {
                instance_id: binding.binding.instance_id,
                event: normalized,
                native_input: input,
            },
            deadline,
        )?;
        return emit(adapter.reply(false));
    }
    let handle: EventHandle = serde_json::from_value(client.call(
        Operation::OpenHookEvent {
            instance_id: binding.binding.instance_id.clone(),
            event: normalized,
            native_input: input,
        },
        deadline,
    )?)?;
    let result: Result<HookOutput> = client
        .call(
            Operation::InvokeStep {
                event_id: handle.event_id.clone(),
                instance_id: binding.binding.instance_id.clone(),
                step_id: step.into(),
                native_environment: Some(std::env::vars_os().collect()),
            },
            deadline,
        )
        .map_err(Into::into)
        .and_then(|output| {
            aw_adapter::callback::response(
                adapter,
                event,
                on_error,
                output,
                &binding.adapter,
                &value,
                &mut std::io::stderr(),
            )
            .map_err(Into::into)
        });
    // SDK loops stop on vetoes or fail-closed errors; unvisited steps cannot complete the event.
    if event == "tool.before"
        && handle.steps.last().map(String::as_str) != Some(step)
        && aw_adapter::callback::stops_before(&binding.adapter, result.as_ref().ok(), on_error)
    {
        if let Err(error) = client.call(
            Operation::CloseEvent {
                event_id: handle.event_id,
                instance_id: binding.binding.instance_id,
            },
            deadline,
        ) {
            // Cleanup failure must not turn a denial into report-mode allow.
            eprintln!("aw hook event close failed: {error}");
        }
    }
    emit(result?)
}

fn emit(output: HookOutput) -> Result<Exit> {
    std::io::stdout().write_all(&output.stdout)?;
    std::io::stdout().flush()?;
    std::io::stderr().write_all(&output.stderr)?;
    std::io::stderr().flush()?;
    Ok(output.exit)
}

pub(super) fn read_input(deadline: Instant) -> Result<Vec<u8>> {
    let mut input = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        if Instant::now() >= deadline {
            return Err("hook stdin deadline exceeded".into());
        }
        let mut fd = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .clamp(1, 100) as i32;
        // SAFETY: poll borrows a valid pollfd, read writes at most the buffer length.
        let ready = unsafe { libc::poll(&mut fd, 1, millis) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if ready == 0 {
            continue;
        }
        let count =
            unsafe { libc::read(libc::STDIN_FILENO, buffer.as_mut_ptr().cast(), buffer.len()) };
        if count < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if count == 0 {
            return Ok(input);
        }
        input.extend_from_slice(&buffer[..count as usize]);
        if input.len() > 1024 * 1024 {
            return Err("native hook stdin exceeds one MiB".into());
        }
    }
}
