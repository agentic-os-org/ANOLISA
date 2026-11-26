//! Tool-specific PII policy; Adapter projections authorize the replaceable slot.

use crate::{
    admitted,
    config::{Config, Mode, OutputMode},
    pii::{self, Source, Verdict},
    trace_context,
};
use serde_json::{json, Value};
use std::{cell::RefCell, path::Path, time::Duration};

pub(super) const WITHHELD: &str = "Sensitive data detected by AW; content withheld.";

pub(super) fn invoke(
    request: &Value,
    config: &Config,
    transport: crate::Transport,
    cli: &Path,
    socket: &Path,
    remaining: impl Fn() -> Result<Duration, &'static str>,
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
                let enforcing = matches!(
                    policy.output_mode,
                    OutputMode::ReplaceResult | OutputMode::Mask
                );
                let result = event["tool"].get("result").ok_or("invalid_tool_result")?;
                (result, Source::ToolOutput, enforcing, "replace_result")
            }
            (Some("check_final_tool_output"), Some("security.violation")) => (
                event["tool"].get("result").ok_or("invalid_tool_result")?,
                Source::ToolOutput,
                true,
                "block",
            ),
            _ => return Err("unsupported_operation_event"),
        };
    if enforcing && !admitted(request, action) {
        return Err("effect_not_admitted");
    }
    let trace = trace_context::from_event(event);
    let masking = action == "replace_result" && policy.output_mode == OutputMode::Mask;
    let first_ruleset: RefCell<Option<Option<String>>> = RefCell::new(None);
    let scan = |text: &str, redact_output: bool| {
        let budget = remaining()?;
        let report = match transport {
            crate::Transport::Cli => {
                pii::scan_cli(cli, socket, text, source, &trace, budget, redact_output)
            }
            crate::Transport::Daemon => {
                pii::scan_daemon(socket, text, source, &trace, budget, redact_output)
            }
        }
        .map_err(|error| error.code())?;
        if masking && report.ruleset_id.is_none() {
            return Err("missing_ruleset_id");
        }
        let mut first = first_ruleset.borrow_mut();
        if first.get_or_insert_with(|| report.ruleset_id.clone()) != &report.ruleset_id {
            return Err("ruleset_changed");
        }
        Ok(report)
    };
    let check = |text: &str| Ok(scan(text, false)?.verdict);
    let ScanOutcome {
        verdict,
        serialized_json_risk,
    } = scan_value(value, &check)?;
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
        let text = if action == "replace_result" {
            let slot = event["tool"]["result_projection"]["text"]
                .as_str()
                .ok_or("unsupported_pii_result")?;
            if let Some(paths) = event["tool"]["result_projection"].get("text_paths") {
                paired_replacement(
                    value,
                    slot,
                    paths,
                    policy.output_mode,
                    serialized_json_risk,
                    &check,
                    |text| scan(text, true),
                )?
            } else {
                replacement(
                    value,
                    slot,
                    policy.output_mode,
                    serialized_json_risk,
                    &check,
                    |text| scan(text, true),
                )?
            }
        } else {
            WITHHELD.into()
        };
        effect["text"] = Value::String(text);
    }
    Ok(serde_json::Map::from_iter([
        ("input_digest".into(), request["input_digest"].clone()),
        ("effects".into(), json!([effect])),
    ]))
}

