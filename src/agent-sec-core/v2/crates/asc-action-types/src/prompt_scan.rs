//! Shared prompt-scan input contracts, independent of detector implementation.

/// Request accepted by the prompt-scan capability.
///
/// `history` carries the raw conversation turns of a `multi_turn` scan; each
/// entry keeps the wire shape (`{"role","content"}` object or legacy
/// `"role: content"` string) and the capability crate owns the tolerant
/// decode, so a malformed turn degrades to an `UNKNOWN` role instead of
/// failing the request. `serde_json::Value` has no `Eq`, so the contract
/// derives `PartialEq` only.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptScanRequest {
    /// Prompt text supplied by the caller (`current_query` in `multi_turn`).
    pub text: String,
    /// Optional scan mode (`fast`, `standard`, `strict`, `multi_turn`).
    pub mode: Option<String>,
    /// Optional input origin label recorded in result metadata.
    pub source: Option<String>,
    /// Optional L2 backend override; only consumed by `standard`/`strict`.
    pub model: Option<String>,
    /// Prior assistant response of the conversation triple (`multi_turn` only).
    pub assistant_response: Option<String>,
    /// Conversation history of the triple (`multi_turn` only).
    pub history: Option<Vec<serde_json::Value>>,
}

/// Probe accepted by the prompt-scan warmup port.
///
/// A readiness check, not a scan: it carries the mode whose layers are probed
/// and the same optional L2 backend override as [`PromptScanRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptScanWarmupRequest {
    /// Scan mode whose layers are probed (`fast`, `standard`, `strict`,
    /// `multi_turn`).
    pub mode: Option<String>,
    /// Optional L2 backend override; only consumed by `standard`/`strict`.
    pub model: Option<String>,
}
