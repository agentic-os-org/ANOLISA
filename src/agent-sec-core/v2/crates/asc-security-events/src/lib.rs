//! Security-event contracts shared by the `JSONL`, `SQLite` and summary layers.
//!
//! This crate is the contract layer only: it defines the event envelope, the
//! data-directory / stream path rules and the `SQLite` schema revision registry.
//! Writing, reading and the process-wide sinks live in the sibling crates.

#![forbid(unsafe_code)]

pub mod config;
pub mod error;
mod event;
mod schema_version;
mod summary;
pub mod timestamp;

pub use error::{ConfigError, EventError, TimestampError};
pub use event::{EventResult, SecurityEvent, extract_verdict};
pub use schema_version::{
    SECURITY_EVENTS_SQLITE_SCHEMA_REVISIONS, SECURITY_EVENTS_SQLITE_SCHEMA_VERSION,
    SECURITY_EVENTS_VERDICT_SCHEMA_VERSION,
};
pub use summary::{CorrelationCandidate, SecurityEventsSummary};
