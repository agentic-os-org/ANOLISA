//! Policy output policy; transport does not select presentation or process exit codes.

use std::io::{self, Write};

use asc_daemon_protocol::DaemonResponse;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// V1-compatible code-scan result ordered for CLI JSON output.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ScanCodeOutput {
    ok: bool,
    verdict: String,
    summary: String,
    findings: Vec<ScanFindingOutput>,
    language: String,
    engine_version: String,
    elapsed_ms: u64,
}

/// V1-compatible finding ordered for CLI JSON output.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ScanFindingOutput {
    rule_id: String,
    severity: String,
    desc_zh: String,
    desc_en: String,
    evidence: Vec<String>,
}

/// Prints a Policy result to stdout or the complete daemon error to stderr.
///
/// # Errors
/// Returns output encoding or write failures, including a closed output pipe.
pub fn render_policy(
    response: &DaemonResponse,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<u8> {
    match response {
        DaemonResponse::Success(success) => {
            serde_json::to_writer_pretty(&mut *stdout, &success.result)?;
            writeln!(stdout)?;
            Ok(0)
        }
        DaemonResponse::Error(error) => {
            serde_json::to_writer(&mut *stderr, error)?;
            writeln!(stderr)?;
            Ok(1)
        }
    }
}

/// Prints the complete Binding mutation result, including a failed lifecycle.
/// GET/LIST remain successful queries even when a Binding has failed.
///
/// # Errors
/// Returns output encoding or write failures.
pub fn render_binding_mutation(
    response: &DaemonResponse,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<u8> {
    let code = render_policy(response, stdout, stderr)?;
    if let DaemonResponse::Success(success) = response
        && matches!(
            success
                .result
                .pointer("/status/phase")
                .and_then(serde_json::Value::as_str),
            Some("APPLY_FAILED" | "DELETE_FAILED")
        )
    {
        return Ok(1);
    }
    Ok(code)
}

/// Renders a V1-compatible scan result rather than the daemon envelope.
///
/// Action failures are complete scan results and remain parseable on stdout;
/// method and parameter failures are written to stderr as V1 scan errors.
///
/// # Errors
///
/// Returns an error if the daemon result lacks a boolean `ok` field or either
/// output stream rejects the rendered result.
pub fn render_scan_code(
    response: &DaemonResponse,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<u8> {
    match response {
        DaemonResponse::Success(success) => {
            // The daemon envelope stores `result` as Value, whose default map
            // representation sorts keys. Deserialize and serialize through the
            // V1 field order so CLI output remains byte-compatible.
            let result: ScanCodeOutput =
                serde_json::from_value(success.result.clone()).map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("invalid code scan result: {error}"),
                    )
                })?;
            let exit_code = u8::from(!result.ok);
            serde_json::to_writer_pretty(&mut *stdout, &result)?;
            writeln!(stdout)?;
            Ok(exit_code)
        }
        DaemonResponse::Error(error) => {
            writeln!(stderr, "scan error: {}", error.error.message())?;
            Ok(1)
        }
    }
}
/// Presentation selected by `scan-prompt --format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptOutputFormat {
    /// Pretty JSON, the scanner's own schema (default).
    Json,
    /// Human-readable summary, the V1 `--format text` layout.
    Text,
}

impl PromptOutputFormat {
    /// Parses the `--format` value; anything but `json`/`text` is a usage
    /// error, exactly like V1 (the check is case-sensitive).
    ///
    /// # Errors
    ///
    /// Returns [`crate::ScanPromptInputError::InvalidFormat`] for an unknown value.
    pub fn parse(value: &str) -> Result<Self, crate::ScanPromptInputError> {
        match value {
            "json" => Ok(Self::Json),
            "text" => Ok(Self::Text),
            other => Err(crate::ScanPromptInputError::InvalidFormat(other.to_owned())),
        }
    }
}

