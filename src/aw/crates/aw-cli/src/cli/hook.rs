//! Bounded callback transport dispatches only dialect mapping to native adapters.

use super::{
    adapter::{self, Adapter, HookBinding, HookOutput},
    Arguments, Exit, Result,
};
use aw_service::{Client, EventHandle, Operation};
use std::{
    io::Write,
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
            if args.flags.contains_key("--close-event")
                || args
                    .flags
                    .get("--on-error")
                    .is_some_and(|value| value == "withhold_result")
            {
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
    let binding = HookBinding::read(args.required("--binding")?)?;
    let deadline = deadline(args, &binding)?;
    let input = read_input(deadline)?;
    invoke_bound(args, adapter, binding, input, deadline)
}

pub(super) fn deadline(args: &Arguments, binding: &HookBinding) -> Result<Instant> {
    deadline_at(args, binding, Instant::now())
}

pub(super) const TRANSPORT_ALLOWANCE: Duration = Duration::from_secs(2);

pub(super) fn deadline_at(
    args: &Arguments,
    binding: &HookBinding,
    started: Instant,
) -> Result<Instant> {
    let event = args.required("--event")?;
    let route = binding
        .events
        .get(event)
        .ok_or("event is absent from binding")?;
    budget_deadline(
        started,
        route.budget_ms,
        args.flags.contains_key("--close-event"),
    )
}

fn budget_deadline(started: Instant, budget_ms: u64, close: bool) -> Result<Instant> {
    if !(1..=55000).contains(&budget_ms) {
        return Err("invalid event budget".into());
    }
    let deadline =
        started + TRANSPORT_ALLOWANCE + Duration::from_millis(if close { 0 } else { budget_ms });
    if deadline <= Instant::now() {
        return Err("event transport deadline expired".into());
    }
    Ok(deadline)
}

// Resident SDK callers keep transport/Provider errors visible and stop the task.
// The legacy hook command retains its configured native failure reply above.
pub(super) fn invoke_bound(
    args: &Arguments,
    adapter: &dyn Adapter,
    binding: HookBinding,
    input: Vec<u8>,
    deadline: Instant,
) -> Result<Exit> {
    let close = match args.flags.get("--close-event").map(String::as_str) {
        None => false,
        Some("true") if aw_adapter::callback::supports_event_close(args.required("--adapter")?) => {
            true
        }
        _ => return Err("unsupported event cleanup bridge".into()),
    };
    let event = args.required("--event")?;
    let step = args.required("--step")?;
    let on_error = args.required("--on-error")?;

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

#[cfg(test)]
mod deadline_tests {
    use super::*;

    #[test]
    fn discovery_consumes_one_transport_budget_without_renewal() {
        let started = Instant::now() - Duration::from_millis(1500);
        for budget in [1, 1200, 55000] {
            let deadline = budget_deadline(started, budget, false).unwrap();
            assert_eq!(deadline - started, Duration::from_millis(budget + 2000));
            assert!(started + TRANSPORT_ALLOWANCE < deadline);
            assert!(deadline < started + Duration::from_millis(budget + 3000));
        }
        assert_eq!(
            budget_deadline(started, 55000, true).unwrap(),
            started + TRANSPORT_ALLOWANCE
        );
        let expired = Instant::now() - Duration::from_secs(60);
        assert!(budget_deadline(expired, 55000, false).is_err());
        for invalid in [0, 55001] {
            assert!(budget_deadline(started, invalid, false).is_err());
        }
    }
}
