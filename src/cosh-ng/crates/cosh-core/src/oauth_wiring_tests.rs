//! Factory-level priority regression: OAuth vs explicit provider vs legacy AK/ECS.
//!
//! HOME-mutating cases use the shared `ENV_TEST_MUTEX` via a synchronous
//! `isolated` frame (the lock guard never crosses an await), matching the
//! repo's other environment-sensitive tests.

use std::sync::{Arc, Mutex};

use futures::StreamExt;

use crate::config::CoreConfig;
use crate::provider::sysom::oauth::contract::OAuthError;
use crate::provider::sysom::oauth::store::{OAuthSecretBackend, OAuthStore};
use crate::provider::{GenerateConfig, GenerateEvent, Message};
use crate::sls::tests::EnvVarGuard;

use super::{create_provider, needs_auth};

#[derive(Default)]
struct MemoryBackend {
    slots: Mutex<std::collections::HashMap<String, String>>,
}
impl OAuthSecretBackend for MemoryBackend {
    fn set(&self, slot: &str, token: &str) -> Result<(), OAuthError> {
        self.slots.lock().unwrap().insert(slot.into(), token.into());
        Ok(())
    }
    fn get(&self, slot: &str) -> Result<Option<String>, OAuthError> {
        Ok(self.slots.lock().unwrap().get(slot).cloned())
    }
    fn delete(&self, slot: &str) -> Result<(), OAuthError> {
        self.slots.lock().unwrap().remove(slot);
        Ok(())
    }
}

fn isolated(test: impl FnOnce(&std::path::Path)) {
    let _lock = crate::sls::tests::ENV_TEST_MUTEX
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".copilot-shell")).unwrap();
    let _home = EnvVarGuard::set("HOME", home.path().to_str().unwrap());
    test(home.path());
}

fn run(test: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(test);
}

fn logged_in_store(dir: &std::path::Path, backend: Arc<MemoryBackend>) -> Arc<OAuthStore> {
    let config_dir = dir.join(".copilot-shell");
    std::fs::create_dir_all(&config_dir).unwrap();
    let store = Arc::new(OAuthStore::new(&config_dir, backend));
    let starting = store.read_state().unwrap();
    let client_id =
        crate::provider::sysom::oauth::contract::expected_client_id().unwrap_or("client-1");
    store
        .commit_login(&starting, client_id, "refresh-canary", None)
        .unwrap();
    store
}

#[test]
fn active_oauth_login_selects_the_oauth_source_with_its_generation() {
    isolated(|home| {
        run(async {
            let backend = Arc::new(MemoryBackend::default());
            crate::auth::oauth::TEST_BACKEND
                .scope(backend.clone(), async {
                    logged_in_store(home, backend.clone());
                    let config = CoreConfig::default();
                    assert!(!needs_auth(&config));
                    assert_eq!(config.resolve_provider().model, "auto");
                    let provider = create_provider(&config);
                    assert_eq!(
                        provider.auth_generation(),
                        Some(1),
                        "factory must pick the OAuth source ahead of environment AK"
                    );
                })
                .await;
        })
    })
}

#[test]
fn needs_login_returns_a_clear_error_without_browser_or_ak_fallback() {
    isolated(|home| {
        run(async {
            let _ak = EnvVarGuard::set("ALIBABA_CLOUD_ACCESS_KEY_ID", "env-ak-canary");
            let _sk = EnvVarGuard::set("ALIBABA_CLOUD_ACCESS_KEY_SECRET", "env-sk-canary");
            let backend = Arc::new(MemoryBackend::default());
            crate::auth::oauth::TEST_BACKEND
                .scope(backend.clone(), async {
                    let store = logged_in_store(home, backend.clone());
                    store.deactivate_generation(1).unwrap();
                    let config = CoreConfig::default();
                    assert!(!needs_auth(&config));
                    let provider = create_provider(&config);
                    let result = provider
                        .generate(&[Message::user("hi")], &[], &GenerateConfig::default())
                        .await;
                    assert!(matches!(result, Err(code) if code == "OAuthLoginRequired"));
                    assert_eq!(provider.auth_generation(), None);
                })
                .await;
        })
    })
}

