#![forbid(unsafe_code)]
#![deny(missing_docs)]
//! Side-effect-free domain contracts shared by COSH Gateway adapters.
//!
//! Transport and persistence crates translate through these types instead of
//! exposing ACP, Shell, channel, or database-specific payloads to the domain.

pub mod capability;
pub mod common;
pub mod error;
pub mod external;
pub mod ids;
pub mod profile;
pub mod runtime;
pub mod task;
