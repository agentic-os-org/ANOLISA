//! Model-aware session context compaction.
//!
//! Compaction reduces the provider-visible context of a long session while
//! keeping the persisted transcript complete and the session identity
//! unchanged. The transcript in [`crate::session::PersistedSession::messages`]
//! stays append-only; compaction only maintains an optional projection
//! ([`CompactionState`]) that tells the runtime to replace an already
//! summarized transcript prefix with a bounded structured snapshot.

mod boundary;
mod budget;
mod engine;
mod preflight;
mod projection;
mod runtime;
mod summarize;

pub use budget::{ContextBudget, ModelCapability};
pub use projection::CompactionState;
pub use runtime::CompactionRuntime;

pub(crate) use boundary::has_new_compactable_prefix;
pub(crate) use budget::estimate_text_tokens;
pub(crate) use engine::run_compact_cli;
pub(crate) use preflight::run_context_preflight;
pub(crate) use projection::{sanitize_loaded_state, source_digest};
pub(crate) use runtime::estimate_prefix_tokens;
