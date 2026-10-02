//! Cross-platform local agent session collector
//!
//! Discovers JSONL session files from well-known agent directories on the
//! local machine and converts them to ATIF format for trajectory display.
//! Works on both Linux and macOS without platform-specific code.

pub mod converter;
pub mod discovery;

pub use converter::convert_jsonl_to_atif;
pub use discovery::{LocalSession, LocalSessionsResponse, discover_local_sessions};
