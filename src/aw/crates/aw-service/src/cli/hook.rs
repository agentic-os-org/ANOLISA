//! Bounded callback transport dispatches only dialect mapping to native adapters.

use super::{
    adapter::{self, Adapter, HookBinding, HookOutput},
    Arguments, Exit, Result,
};
use aw_service::{Client, EventHandle, Operation};
use serde_json::Value;
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    time::{Duration, Instant},
};

pub(super) fn callback(args: &Arguments) -> Exit {
    let selected = args.required("--adapter").and_then(adapter::select);
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
    args.check(
        &["--adapter", "--binding", "--event", "--step", "--on-error"],
        false,
    )?;
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
    let deadline = Instant::now() + Duration::from_millis(route.budget_ms + 2000);
    let input = read_input(deadline)?;
    let value: Value = serde_json::from_slice(&input)?;
    let normalized = adapter.normalize(&binding, event, &value)?;
    let client = Client::connect(&binding.socket, deadline)?;
    if client.identity() != &binding.binding.identity {
        return Err("stale hook binding".into());
    }
    let handle: EventHandle = serde_json::from_value(client.call(
        Operation::OpenHookEvent {
            instance_id: binding.binding.instance_id.clone(),
            event: normalized,
            native_input: input,
        },
        deadline,
    )?)?;
    let output = client.call(
        Operation::InvokeStep {
            event_id: handle.event_id,
            instance_id: binding.binding.instance_id,
            step_id: step.into(),
            native_environment: Some(
                std::env::vars_os()
                    .map(|(key, value)| {
                        Ok((
                            key.into_string()
                                .map_err(|_| "non-UTF-8 native environment key")?,
                            value
                                .into_string()
                                .map_err(|_| "non-UTF-8 native environment value")?,
                        ))
                    })
                    .collect::<Result<std::collections::BTreeMap<String, String>>>()?,
            ),
        },
        deadline,
    )?;
    response(adapter, event, on_error, output)
}

fn response(adapter: &dyn Adapter, event: &str, on_error: &str, output: Value) -> Result<Exit> {
    if output["status"] != "ok" {
        return Err("step execution failed; query the instance audit for metadata".into());
    }
    if output["kind"] == "native" {
        let stdout: Vec<u8> = serde_json::from_value(output["native"]["stdout"].clone())?;
        let stderr: Vec<u8> = serde_json::from_value(output["native"]["stderr"].clone())?;
        let code = output["native"]["exit_code"].as_i64();
        let signal = output["native"]["signal"].as_i64();
        let exit = match (code, signal) {
            (Some(code @ 0..=255), None) => Exit::Code(code as i32),
            (None, Some(signal @ 1..=64)) => Exit::Signal(signal as i32),
            _ => return Err("invalid native exit status".into()),
        };
        std::io::stdout().write_all(&stdout)?;
        std::io::stdout().flush()?;
        std::io::stderr().write_all(&stderr)?;
        std::io::stderr().flush()?;
        return Ok(exit);
    }
    if output["kind"] != "provider" {
        return Err("invalid step response kind".into());
    }
    let effects = output["effects"]
        .as_array()
        .ok_or("missing Provider effects")?;
    let blocked = effects.iter().any(|effect| effect["type"] == "block");
    if blocked {
        if event != "tool.before" {
            return Err("block is unsupported after a tool".into());
        }
        return emit(adapter.reply(true));
    }
    if !matches!(on_error, "report" | "block") {
        return Err("invalid failure action".into());
    }
    // Neutral output preserves the Agent's independent permission checks.
    emit(adapter.reply(false))
}

fn emit(output: HookOutput) -> Result<Exit> {
    std::io::stdout().write_all(&output.stdout)?;
    std::io::stdout().flush()?;
    std::io::stderr().write_all(&output.stderr)?;
    std::io::stderr().flush()?;
    Ok(output.exit)
}

fn read_input(deadline: Instant) -> Result<Vec<u8>> {
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
