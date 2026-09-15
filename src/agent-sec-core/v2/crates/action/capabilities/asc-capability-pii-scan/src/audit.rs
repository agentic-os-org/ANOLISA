//! Explicit PII audit allowlists; client evidence and input text never pass through.

use asc_action_runtime::AuditProjector;
use asc_action_types::{ActionOutcome, AuditProjection, Failure};
use serde_json::{Map, Value, json};

use crate::scanner::digest;
use crate::{PiiScanReport, PiiScanRequest};

const AUDIT_ERROR: &str = "pii_scan error details omitted from audit";
const PARAMETER_ERROR: &str = "PII scan parameters are invalid";

/// Projects only known, safe scan fields into the common terminal event.
#[derive(Debug, Default, Clone, Copy)]
pub struct PiiAuditProjector;

impl PiiAuditProjector {
    /// Safe failure projection when an authorized request cannot be decoded.
    ///
    /// No unvalidated parameters or parser exception text are retained. The
    /// runtime supplies kernel identity and any independently normalized trace.
    pub fn invalid_parameters() -> (Failure, AuditProjection) {
        (
            Failure {
                error: Some(PARAMETER_ERROR.to_owned()),
                error_type: "invalid_parameters".to_owned(),
                exit_code: 1,
            },
            AuditProjection::Failed {
                request: Map::new(),
                error: PARAMETER_ERROR.to_owned(),
                error_type: "invalid_parameters".to_owned(),
            },
        )
    }
}

impl AuditProjector for PiiAuditProjector {
    type Request = PiiScanRequest;

    fn project(&self, request: &PiiScanRequest, outcome: &ActionOutcome) -> AuditProjection {
        let audited_request = request_fields(request);
        // Decode the owned report to discard unknown fields at every typed
        // boundary before constructing the smaller persistence projection.
        let Ok(mut report) =
            serde_json::from_value::<PiiScanReport>(Value::Object(outcome.data.clone()))
        else {
            return AuditProjection::Failed {
                request: audited_request,
                error: AUDIT_ERROR.to_owned(),
                error_type: "invalid_outcome".to_owned(),
            };
        };
        report.summary.error = report.summary.error.map(|_| AUDIT_ERROR.to_owned());
        report.summary.error_type = report
            .summary
            .error_type
            .map(|code| safe_error_code(&code).to_owned());
        let findings: Vec<_> = report
            .findings
            .iter()
            .map(|finding| {
                json!({
                    "type": finding.pii_type,
                    "category": finding.category,
                    "severity": finding.severity,
                    "confidence": finding.confidence,
                    "evidence_redacted": finding.evidence_redacted,
                    "span": finding.span,
                    "metadata": safe_metadata(&finding.metadata),
                })
            })
            .collect();
        let result = object(json!({
            "ok": report.ok,
            "verdict": report.verdict,
            "summary": report.summary,
            "findings": findings,
            "elapsed_ms": report.elapsed_ms,
        }));
        AuditProjection::Completed {
            request: audited_request,
            result,
            failure: (!outcome.success).then(|| Failure {
                error: Some(AUDIT_ERROR.to_owned()),
                error_type: safe_error_code(&outcome.error_type).to_owned(),
                exit_code: 1,
            }),
        }
    }
}

fn request_fields(request: &PiiScanRequest) -> Map<String, Value> {
    let mut fields = object(json!({
        "source": request.options.source,
        "text_length": request.text.chars().count(),
        "text_bytes": request.text.len(),
        "text_sha256": digest(&request.text),
        "max_bytes": request.options.max_bytes,
        "include_low_confidence": request.options.include_low_confidence,
        "redact_output": request.options.redact_output,
        "input_truncated": request.options.input_truncated,
    }));
    if let Some(agent_name) = &request.agent_name {
        fields.insert(
            "agent_name".to_owned(),
            json!(agent_name.chars().take(256).collect::<String>()),
        );
    }
    fields
}

fn safe_metadata(metadata: &std::collections::BTreeMap<String, Value>) -> Map<String, Value> {
    let mut safe = Map::new();
    for (key, allowed) in [
        ("detector", &["regex", "custom_rule"][..]),
        ("engine", &["regex_v1", "regex"][..]),
        (
            "context",
            &["bearer", "remote_identity", "reserved_domain"][..],
        ),
        (
            "validator",
            &[
                "pem_private_key",
                "jwt_structure",
                "luhn",
                "cn_id_checksum",
                "email_syntax",
            ][..],
        ),
    ] {
        if let Some(value) = metadata
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| allowed.contains(value))
        {
            safe.insert(key.to_owned(), json!(value));
        }
    }
    if let Some(value) = metadata.get("evidence_omitted").and_then(Value::as_bool) {
        safe.insert("evidence_omitted".to_owned(), json!(value));
    }
    // The original spelling of a matched secret field is input-derived. It is
    // useful in the client result, but unnecessary for durable audit decisions.
    safe
}

fn safe_error_code(code: &str) -> &str {
    match code {
        "invalid_limit" | "invalid_builtin" | "matching_failed" => code,
        _ => "scan_failed",
    }
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(fields) => fields,
        // All callers construct a literal JSON object.
        _ => unreachable!("audit projection fields are literal objects"),
    }
}
