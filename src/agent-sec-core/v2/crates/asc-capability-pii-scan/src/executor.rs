//! Adapt pure PII detection to the common action outcome without storing inputs.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::sync::Arc;
use std::time::Instant;

use asc_action_runtime::{CapabilityExecutor, ExecutionControl};
use asc_action_types::ActionOutcome;
use serde_json::{Map, Value};

use crate::scanner::digest;
use crate::{
    Coverage, CoverageStatus, PiiRuleSet, PiiScanReport, PiiScanRequest, PiiScanner, PiiSummary,
    ScanError, ScanStatus, Severity, Verdict,
};

// Budget pretty JSON below the 4 MiB RPC frame and 1 MiB Hook stdout buffer.
const REPORT_BYTES: usize = 512 * 1024;
const OMITTED_TEXT: &str = "[REDACTED: output size limit]";

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
                findings_truncated: false,
                redacted_text_omitted: false,
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
                scanner_version: crate::SCANNER_VERSION.to_owned(),
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
        bound_report(&mut report);
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

fn bound_report(report: &mut PiiScanReport) {
    if fits_report(report) {
        return;
    }
    // Detection, totals and full-text redaction have already finished. Preserve
    // one real finding per type/severity, including a deny even at the input tail.
    let mut seen = BTreeSet::new();
    let total = report.findings.len();
    report.findings.retain(|finding| {
        seen.insert((finding.pii_type.clone(), finding.severity == Severity::Deny))
    });
    report.summary.findings_truncated = report.findings.len() < total;
    for finding in &mut report.findings {
        if finding.raw_evidence.take().is_some() {
            finding
                .metadata
                .insert("evidence_omitted".into(), Value::Bool(true));
            report.summary.findings_truncated = true;
        }
    }
    if !fits_report(report) && report.redacted_text.is_some() {
        report.redacted_text = Some(OMITTED_TEXT.to_owned());
        report.summary.redacted_text_omitted = true;
    }
    // At most 11 builtin and 100 custom types remain, with bounded type names,
    // redacted evidence and detector-owned metadata. No input-sized field remains.
    debug_assert!(fits_report(report));
}

fn fits_report(report: &PiiScanReport) -> bool {
    serde_json::to_writer_pretty(SizeBudget(REPORT_BYTES), report).is_ok()
}

struct SizeBudget(usize);

impl Write for SizeBudget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| io::Error::other("report size limit"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
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
