//! OpenAI-compatible LLM client used by the LLM judgment layers.

pub mod client;
pub mod recorder;
pub mod types;

pub use client::LlmClient;
pub use recorder::{RecordParams, TrajectoryRecorder};
pub use types::ChatMessage;
