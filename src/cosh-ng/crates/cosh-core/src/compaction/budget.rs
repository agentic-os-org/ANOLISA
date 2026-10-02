//! Model-window-aware token budgeting for compaction decisions.
//!
//! The usable history budget is `H = W - P - O - B` where `W` is the model
//! context window, `P` the dynamic runtime prefix (system prompt, tool
//! declarations, hooks, skills), `O` the output reserve, and `B` an
//! operations-burst and estimation-error reserve.

use serde::Serialize;

use crate::config::{
    CompactionConfig, DEFAULT_EMERGENCY_RATIO, DEFAULT_TARGET_RATIO, DEFAULT_TRIGGER_RATIO,
};
use crate::provider::token_limits::model_max_output_tokens;
use crate::provider::Message;

use super::projection::{TokenMeasurement, TokenMeasurementSource};

/// Conservative window applied when nothing better is known.
const FALLBACK_CONTEXT_WINDOW: u64 = 32_768;
/// Ceiling applied to a known model's hard output capability when deriving the
/// default per-request output budget.
///
/// A documented maximum output is a *capability*, not a per-turn expectation.
/// Charging the full capability to every request permanently donates that much
/// window to output: a 131 072-token window with a 65 536-token capability
/// keeps only ~40% of the window for history. Ordinary agent turns stay far
/// below this ceiling, so the default budget caps the capability here and only
/// an explicit user override may raise it.
const DEFAULT_OUTPUT_BUDGET: u64 = 16_384;
/// Per-request output budget for a model with no known output capability.
const UNKNOWN_MODEL_OUTPUT_BUDGET: u64 = 4_096;
/// Default agent session token limit; a differing value is user-explicit.
const DEFAULT_SESSION_TOKEN_LIMIT: u64 = 128_000;
/// Smallest burst reserve regardless of window size.
const MIN_BURST_RESERVE: u64 = 2_048;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
/// Where the resolved model capability values came from.
pub enum CapabilitySource {
    /// Explicit `[session.compaction]` user override.
    UserOverride,
    /// Built-in provider profile safety value for a known model family.
    ProviderProfile,
    /// Explicitly configured `agent.session_token_limit`.
    SessionTokenLimit,
    /// Conservative fallback; must be surfaced as estimated.
    ConservativeFallback,
}