#[test]
fn explicit_provider_change_after_login_supersedes_oauth() {
    isolated(|home| {
        run(async {
            let backend = Arc::new(MemoryBackend::default());
            crate::auth::oauth::TEST_BACKEND
                .scope(backend.clone(), async {
                    logged_in_store(home, backend.clone());
                    let config: CoreConfig = toml::from_str(
                        "[ai]\nactive_provider = 'mockp'\n[ai.providers.mockp]\ntype = 'mock'\n",
                    )
                    .unwrap();
                    let provider = create_provider(&config);
                    let events: Vec<_> = provider
                        .generate(
                            &[Message::user("explicit-canary")],
                            &[],
                            &GenerateConfig::default(),
                        )
                        .await
                        .unwrap()
                        .collect()
                        .await;
                    assert!(
                        events.iter().any(|event| matches!(event, GenerateEvent::TextDelta(text) if text.contains("explicit-canary"))),
                        "{events:?}"
                    );
                    assert_eq!(provider.auth_generation(), None);
                })
                .await;
        })
    })
}

#[test]
fn explicit_model_choice_is_preserved_under_oauth() {
    isolated(|home| {
        run(async {
            let backend = Arc::new(MemoryBackend::default());
            crate::auth::oauth::TEST_BACKEND
                .scope(backend.clone(), async {
                    logged_in_store(home, backend.clone());
                    let mut config = CoreConfig::default();
                    config.ai.active_model = Some("qwen3.7-plus".into());
                    assert_eq!(config.resolve_provider().model, "qwen3.7-plus");
                })
                .await;
        })
    })
}

#[test]
fn deactivated_oauth_does_not_block_a_switched_ecs_provider() {
    isolated(|home| {
        run(async {
            let backend = Arc::new(MemoryBackend::default());
            crate::auth::oauth::TEST_BACKEND
                .scope(backend.clone(), async {
                    let store = logged_in_store(home, backend.clone());
                    store.deactivate_generation(1).unwrap();
                    let config: CoreConfig = toml::from_str(
                        "[ai]\nactive_provider = 'ecs'\n[ai.providers.ecs]\ntype = 'aliyun'\nauth_source = 'ecs_ram_role'\n",
                    )
                    .unwrap();
                    assert!(!needs_auth(&config));
                    let provider = create_provider(&config);
                    assert_eq!(
                        provider.auth_generation(),
                        None,
                        "a deactivated OAuth login must not shadow the switched ECS provider"
                    );
                })
                .await;
        })
    })
}

#[test]
fn without_oauth_selection_legacy_priorities_are_unchanged() {
    isolated(|_home| {
        let ecs: CoreConfig = toml::from_str(
            "[ai]\nactive_provider = 'e'\n[ai.providers.e]\ntype = 'aliyun'\nauth_source = 'ecs_ram_role'\n",
        )
        .unwrap();
        assert!(!needs_auth(&ecs));
        let _ak = EnvVarGuard::set("ALIBABA_CLOUD_ACCESS_KEY_ID", "");
        let _sk = EnvVarGuard::set("ALIBABA_CLOUD_ACCESS_KEY_SECRET", "");
        std::env::remove_var("ALIBABA_CLOUD_ACCESS_KEY_ID");
        std::env::remove_var("ALIBABA_CLOUD_ACCESS_KEY_SECRET");
        let generic = CoreConfig::default();
        assert!(needs_auth(&generic));
        std::env::set_var("ALIBABA_CLOUD_ACCESS_KEY_ID", "env-ak");
        std::env::set_var("ALIBABA_CLOUD_ACCESS_KEY_SECRET", "env-sk");
        let aliyun: CoreConfig =
            toml::from_str("[ai]\nactive_provider = 'a'\n[ai.providers.a]\ntype = 'aliyun'\n")
                .unwrap();
        assert!(!needs_auth(&aliyun));
    })
}

#[test]
fn persisting_ai_config_never_touches_the_oauth_state() {
    isolated(|home| {
        let backend = Arc::new(MemoryBackend::default());
        crate::auth::oauth::TEST_BACKEND.sync_scope(backend.clone(), || {
            let store = logged_in_store(home, backend.clone());
            let state_path = home
                .join(".copilot-shell")
                .join(crate::provider::sysom::oauth::contract::state_file_name());
            let before = std::fs::read(&state_path).unwrap();
            let config = CoreConfig::default();
            crate::config::persist_config(&config).unwrap();
            assert_eq!(std::fs::read(&state_path).unwrap(), before);
            let toml = std::fs::read_to_string(home.join(".copilot-shell/config.toml")).unwrap();
            assert!(!toml.contains("oauth"), "[ai] must stay rollback-safe");
            assert!(store.read_state().unwrap().selected);
        });
    })
}
