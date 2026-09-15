//! Local PII detection, independent of transport, authorization, and storage.
//!
//! Verdicts classify findings; coverage records whether they describe the whole
//! input and configured detector set. Neither grants permission to an operation.

#![forbid(unsafe_code)]

mod audit;
mod builtin;
mod custom;
mod executor;
mod models;
mod python_unicode;
mod redact;
mod rules;
mod scanner;
mod validators;

pub use audit::PiiAuditProjector;
pub use executor::{PiiScanExecutor, PiiScanRequest};
pub use models::{
    Coverage, CoverageStatus, CustomRuleStatus, CustomRuleSummary, PiiFinding, PiiScanOptions,
    PiiScanReport, PiiSummary, ScanError, ScanStatus, Severity, Source, Span, Verdict,
};
pub use rules::{DEFAULT_CUSTOM_RULES_PATH, PiiRuleSet};
pub use scanner::PiiScanner;
