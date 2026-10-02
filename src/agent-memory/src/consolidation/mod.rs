//! Consolidation — automatic memory extraction from session audit logs.
//!
//! When an MCP session ends (SIGTERM / ctrl_c), this module analyses the
//! `log.jsonl` and extracts atomic facts (L1 memories) via heuristic
//! rules — zero LLM calls, pure pattern matching.

pub mod episode;
pub mod fact;
pub mod heuristics;
pub mod quality;
pub mod writer;

pub use episode::{Episode, extract_episodes};
pub use fact::{ConsolidatedFact, FactCategory};
pub use heuristics::{OwnedAuditEntry, run_consolidation, run_consolidation_owned};
pub use writer::FactWriter;
