pub(crate) mod audit;
mod hook_feedback;
mod language;
mod load;
mod model;
mod parse;
mod readonly;
mod trust;

pub use hook_feedback::{
    clear_hook_feedback_store, load_hook_feedback_preference_details,
    load_hook_feedback_preferences, record_hook_feedback_key, record_hook_feedback_preference,
    HookFeedbackPreference,
};
pub use language::{
    detect_language_from_env, language_config_status, parse_language_setting,
    resolve_language_setting, write_user_language_config, Language, LanguageConfigStatus,
    LanguageSetting,
};
pub use load::load_config;
pub(crate) use load::parse_recommendations_environment_override;
pub use model::{
    CoshConfig, HealthConfig, HealthServiceConfig, HealthServiceExpectedState,
    RecommendationsConfig,
};
pub use trust::{clear_project_trust_store, trust_project_root, untrust_project_root};

#[cfg(test)]
mod tests;
