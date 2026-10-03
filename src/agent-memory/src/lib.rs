//! Agent memory — filesystem memory for AI agents (Linux-only).
//!
//! This crate exposes 19 MCP tools (10 Tier A file tools, 3 Tier B
//! structured tools, 3 snapshot tools, 2 git tools, mem_session_log)
//! over stdio, layered on a per-namespace mount with a JSONL audit log,
//! optional Linux user-namespace isolation, optional cgroup v2 quota,
//! optional git versioning, optional systemd-journald fan-out, and a
//! background BM25 index.
//!
//! Build target: Linux (x86_64 / aarch64). The implementation directly
//! uses user namespaces, mount(2), cgroup v2, inotify and journald;
//! there is no macOS / Windows path. For development on a non-Linux
//! host, push the branch and SSH into a Linux box (`make remote-test`).

// Stable `let` chains land in 1.88; anolisa's distro toolchain ships 1.86,
// so we stay on nested `if let` blocks. Newer clippys suggest collapsing
// them — opt out crate-wide.
#![allow(clippy::collapsible_if)]

pub mod audit;
pub mod cgroup;
pub mod config;
pub mod consolidation;
pub mod embedding;
pub mod error;
pub mod git_repo;
pub mod host;
pub mod index;
pub mod mcp_server;
pub mod mount;
pub mod ns;
pub mod safe_fs;
pub mod safety;
pub mod service;
pub mod session;
pub mod snapshot;
pub mod tools;

pub use error::{MemoryError, Result};
pub use service::MemoryService;
