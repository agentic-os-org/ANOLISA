//! Token usage tracking module for AgentSight
//!
//! This module provides functionality for:
//! - Extracting token usage from LLM API responses (OpenAI, Anthropic, etc.)
//! - Token record types for storage
//!
//! # Submodules
//!
//! - [`types`] - Core type definitions (TokenRecord, TokenUsage, etc.)
//! - [`parser`] - SSE event parser for extracting token usage from streaming responses
//!
//! # Example
//!
//! ```rust,ignore
//! use agentsight::analyzer::token::TokenParser;
//!
//! // Parse token usage from SSE events
//! let parser = TokenParser::new();
//! for event in sse_events {
//!     if let Some(usage) = parser.parse_event(&event) {
//!         println!("Tokens: {} in, {} out", usage.input_tokens, usage.output_tokens);
//!     }
//! }
//! ```

mod data;
mod parser;
mod record;

// Extractor submodule for JSON token data extraction
mod extractor;
pub use extractor::extract_token_data_from_json;
pub use extractor::openai::extract_response_content;
pub(crate) use extractor::openai::merge_response_output_text;

// Re-export record types
pub use record::TokenRecord;

// Data types are kept for internal use but not re-exported
// pub use data::{TokenData, MessageTokenData, ResponseTokenData};

// Re-export parser
pub use parser::TokenParser;

use serde::{Deserialize, Serialize};

// ============================================================================
// Shared Types
// ============================================================================

/// LLM Provider type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LLMProvider {
    #[default]
    Unknown,
    OpenAI,
    Anthropic,
    Gemini,
    /// Alibaba Cloud DashScope / Bailian **native** protocol
    /// (`/api/v1/services/aigc/{text,multimodal}-generation/generation`).
    ///
    /// Distinct from DashScope's OpenAI-compatible mode, which speaks the
    /// OpenAI schema and is reported as [`LLMProvider::OpenAI`].
    DashScope,
}

impl std::fmt::Display for LLMProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LLMProvider::OpenAI => write!(f, "openai"),
            LLMProvider::Anthropic => write!(f, "anthropic"),
            LLMProvider::Gemini => write!(f, "gemini"),
            LLMProvider::DashScope => write!(f, "dashscope"),
            LLMProvider::Unknown => write!(f, "unknown"),
        }
    }
}

impl std::str::FromStr for LLMProvider {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "openai" | "gpt" => Ok(LLMProvider::OpenAI),
            "anthropic" | "claude" => Ok(LLMProvider::Anthropic),
            "gemini" | "google" => Ok(LLMProvider::Gemini),
            "dashscope" | "bailian" => Ok(LLMProvider::DashScope),
            _ => Ok(LLMProvider::Unknown),
        }
    }
}

/// Extracted token usage information
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Input/prompt tokens
    pub input_tokens: u64,
    /// Output/completion tokens
    pub output_tokens: u64,
    /// Cache creation input tokens (Anthropic)
    pub cache_creation_input_tokens: Option<u64>,
    /// Cache read input tokens (Anthropic)
    pub cache_read_input_tokens: Option<u64>,
    /// Model name
    pub model: Option<String>,
    /// Provider detected
    pub provider: LLMProvider,
}

impl TokenUsage {
    /// Total tokens (input + output)
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    /// Check if usage is empty
    pub fn is_empty(&self) -> bool {
        self.input_tokens == 0 && self.output_tokens == 0
    }
}

// ============================================================================
// Shared Utility Functions
// ============================================================================

