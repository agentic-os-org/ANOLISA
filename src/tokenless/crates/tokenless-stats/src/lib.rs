//! Tokenless Statistics Library
//!
//! Tracks compression metrics (characters, tokens, text content)
//! for Agent hook integrations. Records before/after data for
//! schema compression, response compression, and command rewriting.

pub mod config;
pub mod diff;
pub mod home;
pub mod path_policy;
pub mod query;
pub mod record;
pub mod recorder;
pub mod sls;
pub mod tokenizer;
pub mod trace;

pub use record::{CompressionMode, OperationType, StatsRecord};

pub use recorder::{RetrieveTotals, StatsError, StatsRecorder, StatsResult, StatsSummary};

pub use query::{
    format_compare, format_compare_json, format_list, format_show, format_summary,
    format_summary_json,
};

pub use tokenizer::{Tokenizer, count_chars, estimate_tokens, estimate_tokens_from_bytes};

pub use config::TokenlessConfig;

pub use diff::{
    DiffRecords, DiffReport, DiffSort, format_diff_report, record_report, session_report,
    tool_use_report,
};

pub use home::get_home_dir;

pub use path_policy::{
    PathPolicyError, ensure_state_dir, resolve_data_dir, validate_data_dir, validate_database_path,
};

pub use sls::{SlsRecord, SlsWriter};

pub use trace::TraceContext;

/// Best-effort stderr warning whose own write failure cannot fail the
/// command. `eprintln!` panics when writing to stderr fails (a full
/// filesystem behind redirected logs, a closed descriptor), which would
/// turn a fail-soft stats or SLS warning into a process failure — the
/// stats layer must stay invisible to the compression and retrieval
/// results. The write errors are discarded: there is no fallback channel
/// to report a failed warning on, and failing the command here is
/// exactly the regression to avoid.
pub(crate) fn warn_stats(message: &str) {
    use std::io::Write;
    let mut stderr = std::io::stderr();
    let _ = stderr.write_all(message.as_bytes());
    let _ = stderr.write_all(b"\n");
    let _ = stderr.flush();
}

/// Library version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
