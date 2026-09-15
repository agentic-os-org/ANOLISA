//! Session Log subsystem (Phase 3).
//!
//! Provides a per-process tmpfs scratch area at `/run/anolisa/sessions/<sid>/`
//! holding:
//! - `meta.toml` — owner, agent_id, created_at, mount_ns
//! - `scratch/`  — model-managed temporary files (only place tools may write)
//! - `log.jsonl` — OS-appended trail of tool calls during this session
//!
//! Tests set `MEMORY_SESSION_DIR` to a tempdir to avoid colliding with
//! `/run/anolisa/sessions/` in the host.

pub mod base;
pub mod id;
pub mod paths;
pub mod service;

pub use base::SessionBase;
pub use id::SessionId;
pub use paths::resolve_in_scratch;
pub use service::{EndAction, SessionLogService};
