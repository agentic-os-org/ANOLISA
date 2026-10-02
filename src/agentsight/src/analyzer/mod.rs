//! Analyzer module - pure logic analysis layer
//!
//! Extracts structured records from aggregated results.
//! Contains `AuditAnalyzer` for behavior auditing, `TokenParser` for token usage extraction,
//! and `MessageParser` for LLM API message format parsing.
//!
//! Use `Analyzer` for a unified interface that combines all analyzers.

pub mod audit;
pub mod message;
mod result;
pub mod token;
mod unified;

// Re-export audit types
pub use audit::{AuditAnalyzer, AuditEventType, AuditExtra, AuditRecord, AuditSummary};

// Re-export token types from the token module
pub use token::{LLMProvider, TokenParser, TokenRecord, TokenUsage};

// Re-export message types from the message module
pub use message::{
    AnthropicMessage, AnthropicRequest, AnthropicResponse, AnthropicUsage, MessageParser,
    MessageRole, OpenAIChatMessage, OpenAIChoice, OpenAIContent, OpenAIRequest, OpenAIResponse,
    OpenAIUsage, ParsedApiMessage,
};

// Re-export analysis result
pub use result::{
    AnalysisResult, HttpRecord, MessageTokenCount, OutputTokenCount, PromptTokenCount,
    TokenConsumptionBreakdown,
};

// Re-export unified analyzer
pub use unified::{
    Analyzer, RequestTokenCount, ResponseTokenCount, count_request_tokens, count_response_tokens,
};
