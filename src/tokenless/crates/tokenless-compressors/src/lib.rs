//! Content-domain compressor engines used by the Tokenless Runtime.
//!
//! Engines return complete, stateless outcomes. Runtime owns content routing,
//! final arbitration, and Stash commit or rollback.

mod browser_snapshot;
mod build_log;
mod html;
mod json;
mod search_results;
mod tabular;
mod terminal_cleanup;

pub use browser_snapshot::{
    BrowserSnapshotCompressor, fold_advance, is_cdt_listing, is_pw_element_line, name_continues,
    pw_element_ref,
};
pub use build_log::{
    BuildLogCompressor, BuildLogMetrics, BuildLogMode, BuildLogOperation, BuildLogOutcome,
};
pub use html::{HtmlExtractor, HtmlView};
pub use json::{
    JsonCompressionConfig, JsonCompressionContext, JsonCompressor, JsonError, JsonMetrics,
    JsonOperation, JsonOutcome, Recoverability, numbers_are_exact,
};
pub use search_results::SearchResultsCompressor;
pub use tabular::{TabularCompressor, TabularMetrics, TabularOperation, TabularOutcome};
pub use terminal_cleanup::clean_terminal;
pub use tokenless_protocol::RecoveryMethod;
