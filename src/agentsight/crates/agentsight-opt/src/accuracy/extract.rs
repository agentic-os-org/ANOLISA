//! Shared LLM extraction — one call feeding all strategies.
//!
//! Input: heuristic final_answer + user turns (no raw trajectory).
//! Output: `SharedExtraction` with claims / assertions / checklist / ambiguity.
//! On LLM failure, degrades to an empty extraction so pure-Rust oracles still fire.

use serde::{Deserialize, Serialize};

use crate::llm::{ChatMessage, LlmClient};
use crate::trace::TraceInventory;

const SYSTEM_PROMPT: &str = include_str!("../../prompts/shared_extract.md");

/// A completion claim extracted from the final answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionClaim {
    pub claim: String,
    #[serde(default)]
    pub kind: String, // "done" | "tested" | "passed"
}

/// A verifiable assertion (symbol/file/path/api) extracted from the final answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assertion {
    pub symbol: String,
    #[serde(default)]
    pub kind: String, // "function" | "file" | "path" | "api"
}

/// A requirement/scope/format/constraint item extracted from user turns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChecklistItem {
    pub item: String,
    #[serde(default)]
    pub kind: String, // "需求" | "范围" | "格式" | "约束"
    #[serde(default)]
    pub priority: String, // "must" | "should" | "nice-to-have"
    #[serde(default)]
    pub turn: usize,
}

/// Whether the user request is ambiguous, with a note explaining why.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AmbiguityNote {
    #[serde(default)]
    pub ambiguous: bool,
    #[serde(default)]
    pub note: String,
}

/// Result of the single shared extraction call.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SharedExtraction {
    #[serde(default)]
    pub claims: Vec<CompletionClaim>,
    #[serde(default)]
    pub assertions: Vec<Assertion>,
    #[serde(default)]
    pub checklist: Vec<ChecklistItem>,
    #[serde(default)]
    pub ambiguity: Option<AmbiguityNote>,
}

/// Run the shared extraction call. Never fails: on error returns an empty
/// extraction so strategies with pure-Rust oracles can still produce issues.
pub async fn shared_extract(client: &LlmClient, inv: &TraceInventory) -> SharedExtraction {
    if inv.final_answer.is_empty() && inv.user_turns.is_empty() {
        tracing::debug!("[accuracy] No final answer or user turns, skipping shared extraction");
        return SharedExtraction::default();
    }

    let user_turns_text = if inv.user_turns.is_empty() {
        "（无用户轮次数据）".to_string()
    } else {
        inv.user_turns
            .iter()
            .map(|t| format!("[轮{}] {}", t.turn, t.text))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let messages = vec![
        ChatMessage::system(SYSTEM_PROMPT),
        ChatMessage::user(format!(
            "## 用户各轮原话\n\n{}\n\n## 最终答案\n\n{}\n\n\
             提取完成声明、可验证断言、要点清单和歧义判断。仅返回 JSON。",
            user_turns_text, inv.final_answer
        )),
    ];

    match client
        .chat_json_parsed_labeled::<SharedExtraction>(messages, Some("accuracy:shared_extract"))
        .await
    {
        Ok(out) => {
            tracing::info!(
                "[accuracy] Shared extraction: {} claims, {} assertions, {} checklist items, ambiguous={}",
                out.claims.len(),
                out.assertions.len(),
                out.checklist.len(),
                out.ambiguity.as_ref().map(|a| a.ambiguous).unwrap_or(false)
            );
            out
        }
        Err(e) => {
            tracing::warn!("[accuracy] Shared extraction failed, degrading to empty: {e}");
            SharedExtraction::default()
        }
    }
}
