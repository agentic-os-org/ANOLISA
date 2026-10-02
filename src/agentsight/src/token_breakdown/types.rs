//! Data structures for ChatML token consumption breakdown
//!
//! Defines all types used in the ChatML parsing and token breakdown pipeline:
//! - Input types: ChatMLBlock, ChatMLDocument
//! - Classification types: ConversationTurnType, ConversationTurn, ResponseData
//! - Output types: EventNode, SummaryItem, TokenBreakdownNode, ResponseItem, ChatMLTokenBreakdown

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A single block from a ChatML document (between <|im_start|> and <|im_end|>)
#[derive(Debug, Clone)]
pub struct ChatMLBlock {
    /// Role of this block: "system", "user", or "assistant"
    pub role: String,
    /// Raw content text (after the role line, before <|im_end|>)
    pub raw_content: String,
}

/// Parsed ChatML document containing all blocks
#[derive(Debug, Clone)]
pub struct ChatMLDocument {
    /// All parsed blocks in order
    pub blocks: Vec<ChatMLBlock>,
    /// Original raw text of the entire file
    pub raw_text: String,
}

/// Type of a conversation message (used internally for classification)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationTurnType {
    /// User message (role=user, no <tool_response>)
    UserMessage,
    /// Assistant text response (role=assistant, no <tool_call>)
    AssistantText,
    /// Tool call (role=assistant, contains <tool_call>)
    ToolCall,
    /// Tool response (role=user, contains <tool_response>)
    ToolResponse,
}

/// A single conversation message with its classification
#[derive(Debug, Clone)]
pub struct ConversationTurn {
    /// Type of this message (used for grouping, not serialized to output)
    pub turn_type: ConversationTurnType,
    /// Full text content of the message
    pub content: String,
    /// Whether this is a history message (true) or the current prompt (false)
    pub is_history: bool,
}

/// Response data from AggregatedResponse (SSE stream aggregation)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseData {
    /// Text content fragments from choices[].delta.content
    pub content: Vec<String>,
    /// Reasoning/thinking content from choices[].delta.reasoning_content (may be empty)
    pub reasoning_content: Option<String>,
    /// Tool calls in "name: arguments" format (may be empty)
    pub tool_calls: Vec<String>,
}

/// Result of classifying a ChatML document
#[derive(Debug, Clone)]
pub struct ClassifiedDocument {
    /// System prompt raw content (treated as a whole, no sub-segmentation)
    pub system_content: String,
    /// Classified conversation messages
    pub messages: Vec<ConversationTurn>,
    /// Response data from AggregatedResponse (optional)
    pub response: Option<ResponseData>,
}

// === Output types (serialized to JSON) ===

/// Summary statistics for a label category
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryItem {
    /// Number of items with this label
    pub count: usize,
    /// Total tokens for this label
    pub tokens: usize,
    /// Percentage of event total tokens
    pub percentage: f64,
}

/// An event node in the events array (top-level element)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventNode {
    /// Event type identifier: "request" or "response"
    #[serde(rename = "type")]
    pub event_type: String,
    /// Human-readable label for display
    pub label: String,
    /// Total tokens for this event
    pub tokens: usize,
    /// Percentage of total tokens
    pub percentage: f64,
    /// Character count
    pub char_count: usize,
    /// Summary statistics by label (aggregated from children)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<BTreeMap<String, SummaryItem>>,
    /// Child breakdown nodes
    pub children: Vec<TokenBreakdownNode>,
}

/// A node in the token breakdown tree (JSON output)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBreakdownNode {
    /// Machine-readable name
    pub name: String,
    /// Human-readable label for display
    pub label: String,
    /// Number of tokens in this node
    pub tokens: usize,
    /// Percentage of total tokens (relative to root total_tokens)
    pub percentage: f64,
    /// Character count of the content
    pub char_count: usize,
    /// Whether this is a history message (for request messages)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_history: Option<bool>,
    /// Full text content (for single-item nodes)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Child nodes (for nested structures)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<TokenBreakdownNode>>,
    /// Response items (for response sub-categories with multiple fragments)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_items: Option<Vec<ResponseItem>>,
}

/// A single response item within a response sub-category (no is_history field)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseItem {
    /// Sequential index within this sub-category (0-based)
    pub index: usize,
    /// Number of tokens
    pub tokens: usize,
    /// Character count
    pub char_count: usize,
    /// Full text content
    pub content: String,
}

