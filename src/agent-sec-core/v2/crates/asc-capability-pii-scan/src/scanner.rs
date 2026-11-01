//! Aggregate v1 findings while retaining explicit input coverage.

use crate::custom;
use crate::models::{
    Candidate, Coverage, CoverageStatus, PiiFinding, PiiScanOptions, PiiScanReport, PiiSummary,
    ScanError, ScanStatus, check_deadline,
};
use crate::redact;
use crate::report::{Findings, bound_report};
use crate::rules::PiiRuleSet;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
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
        let mut evidence = ProtectedEvidence::default();
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
            // A provider-owned finding already excludes the generic finding for
            // the same assignment inside the detector, so overlaps with
            // independently meaningful detections such as a JWT are preserved.
            if let Some(redactor) = &mut redactor {
                redactor.push(&candidate);
            }
            evidence.push(candidate, &mut findings, options.raw_evidence);
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

/// Tracks retained findings that can overlap a later protected value.
#[derive(Default)]
struct ProtectedEvidence {
    protected_end: usize,
    active: BTreeMap<usize, Vec<usize>>,
}

impl ProtectedEvidence {
    fn push(&mut self, mut candidate: Candidate<'_>, findings: &mut Findings, raw: bool) {
        let start = candidate.span.start;
        while self
            .active
            .first_key_value()
            .is_some_and(|(end, _)| *end <= start)
        {
            self.active.pop_first();
        }
        let provider_mask = candidate.mask.filter(|_| {
            candidate.metadata.get("provider").and_then(|v| v.as_str()) == Some("alibaba_cloud")
        });
        if provider_mask.is_some() {
            // Earlier findings can enclose a provider value. Every active
            // finding overlaps it because candidates arrive in start order.
            for indices in std::mem::take(&mut self.active).into_values() {
                for index in indices {
                    "[REDACTED]".clone_into(&mut findings.items[index].evidence_redacted);
                }
            }
            self.protected_end = self.protected_end.max(candidate.span.end);
        } else if start < self.protected_end {
            candidate.mask = Some("[REDACTED]");
        }

        let compact_before = findings.is_compact();
        let count_before = findings.items.len();
        findings.push(candidate, raw);
        if !compact_before && findings.is_compact() {
            // Compaction can reindex retained findings; it happens at most once.
            self.active.clear();
            for (index, finding) in findings.items.iter().enumerate() {
                if finding.span.end > start && !whole_masked(finding) {
                    self.active.entry(finding.span.end).or_default().push(index);
                }
            }
        } else if findings.items.len() > count_before {
            let index = findings.items.len() - 1;
            let finding = &findings.items[index];
            if !whole_masked(finding) {
                self.active.entry(finding.span.end).or_default().push(index);
            }
        }
    }
}

fn whole_masked(finding: &PiiFinding) -> bool {
    finding.evidence_redacted == "[REDACTED]"
        || (finding.metadata.get("provider").and_then(|v| v.as_str()) == Some("alibaba_cloud")
            && finding.evidence_redacted.starts_with('['))
}

pub(crate) fn digest(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
