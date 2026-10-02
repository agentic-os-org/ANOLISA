//! ATIF (Agent Trajectory Interchange Format) export module
//!
//! Converts AgentSight GenAI data into the shared ATIF schema defined by the
//! `agentsight-atif` crate (v1.7). This module owns only the conversion logic —
//! the data model is the public one, shared with the trajectory collector, so
//! both capture paths emit the same wire format.
//!
//! This module is independent from the `genai` module — it only depends on
//! storage query result types and `genai::semantic` types for deserialization.

#[cfg(target_os = "linux")]
pub mod converter;

#[cfg(target_os = "linux")]
pub use converter::{convert_session_to_atif, convert_trace_to_atif};
