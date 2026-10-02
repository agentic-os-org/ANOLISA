//! Model-name normalization and per-model output token limits.

use std::sync::OnceLock;

use regex::Regex;

/// Normalize a model name before pattern matching.
///
/// Strips provider prefixes, pipes/colons, date/version suffixes,
/// quantization suffixes, etc.  Each stripping step makes a single pass
/// (not a loop), matching the reference implementation's `replace()`
/// semantics where `$` anchors allow at most one match per step.
#[allow(dead_code)]
fn normalize_model(model: &str) -> String {
    let lower = model.to_lowercase();
    let s = lower.trim();

    // keep final path segment (strip provider prefixes), handle pipe/colon
    let s = s.rsplit(['/', '|', ':']).next().unwrap_or(s);

    // collapse whitespace to single hyphen
    let s: String = s.split_whitespace().collect::<Vec<_>>().join("-");

    // remove -preview everywhere
    let mut s = s.replace("-preview", "");

    // Special handling for model names that include date/version as part
    // of the model identifier:
    // - Qwen models: qwen-plus-latest, qwen-flash-latest, qwen-vl-max-latest
    // - Kimi models: kimi-k2-0905, kimi-k2-0711, etc. (keep date for
    //   version distinction)
    static QWEN_LATEST_RE: OnceLock<Regex> = OnceLock::new();
    static KIMI_DATE_RE: OnceLock<Regex> = OnceLock::new();
    static GPT4_PREVIEW_RE: OnceLock<Regex> = OnceLock::new();
    let qwen_re =
        QWEN_LATEST_RE.get_or_init(|| Regex::new(r"^qwen-(?:plus|flash|vl-max)-latest$").unwrap());
    let kimi_re = KIMI_DATE_RE.get_or_init(|| Regex::new(r"^kimi-k2-\d{4}$").unwrap());
    let gpt4_preview_re = GPT4_PREVIEW_RE.get_or_init(|| Regex::new(r"^gpt-4-\d{4}$").unwrap());
    let keep_suffixes =
        qwen_re.is_match(&s) || kimi_re.is_match(&s) || gpt4_preview_re.is_match(&s);

    if !keep_suffixes {
        // Strip one trailing suffix: date (4+ digits), NxNb, vN[.N],
        // latest, exp.  Single pass -- not a loop.
        static SUFFIX_RE: OnceLock<Regex> = OnceLock::new();
        let re = SUFFIX_RE.get_or_init(|| {
            Regex::new(r"-(?:\d{4,}|\d+x\d+b|v\d+(?:\.\d+)*|latest|exp)$").unwrap()
        });
        if re.is_match(&s) {
            s = re.replace(&s, "").to_string();
        } else {
            // Strip dotted version number preceded by a dash-segment,
            // e.g. "model-test-1.1" -> "model-test".  Rust's regex crate
            // does not support lookbehind (?<=-[^-]+-), so this is
            // handled as a separate alternative with a capturing group.
            static DOT_VER_RE: OnceLock<Regex> = OnceLock::new();
            let dot_re = DOT_VER_RE.get_or_init(|| Regex::new(r"(-[^-]+)-\d+(?:\.\d+)+$").unwrap());
            s = dot_re.replace(&s, "$1").to_string();
        }
    }

    // Strip one quantization / numeric / precision suffix.
    static QUANT_RE: OnceLock<Regex> = OnceLock::new();
    let re = QUANT_RE
        .get_or_init(|| Regex::new(r"-(?:\d?bit|int[48]|bf16|fp16|q[45]|quantized)$").unwrap());
    s = re.replace(&s, "").to_string();

    s
}

