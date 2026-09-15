//! Aggregate v1 findings while retaining explicit input coverage.

use crate::custom;
use crate::models::{
    Candidate, Coverage, CoverageStatus, PiiFinding, PiiScanOptions, PiiScanReport, PiiSummary,
    ScanError, ScanStatus, Severity, Verdict,
};
use crate::redact;
use crate::rules::PiiRuleSet;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
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
        let started = Instant::now();
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
        let mut candidates = self.rules.builtin.detect(text)?;
        let custom = custom::detect(text, &self.rules);
        candidates.extend(custom.candidates);
        let findings = findings(candidates, options);
        let mut reasons = Vec::new();
        if truncated {
            reasons.push("input_truncated".to_owned());
        }
        reasons.extend(custom.reasons.into_iter().map(str::to_owned));
        let verdict = if findings.iter().any(|f| f.severity == Severity::Deny) {
            Verdict::Deny
        } else if findings.is_empty() {
            Verdict::Pass
        } else {
            Verdict::Warn
        };
        let mut by_type = BTreeMap::new();
        let mut by_category = BTreeMap::new();
        let mut by_severity = BTreeMap::new();
        for finding in &findings {
            *by_type.entry(finding.pii_type.clone()).or_insert(0) += 1;
            *by_category.entry(finding.category.clone()).or_insert(0) += 1;
            *by_severity
                .entry(
                    if finding.severity == Severity::Deny {
                        "deny"
                    } else {
                        "warn"
                    }
                    .into(),
                )
                .or_insert(0) += 1;
        }
        let redacted_text = options.redact_output.then(|| redact::text(text, &findings));
        Ok(PiiScanReport {
            ok: true,
            verdict,
            summary: PiiSummary {
                total: findings.len(),
                by_type,
                by_category,
                by_severity,
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
                ruleset_id: self.rules.id.clone(),
                error: None,
                error_type: None,
            },
            findings,
            elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            redacted_text,
        })
    }
}

pub(crate) fn digest(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn findings(mut candidates: Vec<Candidate>, options: &PiiScanOptions) -> Vec<PiiFinding> {
    candidates.sort_by(|a, b| {
        (a.severity != Severity::Deny)
            .cmp(&(b.severity != Severity::Deny))
            .then_with(|| b.confidence.total_cmp(&a.confidence))
            .then(a.span.start.cmp(&b.span.start))
            .then((b.span.end - b.span.start).cmp(&(a.span.end - a.span.start)))
            .then(a.kind.cmp(&b.kind))
    });
    let mut seen = BTreeSet::new();
    candidates.retain(|c| seen.insert((c.kind.clone(), c.span)));
    candidates.sort_by(|a, b| a.span.cmp(&b.span).then(a.kind.cmp(&b.kind)));
    candidates
        .into_iter()
        .filter(|c| options.include_low_confidence || c.confidence >= 0.5)
        .map(|c| {
            let evidence_redacted = redact::value(&c.value, &c.kind, &c.category);
            PiiFinding {
                pii_type: c.kind,
                category: c.category,
                severity: c.severity,
                confidence: (c.confidence * 1000.0).round_ties_even() / 1000.0,
                evidence_redacted,
                span: c.span,
                metadata: c.metadata,
                raw_evidence: options.raw_evidence.then_some(c.value),
            }
        })
        .collect()
}
