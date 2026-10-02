//! Backends for [`StashStore`](crate::StashStore).
//!
//! `in_memory` is always available (no dependencies, useful for tests and
//! single-process runs). `sqlite` is gated behind the `sqlite` feature and is
//! the recommended backend for the production hook path, where each
//! compression call is a short-lived process and stash state must survive
//! across processes.

pub mod in_memory;

#[cfg(feature = "sqlite")]
pub mod sqlite;
