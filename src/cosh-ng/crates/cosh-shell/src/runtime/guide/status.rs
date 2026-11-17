//! Read-only wizard status. An unreadable value is never reported as off.

use crate::config::guide::{GuideStore, RuntimeState};
use crate::config::{
    parse_language_setting, resolve_language_setting, saved_language_setting, Language,
    LanguageSetting,
};
use crate::runtime::login::{query_status, LoginStatus};

use crate::adapter::{CoshCoreAdapter, GuideAuthSummary};
use crate::config::guide::UserPaths;
use crate::config::{load_guidance_config, parse_recommendations_environment_override};
use crate::recommendation::personal_model::resolve_recommendations_enabled;
use crate::recommendation::personal_store::{PersonalStore, StoreError};

pub(crate) fn warn_failure(operation: &'static str, stage: &'static str) {
    // Config and handoff errors may contain secrets; keep diagnostics categorical.
    tracing::warn!(
        operation,
        stage,
        result = "failed",
        "guide operation failed"
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reading<T> {
    Value(T),
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    Config,
    Env,
    Default,
}

pub(crate) struct LanguageStatus {
    pub(crate) saved: Reading<LanguageSetting>,
    pub(crate) effective: Language,
    pub(crate) source: Source,
}

pub(crate) struct Snapshot {
    pub(crate) language: LanguageStatus,
    pub(crate) guide_tip: Reading<bool>,
    pub(crate) guide_runtime: Reading<bool>,
    pub(crate) health: Reading<bool>,
    pub(crate) login: Result<LoginStatus, String>,
}

/// Read-only recommendation facts: saved preference, effective value, source,
/// and whether the store is damaged (recoverable) vs merely unreadable.
pub(crate) struct RecStatus {
    pub(crate) user_enabled: Reading<bool>,
    pub(crate) effective: Reading<bool>,
    pub(crate) forced_off: bool,
    pub(crate) source: &'static str,
    pub(crate) damaged: bool,
}

pub(crate) fn load_recommendations(paths: &UserPaths) -> RecStatus {
    let configured = load_guidance_config().recommendations.enabled;
    let environment = parse_recommendations_environment_override(
        std::env::var("COSH_RECOMMENDATIONS_ENABLED")
            .ok()
            .as_deref(),
    );
    let forced_off = environment == Some(false);
    let unreadable = RecStatus {
        user_enabled: Reading::Unknown,
        effective: Reading::Unknown,
        forced_off,
        source: "default",
        damaged: false,
    };
    let Some(cosh_dir) = &paths.cosh_dir else {
        return unreadable;
    };
    let root = cosh_dir.join("recommendations");
    // Read-only: never create the store directory just to display status.
    let loaded = if root.exists() {
        match PersonalStore::open(&root) {
            Ok(store) => store.peek(),
            Err(error) => Err(error),
        }
    } else {
        Ok(None)
    };
    match loaded {
        Ok(state) => {
            let user_enabled = state.as_ref().and_then(|s| s.preferences.user_enabled);
            let effective = resolve_recommendations_enabled(environment, user_enabled, configured);
            let source = if forced_off {
                "env"
            } else if user_enabled.is_some() {
                "user"
            } else if environment.is_some() {
                "env"
            } else if state.is_some() {
                "config"
            } else {
                "default"
            };
            RecStatus {
                user_enabled: match user_enabled {
                    Some(value) => Reading::Value(value),
                    None => Reading::Unknown,
                },
                effective: Reading::Value(effective),
                forced_off,
                source,
                damaged: false,
            }
        }
        Err(error) => {
            warn_failure("load_recommendations", "read_store");
            RecStatus {
                damaged: matches!(
                    error,
                    StoreError::CorruptState
                        | StoreError::UnsupportedSchema
                        | StoreError::StateTooLarge
                ),
                ..unreadable
            }
        }
    }
}

pub(crate) fn now_hour_bucket() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() / 3600)
        .unwrap_or_default()
}

pub(crate) fn load(paths: &UserPaths) -> Snapshot {
    let (guide_tip, health) = toggles(paths);
    let snapshot = Snapshot {
        language: language(),
        guide_tip,
        guide_runtime: guide_runtime(paths),
        health,
        login: query_status(None),
    };
    if matches!(snapshot.language.saved, Reading::Unknown)
        || matches!(snapshot.guide_tip, Reading::Unknown)
        || snapshot.login.is_err()
    {
        tracing::warn!(
            operation = "load_status",
            stage = "read",
            result = "failed",
            language_unreadable = matches!(snapshot.language.saved, Reading::Unknown),
            guide_unreadable = matches!(snapshot.guide_tip, Reading::Unknown),
            login_unreadable = snapshot.login.is_err(),
            "guide status incomplete"
        );
    }
    snapshot
}

/// Bounded read of the current AI service; runs off the UI thread because the
/// registry query spawns cosh-core. An unavailable core is Unknown, not Off.
pub(crate) fn load_ai() -> Reading<GuideAuthSummary> {
    let Ok(program) = super::core::admit() else {
        warn_failure("load_ai", "core_admission");
        return Reading::Unknown;
    };
    match CoshCoreAdapter::new(program, false).guide_auth_summary() {
        Ok(summary) => Reading::Value(summary),
        Err(_) => {
            warn_failure("load_ai", "query_core");
            Reading::Unknown
        }
    }
}

fn language() -> LanguageStatus {
    let (saved, mut setting, mut source) = match saved_language_setting() {
        Ok(Some(setting)) => (Reading::Value(setting), setting, Source::Config),
        Ok(None) => (
            Reading::Value(LanguageSetting::Auto),
            LanguageSetting::Auto,
            Source::Default,
        ),
        Err(_) => (Reading::Unknown, LanguageSetting::Auto, Source::Default),
    };
    if let Some(override_setting) = std::env::var("COSH_SHELL_LANG")
        .ok()
        .and_then(|value| parse_language_setting(&value))
    {
        setting = override_setting;
        source = Source::Env;
    }
    LanguageStatus {
        saved,
        effective: resolve_language_setting(setting),
        source,
    }
}

fn toggles(paths: &UserPaths) -> (Reading<bool>, Reading<bool>) {
    let Some(cosh_dir) = &paths.cosh_dir else {
        return (Reading::Unknown, Reading::Unknown);
    };
    let store = match GuideStore::open_existing(cosh_dir.join("guide")) {
        Ok(Some(store)) => store,
        Ok(None) => return (Reading::Value(true), Reading::Value(true)),
        Err(_) => return (Reading::Unknown, Reading::Unknown),
    };
    match store.peek_preferences() {
        Ok(Some(preferences)) => (
            Reading::Value(preferences.guide_enabled),
            Reading::Value(preferences.banner_enabled),
        ),
        Ok(None) => (Reading::Value(true), Reading::Value(true)),
        Err(_) => (Reading::Unknown, Reading::Unknown),
    }
}

fn guide_runtime(paths: &UserPaths) -> Reading<bool> {
    let Some(base) = &paths.runtime_base else {
        return Reading::Unknown;
    };
    match RuntimeState::open_existing(base) {
        Ok(Some(runtime)) => match runtime.read() {
            Ok(Some(snapshot)) => Reading::Value(snapshot.enabled),
            _ => Reading::Unknown,
        },
        _ => Reading::Unknown,
    }
}
