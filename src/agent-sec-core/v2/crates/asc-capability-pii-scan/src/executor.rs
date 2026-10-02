//! Adapt pure PII detection to the common action outcome without storing inputs.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use asc_action_runtime::{CapabilityExecutor, ExecutionControl};
use asc_action_types::ActionOutcome;
use serde_json::{Map, Value};

use crate::models::check_deadline;
use crate::scanner::digest;
use crate::{
    Coverage, CoverageStatus, PiiRuleSet, PiiScanReport, PiiScanRequest, PiiScanner, PiiSummary,
    ScanError, ScanStatus, Verdict,
};

const MAX_ACTIVE_SCANS: usize = 2;

/// Executes against the daemon's immutable startup rule collection.
pub struct PiiScanExecutor {
    rules: Arc<PiiRuleSet>,
    active: AtomicUsize,
}

impl PiiScanExecutor {
    /// Shares already compiled rules; performs no configuration or HOME lookup.
    pub fn new(rules: Arc<PiiRuleSet>) -> Self {
        Self {
            rules,
            active: AtomicUsize::new(0),
        }
    }

    fn acquire(&self, control: &ExecutionControl) -> Result<ScanSlot<'_>, ScanError> {
        if control.cancelled {
            return Err(ScanError::Cancelled);
        }
        check_deadline(Some(control.deadline))?;
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_ACTIVE_SCANS).then_some(active + 1)
            })
            .map_err(|_| ScanError::Busy)?;
        Ok(ScanSlot(&self.active))
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

    fn execute(&self, control: &ExecutionControl, request: &PiiScanRequest) -> ActionOutcome {
        let started = Instant::now();
        let scanner = PiiScanner::with_rules(Arc::clone(&self.rules));
        // The worker owns admission until it actually returns, including when
        // the transport has stopped waiting for its blocking task.
        let slot = self.acquire(control);
        let result = match &slot {
            Ok(_) => {
                scanner.scan_with_deadline(&request.text, &request.options, Some(control.deadline))
            }
            Err(error) => Err(*error),
        };
        let mut report = result.unwrap_or_else(|error| self.failed_report(request, error));
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

struct ScanSlot<'a>(&'a AtomicUsize);

impl Drop for ScanSlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn admission_lasts_until_worker_exit_even_after_deadline() {
        let executor = Arc::new(PiiScanExecutor::new(Arc::new(
            PiiRuleSet::builtin().unwrap(),
        )));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = Arc::clone(&executor);
        let handle = std::thread::spawn(move || {
            let control = ExecutionControl {
                deadline: Instant::now() + Duration::from_secs(60),
                cancelled: false,
            };
            let _first = worker.acquire(&control).unwrap();
            let _second = worker.acquire(&control).unwrap();
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            // Expiry does not revoke the two worker-owned guards.
            let expired = ExecutionControl {
                deadline: Instant::now(),
                cancelled: false,
            };
            assert!(matches!(
                worker.acquire(&expired),
                Err(ScanError::DeadlineExceeded)
            ));
            assert_eq!(worker.active.load(Ordering::Acquire), 2);
        });
        started_rx.recv().unwrap();
        let control = ExecutionControl {
            deadline: Instant::now() + Duration::from_secs(60),
            cancelled: false,
        };
        let request = PiiScanRequest {
            text: "private@example.org".into(),
            options: crate::PiiScanOptions::default(),
            agent_name: None,
        };
        let busy = executor.execute(&control, &request);
        assert!(!busy.success);
        assert_eq!(busy.error_type, "scan_busy");
        assert_eq!(busy.data["summary"]["coverage"]["status"], "unavailable");
        release_tx.send(()).unwrap();
        handle.join().unwrap();
        assert_eq!(executor.active.load(Ordering::Acquire), 0);
        assert!(executor.execute(&control, &request).success);
    }
}
