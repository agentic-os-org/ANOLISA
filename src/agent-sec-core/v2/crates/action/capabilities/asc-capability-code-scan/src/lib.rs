//! Pre-execution security scanner for Bash and Python code.
//!
//! Decides whether an agent may run a snippet and reports every rule that
//! matched. Runs inside `agent-sec-daemon`; the rule set is embedded, so the
//! crate needs no filesystem access.

mod audit;
mod engine;
mod errors;
mod executor;
mod extractor;
mod findings;
mod rules;
mod scanner;

pub use audit::CodeScanAuditProjector;
pub use engine::run_regex_rules;
pub use errors::CodeScanError;
pub use executor::{CodeScanExecutor, CodeScanRequest};
pub use extractor::extract_inline_code;
pub use findings::{Finding, Verdict, build_summary, compute_verdict};
pub use rules::{Language, RuleDefinition, Severity, load_rules};
pub use scanner::{ScanResult, scan};
