// SPDX-License-Identifier: Apache-2.0
//! blaze-core: shared types and v0.1 in-memory implementations for the
//! blaze sandbox-orchestration daemon.
//!
//! This crate intentionally has no I/O surface beyond JSON/TOML on local
//! filesystems. Network/UDS surfaces are implemented in the `blazed` daemon
//! crate. Modules map 1:1 to the functional breakdown:
//!
//! - [`config`]: daemon TOML configuration
//! - [`policy`]: workload class + policy file schema
//! - [`backend`]: backend kinds + selection / fallback
//! - [`checkpoint`]: pure checkpoint records and manifest validation
//! - [`guest_protocol`]: guest-agent wire DTOs
//! - [`lifecycle`]: sandbox state machine + JSON persistence
//! - [`kernel`]: kernel hook registry, per-hook mutex
//! - [`error`]: unified [`BlazeError`] error enum

pub mod backend;
pub mod checkpoint;
pub mod config;
pub mod error;
pub mod guest_protocol;
pub mod kernel;
pub mod lifecycle;
pub mod policy;
pub mod storage;

pub use error::{BlazeError, Result};