// Explicit aliases must agree. The complete original is scanned in serialized
// context too: removing an alias before this check could conceal a retained risk.
fn paired_replacement(
    result: &Value,
    slot: &str,
    paths: &Value,
    mode: OutputMode,
    serialized_risk: bool,
    check: &impl Fn(&str) -> Result<Verdict, &'static str>,
    redact: impl Fn(&str) -> Result<pii::ScannedText, &'static str>,
) -> Result<String, &'static str> {
    let [a, b] = paths.as_array().ok_or("unsupported_pii_result")?.as_slice() else {
        return Err("unsupported_pii_result");
    };
    let paths = [
        a.as_str().ok_or("unsupported_pii_result")?,
        b.as_str().ok_or("unsupported_pii_result")?,
    ];
    let write = |value: &mut Value, text: &str| -> Result<(), &'static str> {
        for path in paths {
            let field = value.pointer_mut(path).ok_or("unsupported_pii_result")?;
            if !field.is_string() {
                return Err("unsupported_pii_result");
            }
            *field = json!(text);
        }
        Ok(())
    };
    if paths[0] == paths[1]
        || paths
            .iter()
            .any(|p| result.pointer(p).and_then(Value::as_str) != Some(slot))
    {
        return Err("unsupported_pii_result");
    }
    let mut retained = result.clone();
    write(&mut retained, "")?;
    if scan_value(&retained, check)?.verdict == Verdict::Risk {
        return Err("unsupported_pii_result");
    }

    let raw = result.to_string();
    let candidate = if mode == OutputMode::Mask {
        redact(slot)?.changed_redaction(slot)
    } else {
        None
    };
    let mut text = candidate.as_deref().unwrap_or(WITHHELD);
    if mode == OutputMode::Mask || serialized_risk {
        let full = redact(&raw)?;
        if full.verdict == Verdict::Risk {
            let masked = full
                .changed_redaction(&raw)
                .ok_or("unsupported_pii_result")?;
            let mut decoded = aw_provider::view::decode(masked.as_bytes())
                .map_err(|_| "unsupported_pii_result")?;
            let canonical = decoded.to_string();
            if canonical != masked {
                return Err("unsupported_pii_result");
            }
            if paths
                .iter()
                .any(|p| decoded.pointer(p).and_then(Value::as_str) != Some(text))
            {
                text = WITHHELD;
            }
            write(&mut decoded, "")?;
            if decoded != retained {
                return Err("unsupported_pii_result");
            }
        } else if serialized_risk {
            return Err("unsupported_pii_result");
        }
    }
    loop {
        write(&mut retained, text)?;
        if retained.to_string().len() <= aw_provider::MAX_MESSAGE_BYTES
            && scan_value(&retained, check)?.verdict != Verdict::Risk
        {
            return Ok(text.into());
        }
        if text == WITHHELD {
            return Err("unsupported_pii_result");
        }
        text = WITHHELD;
    }
}

fn replacement(
    result: &Value,
    slot: &str,
    mode: OutputMode,
    serialized_json_risk: bool,
    check: &impl Fn(&str) -> Result<Verdict, &'static str>,
    redact: impl Fn(&str) -> Result<pii::ScannedText, &'static str>,
) -> Result<String, &'static str> {
    let raw_json = (!result.is_string()).then(|| result.to_string());
    let whole_projection = raw_json.as_deref() == Some(slot);
    let mut envelope = if whole_projection {
        Value::Null
    } else {
        result.clone()
    };
    if !whole_projection && replace_slot(&mut envelope, slot, &Value::String(String::new())) != 1 {
        return Err("unsupported_pii_result");
    }
    if scan_value(&envelope, check)?.verdict == Verdict::Risk {
        return Err("unsupported_pii_result");
    }
    if mode != OutputMode::Mask {
        return Ok(WITHHELD.into());
    }
    let mut candidate = redact(slot)?.changed_redaction(slot);
    if let Some(text) = &candidate {
        if text.len() >= aw_provider::MAX_MESSAGE_BYTES || check(text)? == Verdict::Risk {
            candidate = None;
        }
    }
    if let Some(raw) = raw_json.filter(|_| !whole_projection) {
        // Even the fixed notice retains fields outside this slot.
        let full = redact(&raw)?;
        if serialized_json_risk && full.verdict == Verdict::Pass {
            return Err("unsupported_pii_result");
        }
        return nested_replacement(result, slot, &raw, full, candidate, check);
    }
    let Some(text) = candidate else {
        return Ok(WITHHELD.into());
    };
    if whole_projection {
        let Ok(decoded) = aw_provider::view::decode(text.as_bytes()) else {
            return Ok(WITHHELD.into());
        };
        if scan_strings(&decoded, check)? == Verdict::Risk
            || !decoded_redaction_matches(result, &decoded, &redact)?
        {
            return Ok(WITHHELD.into());
        }
    }
    Ok(text)
}