/// Complete token breakdown result (JSON output root)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMLTokenBreakdown {
    /// Model name used for tokenization
    pub model_name: String,
    /// Total tokens across all events
    pub total_tokens: usize,
    /// Summary statistics by input/output and label
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<BTreeMap<String, BTreeMap<String, SummaryItem>>>,
    /// Ordered array of trace events (request, response, ...)
    pub events: Vec<EventNode>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_conversation_turn_type_eq() {
        assert_eq!(ConversationTurnType::UserMessage, ConversationTurnType::UserMessage);
        assert_ne!(ConversationTurnType::ToolCall, ConversationTurnType::ToolResponse);
    }

    #[test]
    fn test_chatml_block_debug() {
        let block = ChatMLBlock {
            role: "user".to_string(),
            raw_content: "Hello".to_string(),
        };
        let debug = format!("{:?}", block);
        assert!(debug.contains("user"));
    }

    #[test]
    fn test_response_data_serde() {
        let resp = ResponseData {
            content: vec!["Hello!".to_string()],
            reasoning_content: Some("thinking...".to_string()),
            tool_calls: vec!["search: {}".to_string()],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: ResponseData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.content.len(), 1);
        assert_eq!(back.reasoning_content, Some("thinking...".to_string()));
        assert_eq!(back.tool_calls.len(), 1);
    }

    #[test]
    fn test_summary_item_serde() {
        let item = SummaryItem {
            count: 3,
            tokens: 100,
            percentage: 25.5,
        };
        let json = serde_json::to_string(&item).unwrap();
        let back: SummaryItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back.count, 3);
        assert_eq!(back.tokens, 100);
        assert!((back.percentage - 25.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_event_node_serde() {
        let node = EventNode {
            event_type: "request".to_string(),
            label: "请求".to_string(),
            tokens: 500,
            percentage: 80.0,
            char_count: 1000,
            summary: None,
            children: vec![],
        };
        let json = serde_json::to_string(&node).unwrap();
        assert!(json.contains("\"type\":\"request\""));
        let back: EventNode = serde_json::from_str(&json).unwrap();
        assert_eq!(back.event_type, "request");
    }

    #[test]
    fn test_token_breakdown_node_skip_none() {
        let node = TokenBreakdownNode {
            name: "system_prompt".to_string(),
            label: "系统提示词".to_string(),
            tokens: 50,
            percentage: 10.0,
            char_count: 100,
            is_history: None,
            content: None,
            children: None,
            response_items: None,
        };
        let json = serde_json::to_string(&node).unwrap();
        assert!(!json.contains("is_history"));
        assert!(!json.contains("content"));
        assert!(!json.contains("children"));
        assert!(!json.contains("response_items"));
    }

    #[test]
    fn test_token_breakdown_node_with_children() {
        let child = TokenBreakdownNode {
            name: "msg".to_string(),
            label: "消息".to_string(),
            tokens: 20,
            percentage: 4.0,
            char_count: 40,
            is_history: Some(true),
            content: Some("hello".to_string()),
            children: None,
            response_items: None,
        };
        let parent = TokenBreakdownNode {
            name: "request".to_string(),
            label: "请求".to_string(),
            tokens: 50,
            percentage: 10.0,
            char_count: 100,
            is_history: None,
            content: None,
            children: Some(vec![child]),
            response_items: None,
        };
        let json = serde_json::to_string(&parent).unwrap();
        let back: TokenBreakdownNode = serde_json::from_str(&json).unwrap();
        assert_eq!(back.children.unwrap().len(), 1);
    }

    #[test]
    fn test_response_item_serde() {
        let item = ResponseItem {
            index: 0,
            tokens: 10,
            char_count: 20,
            content: "hello world".to_string(),
        };
        let json = serde_json::to_string(&item).unwrap();
        let back: ResponseItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back.index, 0);
        assert_eq!(back.content, "hello world");
    }

    #[test]
    fn test_chatml_token_breakdown_serde() {
        let breakdown = ChatMLTokenBreakdown {
            model_name: "gpt-4".to_string(),
            total_tokens: 1000,
            summary: None,
            events: vec![EventNode {
                event_type: "request".to_string(),
                label: "请求".to_string(),
                tokens: 1000,
                percentage: 100.0,
                char_count: 2000,
                summary: None,
                children: vec![],
            }],
        };
        let json = serde_json::to_string(&breakdown).unwrap();
        let back: ChatMLTokenBreakdown = serde_json::from_str(&json).unwrap();
        assert_eq!(back.model_name, "gpt-4");
        assert_eq!(back.total_tokens, 1000);
        assert_eq!(back.events.len(), 1);
    }

    #[test]
    fn test_classified_document() {
        let doc = ClassifiedDocument {
            system_content: "Be helpful".to_string(),
            messages: vec![ConversationTurn {
                turn_type: ConversationTurnType::UserMessage,
                content: "Hello".to_string(),
                is_history: false,
            }],
            response: None,
        };
        assert_eq!(doc.system_content, "Be helpful");
        assert_eq!(doc.messages.len(), 1);
        assert!(!doc.messages[0].is_history);
    }

    #[test]
    fn test_chatml_document() {
        let doc = ChatMLDocument {
            blocks: vec![ChatMLBlock {
                role: "system".to_string(),
                raw_content: "prompt".to_string(),
            }],
            raw_text: "<|im_start|>system\nprompt\n<|im_end|>".to_string(),
        };
        assert_eq!(doc.blocks.len(), 1);
        assert!(doc.raw_text.contains("im_start"));
    }
}