impl CapabilitySource {
    /// Returns the stable protocol label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::UserOverride => "user_override",
            Self::ProviderProfile => "provider_profile",
            Self::SessionTokenLimit => "session_token_limit",
            Self::ConservativeFallback => "estimated_fallback",
        }
    }

    /// Whether the window is a guess rather than a known capability.
    pub fn is_estimated(&self) -> bool {
        matches!(self, Self::ConservativeFallback)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
/// Resolved context capability for the active model.
pub struct ModelCapability {
    /// Total model context window in tokens.
    pub context_window: u64,
    /// Hard output ceiling the model family accepts, in tokens; `None` when the
    /// model is unrecognized.
    ///
    /// This is the capability from the provider table, deliberately *not* the
    /// amount budgeted per request — see [`Self::output_budget_tokens`].
    pub output_capability_tokens: Option<u64>,
    /// Output tokens budgeted for one ordinary request.
    ///
    /// Single source of truth for both [`ContextBudget::output_reserve`] and the
    /// `max_tokens` actually sent in the provider request, so the budget can
    /// never reserve less than a request is allowed to spend.
    pub output_budget_tokens: u64,
    /// Provenance of the resolved values.
    pub source: CapabilitySource,
}

impl ModelCapability {
    /// Resolves capability using the documented precedence: explicit user
    /// override, provider profile safety value, explicitly configured session
    /// token limit, then a conservative fallback marked as estimated.
    pub fn resolve(compaction: &CompactionConfig, session_token_limit: u64, model: &str) -> Self {
        let (context_window, source) =
            resolve_context_window(compaction, session_token_limit, model);
        Self {
            context_window,
            output_capability_tokens: profile_output_capability(model),
            output_budget_tokens: resolve_output_budget(compaction, model, context_window),
            source,
        }
    }

    /// Output cap to send as `GenerateConfig::max_tokens` for one request.
    ///
    /// Reads the same resolved value the budget reserves, so lowering the
    /// reserve can never leave the request authorized to spend more.
    pub fn request_max_tokens(&self) -> u32 {
        // `resolve_output_budget` already clamps into the u32 request range;
        // the repeated bound keeps a hand-built capability safe too.
        self.output_budget_tokens.min(u64::from(u32::MAX)) as u32
    }
}

/// Resolves the context window `W` and records where the value came from.
fn resolve_context_window(
    compaction: &CompactionConfig,
    session_token_limit: u64,
    model: &str,
) -> (u64, CapabilitySource) {
    if let Some(window) = compaction.model_context_window.filter(|value| *value > 0) {
        return (window, CapabilitySource::UserOverride);
    }
    if let Some(window) = profile_context_window(model) {
        return (window, CapabilitySource::ProviderProfile);
    }
    if session_token_limit != DEFAULT_SESSION_TOKEN_LIMIT && session_token_limit > 0 {
        return (session_token_limit, CapabilitySource::SessionTokenLimit);
    }
    (
        FALLBACK_CONTEXT_WINDOW,
        CapabilitySource::ConservativeFallback,
    )
}

/// Shared resolution of the per-request output budget `O`.
///
/// Precedence:
/// 1. an explicit `[session.compaction] model_max_output_tokens` override;
/// 2. `min(model output capability, `[`DEFAULT_OUTPUT_BUDGET`]`)` for a known
///    model family;
/// 3. [`UNKNOWN_MODEL_OUTPUT_BUDGET`] for an unrecognized model.
///
/// The result is then bounded by `window / 2` — the window-safety limit the
/// history budget relies on — and by the `u32` range of a provider request, so
/// the same value can drive `ContextBudget::output_reserve` and
/// `GenerateConfig::max_tokens` without either side rounding differently.
///
/// Adaptive sizing from observed output percentiles is deliberately out of
/// scope; this only stops a capability ceiling from being charged every turn.
fn resolve_output_budget(compaction: &CompactionConfig, model: &str, context_window: u64) -> u64 {
    compaction
        .model_max_output_tokens
        .filter(|value| *value > 0)
        .unwrap_or_else(|| {
            profile_output_capability(model)
                .map(|capability| capability.min(DEFAULT_OUTPUT_BUDGET))
                .unwrap_or(UNKNOWN_MODEL_OUTPUT_BUDGET)
        })
        .min(context_window / 2)
        .min(u64::from(u32::MAX))
}

/// Conservative window table for known model families.
///
/// Values intentionally sit at or below vendor-documented limits; a full
/// provider model catalog is an explicit non-goal of the MVP.
fn profile_context_window(model: &str) -> Option<u64> {
    let model = model.to_ascii_lowercase();
    const TABLE: &[(&str, u64)] = &[
        ("qwen-max", 32_768),
        ("qwen-plus", 131_072),
        ("qwen-turbo", 131_072),
        ("qwen-long", 131_072),
        ("qwen3", 131_072),
        ("qwen2.5", 131_072),
        ("deepseek", 65_536),
        ("gpt-4o", 128_000),
        ("gpt-4.1", 128_000),
        ("o3", 128_000),
        ("o4", 128_000),
        ("claude", 200_000),
    ];
    TABLE
        .iter()
        .find(|(prefix, _)| model.starts_with(prefix))
        .map(|(_, window)| *window)
}

/// Hard output capability of a known model family, in tokens.
fn profile_output_capability(model: &str) -> Option<u64> {
    model_max_output_tokens(model).map(u64::from)
}

/// Conservative token estimate for text that never splits UTF-8 code points.
///
/// ASCII is charged at four bytes per token; every non-ASCII character is
/// charged as a full token so CJK-heavy content is over- rather than
/// under-estimated.
pub(crate) fn estimate_text_tokens(text: &str) -> u64 {
    let mut ascii_bytes: u64 = 0;
    let mut wide_chars: u64 = 0;
    for character in text.chars() {
        if character.is_ascii() {
            ascii_bytes += 1;
        } else {
            wide_chars += 1;
        }
    }
    ascii_bytes.div_ceil(4) + wide_chars
}

/// Conservative token estimate for a message slice, including role and
/// tool-call payload overhead.
pub(crate) fn estimate_messages_tokens(messages: &[Message]) -> u64 {
    messages
        .iter()
        .map(|message| {
            let mut tokens = 4 + estimate_text_tokens(&message.content.as_text());
            for call in message.tool_calls.iter().flatten() {
                tokens += estimate_text_tokens(&call.function.name)
                    + estimate_text_tokens(&call.function.arguments)
                    + 8;
            }
            tokens
        })
        .sum()
}

#[derive(Debug, Clone, Copy, Serialize)]
/// Derived thresholds over the usable history budget.
pub struct ContextBudget {
    /// Model context window `W`.
    pub context_window: u64,
    /// Runtime prefix estimate `P`.
    pub prefix_tokens: u64,
    /// Output reserve `O`.
    pub output_reserve: u64,
    /// Burst and estimation-error reserve `B`.
    pub burst_reserve: u64,
    /// Usable history budget `H = W - P - O - B`.
    pub usable_history: u64,
    /// Normal automatic trigger (defaults to 70% of `H`).
    pub trigger_tokens: u64,
    /// Emergency protection threshold (defaults to 90% of `H`).
    pub emergency_tokens: u64,
    /// Best-effort post-compaction target (defaults to 30% of `H`).
    pub target_tokens: u64,
    /// Whether the window itself is an estimate.
    pub window_estimated: bool,
}

impl ContextBudget {
    /// Computes thresholds for a capability, runtime prefix, and policy.
    ///
    /// Ratios are clamped into sane ranges and an explicit
    /// `auto_compact_token_limit` is clamped into the usable budget so user
    /// configuration can never authorize an oversized provider request.
    ///
    /// This is a second line of defense behind the config-load sanitization:
    /// even a hand-built [`CompactionConfig`] carrying NaN or infinite ratios
    /// must never panic (`f64::clamp` panics on a NaN bound) and must always
    /// yield finite thresholds with `target <= trigger <= emergency`.
    pub fn compute(
        capability: ModelCapability,
        prefix_tokens: u64,
        config: &CompactionConfig,
    ) -> Self {
        let window = capability.context_window;
        // The resolver already applied this bound; repeating it keeps the
        // invariant for a hand-built capability and documents that the reserve
        // may never claim more than half the window.
        let output_reserve = capability.output_budget_tokens.min(window / 2);
        let burst_reserve = (window / 20).max(MIN_BURST_RESERVE);
        let usable_history = window
            .saturating_sub(prefix_tokens)
            .saturating_sub(output_reserve)
            .saturating_sub(burst_reserve);

        let trigger_ratio =
            finite_or(config.trigger_ratio, DEFAULT_TRIGGER_RATIO).clamp(0.10, 0.95);
        // `max`/`min` (not clamp bounds) enforce the threshold ordering so a
        // non-finite or inverted override can never produce emergency below
        // trigger or target above trigger.
        let emergency_ratio = finite_or(config.emergency_ratio, DEFAULT_EMERGENCY_RATIO)
            .clamp(0.10, 0.99)
            .max(trigger_ratio);
        let target_ratio = finite_or(config.target_ratio, DEFAULT_TARGET_RATIO)
            .clamp(0.05, 0.95)
            .min(trigger_ratio);

        let mut trigger_tokens = ratio_of(usable_history, trigger_ratio);
        if let Some(limit) = config.auto_compact_token_limit.filter(|value| *value > 0) {
            // The absolute override may only tighten, never loosen, the
            // model-derived budget.
            trigger_tokens = trigger_tokens.min(limit).min(usable_history);
        }
        // Re-constrain the proportional target beneath a possibly tightened
        // trigger. An absolute `auto_compact_token_limit` can pull the trigger
        // below the ratio-derived target, so clamping here preserves the
        // invariant `target <= trigger <= emergency <= usable_history`.
        let target_tokens = ratio_of(usable_history, target_ratio).min(trigger_tokens);

        Self {
            context_window: window,
            prefix_tokens,
            output_reserve,
            burst_reserve,
            usable_history,
            trigger_tokens,
            emergency_tokens: ratio_of(usable_history, emergency_ratio),
            target_tokens,
            window_estimated: capability.source.is_estimated(),
        }
    }

    /// Whether normal background compaction should be scheduled.
    pub fn over_trigger(&self, history_tokens: u64) -> bool {
        self.usable_history > 0 && history_tokens > self.trigger_tokens
    }

    /// Whether the next request would cross the emergency threshold.
    pub fn over_emergency(&self, history_tokens: u64) -> bool {
        self.usable_history == 0 || history_tokens > self.emergency_tokens
    }
}

/// Returns the ratio when finite, otherwise the compiled-in default.
///
/// Serde deserializes TOML `nan`/`inf`/`-inf` into `f64` and callers may build
/// a `CompactionConfig` by hand, so `compute` must never feed a non-finite
/// value into `clamp` (which panics on a NaN bound) or [`ratio_of`].
fn finite_or(value: f64, default: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        default
    }
}

