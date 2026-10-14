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
    let mut input = read_input(deadline)?;
    let mut value: Value = serde_json::from_slice(&input)?;
    let completion = if binding.adapter == "openclaw" {
        value.get("aw_openclaw_completion").cloned()
    } else {
        None
    };
    if completion.is_some() {
        input = value["native_input"]
            .as_str()
            .ok_or("missing OpenClaw native input")?
            .as_bytes()
            .to_vec();
        value = serde_json::from_slice(&input)?;
    }
    let mut normalized = adapter.normalize(&binding, event, &value)?;
    if event == "tool.after" && route.replacement_step.is_some() {
        if let Some(projection) = super::projection::project(&binding.adapter, &value) {
            project_within_limit(&mut normalized, projection)?;
        }
    }
    if let Some(completion) = completion {
        if event != "tool.after" || route.replacement_step.is_none() {
            return Err("unexpected OpenClaw completion frame".into());
        }
        let snapshot_input = completion["snapshot"]
            .as_str()
            .ok_or("missing OpenClaw snapshot")?;
        let snapshot: Value = serde_json::from_str(snapshot_input)?;
        let mut bound = adapter.normalize(&binding, event, &snapshot)?;
        if let Some(projection) = super::projection::project(&binding.adapter, &snapshot) {
            project_within_limit(&mut bound, projection)?;
        }
        normalized["native"]["aw_completion"] = serde_json::json!({
            "phase":completion["phase"], "snapshot":bound, "input":snapshot_input
        });
    }
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
        .and_then(|output| response(adapter, event, on_error, output, &binding.adapter, &value));
    // SDK loops stop on vetoes or fail-closed errors; unvisited steps cannot complete the event.
    if matches!(binding.adapter.as_str(), "pi" | "claude-sdk")
        && event == "tool.before"
        && handle.steps.last().map(String::as_str) != Some(step)
        && stops_before(&binding.adapter, &result, on_error)
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

fn stops_before(adapter: &str, result: &Result<HookOutput>, on_error: &str) -> bool {
    match result {
        Ok(output) if adapter == "claude-sdk" => {
            matches!(output.exit, Exit::Code(2))
                || (on_error == "block" && !matches!(output.exit, Exit::Code(0)))
        }
        Ok(output) => serde_json::from_slice::<Value>(&output.stdout)
            .is_ok_and(|value| value["block"] == true),
        Err(_) => on_error == "block",
    }
}

fn project_within_limit(event: &mut Value, projection: Value) -> Result<()> {
    event["tool"]["result_projection"] = projection;
    if serde_json::to_vec(event)?.len() > aw_provider::MAX_MESSAGE_BYTES {
        event["tool"]
            .as_object_mut()
            .ok_or("missing tool")?
            .remove("result_projection");
    }
    Ok(())
}

