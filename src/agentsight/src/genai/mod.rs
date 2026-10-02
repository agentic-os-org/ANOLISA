//! GenAI Semantic Module
//!
//! This module provides GenAI-specific semantic conversion and storage
//! for LLM API calls, tool uses, and agent interactions.

pub mod anolisa_release;
pub mod builder;
mod call_builder;
pub mod encrypt;
pub mod exporter;
mod helpers;
pub mod id_resolver;
pub mod instance_id;
pub mod logtail;
mod openai_parse;
pub mod semantic;
pub mod storage;

pub use builder::GenAIBuilder;
pub use exporter::GenAIExporter;
pub use logtail::LogtailExporter;
pub use semantic::{
    AgentInteraction, GenAISemanticEvent, InputMessage, LLMCall, LLMRequest, LLMResponse,
    MessagePart, OutputMessage, StreamChunk, TokenUsage, ToolDefinition, ToolUse,
};
pub use storage::{GenAIStore, GenAIStoreStats};

// Blanket implementation: Arc<T> implements GenAIExporter if T does.
// This allows storing an Arc<GenAISqliteStore> both in genai_exporters and
// as a direct handle for two-phase pending/complete writes.
use std::sync::Arc;
impl<T: GenAIExporter + Sync> GenAIExporter for Arc<T> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn export(&self, events: &[GenAISemanticEvent]) {
        (**self).export(events);
    }
}