/// Renders a prompt-scan result rather than the daemon envelope.
///
/// A completed scan is a successful invocation regardless of its verdict —
/// deny, warn, or degraded are results the caller acts on, not command
/// failures — so the scanner's own JSON is printed verbatim. The workspace's
/// `serde_json` preserves insertion order, so the scanner's logical key order
/// survives the daemon envelope end to end. The process exit code follows
/// the V1 contract: only an `error` verdict (the scan could not complete)
/// exits 1; deny and warn stay 0.
///
/// # Errors
///
/// Returns an error if either output stream rejects the rendered result.
pub fn render_scan_prompt(
    response: &DaemonResponse,
    format: PromptOutputFormat,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<u8> {
    match response {
        DaemonResponse::Success(success) => {
            match format {
                PromptOutputFormat::Json => {
                    serde_json::to_writer_pretty(&mut *stdout, &success.result)?;
                    writeln!(stdout)?;
                }
                PromptOutputFormat::Text => {
                    write!(stdout, "{}", format_prompt_text(&success.result))?;
                }
            }
            Ok(u8::from(
                success.result.pointer("/verdict").and_then(Value::as_str) == Some("error"),
            ))
        }
        DaemonResponse::Error(error) => {
            writeln!(stderr, "scan error: {}", error.error.message())?;
            Ok(1)
        }
    }
}

/// Formats one scan result as the V1 human-readable text block.
fn format_prompt_text(result: &Value) -> String {
    let verdict = result
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_uppercase();
    let icon = match verdict.as_str() {
        "PASS" => "\u{2705}",
        "WARN" => "\u{26a0}\u{fe0f}",
        "DENY" => "\u{274c}",
        "ERROR" => "\u{1f4a5}",
        _ => "?",
    };
    let confidence = result
        .get("confidence")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let mut lines = vec![
        format!("{icon}  Verdict : {verdict}"),
        format!(
            "    Risk    : {} (score: {confidence:.3})",
            result
                .get("risk_level")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        ),
        format!(
            "    Threat  : {}",
            result
                .get("threat_type")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        ),
        format!(
            "    Summary : {}",
            result.get("summary").and_then(Value::as_str).unwrap_or("")
        ),
    ];
    if let Some(findings) = result.get("findings").and_then(Value::as_array)
        && !findings.is_empty()
    {
        lines.push("    Findings:".to_owned());
        for finding in findings {
            lines.push(format!(
                "      {} \u{2014} {}",
                finding
                    .get("rule_id")
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
                finding.get("title").and_then(Value::as_str).unwrap_or("")
            ));
            if let Some(evidence) = finding.get("evidence").and_then(Value::as_str) {
                let truncated: String = evidence.chars().take(80).collect();
                lines.push(format!("        evidence: {truncated:?}"));
            }
        }
    }
    // `elapsed_ms` is the total; break out the one-time engine init cost so a
    // slow invocation points at the rule-set compile, not the scan.
    let elapsed = result
        .get("elapsed_ms")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let engine_init = result
        .get("engine_init_ms")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    if engine_init > 0.0 {
        lines.push(format!(
            "    Elapsed : {elapsed} ms (engine init {engine_init}, scan {})",
            result.get("scan_ms").and_then(Value::as_f64).unwrap_or(0.0)
        ));
    } else {
        lines.push(format!("    Elapsed : {elapsed} ms"));
    }
    format!("{}\n", lines.join("\n"))
}

/// Modes whose warmup reaches a model service: standard/strict probe the L2
/// model, `multi_turn` the fixed L4 one. fast builds the L1 rule engine only, so
/// its completion message states that no model was checked.
const MODEL_BACKED_MODES: [&str; 3] = ["standard", "strict", "multi_turn"];

/// Renders a warmup probe result with the V1 message contract.
///
/// The probe is a single readiness check, not a scan: success prints one
/// completion line whose wording depends on whether the mode reaches a model
/// at all, and an unavailable model is a failed check on stderr with exit 1.
///
/// # Errors
///
/// Returns an error if either output stream rejects the rendered message.
pub fn render_prompt_warmup(
    response: &DaemonResponse,
    mode: &str,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<u8> {
    match response {
        DaemonResponse::Success(success) => {
            if success.result.pointer("/ready").and_then(Value::as_bool) == Some(true) {
                if MODEL_BACKED_MODES.contains(&mode) {
                    writeln!(stdout, "Check complete. Ollama can serve the model.")?;
                } else {
                    writeln!(
                        stdout,
                        "Check complete. {mode} mode runs rules only; no model was checked."
                    )?;
                }
                Ok(0)
            } else {
                let reason = success
                    .result
                    .pointer("/error")
                    .and_then(Value::as_str)
                    .unwrap_or("the model service is not ready");
                writeln!(stderr, "Model check failed: {reason}")?;
                Ok(1)
            }
        }
        DaemonResponse::Error(error) => {
            writeln!(stderr, "Model check failed: {}", error.error.message())?;
            Ok(1)
        }
    }
}

/// Warns when a multi-turn scan produced no layer result, mirroring the V1
/// diagnostic: L4 is the only layer of that mode, so an empty `layer_results`
/// means the scan never ran to a verdict the caller can trust.
///
/// # Errors
///
/// Returns an error if stderr rejects the warning.
pub fn warn_multi_turn_incomplete(
    response: &DaemonResponse,
    stderr: &mut impl Write,
) -> io::Result<()> {
    let DaemonResponse::Success(success) = response else {
        return Ok(());
    };
    let layers_ran = success
        .result
        .get("layer_results")
        .and_then(Value::as_array)
        .is_some_and(|layers| !layers.is_empty());
    if !layers_ran {
        writeln!(
            stderr,
            "Warning: no detection layer ran \u{2014} the multi-turn scan did not \
             complete (check that Ollama is reachable). Treat the verdict as unknown."
        )?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    fn prompt_response(result: &serde_json::Value) -> DaemonResponse {
        serde_json::from_value(serde_json::json!({
            "requestId": "10000000-0000-4000-8000-000000000001",
            "result": result
        }))
        .unwrap()
    }

    #[test]
    fn a_deny_verdict_is_a_completed_scan_and_exits_zero() {
        let response = prompt_response(&serde_json::json!({
            "verdict": "deny",
            "layer_results": [{"layer": "rule_engine", "detected": true}]
        }));
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let code = render_scan_prompt(
            &response,
            PromptOutputFormat::Json,
            &mut stdout,
            &mut stderr,
        )
        .unwrap();
        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        // The result is printed verbatim, preserving the scanner's key order.
        let rendered = String::from_utf8(stdout).unwrap();
        assert!(rendered.contains("\"verdict\": \"deny\""));
    }

    #[test]
    fn an_error_verdict_exits_one_like_the_v1_cli() {
        let response = prompt_response(&serde_json::json!({
            "verdict": "error",
            "layer_results": []
        }));
        let code = render_scan_prompt(
            &response,
            PromptOutputFormat::Json,
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(code, 1);
    }

    #[test]
    fn the_text_format_renders_the_v1_layout() {
        let response = prompt_response(&serde_json::json!({
            "verdict": "deny",
            "risk_level": "high",
            "confidence": 0.87,
            "threat_type": "prompt_injection",
            "summary": "Injection attempt",
            "findings": [{
                "rule_id": "INJ-001",
                "title": "Ignore instruction",
                "evidence": "ignore the system prompt"
            }],
            "layer_results": [{"layer": "rule_engine", "detected": true}],
            "elapsed_ms": 12.0,
            "engine_init_ms": 3.0,
            "scan_ms": 9.0,
        }));
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let code = render_scan_prompt(
            &response,
            PromptOutputFormat::Text,
            &mut stdout,
            &mut stderr,
        )
        .unwrap();
        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).unwrap();
        assert!(rendered.contains("Verdict : DENY"));
        assert!(rendered.contains("Risk    : high (score: 0.870)"));
        assert!(rendered.contains("Threat  : prompt_injection"));
        assert!(rendered.contains("INJ-001"));
        assert!(rendered.contains("engine init 3"));
    }

    #[test]
    fn a_scan_error_response_goes_to_stderr_and_exits_one() {
        let response: DaemonResponse = serde_json::from_value(serde_json::json!({
            "requestId": "10000000-0000-4000-8000-000000000001",
            "error": {"code": "invalid_argument", "message": "invalid mode 'turbo'"}
        }))
        .unwrap();
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let code = render_scan_prompt(
            &response,
            PromptOutputFormat::Json,
            &mut stdout,
            &mut stderr,
        )
        .unwrap();
        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let stderr = String::from_utf8(stderr).unwrap();
        assert!(stderr.contains("scan error: invalid mode 'turbo'"));
    }

    #[test]
    fn a_ready_warmup_in_a_model_backed_mode_completes_the_check() {
        let response = prompt_response(&serde_json::json!({"ok": true, "ready": true}));
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let code = render_prompt_warmup(&response, "standard", &mut stdout, &mut stderr).unwrap();
        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).unwrap();
        assert_eq!(rendered, "Check complete. Ollama can serve the model.\n");
        // multi_turn probes the fixed L4 model, so it shares the message.
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        render_prompt_warmup(&response, "multi_turn", &mut stdout, &mut stderr).unwrap();
        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            "Check complete. Ollama can serve the model.\n"
        );
    }

    #[test]
    fn a_ready_warmup_in_fast_mode_reports_a_rules_only_check() {
        let response = prompt_response(&serde_json::json!({"ok": true, "ready": true}));
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let code = render_prompt_warmup(&response, "fast", &mut stdout, &mut stderr).unwrap();
        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            "Check complete. fast mode runs rules only; no model was checked.\n"
        );
    }

    #[test]
    fn an_unavailable_model_fails_the_check_on_stderr() {
        let response = prompt_response(&serde_json::json!({
            "ok": false,
            "ready": false,
            "error": "ollama at http://127.0.0.1:11434 is unreachable"
        }));
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let code = render_prompt_warmup(&response, "standard", &mut stdout, &mut stderr).unwrap();
        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "Model check failed: ollama at http://127.0.0.1:11434 is unreachable\n"
        );
    }

    #[test]
    fn a_protocol_error_fails_the_check_like_the_v1_native_exception() {
        let response: DaemonResponse = serde_json::from_value(serde_json::json!({
            "requestId": "10000000-0000-4000-8000-000000000001",
            "error": {"code": "invalid_argument", "message": "invalid mode 'turbo'"}
        }))
        .unwrap();
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let code = render_prompt_warmup(&response, "standard", &mut stdout, &mut stderr).unwrap();
        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        let stderr = String::from_utf8(stderr).unwrap();
        assert_eq!(stderr, "Model check failed: invalid mode 'turbo'\n");
    }

    #[test]
    fn an_empty_layer_result_warns_for_multi_turn_scans() {
        let response = prompt_response(&serde_json::json!({
            "verdict": "error",
            "layer_results": []
        }));
        let mut stderr = Vec::new();
        warn_multi_turn_incomplete(&response, &mut stderr).unwrap();
        let stderr = String::from_utf8(stderr).unwrap();
        assert!(stderr.contains("no detection layer ran"));
        // A scan that ran a layer stays silent.
        let response = prompt_response(&serde_json::json!({
            "verdict": "pass",
            "layer_results": [{"layer": "multi_turn_intent", "detected": false}]
        }));
        let mut stderr = Vec::new();
        warn_multi_turn_incomplete(&response, &mut stderr).unwrap();
        assert!(stderr.is_empty());
    }

    #[test]
    fn scheduling_errors_and_binding_reasons_preserve_wire_output() {
        let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../../../fixtures/reconciliation/admission-wire.json"
        ))
        .unwrap();
        for case in cases {
            let response: DaemonResponse = serde_json::from_value(
                serde_json::json!({"requestId":"10000000-0000-4000-8000-000000000001", "result":case["binding"]})
            ).unwrap();
            for mutation in [false, true] {
                let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
                let code = if mutation {
                    render_binding_mutation(&response, &mut stdout, &mut stderr)
                } else {
                    render_policy(&response, &mut stdout, &mut stderr)
                }
                .unwrap();
                assert_eq!(code, u8::from(mutation));
                assert!(stderr.is_empty());
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&stdout).unwrap(),
                    case["binding"]
                );
            }
            let mut pending = case["binding"].clone();
            pending["status"] = serde_json::json!({"phase":"PENDING_APPLY"});
            let response: DaemonResponse = serde_json::from_value(
                serde_json::json!({"requestId":"10000000-0000-4000-8000-000000000001", "result":pending})
            ).unwrap();
            assert_eq!(
                render_binding_mutation(&response, &mut Vec::new(), &mut Vec::new()).unwrap(),
                0
            );
        }
    }
}
