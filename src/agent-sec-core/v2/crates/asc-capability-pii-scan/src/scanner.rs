//! Aggregate v1 findings while retaining explicit input coverage.

use crate::custom;
use crate::models::{
    Coverage, CoverageStatus, PiiScanOptions, PiiScanReport, PiiSummary, ScanError, ScanStatus,
    check_deadline,
};
use crate::redact;
use crate::report::{Findings, bound_report};
use crate::rules::PiiRuleSet;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Instant;

/// Reusable in-process scanner; construct once and share across requests.
pub struct PiiScanner {
    rules: Arc<PiiRuleSet>,
}

impl PiiScanner {
    /// Compiles the shipped detector rules.
    ///
    /// # Errors
    /// Returns an input-independent error if the shipped patterns are invalid.
    pub fn new() -> Result<Self, ScanError> {
        Ok(Self::with_rules(Arc::new(PiiRuleSet::builtin()?)))
    }

    /// Uses one previously loaded, immutable collection for every request.
    pub fn with_rules(rules: Arc<PiiRuleSet>) -> Self {
        Self { rules }
    }

    /// Scans text and returns the v1 response plus completeness metadata.
    ///
    /// # Errors
    /// Returns an error for a zero byte limit or builtin matching failure.
    pub fn scan(&self, input: &str, options: &PiiScanOptions) -> Result<PiiScanReport, ScanError> {
        self.scan_with_deadline(input, options, None)
    }

    pub(crate) fn scan_with_deadline(
        &self,
        input: &str,
        options: &PiiScanOptions,
        deadline: Option<Instant>,
    ) -> Result<PiiScanReport, ScanError> {
        let started = Instant::now();
        check_deadline(deadline)?;
        if options.max_bytes == Some(0) {
            return Err(ScanError::InvalidLimit);
        }
        let bytes_scanned = options.max_bytes.unwrap_or(input.len()).min(input.len());
        let mut boundary = bytes_scanned;
        while !input.is_char_boundary(boundary) {
            boundary -= 1;
        }
        let text = &input[..boundary];
        let truncated = boundary < input.len() || options.input_truncated;
        let custom = custom::detect(text, &self.rules, deadline)?;
        let mut builtin = self.rules.builtin.detect(text, deadline).peekable();
        let mut candidates = custom.candidates.into_iter().peekable();
        let mut findings = Findings::default();
        let mut redactor = options.redact_output.then(|| redact::Text::new(text));
        let mut previous = None;
        loop {
            check_deadline(deadline)?;
            let take_builtin = match (builtin.peek(), candidates.peek()) {
                (Some(Err(_)), _) | (Some(_), None) => true,
                (Some(Ok(a)), Some(b)) => a.compare(b).is_le(),
                (None, Some(_)) => false,
                (None, None) => break,
            };
            let candidate = if take_builtin {
                builtin.next().transpose()?
            } else {
                candidates.next()
            };
            let Some(candidate) = candidate else {
                break;
            };
            let key = (candidate.span, candidate.kind);
            if previous == Some(key) {
                continue;
            }
            previous = Some(key);
            if !options.include_low_confidence && candidate.confidence < 0.5 {
                continue;
            }
            if let Some(redactor) = &mut redactor {
                redactor.push(&candidate);
            }
            findings.push(candidate, options.raw_evidence);
        }
        let mut reasons = Vec::new();
        if truncated {
            reasons.push("input_truncated".to_owned());
        }
        reasons.extend(custom.reasons.into_iter().map(str::to_owned));
        let (redacted_text, redacted_text_omitted) = redactor.map_or((None, false), |r| {
            let (text, omitted) = r.finish();
            (Some(text), omitted)
        });
        check_deadline(deadline)?;
        let mut report = PiiScanReport {
            ok: true,
            verdict: findings.verdict(),
            summary: PiiSummary {
                total: findings.total,
                findings_truncated: findings.truncated,
                redacted_text_omitted,
                by_type: findings.by_type,
                by_category: findings.by_category,
                by_severity: findings.by_severity,
                source: options.source,
                bytes_scanned: if options.input_truncated {
                    options.input_bytes_scanned.unwrap_or(bytes_scanned)
                } else {
                    bytes_scanned
                },
                truncated,
                custom_rules: custom.summary,
                execution_status: ScanStatus::Completed,
                coverage: Coverage {
                    status: if reasons.is_empty() {
                        CoverageStatus::Complete
                    } else {
                        CoverageStatus::Partial
                    },
                    reasons,
                },
                input_sha256: digest(input),
                scanned_input_sha256: digest(text),
                scanned_bytes: text.len(),
                scanner_version: crate::SCANNER_VERSION.to_owned(),
                ruleset_id: self.rules.id.clone(),
                error: None,
                error_type: None,
            },
            findings: findings.items,
            elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            redacted_text,
        };
        bound_report(&mut report);
        check_deadline(deadline)?;
        Ok(report)
    }
}

pub(crate) fn digest(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