/// Model-specific maximum output token limits.
///
/// Returns the maximum output tokens for recognized model names, or `None`
/// when the model is unknown.  Intended for **config creators** (e.g.
/// `core.rs`) to set a dynamic default; the provider itself always respects
/// the `max_tokens` value from [`super::GenerateConfig`] without override.
///
/// The model string is first normalized via [`normalize_model`] and then
/// tested against an ordered list of regex patterns -- first match wins.
///
/// `dead_code` is allowed because the sole non-test consumer is the binary
/// target (`core.rs`); on non-Linux hosts the binary does not compile so
/// the lint fires spuriously.
#[allow(dead_code)]
pub fn model_max_output_tokens(model: &str) -> Option<u32> {
    let norm = normalize_model(model);

    static PATTERNS: OnceLock<Vec<(Regex, u32)>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        vec![
            // Values are capped at context_window / 2 so the budget's
            // `.min(window / 2)` reserve matches the actual max_tokens sent.
            //
            // Google Gemini — fallback context window 32 768.
            (Regex::new(r"^gemini-3").unwrap(), 16_384),
            (Regex::new(r"^gemini-").unwrap(), 8_192),
            // OpenAI — gpt-3.5: 4 096; gpt-4 preview/turbo snapshots: 4 096;
            // other gpt-4 family (gpt-4o, base gpt-4): 8 192; o-series: 64 000.
            // gpt-5 and later have no pattern and fall through to the fallback.
            (Regex::new(r"^gpt-3.5").unwrap(), 4_096),
            (Regex::new(r"^gpt-4-\d{4}$").unwrap(), 4_096),
            (Regex::new(r"^gpt-4-turbo").unwrap(), 4_096),
            (Regex::new(r"^gpt-4").unwrap(), 8_192),
            (Regex::new(r"^o\d").unwrap(), 64_000),
            // Anthropic Claude — context window 200 000.
            (Regex::new(r"^claude-opus-4-6").unwrap(), 100_000),
            (Regex::new(r"^claude-3.5").unwrap(), 8_192),
            // Alibaba / Qwen — qwen3 has 131 072 context window.
            (Regex::new(r"^qwen3\.\d").unwrap(), 65_536),
            (Regex::new(r"^coder-model$").unwrap(), 16_384),
            (Regex::new(r"^qwen3-max").unwrap(), 65_536),
            (Regex::new(r"^qwen3-coder").unwrap(), 65_536),
            // DeepSeek — context window 65 536.
            (Regex::new(r"^deepseek-v4").unwrap(), 32_768),
            (Regex::new(r"^deepseek-reasoner").unwrap(), 32_768),
            (Regex::new(r"^deepseek-r1").unwrap(), 32_768),
            (Regex::new(r"^deepseek-chat").unwrap(), 8_192),
            // Zhipu GLM — fallback context window 32 768.
            (Regex::new(r"^glm-5").unwrap(), 16_384),
            (Regex::new(r"^glm-4\.7").unwrap(), 16_384),
            // MiniMax — fallback context window 32 768.
            (Regex::new(r"(?i)^minimax-m2\.5").unwrap(), 16_384),
            // Kimi — fallback context window 32 768.
            (Regex::new(r"^kimi-k2\.5").unwrap(), 16_384),
        ]
    });

    for (re, limit) in patterns {
        if re.is_match(&norm) {
            return Some(*limit);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_max_output_tokens_known_models() {
        // Google Gemini
        assert_eq!(model_max_output_tokens("gemini-3.0"), Some(16_384));
        assert_eq!(model_max_output_tokens("gemini-1.5"), Some(8_192));
        // OpenAI — gpt-4 preview/turbo snapshots: 4 096; gpt-4o: 8 192;
        // o-series: 64 000. gpt-5 and later fall through to the caller's fallback.
        assert_eq!(model_max_output_tokens("gpt-4-turbo"), Some(4_096));
        assert_eq!(
            model_max_output_tokens("gpt-4-turbo-2024-04-09"),
            Some(4_096)
        );
        assert_eq!(model_max_output_tokens("gpt-4-1106-preview"), Some(4_096));
        assert_eq!(model_max_output_tokens("gpt-4-0125-preview"), Some(4_096));
        assert_eq!(model_max_output_tokens("gpt-4o"), Some(8_192));
        assert_eq!(model_max_output_tokens("o3"), Some(64_000));
        // Anthropic Claude — only 3.5 (small output) and opus-4-6
        // have explicit patterns; other Claude models fall through to
        // the caller's fallback (budget: 8 192, core.rs: 4 096).
        assert_eq!(model_max_output_tokens("claude-opus-4-6"), Some(100_000));
        assert_eq!(model_max_output_tokens("claude-3.5-sonnet"), Some(8_192));
        // Alibaba / Qwen — qwen3.* , qwen3-max, qwen3-coder are known;
        // legacy qwen-max / qwen-plus fall through to the fallback.
        assert_eq!(model_max_output_tokens("qwen3.7-max"), Some(65_536));
        assert_eq!(model_max_output_tokens("coder-model"), Some(16_384));
        assert_eq!(model_max_output_tokens("qwen3-max"), Some(65_536));
        assert_eq!(model_max_output_tokens("qwen3-coder"), Some(65_536));
        // DeepSeek
        assert_eq!(model_max_output_tokens("deepseek-v4-flash"), Some(32_768));
        assert_eq!(model_max_output_tokens("deepseek-reasoner"), Some(32_768));
        assert_eq!(model_max_output_tokens("deepseek-r1"), Some(32_768));
        assert_eq!(model_max_output_tokens("deepseek-chat"), Some(8_192));
        // Zhipu GLM
        assert_eq!(model_max_output_tokens("glm-5"), Some(16_384));
        assert_eq!(model_max_output_tokens("glm-4.7"), Some(16_384));
        // MiniMax (case-insensitive)
        assert_eq!(model_max_output_tokens("MiniMax-M2.5"), Some(16_384));
        // Kimi
        assert_eq!(model_max_output_tokens("kimi-k2.5"), Some(16_384));
    }

    #[test]
    fn model_max_output_tokens_unknown_falls_back() {
        assert_eq!(model_max_output_tokens("unknown-model"), None);
        assert_eq!(model_max_output_tokens("deepseek-v3"), None);
        assert_eq!(model_max_output_tokens("minimax-m2.1"), None);
        // Removed catch-alls: gpt-5, claude-sonnet-4-6 and qwen-max no
        // longer match any pattern and fall through to the caller's fallback.
        assert_eq!(model_max_output_tokens("gpt-5.0"), None);
        assert_eq!(model_max_output_tokens("claude-sonnet-4-6"), None);
        assert_eq!(model_max_output_tokens("qwen-max"), None);
    }

    #[test]
    fn model_max_output_tokens_normalizes_date_suffix() {
        // After normalize, date suffix is stripped before matching.
        assert_eq!(
            model_max_output_tokens("deepseek-reasoner-20250219"),
            Some(32_768)
        );
        assert_eq!(model_max_output_tokens("qwen3.7-max-latest"), Some(65_536));
    }
}
