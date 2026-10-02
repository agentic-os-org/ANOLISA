use serde_json::Value;

pub trait ProviderProfile: Send + Sync {
    fn name(&self) -> &str;

    fn max_tokens_field(&self) -> &str {
        "max_tokens"
    }

    fn thinking_field(&self) -> Option<&str> {
        None
    }

    /// Whether this backend accepts `stream_options: {include_usage: true}`.
    ///
    /// Usage telemetry only refines compaction thresholds — a local token
    /// estimate always exists as a fallback — so it must never become a hard
    /// dependency. Generic OpenAI-compatible endpoints frequently reject
    /// unknown `stream_options`, failing the whole model turn, so the default
    /// is `false`; only profiles verified to support the field opt in.
    fn supports_stream_usage(&self) -> bool {
        false
    }

    fn adjust_request(&self, _body: &mut Value) {}

    /// Whether this backend supports DashScope-style prompt caching.
    ///
    /// When `true` and `explicit_cache` is enabled, the provider injects
    /// `X-DashScope-CacheControl: enable` and `cache_control` markers into the
    /// request. Only DashScope supports this; OpenAI has automatic caching and
    /// other backends have no cache API.
    ///
    /// New profiles must override this explicitly — the default is `false` to
    /// avoid sending DashScope-specific headers to unrelated endpoints. Add the
    /// new profile to `dashscope_profile_supports_cache_control` so the choice
    /// is reviewed at test time.
    fn supports_cache_control(&self) -> bool {
        false
    }

    fn auth_header_value(&self, api_key: &str) -> String {
        format!("Bearer {api_key}")
    }
}

pub struct GenericProfile;

impl ProviderProfile for GenericProfile {
    fn name(&self) -> &str {
        "generic"
    }
}

pub struct DashScopeProfile;

impl ProviderProfile for DashScopeProfile {
    fn name(&self) -> &str {
        "dashscope"
    }

    fn thinking_field(&self) -> Option<&str> {
        Some("reasoning_content")
    }

    fn supports_stream_usage(&self) -> bool {
        true
    }

    fn supports_cache_control(&self) -> bool {
        true
    }
}

pub struct OpenAIProfile;

impl ProviderProfile for OpenAIProfile {
    fn name(&self) -> &str {
        "openai"
    }

    fn max_tokens_field(&self) -> &str {
        "max_completion_tokens"
    }

    fn supports_stream_usage(&self) -> bool {
        true
    }
}

pub struct DeepSeekProfile;

impl ProviderProfile for DeepSeekProfile {
    fn name(&self) -> &str {
        "deepseek"
    }

    fn thinking_field(&self) -> Option<&str> {
        Some("reasoning_content")
    }

    fn supports_stream_usage(&self) -> bool {
        true
    }
}

pub fn profile_from_name(name: &str) -> Box<dyn ProviderProfile> {
    match name {
        "dashscope" | "coding_plan" | "token_plan" => Box::new(DashScopeProfile),
        "openai" => Box::new(OpenAIProfile),
        "deepseek" => Box::new(DeepSeekProfile),
        _ => Box::new(GenericProfile),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_profile_defaults() {
        let p = GenericProfile;
        assert_eq!(p.name(), "generic");
        assert_eq!(p.max_tokens_field(), "max_tokens");
        assert!(p.thinking_field().is_none());
        assert_eq!(p.auth_header_value("sk-test"), "Bearer sk-test");
    }

    #[test]
    fn dashscope_profile_thinking_field() {
        let p = DashScopeProfile;
        assert_eq!(p.name(), "dashscope");
        assert_eq!(p.thinking_field(), Some("reasoning_content"));
        assert_eq!(p.max_tokens_field(), "max_tokens");
    }

    #[test]
    fn openai_profile_max_completion_tokens() {
        let p = OpenAIProfile;
        assert_eq!(p.name(), "openai");
        assert_eq!(p.max_tokens_field(), "max_completion_tokens");
        assert!(p.thinking_field().is_none());
    }

    #[test]
    fn deepseek_profile_thinking_field() {
        let p = DeepSeekProfile;
        assert_eq!(p.name(), "deepseek");
        assert_eq!(p.thinking_field(), Some("reasoning_content"));
    }

    #[test]
    fn dashscope_profile_supports_cache_control() {
        assert!(DashScopeProfile.supports_cache_control());
        assert!(!GenericProfile.supports_cache_control());
        assert!(!OpenAIProfile.supports_cache_control());
        assert!(!DeepSeekProfile.supports_cache_control());
        assert!(!profile_from_name("unknown").supports_cache_control());
    }

    #[test]
    fn stream_usage_support_is_opt_in() {
        // Generic endpoints must never receive stream_options; the named
        // profiles we integrate against do support usage telemetry.
        assert!(!GenericProfile.supports_stream_usage());
        assert!(OpenAIProfile.supports_stream_usage());
        assert!(DashScopeProfile.supports_stream_usage());
        assert!(DeepSeekProfile.supports_stream_usage());
        assert!(!profile_from_name("unknown").supports_stream_usage());
    }

    #[test]
    fn profile_from_name_routing() {
        assert_eq!(profile_from_name("dashscope").name(), "dashscope");
        assert_eq!(profile_from_name("coding_plan").name(), "dashscope");
        assert_eq!(profile_from_name("token_plan").name(), "dashscope");
        assert_eq!(profile_from_name("openai").name(), "openai");
        assert_eq!(profile_from_name("deepseek").name(), "deepseek");
        assert_eq!(profile_from_name("unknown").name(), "generic");
        assert_eq!(profile_from_name("").name(), "generic");
    }
}
