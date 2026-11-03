//! V1 PII presentation without coupling the CLI to a detector implementation.

use std::io::{self, Write};

use asc_daemon_protocol::DaemonResponse;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::PiiOutputFormat;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PiiOutput {
    ok: bool,
    verdict: String,
    summary: Map<String, Value>,
    findings: Vec<Map<String, Value>>,
    elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    redacted_text: Option<String>,
}

/// Renders the scan result and returns zero for pass/warn/deny, one for failure.
///
/// Text findings always use redacted evidence, even with `--raw-evidence`.
///
/// # Errors
/// Returns a malformed-result, serialization, or output-write error.
pub fn render_pii_scan(
    response: &DaemonResponse,
    format: PiiOutputFormat,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<u8> {
    let DaemonResponse::Success(success) = response else {
        if let DaemonResponse::Error(error) = response {
            writeln!(stderr, "scan error: {}", error.error.message())?;
        }
        return Ok(1);
    };
    let result: PiiOutput = serde_json::from_value(success.result.clone())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid PII scan result"))?;
    if !matches!(result.verdict.as_str(), "pass" | "warn" | "deny" | "error")
        || result.ok == (result.verdict == "error")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "inconsistent PII scan result",
        ));
    }
    if let Some(custom) = result.summary.get("custom_rules")
        && custom.get("status").and_then(Value::as_str) == Some("invalid")
    {
        let code = custom
            .get("error_code")
            .and_then(Value::as_str)
            .filter(|s| s.len() <= 64 && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
            .unwrap_or("invalid_configuration");
        writeln!(stderr, "Warning: custom PII rules disabled ({code}).")?;
    }
    match format {
        PiiOutputFormat::Json => {
            serde_json::to_writer_pretty(&mut *stdout, &result)?;
            writeln!(stdout)?;
        }
        PiiOutputFormat::Text => render_text(&result, stdout)?,
    }
    if let Some(error) = result.summary.get("error").and_then(Value::as_str) {
        writeln!(stderr, "{error}")?;
    }
    Ok(u8::from(!result.ok))
}

fn render_text(result: &PiiOutput, output: &mut impl Write) -> io::Result<()> {
    writeln!(output, "Verdict: {}", result.verdict)?;
    writeln!(
        output,
        "Findings: {}",
        result
            .summary
            .get("total")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    )?;
    if result
        .summary
        .get("findings_truncated")
        .and_then(Value::as_bool)
        == Some(true)
    {
        writeln!(
            output,
            "Details: representative findings; additional details omitted"
        )?;
    }
    if let Some(source) = result.summary.get("source").and_then(Value::as_str) {
        writeln!(output, "Source: {source}")?;
    }
    if !result.findings.is_empty() {
        writeln!(output)?;
    }
    for finding in &result.findings {
        let text = |key, fallback| finding.get(key).and_then(Value::as_str).unwrap_or(fallback);
        writeln!(
            output,
            "- {} ({}, confidence={}): {}",
            text("type", "unknown"),
            text("severity", "unknown"),
            finding
                .get("confidence")
                .map_or_else(|| "?".to_owned(), Value::to_string),
            text("evidence_redacted", "[REDACTED]")
        )?;
    }
    if let Some(text) = &result.redacted_text {
        write!(output, "\nRedacted text:\n{text}\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use asc_daemon_protocol::RequestId;
    use serde_json::json;

    #[test]
    fn verdict_exit_codes_and_text_never_expose_raw_evidence() {
        for verdict in ["pass", "warn", "deny", "error"] {
            let response = DaemonResponse::success(
                RequestId::new("pii").unwrap(),
                json!({
                    "ok": verdict != "error", "verdict": verdict, "summary": {"total":1,"source":"manual"},
                    "findings": [{"type":"email","severity":"warn","confidence":0.9,
                        "evidence_redacted":"a***@company.cn","raw_evidence":"alice@company.cn"}], "elapsed_ms":0,
                }),
            );
            let mut text = Vec::new();
            assert_eq!(
                render_pii_scan(&response, PiiOutputFormat::Text, &mut text, &mut Vec::new())
                    .unwrap(),
                u8::from(verdict == "error")
            );
            let text = String::from_utf8(text).unwrap();
            assert!(text.contains("a***@company.cn"));
            assert!(!text.contains("alice@company.cn"));
            let mut json_output = Vec::new();
            render_pii_scan(
                &response,
                PiiOutputFormat::Json,
                &mut json_output,
                &mut Vec::new(),
            )
            .unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&json_output).unwrap()["verdict"],
                verdict
            );
        }
    }
}
