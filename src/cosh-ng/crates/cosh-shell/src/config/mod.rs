pub(crate) mod audit;
pub(crate) mod guide;
mod hook_feedback;
mod language;
mod login_health;
pub(crate) use login_health::{read_login_health_config, HealthLoginConfig};
mod load;
mod model;
mod parse;
pub(crate) mod private_state;
mod readonly;
mod shell_prefs;
mod trust;

pub use hook_feedback::{
    clear_hook_feedback_store, load_hook_feedback_preference_details,
    load_hook_feedback_preferences, record_hook_feedback_key, record_hook_feedback_preference,
    HookFeedbackPreference,
};
mod guarded_write;
pub(crate) use language::write_user_language_config_checked;
pub use language::{
    detect_language_from_env, language_config_status, parse_language_setting,
    resolve_language_setting, write_user_language_config, Language, LanguageConfigStatus,
    LanguageSetting,
};
pub(crate) use language::{read_guidance_language, saved_language_setting};
pub use load::load_config;
pub(crate) use load::{load_guidance_config, parse_recommendations_environment_override};
pub(crate) use model::ConsentedProjectHook;
pub use model::{
    CoshConfig, HealthConfig, HealthServiceConfig, HealthServiceExpectedState,
    RecommendationsConfig,
};
pub(crate) use shell_prefs::{
    analysis_mode_status, bash_history_status, parse_analysis_mode, write_analysis_mode,
    write_analysis_mode_checked, write_bash_history, write_bash_history_checked,
    AnalysisModeStatus, BashHistoryStatus,
};
pub(crate) use trust::{
    add_consented_project_hook_to_store_path, project_hook_trust,
    read_consented_project_hooks_from_store_path, trust_project_root_to_store_path,
    ProjectHookTrust,
};
pub use trust::{clear_project_trust_store, trust_project_root, untrust_project_root};

#[cfg(test)]
mod tests;
