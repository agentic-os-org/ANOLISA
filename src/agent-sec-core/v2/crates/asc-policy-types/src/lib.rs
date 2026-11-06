//! Stable domain and wire contracts for the `AgentSecCore` policy control plane.
//!
//! This crate intentionally contains data contracts only. Policy storage,
//! resolution, target compilation, and enforcement live in higher-level
//! crates. Rust types are canonical; source-derived ASTs stay in memory, while
//! serializable contracts use serde JSON on the wire.

#![forbid(unsafe_code)]

pub mod authoring;
pub mod binding;
pub mod cedar;
pub mod error;
pub mod identifiers;
pub mod policy;
pub mod process_discovery;
pub mod scope;
pub mod target;

pub use error::{Validate, ValidationError};
