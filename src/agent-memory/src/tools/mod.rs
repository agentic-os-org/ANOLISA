//! Tier A file tools — the "pen and notebook" given to the agent.
//!
//! All tools share the same shape:
//! - Take a `&MemoryService` plus path arguments
//! - Resolve every path through `ns::paths::resolve_path` (sandbox)
//! - Perform real IO under the namespace mount
//! - Emit one `AuditEntry` per call (success or failure)

pub mod append;
pub mod diff;
pub mod edit;
pub mod grep;
pub mod list;
pub mod mem_log;
pub mod mem_revert;
pub mod mem_snapshot;
pub mod mem_snapshot_list;
pub mod mem_snapshot_restore;
pub mod memory_export;
pub mod memory_get_context;
pub mod memory_import;
pub mod memory_observe;
pub mod memory_search;
pub mod memory_sovereignty;
pub mod memory_task;
pub mod mkdir;
pub mod promote;
pub mod read;
pub mod remove;
pub mod session_history;
pub mod session_log;
pub mod write;

pub use append::append;
pub use diff::diff;
pub use edit::edit;
pub use grep::{GrepHit, GrepOptions, grep};
pub use list::{ListEntry, ListOptions, list};
pub use mem_log::mem_log;
pub use mem_revert::mem_revert;
pub use mem_snapshot::snapshot;
pub use mem_snapshot_list::snapshot_list;
pub use mem_snapshot_restore::snapshot_restore;
pub use memory_get_context::memory_get_context;
pub use memory_observe::memory_observe;
pub use memory_search::memory_search;
pub use mkdir::mkdir;
pub use promote::promote;
pub use read::read;
pub use remove::remove;
pub use session_log::session_log;
pub use write::write;
pub mod memory_index;
pub mod memory_summary_tool;
pub mod session_context;
pub mod user_profile;
