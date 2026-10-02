//! Bound retained response details independently of complete detection counts.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};

use serde_json::Value;

use crate::models::{Candidate, PiiFinding, PiiScanReport, Severity, Verdict};
use crate::redact;

// Pretty JSON must fit below both the RPC frame and Hook stdout limits.
pub(crate) const REPORT_BYTES: usize = 512 * 1024;
pub(crate) const OMITTED_TEXT: &str = "[REDACTED: output size limit]";

#[derive(Default)]
pub(crate) struct Findings {
    pub(crate) total: usize,
    pub(crate) by_type: BTreeMap<String, usize>,
    pub(crate) by_category: BTreeMap<String, usize>,
    pub(crate) by_severity: BTreeMap<String, usize>,
    pub(crate) items: Vec<PiiFinding>,
    pub(crate) truncated: bool,
    bytes: usize,
    compact: bool,
    seen: BTreeSet<(String, bool)>,
}

impl Findings {
    pub(crate) fn verdict(&self) -> Verdict {
        if self.by_severity.contains_key("deny") {
            Verdict::Deny
        } else if self.total > 0 {
            Verdict::Warn
        } else {
            Verdict::Pass
        }
    }

    pub(crate) fn push(&mut self, candidate: Candidate<'_>, raw: bool) {
        self.total += 1;
        *self.by_type.entry(candidate.kind.to_owned()).or_insert(0) += 1;
        *self
            .by_category
            .entry(candidate.category.to_owned())
            .or_insert(0) += 1;
        *self
            .by_severity
            .entry(
                if candidate.severity == Severity::Deny {
                    "deny"
                } else {
                    "warn"
                }
                .to_owned(),
            )
            .or_insert(0) += 1;
        let key = (
            candidate.kind.to_owned(),
            candidate.severity == Severity::Deny,
        );
        if self.compact && self.seen.contains(&key) {
            self.truncated = true;
            return;
        }
        // Do not clone a large match merely to discover it cannot fit.
        if raw && candidate.value.len() > REPORT_BYTES.saturating_sub(self.bytes) {
            self.compact();
        }
        let mut finding = PiiFinding {
            pii_type: candidate.kind.to_owned(),
            category: candidate.category.to_owned(),
            severity: candidate.severity,
            confidence: (candidate.confidence * 1000.0).round_ties_even() / 1000.0,
            evidence_redacted: redact::value(candidate.value, candidate.kind, candidate.category),
            span: candidate.span,
            metadata: candidate.metadata,
            raw_evidence: (raw && !self.compact).then(|| candidate.value.to_owned()),
        };
        if raw && self.compact {
            finding
                .metadata
                .insert("evidence_omitted".into(), Value::Bool(true));
            self.truncated = true;
        }
        let mut budget = SizeBudget(REPORT_BYTES.saturating_sub(self.bytes));
        if !self.compact && serde_json::to_writer_pretty(&mut budget, &finding).is_ok() {
            self.bytes = REPORT_BYTES - budget.0;
            self.items.push(finding);
            return;
        }
        self.compact();
        self.truncated |= omit_raw(&mut finding);
        if self.seen.insert(key) {
            self.items.push(finding);
        } else {
            self.truncated = true;
        }
    }

    fn compact(&mut self) {
        if self.compact {
            return;
        }
        self.compact = true;
        let total = self.items.len();
        self.items.retain(|f| {
            self.seen
                .insert((f.pii_type.clone(), f.severity == Severity::Deny))
        });
        self.truncated |= self.items.len() < total;
        for finding in &mut self.items {
            self.truncated |= omit_raw(finding);
        }
    }
}

fn omit_raw(finding: &mut PiiFinding) -> bool {
    if finding.raw_evidence.take().is_some() {
        finding
            .metadata
            .insert("evidence_omitted".into(), Value::Bool(true));
        true
    } else {
        false
    }
}

pub(crate) fn bound_report(report: &mut PiiScanReport) {
    if fits_report(report) {
        return;
    }
    let mut seen = BTreeSet::new();
    let total = report.findings.len();
    report
        .findings
        .retain(|f| seen.insert((f.pii_type.clone(), f.severity == Severity::Deny)));
    report.summary.findings_truncated |= report.findings.len() < total;
    for finding in &mut report.findings {
        report.summary.findings_truncated |= omit_raw(finding);
    }
    if !fits_report(report) && report.redacted_text.is_some() {
        report.redacted_text = Some(OMITTED_TEXT.to_owned());
        report.summary.redacted_text_omitted = true;
    }
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