fn nested_replacement(
    result: &Value,
    slot: &str,
    raw: &str,
    full: pii::ScannedText,
    candidate: Option<String>,
    check: &impl Fn(&str) -> Result<Verdict, &'static str>,
) -> Result<String, &'static str> {
    // Mark the known unique value with null. Its first byte differs from the
    // opening quote even if another field's *key* has exactly the slot text.
    let mut marked = result.clone();
    replace_slot(&mut marked, slot, &Value::Null);
    let start = raw
        .bytes()
        .zip(marked.to_string().bytes())
        .position(|(a, b)| a != b)
        .ok_or("unsupported_pii_result")?;
    let (prefix, rest) = raw.split_at(start);
    let quoted_slot = serde_json::to_string(slot).map_err(|_| "unsupported_pii_result")?;
    let suffix = rest
        .strip_prefix(&quoted_slot)
        .ok_or("unsupported_pii_result")?;

    let full_text = if full.verdict == Verdict::Risk {
        let masked = full
            .changed_redaction(raw)
            .ok_or("unsupported_pii_result")?;
        masked
            .strip_prefix(prefix)
            .and_then(|s| s.strip_suffix(suffix))
            .ok_or("unsupported_pii_result")?;
        Some(masked)
    } else {
        None
    };
    let Some(text) = candidate else {
        return Ok(WITHHELD.into());
    };
    let quoted = serde_json::to_string(&text).map_err(|_| "unsupported_pii_result")?;
    let reconstructed = format!("{prefix}{quoted}{suffix}");
    if reconstructed.len() > aw_provider::MAX_MESSAGE_BYTES
        || full_text.is_some_and(|masked| masked != reconstructed)
        || check(&reconstructed)? == Verdict::Risk
    {
        return Ok(WITHHELD.into());
    }
    Ok(text)
}

// The candidate's decoded keys and values have passed. Check each changed
// string against redaction in its original decoded context.
fn decoded_redaction_matches(
    original: &Value,
    candidate: &Value,
    redact: &impl Fn(&str) -> Result<pii::ScannedText, &'static str>,
) -> Result<bool, &'static str> {
    match (original, candidate) {
        (Value::String(a), Value::String(b)) if a != b => {
            return Ok(redact(a)?.redaction().as_deref() == Some(b.as_str()));
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (before, after) in a.iter().zip(b) {
                if !decoded_redaction_matches(before, after, redact)? {
                    return Ok(false);
                }
            }
        }
        (Value::Object(a), Value::Object(b)) if a.len() == b.len() => {
            for (key, before) in a {
                let Some(after) = b.get(key) else {
                    return Ok(false);
                };
                if !decoded_redaction_matches(before, after, redact)? {
                    return Ok(false);
                }
            }
        }
        _ => return Ok(original == candidate),
    }
    Ok(true)
}

struct ScanOutcome {
    verdict: Verdict,
    serialized_json_risk: bool,
}

fn scan_value(
    value: &Value,
    scan: &impl Fn(&str) -> Result<Verdict, &'static str>,
) -> Result<ScanOutcome, &'static str> {
    // JSON retains field context. Decoded keys and strings preserve boundaries
    // such as newlines, tabs and anchored rules.
    let serialized_json_risk = !value.is_string() && scan(&value.to_string())? == Verdict::Risk;
    let verdict = if serialized_json_risk {
        Verdict::Risk
    } else {
        scan_strings(value, scan)?
    };
    Ok(ScanOutcome {
        verdict,
        serialized_json_risk,
    })
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

fn replace_slot(value: &mut Value, text: &str, replacement: &Value) -> usize {
    match value {
        Value::String(s) if s == text => {
            value.clone_from(replacement);
            1
        }
        Value::Array(values) => values
            .iter_mut()
            .map(|v| replace_slot(v, text, replacement))
            .sum(),
        Value::Object(values) => values
            .values_mut()
            .map(|v| replace_slot(v, text, replacement))
            .sum(),
        _ => 0,
    }
}
