#![forbid(unsafe_code)]
//! cosh-types: Core type definitions for the cosh deterministic interaction layer.
//!
//! Pure type layer — no I/O, no runtime logic.
//! Defines errors, responses, and data types for package management,
//! service management, checkpoint, and audit operations.

pub mod audit;
pub mod checkpoint;
pub mod config;
pub mod error;
pub mod output;
pub mod pkg;
pub mod svc;
