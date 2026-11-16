//! Agent health check module
//!
//! Provides periodic health checking of discovered AI agent processes by:
//! - Detecting their listening TCP ports via `/proc`
//! - Probing them with HTTP requests
//! - Storing results in a shared `HealthStore`
//!
//! It also diagnoses prompt-cache hit rates of recorded LLM calls ([`cache`]).

pub mod cache;
pub mod checker;
pub mod port_detector;
pub(crate) mod resource;
pub mod store;

pub use checker::HealthChecker;
pub use store::{AgentHealthState, AgentHealthStatus, HealthStore};
