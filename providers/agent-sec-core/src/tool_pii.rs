//! Tool-specific PII policy; Adapter projections authorize the replaceable slot.

use crate::{
    admitted,
    config::{Config, Mode, OutputMode},
    pii::{self, Source, Verdict},
    trace_context,
};
use serde_json::{json, Value};
use std::{
    path::Path,
    time::{Duration, Instant},
};

pub(super) fn invoke(
    request: &Value,
    config: &Config,
    cli: &Path,
    socket: &Path,
    remaining: Duration,
) -> Result<serde_json::Map<String, Value>, &'static str> {
    let policy = config.pii.as_ref().ok_or("pii_not_configured")?;
    let event = &request["event"];
    let (value, source, enforcing, action) =
        match (request["operation"].as_str(), event["name"].as_str()) {
            (Some("scan_tool_input"), Some("tool.before")) => (
                event["tool"].get("input").ok_or("invalid_tool_input")?,
                Source::ToolInput,
                policy.input_mode == Mode::Block,
                "block",
            ),
            (Some("scan_tool_output"), Some("tool.after")) => {
                let enforcing = policy.output_mode == OutputMode::ReplaceResult;
                let result = event["tool"].get("result").ok_or("invalid_tool_result")?;
                (result, Source::ToolOutput, enforcing, "replace_result")
            }
            _ => return Err("unsupported_operation_event"),
        };
    if enforcing && !admitted(request, action) {
        return Err("effect_not_admitted");
    }
    let started = Instant::now();
    let trace = trace_context::from_event(event);
    let scan = |text: &str| {
        let budget = remaining
            .checked_sub(started.elapsed())
            .filter(|d| !d.is_zero())
            .ok_or("deadline_exceeded")?;
        pii::call(cli, socket, text, source, &trace, budget).map_err(|error| error.code())
    };
    let verdict = scan_value(value, &scan)?;
    if enforcing && verdict == Verdict::Risk && action == "replace_result" {
        let slot = event["tool"]["result_projection"]["text"]
            .as_str()
            .ok_or("unsupported_pii_result")?;
        let mut residual = event["tool"]["result"].clone();
        if clear_slot(&mut residual, slot) != 1 {
            return Err("unsupported_pii_result");
        }
        // Scan the retained envelope too: a credential in stderr, another block
        // or metadata must not survive a successful text-slot replacement.
        if scan_value(&residual, &scan)? == Verdict::Risk {
            return Err("unsupported_pii_result");
        }
    }
    let effect = if enforcing && verdict == Verdict::Risk {
        action
    } else {
        "observe"
    };
    if !admitted(request, effect) {
        return Err("effect_not_admitted");
    }
    let mut effect = json!({"type":effect, "reason_code": if verdict == Verdict::Risk {"pii_risk"} else {"pii_pass"}});
    if verdict == Verdict::Risk && enforcing {
        effect["text"] = json!("Sensitive data detected by AW; content withheld.");
    }
    Ok(serde_json::Map::from_iter([
        ("input_digest".into(), request["input_digest"].clone()),
        ("effects".into(), json!([effect])),
    ]))
}

fn scan_value(
    value: &Value,
    scan: &impl Fn(&str) -> Result<Verdict, &'static str>,
) -> Result<Verdict, &'static str> {
    if let Value::String(text) = value {
        return scan(text);
    }
    // Keep field/value context and non-string scalars, then preserve decoded
    // string boundaries: JSON escapes such as \n can conceal an API-key prefix.
    if scan(&value.to_string())? == Verdict::Risk {
        return Ok(Verdict::Risk);
    }
    scan_strings(value, scan)
}

fn scan_strings(
    value: &Value,
    scan: &impl Fn(&str) -> Result<Verdict, &'static str>,
) -> Result<Verdict, &'static str> {
    match value {
        Value::String(text) => return scan(text),
        Value::Array(values) => {
            for value in values {
                if scan_strings(value, scan)? == Verdict::Risk {
                    return Ok(Verdict::Risk);
                }
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                if scan(key)? == Verdict::Risk || scan_strings(value, scan)? == Verdict::Risk {
                    return Ok(Verdict::Risk);
                }
            }
        }
        _ => {}
    }
    Ok(Verdict::Pass)
}

fn clear_slot(value: &mut Value, text: &str) -> usize {
    match value {
        Value::String(s) if s == text => {
            s.clear();
            1
        }
        Value::Array(values) => values.iter_mut().map(|v| clear_slot(v, text)).sum(),
        Value::Object(values) => values.values_mut().map(|v| clear_slot(v, text)).sum(),
        _ => 0,
    }
}