fn ratio_of(value: u64, ratio: f64) -> u64 {
    // Ratios are pre-sanitized to finite values in [0.05, 0.99]; the product
    // is finite and cannot overflow u64.
    (value as f64 * ratio) as u64
}

/// Wraps a history measurement, preferring provider-reported input tokens.
pub(crate) fn measure_history(provider_reported: Option<u64>, estimated: u64) -> TokenMeasurement {
    match provider_reported {
        Some(value) => TokenMeasurement {
            value,
            source: TokenMeasurementSource::ProviderReported,
        },
        None => TokenMeasurement {
            value: estimated,
            source: TokenMeasurementSource::Estimated,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> CompactionConfig {
        CompactionConfig::default()
    }

    /// Hand-builds a capability so threshold tests stay independent of the
    /// model tables.
    fn capability(
        context_window: u64,
        output_budget_tokens: u64,
        source: CapabilitySource,
    ) -> ModelCapability {
        ModelCapability {
            context_window,
            output_capability_tokens: Some(output_budget_tokens),
            output_budget_tokens,
            source,
        }
    }

    #[test]
    fn capability_prefers_user_override() {
        let mut cfg = config();
        cfg.model_context_window = Some(200_000);
        let capability = ModelCapability::resolve(&cfg, DEFAULT_SESSION_TOKEN_LIMIT, "qwen-max");
        assert_eq!(capability.context_window, 200_000);
        assert_eq!(capability.source, CapabilitySource::UserOverride);
        assert!(!capability.source.is_estimated());
    }

    #[test]
    fn capability_uses_profile_for_known_models() {
        let capability =
            ModelCapability::resolve(&config(), DEFAULT_SESSION_TOKEN_LIMIT, "qwen-max-latest");
        assert_eq!(capability.context_window, 32_768);
        assert_eq!(capability.source, CapabilitySource::ProviderProfile);
    }

    #[test]
    fn capability_uses_explicit_session_limit_for_unknown_models() {
        let capability = ModelCapability::resolve(&config(), 60_000, "totally-unknown-model");
        assert_eq!(capability.context_window, 60_000);
        assert_eq!(capability.source, CapabilitySource::SessionTokenLimit);
    }

    #[test]
    fn capability_falls_back_conservatively_and_marks_estimated() {
        let capability =
            ModelCapability::resolve(&config(), DEFAULT_SESSION_TOKEN_LIMIT, "unknown-model");
        assert_eq!(capability.context_window, FALLBACK_CONTEXT_WINDOW);
        assert!(capability.source.is_estimated());
    }

    #[test]
    fn large_output_capability_is_not_charged_per_request() {
        // #2240: the 65 536-token capability of qwen3.x-max used to be both the
        // request cap and the reserve, leaving a 131 072-token window with only
        // ~52K of usable history.
        let capability =
            ModelCapability::resolve(&config(), DEFAULT_SESSION_TOKEN_LIMIT, "qwen3.7-max");
        assert_eq!(capability.context_window, 131_072);
        assert_eq!(capability.output_capability_tokens, Some(65_536));
        assert_eq!(capability.output_budget_tokens, DEFAULT_OUTPUT_BUDGET);
        assert_eq!(capability.request_max_tokens(), 16_384);

        // P mirrors the session reported in the issue.
        let budget = ContextBudget::compute(capability, 6_251, &config());
        assert_eq!(budget.output_reserve, 16_384);
        assert_eq!(budget.usable_history, 101_884);
        assert_eq!(budget.emergency_tokens, 91_695);
        assert!(budget.usable_history > 100_000);
    }

    #[test]
    fn output_reserve_and_request_cap_are_the_same_value() {
        // Both sides must read the one resolver, for every provenance class:
        // a request may never be authorized to spend more than was reserved.
        let mut explicit_window = config();
        explicit_window.model_context_window = Some(200_000);
        let mut explicit_output = config();
        explicit_output.model_max_output_tokens = Some(32_000);
        let cases: [(&CompactionConfig, u64, &str); 5] = [
            (&config(), DEFAULT_SESSION_TOKEN_LIMIT, "qwen3.7-max"),
            (&config(), DEFAULT_SESSION_TOKEN_LIMIT, "gpt-4o"),
            (&config(), DEFAULT_SESSION_TOKEN_LIMIT, "unknown-model"),
            (&explicit_window, DEFAULT_SESSION_TOKEN_LIMIT, "gpt-4o"),
            (&explicit_output, DEFAULT_SESSION_TOKEN_LIMIT, "qwen3.7-max"),
        ];
        for (cfg, session_limit, model) in cases {
            let capability = ModelCapability::resolve(cfg, session_limit, model);
            let budget = ContextBudget::compute(capability, 1_000, cfg);
            assert_eq!(
                u64::from(capability.request_max_tokens()),
                budget.output_reserve,
                "model {model} sends a different cap than it reserves"
            );
        }
    }

    #[test]
    fn small_output_models_are_never_raised_to_the_default_ceiling() {
        // The ceiling only lowers a large capability; it must not inflate a
        // model that documents a smaller output limit.
        for (model, expected) in [("gpt-4o", 8_192), ("deepseek-chat", 8_192)] {
            let capability =
                ModelCapability::resolve(&config(), DEFAULT_SESSION_TOKEN_LIMIT, model);
            assert_eq!(
                capability.output_budget_tokens, expected,
                "model {model} output budget"
            );
            assert_eq!(capability.output_capability_tokens, Some(expected));
        }
    }

    #[test]
    fn unknown_models_share_one_safe_output_default() {
        let capability =
            ModelCapability::resolve(&config(), DEFAULT_SESSION_TOKEN_LIMIT, "brand-new-model");
        assert_eq!(capability.output_capability_tokens, None);
        assert_eq!(capability.output_budget_tokens, UNKNOWN_MODEL_OUTPUT_BUDGET);
        assert_eq!(capability.request_max_tokens(), 4_096);
        // The explicit-session-limit branch resolves the same default.
        let sized = ModelCapability::resolve(&config(), 60_000, "brand-new-model");
        assert_eq!(sized.output_budget_tokens, UNKNOWN_MODEL_OUTPUT_BUDGET);
    }

    #[test]
    fn explicit_output_override_drives_both_reserve_and_request() {
        let mut cfg = config();
        cfg.model_max_output_tokens = Some(48_000);
        let capability = ModelCapability::resolve(&cfg, DEFAULT_SESSION_TOKEN_LIMIT, "qwen3.7-max");
        assert_eq!(capability.output_budget_tokens, 48_000);
        assert_eq!(capability.request_max_tokens(), 48_000);
        let budget = ContextBudget::compute(capability, 6_251, &cfg);
        assert_eq!(budget.output_reserve, 48_000);
        // The user override raises the reserve, so history shrinks accordingly.
        assert_eq!(budget.usable_history, 131_072 - 6_251 - 48_000 - 6_553);
    }

    #[test]
    fn extreme_output_configuration_never_overflows_or_exceeds_the_window() {
        let mut cfg = config();
        cfg.model_max_output_tokens = Some(u64::MAX);
        cfg.model_context_window = Some(u64::MAX);
        let capability = ModelCapability::resolve(&cfg, DEFAULT_SESSION_TOKEN_LIMIT, "qwen3.7-max");
        assert_eq!(capability.output_budget_tokens, u64::from(u32::MAX));
        assert_eq!(capability.request_max_tokens(), u32::MAX);

        // A window smaller than the default budget still caps at W/2 on both
        // sides, so no request can be larger than the window allows.
        cfg.model_max_output_tokens = Some(u64::MAX);
        cfg.model_context_window = Some(2_000);
        let tiny = ModelCapability::resolve(&cfg, DEFAULT_SESSION_TOKEN_LIMIT, "qwen3.7-max");
        assert_eq!(tiny.output_budget_tokens, 1_000);
        assert_eq!(tiny.request_max_tokens(), 1_000);
        let budget = ContextBudget::compute(tiny, 200, &cfg);
        assert_eq!(budget.output_reserve, 1_000);
        assert!(u64::from(tiny.request_max_tokens()) <= tiny.context_window);
        assert_budget_is_sane(&budget);

        // A zero or absent override falls back instead of reserving nothing.
        cfg.model_max_output_tokens = Some(0);
        cfg.model_context_window = None;
        let zeroed = ModelCapability::resolve(&cfg, DEFAULT_SESSION_TOKEN_LIMIT, "qwen3.7-max");
        assert_eq!(zeroed.output_budget_tokens, DEFAULT_OUTPUT_BUDGET);
    }

    #[test]
    fn budget_excludes_prefix_output_and_burst() {
        let capability = capability(100_000, 8_000, CapabilitySource::UserOverride);
        let budget = ContextBudget::compute(capability, 12_000, &config());
        assert_eq!(budget.output_reserve, 8_000);
        assert_eq!(budget.burst_reserve, 5_000);
        assert_eq!(budget.usable_history, 75_000);
        assert_eq!(budget.trigger_tokens, 52_500);
        assert_eq!(budget.emergency_tokens, 67_500);
        assert_eq!(budget.target_tokens, 22_500);
    }

    #[test]
    fn thresholds_bound_trigger_and_emergency_semantics() {
        let capability = capability(100_000, 8_000, CapabilitySource::UserOverride);
        let budget = ContextBudget::compute(capability, 12_000, &config());
        assert!(!budget.over_trigger(budget.trigger_tokens));
        assert!(budget.over_trigger(budget.trigger_tokens + 1));
        assert!(!budget.over_emergency(budget.emergency_tokens));
        assert!(budget.over_emergency(budget.emergency_tokens + 1));
    }

    #[test]
    fn absolute_limit_is_clamped_into_model_budget() {
        let mut cfg = config();
        cfg.auto_compact_token_limit = Some(1_000_000);
        let capability = capability(100_000, 8_000, CapabilitySource::UserOverride);
        let budget = ContextBudget::compute(capability, 12_000, &cfg);
        assert!(budget.trigger_tokens <= budget.usable_history);
        cfg.auto_compact_token_limit = Some(10_000);
        let tightened = ContextBudget::compute(capability, 12_000, &cfg);
        assert_eq!(tightened.usable_history, 75_000);
        assert_eq!(tightened.trigger_tokens, 10_000);
        // The proportional target (30% of 75k = 22.5k) sits above the tightened
        // trigger; it must be pulled beneath it so the budget invariant holds.
        assert_eq!(tightened.target_tokens, 10_000);
        assert!(tightened.target_tokens <= tightened.trigger_tokens);
        assert_budget_is_sane(&tightened);
    }

    #[test]
    fn oversized_prefix_yields_zero_history_budget() {
        let capability = capability(16_000, 8_000, CapabilitySource::ConservativeFallback);
        let budget = ContextBudget::compute(capability, 20_000, &config());
        assert_eq!(budget.usable_history, 0);
        assert!(budget.over_emergency(0));
        assert!(!budget.over_trigger(0));
    }

    fn assert_budget_is_sane(budget: &ContextBudget) {
        assert!(budget.trigger_tokens <= budget.usable_history);
        assert!(budget.emergency_tokens >= budget.trigger_tokens);
        assert!(budget.emergency_tokens <= budget.usable_history);
        assert!(budget.target_tokens <= budget.trigger_tokens);
    }

    #[test]
    fn non_finite_ratios_never_panic_and_keep_threshold_order() {
        let capability = capability(100_000, 8_000, CapabilitySource::UserOverride);
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            // Each field individually poisoned.
            for field in 0..3 {
                let mut cfg = config();
                match field {
                    0 => cfg.trigger_ratio = bad,
                    1 => cfg.emergency_ratio = bad,
                    _ => cfg.target_ratio = bad,
                }
                let budget = ContextBudget::compute(capability, 12_000, &cfg);
                assert_budget_is_sane(&budget);
            }
            // All fields poisoned at once fall back to the default 70/90/30.
            let mut cfg = config();
            cfg.trigger_ratio = bad;
            cfg.emergency_ratio = bad;
            cfg.target_ratio = bad;
            let budget = ContextBudget::compute(capability, 12_000, &cfg);
            assert_budget_is_sane(&budget);
            assert_eq!(budget.trigger_tokens, 52_500);
            assert_eq!(budget.emergency_tokens, 67_500);
            assert_eq!(budget.target_tokens, 22_500);
        }
    }

    #[test]
    fn inverted_ratio_overrides_are_reordered_not_panicking() {
        let capability = capability(100_000, 8_000, CapabilitySource::UserOverride);
        let mut cfg = config();
        cfg.trigger_ratio = 0.90;
        cfg.emergency_ratio = 0.20;
        cfg.target_ratio = 0.95;
        let budget = ContextBudget::compute(capability, 12_000, &cfg);
        assert_budget_is_sane(&budget);
    }

    #[test]
    fn legal_ratio_overrides_still_apply() {
        let capability = capability(100_000, 8_000, CapabilitySource::UserOverride);
        let mut cfg = config();
        cfg.trigger_ratio = 0.50;
        cfg.emergency_ratio = 0.80;
        cfg.target_ratio = 0.20;
        let budget = ContextBudget::compute(capability, 12_000, &cfg);
        // usable_history = 100k - 12k - 8k - 5k = 75k.
        assert_eq!(budget.trigger_tokens, 37_500);
        assert_eq!(budget.emergency_tokens, 60_000);
        assert_eq!(budget.target_tokens, 15_000);
    }

    #[test]
    fn text_estimation_is_utf8_safe_and_conservative_for_cjk() {
        assert_eq!(estimate_text_tokens(""), 0);
        assert_eq!(estimate_text_tokens("abcd"), 1);
        // Every CJK character is charged as one token.
        assert_eq!(estimate_text_tokens("内存泄漏"), 4);
        // Mixed content combines both charges without panicking.
        let mixed = "check 内存 use 🎉";
        assert!(estimate_text_tokens(mixed) >= 3);
    }

    #[test]
    fn message_estimation_counts_tool_payloads() {
        let plain = vec![Message::user("hello world")];
        let with_tools = vec![Message::assistant_with_tool_calls(
            "",
            vec![crate::provider::ToolCallInfo {
                id: "c1".to_string(),
                call_type: "function".to_string(),
                function: crate::provider::ToolCallFunction {
                    name: "shell".to_string(),
                    arguments: "{\"command\":\"free -m\"}".to_string(),
                },
            }],
        )];
        assert!(estimate_messages_tokens(&with_tools) > estimate_messages_tokens(&plain));
    }

    #[test]
    fn history_measurement_prefers_provider_usage() {
        let reported = measure_history(Some(1234), 999);
        assert_eq!(reported.value, 1234);
        assert_eq!(reported.source, TokenMeasurementSource::ProviderReported);
        let estimated = measure_history(None, 999);
        assert_eq!(estimated.value, 999);
        assert_eq!(estimated.source, TokenMeasurementSource::Estimated);
    }
}