/// Extract usage from a JSON usage object
///
/// This is the core extraction logic used by TokenParser.
pub fn extract_usage_object(
    usage: &serde_json::Value,
    provider: LLMProvider,
    full_json: &serde_json::Value,
) -> Option<TokenUsage> {
    let (input_tokens, output_tokens) = match provider {
        LLMProvider::OpenAI => {
            // Chat Completions names the counters prompt/completion, the
            // Responses API calls them input/output. Both are OpenAI billing
            // models (the cached prefix is already inside the input count), so
            // accept either; requiring prompt_tokens dropped every
            // `/v1/responses` usage object, because the `?` returned None
            // before the cache and model fields were read.
            let input = usage
                .get("prompt_tokens")
                .or_else(|| usage.get("input_tokens"))
                .and_then(|v| v.as_u64())?;
            let output = usage
                .get("completion_tokens")
                .or_else(|| usage.get("output_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            (input, output)
        }
        LLMProvider::Anthropic => {
            // Anthropic splits usage across events: message_start carries
            // input_tokens (+cache), while the terminal message_delta carries
            // only output_tokens. Accept either so the caller can merge them;
            // requiring input_tokens here would drop the output-only delta.
            let input = usage.get("input_tokens").and_then(|v| v.as_u64());
            let output = usage.get("output_tokens").and_then(|v| v.as_u64());
            if input.is_none() && output.is_none() {
                return None;
            }
            (input.unwrap_or(0), output.unwrap_or(0))
        }
        LLMProvider::Gemini => {
            // The wire format is camelCase (`promptTokenCount` /
            // `candidatesTokenCount` under `usageMetadata`); keep the
            // snake_case spellings as a gateway fallback.
            let input = usage
                .get("promptTokenCount")
                .or_else(|| usage.get("prompt_token_count"))
                .and_then(|v| v.as_u64())?;
            let output = usage
                .get("candidatesTokenCount")
                .or_else(|| usage.get("candidates_token_count"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            // Thinking models bill their reasoning budget as output but report
            // it in a counter of its own, outside `candidatesTokenCount`. A
            // gemini-2.5 response looks like `{promptTokenCount: 10,
            // candidatesTokenCount: 1, thoughtsTokenCount: 815,
            // totalTokenCount: 826}`: reading only the candidate counter
            // reports 1 output token for that call and breaks the
            // reconciliation this module keeps everywhere else (`input + output
            // == total`, asserted for DashScope below). Fold the thoughts in,
            // the way OpenAI already ships reasoning inside `completion_tokens`
            // and Anthropic inside `output_tokens`.
            let thoughts = usage
                .get("thoughtsTokenCount")
                .or_else(|| usage.get("thoughts_token_count"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            (input, output.saturating_add(thoughts))
        }
        LLMProvider::DashScope => {
            // Native protocol reuses Anthropic's field names.
            //
            // `input_tokens` is already the TOTAL input side: a real
            // multimodal-generation response reports
            // `input_tokens_details: {image_tokens: 1249, text_tokens: 12}`
            // with `input_tokens: 1261` (= 1249 + 12) and
            // `total_tokens: 1643` (= 1261 + 382 output). The sibling
            // `image_tokens` is an itemisation, NOT an addition — adding it
            // would double-count every image.
            let input = usage.get("input_tokens").and_then(|v| v.as_u64())?;
            let output = usage
                .get("output_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            (input, output)
        }
        LLMProvider::Unknown => {
            // Try OpenAI format first, then Anthropic
            if let (Some(input), Some(output)) = (
                usage.get("prompt_tokens").and_then(|v| v.as_u64()),
                usage.get("completion_tokens").and_then(|v| v.as_u64()),
            ) {
                (input, output)
            } else if let (Some(input), Some(output)) = (
                usage.get("input_tokens").and_then(|v| v.as_u64()),
                usage.get("output_tokens").and_then(|v| v.as_u64()),
            ) {
                (input, output)
            } else {
                return None;
            }
        }
    };

    // Extract cache tokens.
    //
    // Anthropic reports two separate counters: `cache_creation_input_tokens`
    // (write) and `cache_read_input_tokens` (hit).
    //
    // OpenAI nests cache hits under `prompt_tokens_details.cached_tokens` and
    // the Responses API under `input_tokens_details.cached_tokens`; DashScope
    // may surface them at the top level as `cached_tokens`.
    // DashScope also nests `cache_creation_input_tokens` under
    // `prompt_tokens_details`, so we fall back there as well.
    // Gemini reports hits as `cachedContentTokenCount`; like the OpenAI
    // billing model the cached prefix is already inside `promptTokenCount`,
    // so it is recorded but never added on top (TokenRecord::billed_input_tokens).
    let cache_creation_input_tokens = usage
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_u64())
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cache_creation_input_tokens"))
                .and_then(|v| v.as_u64())
        });
    let cache_read_input_tokens = usage
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_u64())
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(|v| v.as_u64())
        })
        .or_else(|| {
            usage
                .get("input_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(|v| v.as_u64())
        })
        .or_else(|| {
            usage
                .get("cachedContentTokenCount")
                .and_then(|v| v.as_u64())
        })
        .or_else(|| usage.get("cached_tokens").and_then(|v| v.as_u64()));

    // Extract model name. Gemini chunks carry it as `modelVersion`.
    let model = full_json
        .get("model")
        .and_then(|v| v.as_str())
        .or_else(|| full_json.get("modelVersion").and_then(|v| v.as_str()))
        .map(|s| s.to_string());

    Some(TokenUsage {
        input_tokens,
        output_tokens,
        cache_creation_input_tokens,
        cache_read_input_tokens,
        model,
        provider,
    })
}

/// Merge two token-usage snapshots from the same SSE stream.
///
/// All counters are cumulative within a stream, so the max resolves the
/// split-across-events case (message_start input/cache vs message_delta
/// output) and tolerates zero-placeholder events without ever regressing a
/// larger value. Model and provider are taken from the first event that
/// carries them.
///
/// Anthropic and Anthropic-compatible proxies split usage this way, so every
/// consumer that folds SSE events must merge rather than let one event win.
pub(crate) fn merge_usage(acc: Option<TokenUsage>, next: TokenUsage) -> Option<TokenUsage> {
    let Some(mut cur) = acc else {
        return Some(next);
    };
    cur.input_tokens = cur.input_tokens.max(next.input_tokens);
    cur.output_tokens = cur.output_tokens.max(next.output_tokens);
    cur.cache_creation_input_tokens = max_opt(
        cur.cache_creation_input_tokens,
        next.cache_creation_input_tokens,
    );
    cur.cache_read_input_tokens =
        max_opt(cur.cache_read_input_tokens, next.cache_read_input_tokens);
    if cur.model.is_none() {
        cur.model = next.model;
    }
    if cur.provider == LLMProvider::Unknown {
        cur.provider = next.provider;
    }
    Some(cur)
}

/// Max of two optional counters, preserving a value when only one is set.
fn max_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

/// Detect provider from usage object structure
///
/// Note: `total_tokens` is deliberately NOT used to discriminate DashScope from
/// Anthropic. Real Anthropic-protocol gateway traffic (Claude/Kimi via
/// `/v1/messages`) reports `total_tokens` inside `usage` alongside the cache
/// counters, so keying on it mislabels genuine Anthropic calls. The native
/// DashScope protocol is recognised from its response envelope instead — see
/// [`TokenParser::parse_json`].
pub fn detect_provider_from_usage(usage: &serde_json::Value) -> LLMProvider {
    // OpenAI uses prompt_tokens/completion_tokens
    if usage.get("prompt_tokens").is_some() && usage.get("completion_tokens").is_some() {
        return LLMProvider::OpenAI;
    }

    // DashScope multimodal-generation is the one usage shape that is
    // unambiguous on its own: no OpenAI or Anthropic usage object carries
    // per-modality token counters.
    if usage.get("input_tokens").is_some()
        && (usage.get("image_tokens").is_some()
            || usage.get("audio_tokens").is_some()
            || usage.get("video_tokens").is_some())
    {
        return LLMProvider::DashScope;
    }

    // Anthropic uses input_tokens/output_tokens
    if usage.get("input_tokens").is_some() && usage.get("output_tokens").is_some() {
        // ... but so does the Responses API (`/v1/responses`), which reuses those
        // names while keeping OpenAI's billing rule that the cache-hit count is
        // already part of `input_tokens`. Only the Responses shape nests
        // `cached_tokens` (and `reasoning_tokens`) under a details object, so that
        // is the discriminator: classifying it as Anthropic would make
        // `billed_input_tokens` add the cached prefix a second time. Callers that
        // know the endpoint resolve it themselves; this only stops usage alone
        // from guessing wrong.
        if usage.get("input_tokens_details").is_some()
            || usage.get("output_tokens_details").is_some()
        {
            return LLMProvider::OpenAI;
        }
        return LLMProvider::Anthropic;
    }

    // Gemini uses prompt_token_count/candidates_token_count
    if usage.get("prompt_token_count").is_some() {
        return LLMProvider::Gemini;
    }

    LLMProvider::Unknown
}

/// Detect provider from API endpoint URL
pub fn detect_provider_from_endpoint(endpoint: Option<&str>) -> LLMProvider {
    match endpoint {
        Some(ep) if ep.contains("openai.com") || ep.contains("api.openai.com") => {
            LLMProvider::OpenAI
        }
        Some(ep) if ep.contains("anthropic.com") || ep.contains("api.anthropic.com") => {
            LLMProvider::Anthropic
        }
        Some(ep) if ep.contains("generativelanguage.googleapis.com") || ep.contains("gemini") => {
            LLMProvider::Gemini
        }
        // Bailian workspaces are served from `{WorkspaceId}.{region}.maas.aliyuncs.com`.
        Some(ep) if ep.contains("dashscope.aliyuncs.com") || ep.contains("maas.aliyuncs.com") => {
            LLMProvider::DashScope
        }
        _ => LLMProvider::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression guard built from **real captured traffic** on a host running
    /// Claude/Kimi through Anthropic-protocol gateways: 105 records on
    /// `/api/anthropic/v1/messages` carry `total_tokens` *inside* `usage`
    /// alongside the Anthropic-only cache counters.
    ///
    /// An earlier version of the DashScope discriminator keyed off
    /// `input_tokens + total_tokens` and mislabelled 33% of those real records
    /// as `dashscope`, corrupting the token database. `total_tokens` is
    /// therefore NOT a usable DashScope signal.
    #[test]
    fn test_detect_provider_from_usage_anthropic_with_total_tokens() {
        let usage = serde_json::json!({
            "input_tokens": 1111,
            "output_tokens": 803,
            "total_tokens": 26490,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 24576
        });
        assert_eq!(detect_provider_from_usage(&usage), LLMProvider::Anthropic);
    }

    /// Bare `input_tokens`/`output_tokens`/`total_tokens` is genuinely ambiguous
    /// between Anthropic gateways and DashScope native, so usage alone must not
    /// guess: the native protocol is identified from its response envelope
    /// instead (see `TokenParser::parse_json`).
    #[test]
    fn test_detect_provider_from_usage_input_output_total_stays_anthropic() {
        let usage = serde_json::json!({
            "input_tokens": 22,
            "output_tokens": 8,
            "total_tokens": 30
        });
        assert_eq!(detect_provider_from_usage(&usage), LLMProvider::Anthropic);
    }

    /// Modality counters *are* DashScope-only — no Anthropic or OpenAI usage
    /// object carries them — so they remain a safe signal.
    #[test]
    fn test_detect_provider_from_usage_dashscope_multimodal() {
        let usage = serde_json::json!({
            "input_tokens": 30,
            "output_tokens": 12,
            "image_tokens": 1247
        });
        assert_eq!(detect_provider_from_usage(&usage), LLMProvider::DashScope);
    }

    /// Regression guard: plain Anthropic usage must keep resolving to Anthropic.
    #[test]
    fn test_detect_provider_from_usage_anthropic_unchanged() {
        let usage = serde_json::json!({"input_tokens": 10, "output_tokens": 5});
        assert_eq!(detect_provider_from_usage(&usage), LLMProvider::Anthropic);

        let cached = serde_json::json!({
            "input_tokens": 10,
            "output_tokens": 5,
            "cache_creation_input_tokens": 3,
            "cache_read_input_tokens": 4
        });
        assert_eq!(detect_provider_from_usage(&cached), LLMProvider::Anthropic);
    }

    /// Regression guard: OpenAI usage also carries `total_tokens`; the
    /// prompt/completion names must still win.
    #[test]
    fn test_detect_provider_from_usage_openai_unchanged() {
        let usage = serde_json::json!({
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15
        });
        assert_eq!(detect_provider_from_usage(&usage), LLMProvider::OpenAI);
    }

    /// The Responses API reuses `input_tokens`/`output_tokens` but keeps OpenAI's
    /// billing rule: `input_tokens` already contains the cached prefix. Its
    /// `*_tokens_details` objects are the only signal that separates it from
    /// Anthropic, where the cache counters are billed on top of `input_tokens`.
    /// Guessing Anthropic here doubles the cached prefix in
    /// `TokenRecord::billed_input_tokens`.
    #[test]
    fn test_detect_provider_from_usage_responses_shape_is_not_anthropic() {
        let with_cache = serde_json::json!({
            "input_tokens": 57,
            "output_tokens": 3,
            "total_tokens": 60,
            "input_tokens_details": {"cached_tokens": 2, "text_tokens": 55}
        });
        assert_eq!(detect_provider_from_usage(&with_cache), LLMProvider::OpenAI);

        let reasoning = serde_json::json!({
            "input_tokens": 57,
            "output_tokens": 3,
            "output_tokens_details": {"reasoning_tokens": 0}
        });
        assert_eq!(detect_provider_from_usage(&reasoning), LLMProvider::OpenAI);

        // No details object means it can still be Anthropic.
        let bare = serde_json::json!({"input_tokens": 57, "output_tokens": 3});
        assert_eq!(detect_provider_from_usage(&bare), LLMProvider::Anthropic);
    }

    #[test]
    fn test_extract_usage_object_dashscope_native() {
        let usage = serde_json::json!({
            "input_tokens": 25,
            "output_tokens": 1,
            "total_tokens": 26,
            "prompt_tokens_details": {"cached_tokens": 0}
        });
        // Verified against a real non-streaming native response: the envelope
        // carries only output/usage/request_id — no top-level `model`.
        let full = serde_json::json!({
            "output": {"choices": [{
                "message": {"content": "收到了", "role": "assistant"},
                "finish_reason": "stop"
            }]},
            "usage": usage.clone(),
            "request_id": "b82b259c-ed1d-95bd-8c0e-4f357868a622"
        });
        let out = extract_usage_object(&usage, LLMProvider::DashScope, &full).unwrap();
        assert_eq!(out.input_tokens, 25);
        assert_eq!(out.output_tokens, 1);
        assert_eq!(out.provider, LLMProvider::DashScope);
        assert!(out.model.is_none(), "native response has no model field");
        assert_eq!(out.cache_read_input_tokens, Some(0));
    }

    /// Verified against a **real** multimodal-generation response:
    ///
    /// ```text
    /// "usage": {"input_tokens_details": {"image_tokens": 1249, "text_tokens": 12},
    ///            "input_tokens": 1261, "output_tokens": 382, "total_tokens": 1643,
    ///            "image_tokens": 1249}
    /// ```
    ///
    /// 1249 + 12 == 1261 == `input_tokens`, and 1261 + 382 == 1643 ==
    /// `total_tokens`: `image_tokens` is an *itemisation* of `input_tokens`,
    /// not an addition to it. Adding them double-counts the image.
    #[test]
    fn test_extract_usage_object_dashscope_does_not_double_count_image_tokens() {
        let usage = serde_json::json!({
            "input_tokens_details": {"image_tokens": 1249, "text_tokens": 12},
            "prompt_tokens_details": {"cached_tokens": 0},
            "total_tokens": 1643,
            "output_tokens": 382,
            "input_tokens": 1261,
            "output_tokens_details": {"text_tokens": 382},
            "image_tokens": 1249
        });
        let full = serde_json::json!({"output": {}, "usage": usage.clone()});
        let out = extract_usage_object(&usage, LLMProvider::DashScope, &full).unwrap();
        assert_eq!(out.input_tokens, 1261, "image tokens are already included");
        assert_eq!(out.output_tokens, 382);
        assert_eq!(
            out.input_tokens + out.output_tokens,
            1643,
            "must reconcile with the provider-reported total_tokens"
        );
    }

    #[test]
    fn test_detect_provider_from_endpoint_dashscope() {
        assert_eq!(
            detect_provider_from_endpoint(Some("https://dashscope.aliyuncs.com/api/v1")),
            LLMProvider::DashScope
        );
        assert_eq!(
            detect_provider_from_endpoint(Some("https://ws-1.cn-beijing.maas.aliyuncs.com/api/v1")),
            LLMProvider::DashScope
        );
    }

    #[test]
    fn test_llm_provider_display_and_parse_dashscope() {
        assert_eq!(LLMProvider::DashScope.to_string(), "dashscope");
        assert_eq!(
            "dashscope".parse::<LLMProvider>().unwrap(),
            LLMProvider::DashScope
        );
        assert_eq!(
            "bailian".parse::<LLMProvider>().unwrap(),
            LLMProvider::DashScope
        );
    }
}
