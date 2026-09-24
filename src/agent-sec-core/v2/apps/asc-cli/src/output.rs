//! Policy output policy; transport does not select presentation or process exit codes.

use std::io::{self, Write};

use asc_daemon_protocol::DaemonResponse;
use serde::{Deserialize, Serialize};

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
#[cfg(test)]
mod tests {
    use super::*;
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

/// Prints Skill Ledger business JSON and preserves its explicit exit code.
/// Protocol errors remain stderr diagnostics; completed risk results remain stdout JSON.
///
/// # Errors
/// Rejects malformed action envelopes and reports output write failures.
pub fn render_skill_sec(
    response: &DaemonResponse,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<u8> {
    match response {
        DaemonResponse::Error(error) => {
            writeln!(stderr, "{}", error.error.message())?;
            Ok(1)
        }
        DaemonResponse::Success(success) => {
            let value = &success.result;
            let code = value["exitCode"]
                .as_u64()
                .filter(|n| *n <= 2)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid SkillSec exit code")
                })?;
            if !value["success"].is_boolean()
                || !value["errorType"].is_string()
                || !value["data"].is_object()
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid SkillSec result",
                ));
            }
            serde_json::to_writer_pretty(&mut *stdout, &value["data"])?;
            writeln!(stdout)?;
            if let Some(warnings) = value["data"]["warnings"].as_array() {
                for warning in warnings {
                    if let Some(warning) = warning.as_str() {
                        writeln!(stderr, "{warning}")?;
                    }
                }
            }
            if let Some(error) = value["error"].as_str() {
                writeln!(stderr, "{error}")?;
            }
            Ok(u8::try_from(code).map_err(io::Error::other)?)
        }
    }
}

#[cfg(test)]
mod skill_sec_contract_tests {
    use super::*;

    #[test]
    fn frozen_consumer_samples_keep_business_json_exit_codes_and_failure_layers() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/skillsec/consumer.json")).unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let response: DaemonResponse =
                serde_json::from_value(case["response"].clone()).unwrap();
            let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
            let code = render_skill_sec(&response, &mut stdout, &mut stderr).unwrap();
            assert_eq!(
                u64::from(code),
                case["exitCode"].as_u64().unwrap(),
                "{}",
                case["name"]
            );
            let error = String::from_utf8(stderr).unwrap();
            let expected = case["stderrContains"].as_str().unwrap();
            if expected.is_empty() {
                assert!(error.is_empty(), "{}", case["name"]);
            } else {
                assert!(error.contains(expected));
            }
            if case["response"].get("result").is_some() {
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&stdout).unwrap(),
                    case["response"]["result"]["data"]
                );
            } else {
                assert!(stdout.is_empty());
            }
        }
    }
}
