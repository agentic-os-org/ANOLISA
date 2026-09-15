//! Adapt pure PII detection to the common action outcome without storing inputs.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use asc_action_runtime::{CapabilityExecutor, ExecutionControl};
use asc_action_types::ActionOutcome;
use serde_json::{Map, Value};

use crate::scanner::digest;
use crate::{
    Coverage, CoverageStatus, PiiRuleSet, PiiScanOptions, PiiScanReport, PiiScanner, PiiSummary,
    ScanError, ScanStatus, Verdict,
};

/// One scan invocation; raw text deliberately has no diagnostic `Debug` projection.
#[derive(Clone)]
pub struct PiiScanRequest {
    /// Exact UTF-8 text received from the caller.
    pub text: String,
    /// Explicit input coverage and response options.
    pub options: PiiScanOptions,
    /// Optional business metadata, never a trusted identity.
    pub agent_name: Option<String>,
}

/// Executes against the daemon's immutable startup rule collection.
pub struct PiiScanExecutor {
    rules: Arc<PiiRuleSet>,
}

impl PiiScanExecutor {
    /// Shares already compiled rules; performs no configuration or HOME lookup.
    pub fn new(rules: Arc<PiiRuleSet>) -> Self {
        Self { rules }
    }

    fn failed_report(&self, request: &PiiScanRequest, error: ScanError) -> PiiScanReport {
        PiiScanReport {
            ok: false,
            verdict: Verdict::Error,
            summary: PiiSummary {
                total: 0,
                by_type: BTreeMap::new(),
                by_category: BTreeMap::new(),
                by_severity: BTreeMap::new(),
                source: request.options.source,
                bytes_scanned: 0,
                truncated: request.options.input_truncated
                    || request
                        .options
                        .max_bytes
                        .is_some_and(|n| n < request.text.len()),
                custom_rules: self.rules.custom_rules().clone(),
                execution_status: ScanStatus::Failed,
                coverage: Coverage {
                    status: CoverageStatus::Unavailable,
                    reasons: vec!["scan_failed".to_owned()],
                },
                input_sha256: digest(&request.text),
                // No completed detector result can attest to an examined prefix.
                scanned_input_sha256: digest(""),
                scanned_bytes: 0,
                ruleset_id: self.rules.id().to_owned(),
                error: Some(error.to_string()),
                error_type: Some(error.code().to_owned()),
            },
            findings: Vec::new(),
            elapsed_ms: 0,
            redacted_text: None,
        }
    }
}

impl CapabilityExecutor for PiiScanExecutor {
    type Request = PiiScanRequest;

    fn execute(&self, _: &ExecutionControl, request: &PiiScanRequest) -> ActionOutcome {
        let started = Instant::now();
        let scanner = PiiScanner::with_rules(Arc::clone(&self.rules));
        let mut report = scanner
            .scan(&request.text, &request.options)
            .unwrap_or_else(|error| self.failed_report(request, error));
        report.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let success = report.ok;
        ActionOutcome {
            success,
            exit_code: i64::from(!success),
            error: report.summary.error.clone(),
            error_type: report.summary.error_type.clone().unwrap_or_default(),
            data: report_object(&report),
        }
    }
}

fn report_object(report: &PiiScanReport) -> Map<String, Value> {
    // This owned DTO contains only primitives, string-keyed maps and derived
    // serializers. Value serialization has neither I/O nor fallible map keys.
    let Value::Object(data) = serde_json::to_value(report)
        .expect("PII report has only infallibly serializable owned fields")
    else {
        unreachable!("the derived PiiScanReport serializer always emits an object")
    };
    data
}
