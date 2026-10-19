//! Native callback normalization and response adoption, independent of service RPCs.

use crate::{
    adapter::{self, Adapter, HookOutput},
    Exit, Result,
};
use serde_json::Value;

/// Whether the native bridge explicitly closes an abandoned event.
pub fn supports_event_close(adapter: &str) -> bool {
    adapter == "agentscope"
}

/// Normalize the callback and its optional completion frame without service state.
pub fn normalize(
    adapter: &dyn Adapter,
    adapter_id: &str,
    context: &crate::NativeContext,
    event: &str,
    mut input: Vec<u8>,
    replacement: bool,
) -> Result<(Value, Value, Vec<u8>)> {
    let mut value: Value = serde_json::from_slice(&input)?;
    let completion = if adapter_id == "openclaw" {
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
    let mut normalized = adapter.normalize(context, event, &value)?;
    if event == "tool.after" && replacement {
        if let Some(projection) = crate::projection::project(adapter_id, &value) {
            project_within_limit(&mut normalized, projection)?;
        }
    }
    if let Some(completion) = completion {
        if event != "tool.after" || !replacement {
            return Err("unexpected OpenClaw completion frame".into());
        }
        let snapshot_input = completion["snapshot"]
            .as_str()
            .ok_or("missing OpenClaw snapshot")?;
        let snapshot: Value = serde_json::from_str(snapshot_input)?;
        let mut bound = adapter.normalize(context, event, &snapshot)?;
        if let Some(projection) = crate::projection::project(adapter_id, &snapshot) {
            project_within_limit(&mut bound, projection)?;
        }
        normalized["native"]["aw_completion"] = serde_json::json!({
            "phase":completion["phase"], "snapshot":bound, "input":snapshot_input
        });
    }
    Ok((normalized, value, input))
}

/// Whether the native loop stops before visiting remaining callbacks.
pub fn stops_before(adapter: &str, output: Option<&HookOutput>, on_error: &str) -> bool {
    match (adapter, output) {
        ("pi", Some(output)) => serde_json::from_slice::<Value>(&output.stdout)
            .is_ok_and(|value| value["block"] == true),
        ("openclaw", Some(output)) => {
            if !matches!(output.exit, Exit::Code(0)) {
                return on_error == "block";
            }
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stdout = stdout.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
            if stdout.is_empty() {
                return false;
            }
            // The bridge returns native objects unchanged; its runner tests block truthiness.
            match serde_json::from_str::<Value>(stdout) {
                Ok(Value::Object(value)) => value.get("block").is_some_and(|block| match block {
                    Value::Null => false,
                    Value::Bool(value) => *value,
                    Value::Number(value) => value.as_f64() != Some(0.0),
                    Value::String(value) => !value.is_empty(),
                    Value::Array(_) | Value::Object(_) => true,
                }),
                _ => on_error == "block",
            }
        }
        ("claude-sdk" | "agentscope" | "qwenpaw", Some(output)) => {
            matches!(output.exit, Exit::Code(2))
                || (on_error == "block" && !matches!(output.exit, Exit::Code(0)))
        }
        ("pi" | "openclaw" | "claude-sdk" | "agentscope" | "qwenpaw", None) => on_error == "block",
        _ => false,
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

/// Adopt a native or Provider response using the selected Agent dialect.
pub fn response(
    adapter: &dyn Adapter,
    event: &str,
    on_error: &str,
    output: Value,
    adapter_id: &str,
    native: &Value,
    diagnostics: &mut dyn std::io::Write,
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
            diagnostics.write_all(&stderr)?;
            diagnostics.flush()?;
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

/// Recognize a native readiness degradation already covered by the admitted profile.
pub fn readiness_degraded(adapter: &str, reason: &str) -> bool {
    adapter == "openclaw" && reason == "result_middleware_unavailable"
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pi_early_close_uses_json_denial_even_with_report_errors() {
        let adapter = adapter::select("pi").unwrap();
        assert!(stops_before("pi", Some(&adapter.reply(true)), "report"));
        assert!(!stops_before("pi", Some(&adapter.reply(false)), "block"));
        assert!(stops_before("pi", None, "block"));
        assert!(!stops_before("pi", None, "report"));
    }

    #[test]
    fn openclaw_early_close_matches_native_terminal_and_failure_results() {
        for on_error in ["block", "report"] {
            for (stdout, expected) in [
                ("", false),
                (" \u{feff} ", false),
                ("{}", false),
                (r#"{"block":false}"#, false),
                (r#"{"block":true}"#, true),
                (r#"{"block":0}"#, false),
                (r#"{"block":""}"#, false),
                (r#"{"block":"native veto"}"#, true),
                (r#"{"block":[]}"#, true),
                ("invalid", on_error == "block"),
                ("null", on_error == "block"),
                ("[]", on_error == "block"),
            ] {
                let output = HookOutput {
                    exit: Exit::Code(0),
                    stdout: stdout.as_bytes().to_vec(),
                    stderr: Vec::new(),
                };
                assert_eq!(stops_before("openclaw", Some(&output), on_error), expected);
            }
            for exit in [Exit::Code(1), Exit::Code(2), Exit::Signal(15)] {
                let output = HookOutput {
                    exit,
                    stdout: br#"{"block":true}"#.to_vec(),
                    stderr: Vec::new(),
                };
                assert_eq!(
                    stops_before("openclaw", Some(&output), on_error),
                    on_error == "block"
                );
            }
            assert_eq!(
                stops_before("openclaw", None, on_error),
                on_error == "block"
            );
        }
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
            &json!({}),
            &mut Vec::new()
        )
        .is_err());
    }

    #[test]
    fn sdk_early_close_preserves_denial_exit_and_failure_policy() {
        for adapter in ["agentscope", "claude-sdk", "qwenpaw"] {
            for (exit, on_error, expected) in [
                (Exit::Code(0), "block", false),
                (Exit::Code(0), "report", false),
                (Exit::Code(2), "block", true),
                (Exit::Code(2), "report", true),
                (Exit::Code(1), "block", true),
                (Exit::Code(1), "report", false),
                (Exit::Signal(15), "block", true),
                (Exit::Signal(15), "report", false),
            ] {
                let result = HookOutput {
                    exit,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                };
                assert_eq!(stops_before(adapter, Some(&result), on_error), expected);
            }
            assert!(stops_before(adapter, None, "block"));
            assert!(!stops_before(adapter, None, "report"));
        }
    }

    #[test]
    fn agentscope_native_output_reaches_the_bridge_without_decoding() {
        let adapter = adapter::select("agentscope").unwrap();
        for stdout in [
            b"{\"action\":\"ask\"}".to_vec(),
            b"\xef\xbb\xbf{\"action\":\"approve\"}".to_vec(),
            b"\xff\xfe{\x00}\x00".to_vec(),
        ] {
            let output = json!({"status": "ok", "kind": "native", "native": {
                "stdout": stdout, "stderr": [], "exit_code": 0, "signal": null}});
            let reply = response(
                adapter,
                "tool.before",
                "block",
                output,
                "agentscope",
                &Value::Null,
                &mut Vec::new(),
            )
            .unwrap();
            assert_eq!(reply.stdout, stdout);
            assert!(matches!(reply.exit, Exit::Code(0)));
        }
    }
}
