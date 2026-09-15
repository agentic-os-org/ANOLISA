// Crate-level clippy allows for lints that require architectural changes.
#![allow(clippy::type_complexity)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::large_enum_variant)]
#![allow(clippy::doc_lazy_continuation)]
#![allow(clippy::doc_overindented_list_items)]
#![allow(clippy::unnecessary_cast)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::missing_safety_doc)]

//! AgentSight - AI Agent observability library
//!
//! This crate provides eBPF-based observability for AI agents, including:
//! - SSL/TLS traffic capture and parsing
//! - HTTP request/response aggregation
//! - LLM token usage tracking
//! - Process lifecycle monitoring
//!
//! On Linux: full eBPF observability pipeline.
//! On macOS: cross-platform modules + local trajectory viewer under `local/`.
//! The `agentsight serve` command delegates to `local::server` on macOS.

// ─── Cross-platform modules (always compiled) ───────────────────────────────

pub mod atif;
pub mod chrome_trace;
pub mod config;
pub mod ecs_metadata;
mod logging;
mod private_sqlite;
pub mod security;
pub mod tokenizer;
pub mod utils;

// ─── Linux-only modules (eBPF observability pipeline) ──────────────────────

#[cfg(all(feature = "server", target_os = "linux"))]
pub mod agent_sec;
#[cfg(target_os = "linux")]
pub mod aggregator;
#[cfg(target_os = "linux")]
pub mod analyzer;
#[cfg(target_os = "linux")]
pub(crate) mod background;
#[cfg(target_os = "linux")]
pub mod container;
#[cfg(target_os = "linux")]
pub mod discovery;
#[cfg(target_os = "linux")]
pub mod enforcement;
#[cfg(target_os = "linux")]
pub mod event;
#[cfg(target_os = "linux")]
pub mod ffi;
#[cfg(target_os = "linux")]
pub mod genai;
#[cfg(target_os = "linux")]
pub mod grader;
#[cfg(target_os = "linux")]
pub mod health;
#[cfg(target_os = "linux")]
pub mod interruption;
#[cfg(target_os = "linux")]
pub mod parser;
#[cfg(target_os = "linux")]
pub mod probes;
#[cfg(target_os = "linux")]
pub mod response_map;
#[cfg(all(feature = "server", target_os = "linux"))]
pub mod server;
#[cfg(target_os = "linux")]
pub mod skill_metrics;
#[cfg(target_os = "linux")]
pub mod storage;
#[cfg(target_os = "linux")]
mod unified;

// ─── macOS local modules (trajectory viewer without eBPF) ────────────────────

#[cfg(all(feature = "server", not(target_os = "linux")))]
pub mod local;

// ─── Re-exports ─────────────────────────────────────────────────────────────

pub use chrome_trace::{ChromeTraceEvent, ToChromeTraceEvent, TraceArgs, next_flow_id, ns_to_us};
pub use config::default_cmdline_rules;
pub use config::{AgentsightConfig, default_base_path};

#[cfg(target_os = "linux")]
pub use aggregator::{
    AggregatedProcess, AggregatedResponse, AggregatedResult, Aggregator, ConnectionId,
    HttpConnectionAggregator, HttpPair, ProcessEventAggregator,
};
#[cfg(target_os = "linux")]
pub use analyzer::{
    AnalysisResult, Analyzer, AnthropicMessage, AnthropicRequest, AnthropicResponse,
    AnthropicUsage, AuditAnalyzer, AuditEventType, AuditExtra, AuditRecord, AuditSummary,
    HttpRecord, LLMProvider, MessageParser, MessageRole, OpenAIChatMessage, OpenAIContent,
    OpenAIRequest, OpenAIResponse, OpenAIUsage, ParsedApiMessage, PromptTokenCount, TokenParser,
    TokenRecord, TokenUsage,
};
#[cfg(target_os = "linux")]
pub use discovery::{AgentInfo, AgentScanner, CmdlineGlobMatcher, DiscoveredAgent, ProcessContext};
#[cfg(target_os = "linux")]
pub use genai::{
    AgentInteraction, GenAIBuilder, GenAIExporter, GenAISemanticEvent, GenAIStore, GenAIStoreStats,
    InputMessage, LLMCall, LLMRequest, LLMResponse, LogtailExporter, MessagePart, OutputMessage,
    StreamChunk, ToolDefinition, ToolUse,
};
#[cfg(target_os = "linux")]
pub use parser::{
    Http2FrameType, Http2Parser, HttpParser, ParseResult, ParsedHttp2Frame, ParsedHttpMessage,
    ParsedMessage, ParsedProcEvent, ParsedRequest, ParsedResponse, ParsedSseEvent, Parser,
    ProcEventType, ProcTraceParser, SseParser,
};
#[cfg(target_os = "linux")]
pub use probes::FileWatchEvent;
#[cfg(target_os = "linux")]
pub use response_map::ResponseSessionMapper;
#[cfg(target_os = "linux")]
pub use storage::{
    AuditStore, HttpStore, SqliteConfig, SqliteStore, Storage, StorageBackend, TimePeriod,
    TokenBreakdown, TokenComparison, TokenQuery, TokenQueryResult, TokenStore, Trend,
    check_data_file, format_tokens, format_tokens_with_commas,
};
#[cfg(target_os = "linux")]
pub use unified::AgentSight;

#[cfg(all(test, feature = "server", target_os = "linux"))]
mod tests {
    #[test]
    fn agent_sec_module_is_available_with_server_feature() {
        let socket_path = std::path::PathBuf::from("agent-sec-daemon.sock");
        let client = crate::agent_sec::AgentSecClient::new(Some(socket_path.clone()))
            .expect("server feature should expose the agent-sec client");

        assert_eq!(client.socket_path(), &socket_path);
    }
}
