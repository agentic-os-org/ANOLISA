//! Typed scan contracts and the compatible public result projection.

pub use asc_action_types::{PiiScanOptions, Source};

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::ops::Range;
use std::time::Instant;

/// Finding severity retained from v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Personal information or a custom warning rule.
    Warn,
    /// Credentials or a custom denial rule.
    Deny,
}

/// Classification of findings, distinct from enforcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// No detected findings in the scanned coverage.
    Pass,
    /// Warning findings only.
    Warn,
    /// At least one denial finding.
    Deny,
    /// Execution could not produce a report.
    Error,
}

/// Whether scan execution completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScanStatus {
    /// Execution produced findings and coverage.
    Completed,
    /// Execution failed.
    Failed,
}

/// Whether evidence covers all supplied text and configured rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CoverageStatus {
    /// All supplied text and valid configured rules were evaluated.
    Complete,
    /// Some input or configured detection was omitted.
    Partial,
    /// No usable scan could be performed.
    Unavailable,
}

/// Coverage is independent of a pass/warn/deny classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    /// Completeness of the evidence.
    pub status: CoverageStatus,
    /// Stable reason codes, containing no input or rule text.
    pub reasons: Vec<String>,
}

/// Startup validation state of the centralized custom configuration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CustomRuleStatus {
    /// No optional default file was configured.
    #[default]
    Absent,
    /// Every custom rule validated and compiled.
    Loaded,
    /// The entire custom collection is disabled; builtin rules remain active.
    Invalid,
}

/// Safe configuration identity and request-local custom execution counters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomRuleSummary {
    /// Startup configuration state.
    pub status: CustomRuleStatus,
    /// Number of active custom rules.
    pub rule_count: usize,
    /// Number of custom matching errors or zero-width results.
    pub runtime_error_count: usize,
    /// The scan loop reached its 200 ms allowance.
    pub budget_exhausted: bool,
    /// Additional custom findings were omitted after the first 100.
    pub truncated: bool,
    /// Hash of the configuration bytes, when read within the file size bound.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ruleset_sha256: Option<String>,
    /// Input-independent configuration error code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

/// Half-open Unicode scalar offsets, matching Python string indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Span {
    /// Inclusive character offset.
    pub start: usize,
    /// Exclusive character offset.
    pub end: usize,
}

/// Client-visible finding; raw evidence is opt-in and never audit-safe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PiiFinding {
    /// Stable builtin type or validated custom type.
    #[serde(rename = "type")]
    pub pii_type: String,
    /// `personal_data`, credential, or custom.
    pub category: String,
    /// Existing v1 severity.
    pub severity: Severity,
    /// Heuristic score rounded to three decimal places, not a probability.
    pub confidence: f64,
    /// Type-specific redacted evidence.
    pub evidence_redacted: String,
    /// Character offsets into scanned input.
    pub span: Span,
    /// Detector-owned provenance fields.
    pub metadata: BTreeMap<String, serde_json::Value>,
    /// Returned only when explicitly requested; excluded from audit projections.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_evidence: Option<String>,
}

/// Typed aggregation with additive evidence metadata under v1's summary key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PiiSummary {
    /// Number of detected findings, including omitted response details.
    pub total: usize,
    /// Response details were reduced; totals and scan coverage remain unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    pub findings_truncated: bool,
    /// The full redacted text was replaced with an output-limit placeholder.
    #[serde(default, skip_serializing_if = "is_false")]
    pub redacted_text_omitted: bool,
    /// Counts by type.
    pub by_type: BTreeMap<String, usize>,
    /// Counts by category.
    pub by_category: BTreeMap<String, usize>,
    /// Counts by severity.
    pub by_severity: BTreeMap<String, usize>,
    /// Caller-declared origin.
    pub source: Source,
    /// Legacy prefix byte counter, including a discarded partial UTF-8 tail.
    pub bytes_scanned: usize,
    /// Whether the supplied input was shortened.
    pub truncated: bool,
    /// Configuration and execution state of the centralized custom collection.
    pub custom_rules: CustomRuleSummary,
    /// Execution status independent of finding verdict.
    pub execution_status: ScanStatus,
    /// Input and detector completeness.
    pub coverage: Coverage,
    /// SHA-256 of exactly the UTF-8 text received by the scanner, before its limit.
    pub input_sha256: String,
    /// SHA-256 of exactly the UTF-8 prefix examined by detectors.
    pub scanned_input_sha256: String,
    /// Actual bytes examined, excluding any partial UTF-8 tail.
    pub scanned_bytes: usize,
    /// Detection semantics version, independent of package and schema versions.
    pub scanner_version: String,
    /// Identifies this detector revision and active rule content.
    pub ruleset_id: String,
    /// Input-independent execution failure message, absent on completion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Stable execution error code, absent on completion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
}

/// Stable public scan response, compatible with v1 Hook consumers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PiiScanReport {
    /// Whether execution produced a report, not an authorization result.
    pub ok: bool,
    /// Aggregated finding classification.
    pub verdict: Verdict,
    /// Counts and completeness metadata.
    pub summary: PiiSummary,
    /// Ordered retained details; summary counts may include omitted findings.
    pub findings: Vec<PiiFinding>,
    /// Scan duration in whole milliseconds.
    pub elapsed_ms: u64,
    /// Redacted prefix or an explicit output-limit placeholder; never audit-safe.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redacted_text: Option<String>,
}

// Serde's skip_serializing_if predicate borrows the field.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !value
}

/// Bounded, input-independent scan errors suitable for adapter projection.
#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum ScanError {
    /// A zero byte limit cannot describe a meaningful prefix.
    #[error("max_bytes must be greater than zero")]
    InvalidLimit,
    /// A shipped pattern did not compile.
    #[error("builtin PII rules are invalid")]
    InvalidBuiltin,
    /// A builtin engine failed during matching.
    #[error("builtin PII matching failed")]
    Matching,
    /// The inherited execution deadline expired between matching steps.
    #[error("PII scan deadline exceeded")]
    DeadlineExceeded,
    /// The caller had already cancelled before execution started.
    #[error("PII scan cancelled")]
    Cancelled,
    /// Both process-owned PII execution slots are occupied.
    #[error("PII scanner is busy")]
    Busy,
}

impl ScanError {
    /// Stable, input-independent code for public responses and audit projections.
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimit => "invalid_limit",
            Self::InvalidBuiltin => "invalid_builtin",
            Self::Matching => "matching_failed",
            Self::DeadlineExceeded => "scan_deadline_exceeded",
            Self::Cancelled => "scan_cancelled",
            Self::Busy => "scan_busy",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Candidate<'a> {
    pub kind: &'a str,
    pub category: &'static str,
    pub severity: Severity,
    pub confidence: f64,
    pub value: &'a str,
    pub span: Span,
    pub bytes: Range<usize>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl Candidate<'_> {
    // Equal type/spans are adjacent, with the same winning priority as V1.
    pub(crate) fn compare(&self, other: &Self) -> Ordering {
        self.span
            .cmp(&other.span)
            .then(self.kind.cmp(other.kind))
            .then((self.severity != Severity::Deny).cmp(&(other.severity != Severity::Deny)))
            .then_with(|| other.confidence.total_cmp(&self.confidence))
    }
}

pub(crate) fn check_deadline(deadline: Option<Instant>) -> Result<(), ScanError> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        Err(ScanError::DeadlineExceeded)
    } else {
        Ok(())
    }
}