fn response(
    adapter: &dyn Adapter,
    event: &str,
    on_error: &str,
    output: Value,
    adapter_id: &str,
    native: &Value,
) -> Result<HookOutput> {
    if !matches!(on_error, "report" | "block") {
        return Err("invalid failure action".into());
    }
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
        let claude_before = adapter_id == "claude-sdk" && event == "tool.before";
        // Validate before success so fail-closed vetoes use the existing event closure.
        if claude_before {
            std::io::stderr().write_all(&stderr)?;
            std::io::stderr().flush()?;
            if code == Some(2) {
                // Native exit 2 explicitly designates stderr as the blocking reason.
                let mut reply = adapter.reply(true);
                let reason = String::from_utf8_lossy(&stderr);
                if !reason.trim().is_empty() {
                    adapter::block_reason(adapter_id, &mut reply, reason.trim())?;
                }
                return Ok(reply);
            }
        }
        if claude_before && code == Some(0) {
            let value: Value =
                serde_json::from_slice(if stdout.iter().all(u8::is_ascii_whitespace) {
                    b"{}"
                } else {
                    &stdout
                })?;
            let specific = &value["hookSpecificOutput"];
            if specific["hookEventName"] == "PreToolUse" && specific["permissionDecision"] == "deny"
            {
                let mut reply = adapter.reply(true);
                if let Some(reason) = specific["permissionDecisionReason"]
                    .as_str()
                    .filter(|reason| !reason.is_empty())
                {
                    adapter::block_reason(adapter_id, &mut reply, reason)?;
                }
                return Ok(reply);
            }
            if value != serde_json::json!({}) {
                return Err("invalid_native_response".into());
            }
        }
        if adapter_id == "pi" {
            validate_pi_native(event, &exit, &stdout)?;
        }
        return Ok(HookOutput {
            exit,
            stdout,
            stderr: if claude_before { Vec::new() } else { stderr },
        });
    }
    if output["kind"] != "provider" {
        return Err("invalid step response kind".into());
    }
    let effects = output["effects"]
        .as_array()
        .ok_or("missing Provider effects")?;
    if let Some(effect) = effects
        .iter()
        .find(|effect| effect["type"] == "replace_result")
    {
        // Observe effects already reached the journal and need no native action.
        if event != "tool.after"
            || effects
                .iter()
                .filter(|effect| effect["type"] == "replace_result")
                .count()
                != 1
        {
            return Err("invalid result replacement".into());
        }
        return super::projection::reply(
            adapter_id,
            native,
            effect["text"].as_str().ok_or("missing replacement text")?,
        );
    }
    let blocked = effects.iter().any(|effect| effect["type"] == "block");
    if blocked {
        if event != "tool.before" {
            return Err("block is unsupported after a tool".into());
        }
        let mut reply = adapter.reply(true);
        if let Some(text) = effects
            .iter()
            .find(|effect| effect["type"] == "block")
            .and_then(|effect| effect["text"].as_str())
        {
            super::adapter::block_reason(adapter_id, &mut reply, text)?;
        }
        return Ok(reply);
    }
    // Neutral output preserves the Agent's independent permission checks.
    Ok(adapter.reply(false))
}

// Validate raw native output before returning to the SDK so an invalid directive
// and a nonzero/signal exit share the same error action and early-close path.
fn validate_pi_native(event: &str, exit: &Exit, stdout: &[u8]) -> Result<()> {
    if !matches!(exit, Exit::Code(0)) {
        return Err("Pi native callback must exit zero".into());
    }
    let value: Value = if stdout.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_slice(stdout)?
    };
    let object = value
        .as_object()
        .ok_or("Pi native directive must be an object")?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "block" | "reason"))
        || object.get("block").is_some_and(|value| !value.is_boolean())
        || object.get("reason").is_some_and(|value| !value.is_string())
        || (event == "tool.after" && value["block"] == true)
    {
        return Err("unsupported Pi native directive".into());
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pi_early_close_uses_json_denial_even_with_report_errors() {
        let adapter = adapter::select("pi").unwrap();
        assert!(stops_before("pi", &Ok(adapter.reply(true)), "report"));
        assert!(!stops_before("pi", &Ok(adapter.reply(false)), "block"));
        assert!(stops_before("pi", &Err("failure".into()), "block"));
        assert!(!stops_before("pi", &Err("failure".into()), "report"));
    }

    #[test]
    fn pi_native_validation_cannot_bypass_early_close_or_inject_replacement() {
        for valid in [
            b"".as_slice(),
            br#"{}"#,
            br#"{"block":true,"reason":"retry"}"#,
        ] {
            assert!(validate_pi_native("tool.before", &Exit::Code(0), valid).is_ok());
        }
        for invalid in [
            b" ".as_slice(),
            b"invalid",
            b"null",
            b"[]",
            br#"{"block":"yes"}"#,
            br#"{"reason":null}"#,
            br#"{"aw_replacement":{"content":[]}}"#,
            br#"{"approval":true}"#,
        ] {
            assert!(validate_pi_native("tool.before", &Exit::Code(0), invalid).is_err());
        }
        assert!(validate_pi_native("tool.after", &Exit::Code(0), br#"{"block":true}"#).is_err());
        assert!(validate_pi_native("tool.before", &Exit::Code(2), b"{}").is_err());
        assert!(validate_pi_native("tool.before", &Exit::Signal(15), b"{}").is_err());
        let output = json!({"status":"ok","kind":"native","native":{"stdout":br#"{"aw_replacement":{}}"#.to_vec(),"stderr":[],"exit_code":0,"signal":null}});
        assert!(response(
            adapter::select("pi").unwrap(),
            "tool.after",
            "report",
            output,
            "pi",
            &json!({})
        )
        .is_err());
    }
}
