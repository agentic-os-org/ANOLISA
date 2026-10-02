//! Content-domain compressor engines used by the Tokenless Runtime.
//!
//! Engines return complete, stateless outcomes. Runtime owns content routing,
//! final arbitration, and Stash commit or rollback.

mod build_log;
mod html;
mod json;
mod search_results;
mod tabular;
mod terminal_cleanup;

pub use build_log::{BuildLogCompressor, BuildLogMetrics, BuildLogOperation, BuildLogOutcome};
pub use html::{HtmlExtractor, HtmlView};
pub use json::{
    JsonCompressionConfig, JsonCompressionContext, JsonCompressor, JsonError, JsonMetrics,
    JsonOperation, JsonOutcome, Recoverability,
};
pub use search_results::SearchResultsCompressor;
pub use tabular::{TabularCompressor, TabularMetrics, TabularOperation, TabularOutcome};
pub use tokenless_protocol::RecoveryMethod;
