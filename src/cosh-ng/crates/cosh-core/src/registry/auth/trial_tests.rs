use super::*;
use crate::config::ProviderConfig;
use std::future::Future;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;

struct Metadata {
    available: bool,
    requests: Arc<std::sync::atomic::AtomicUsize>,
}

impl Metadata {
    async fn start(available: bool) -> Self {
        Self {
            available,
            requests: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }
}

fn isolated(test: impl FnOnce(&Path)) {
    let _lock = crate::sls::tests::ENV_TEST_MUTEX
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let home = tempfile::tempdir().unwrap();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(home.path().join(".copilot-shell"))
        .unwrap();
    let _home = crate::sls::tests::EnvVarGuard::set("HOME", home.path().to_str().unwrap());
    test(home.path());
}

fn run(test: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(test);
}

async fn query(config: &mut CoreConfig, action: &str, params: Value) -> Value {
    serde_json::to_value(handle_auth("trial-test", action, &params, config).await).unwrap()
}

fn configured() -> CoreConfig {
    let mut config = CoreConfig::default();
    config.ai.active_provider = Some("mine".into());
    config.ai.active_model = Some("own-model".into());
    config.ai.providers.insert(
        "mine".into(),
        ProviderConfig {
            provider_type: Some("mock".into()),
            model: Some("own-model".into()),
            api_key: Some("preserved-user-key".into()),
            ..Default::default()
        },
    );
    config.user_ai = config.ai.clone();
    config
}

#[test]
fn ecs_builtin_project_model_cannot_become_a_persistent_user_choice() {
    isolated(|home| {
        run(async {
            for project_ai in [
                "",
                "[ai]\nactive_model = 'project-model'\n",
                "[ai]\nactive_model = 'project-model'\nactive_provider = 'untrusted'\n\
                 [ai.providers.untrusted]\ntype = 'dashscope'\nmodel = 'untrusted-model'\n",
            ] {
                let project = tempfile::tempdir().unwrap();
                std::fs::create_dir(project.path().join(".copilot-shell")).unwrap();
                std::fs::write(
                    project.path().join(".copilot-shell/config.toml"),
                    project_ai,
                )
                .unwrap();
                let user_path = home.join(".copilot-shell/config.toml");
                if user_path.exists() {
                    std::fs::remove_file(&user_path).unwrap();
                }
                let mut config = CoreConfig::load_for_workspace(project.path());
                TEST_ECS_PROBE
                    .scope(
                        (Ok(true), Arc::new(std::sync::atomic::AtomicUsize::new(0))),
                        async {
                            let state = query(
                                &mut config,
                                "state",
                                serde_json::json!({"probe_ecs_trial":true}),
                            )
                            .await;
                            assert_eq!(state["data"]["ecs_trial"]["model"], "auto");
                            let result = query(&mut config, "activate_trial", Value::Null).await;
                            assert_eq!(result["success"], true, "{result}");
                            assert_eq!(result["data"]["model"], "auto");
                        },
                    )
                    .await;
                let saved: CoreConfig =
                    toml::from_str(&std::fs::read_to_string(&user_path).unwrap()).unwrap();
                assert_eq!(saved.ai.active_model.as_deref(), Some("auto"));
                assert_eq!(
                    saved.ai.providers["ecs-free-trial"].model.as_deref(),
                    Some("auto")
                );
                assert!(!saved.ai.providers.contains_key("untrusted"));
                let neutral = tempfile::tempdir().unwrap();
                assert_eq!(
                    CoreConfig::load_for_workspace(neutral.path())
                        .resolve_provider()
                        .model,
                    "auto"
                );
            }
        });
    });
}

#[test]
fn ecs_builtin_state_probes_only_on_explicit_auth_and_never_saves_a_provider() {
    isolated(|home| {
        run(async {
            let server = Metadata::start(true).await;
            let mut config = CoreConfig::default();
            TEST_ECS_PROBE
                .scope(
                    (Ok(server.available), Arc::clone(&server.requests)),
                    async {
                        let startup = query(&mut config, "state", Value::Null).await;
                        assert_eq!(startup["data"]["ecs_trial"]["available"], false);
                        assert!(server.requests.load(Ordering::SeqCst) == 0);
                        let state = query(
                            &mut config,
                            "state",
                            serde_json::json!({"probe_ecs_trial":true}),
                        )
                        .await;
                        assert_eq!(state["data"]["ecs_trial"]["available"], true);
                        assert_eq!(state["data"]["ecs_trial"]["active"], true);
                        assert_eq!(state["data"]["ecs_trial"]["model"], "auto");
                        assert!(config.ai.providers.is_empty());
                        assert!(!home.join(".copilot-shell/config.toml").exists());
                        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
                    },
                )
                .await;
        })
    });
}

#[test]
fn ecs_builtin_non_ecs_cannot_activate_or_change_existing_configuration() {
    isolated(|home| {
        run(async {
            let server = Metadata::start(false).await;
            let mut config = configured();
            crate::config::persist_config(&config).unwrap();
            let before = std::fs::read(home.join(".copilot-shell/config.toml")).unwrap();
            TEST_ECS_PROBE
                .scope(
                    (Ok(server.available), Arc::clone(&server.requests)),
                    async {
                        let state = query(
                            &mut config,
                            "state",
                            serde_json::json!({"probe_ecs_trial":true}),
                        )
                        .await;
                        assert_eq!(state["data"]["ecs_trial"]["available"], false);
                        let result = query(&mut config, "activate_trial", Value::Null).await;
                        assert_eq!(result["success"], false);
                        assert_eq!(config.ai.active_provider.as_deref(), Some("mine"));
                        assert_eq!(
                            std::fs::read(home.join(".copilot-shell/config.toml")).unwrap(),
                            before
                        );
                        assert!(!home.join(".copilot-shell/cosh/preferences.toml").exists());
                    },
                )
                .await;
        })
    });
}

#[test]
fn ecs_builtin_activation_round_trip_preserves_other_provider_and_clears_suppression() {
    isolated(|home| {
        run(async {
            let server = Metadata::start(true).await;
            let mut config = configured();
            crate::auth::trial_preferences::save(false).unwrap();
            TEST_ECS_PROBE
                .scope(
                    (Ok(server.available), Arc::clone(&server.requests)),
                    async {
                        let activated = query(&mut config, "activate_trial", Value::Null).await;
                        assert_eq!(activated["success"], true, "{activated}");
                        let id = config.ai.active_provider.clone().unwrap();
                        let trial = config.ai.providers.get(&id).unwrap();
                        assert_eq!(trial.provider_type.as_deref(), Some("aliyun"));
                        assert_eq!(trial.auth_source.as_deref(), Some("ecs_identity"));
                        assert_eq!(trial.model.as_deref(), Some("auto"));
                        assert!(
                            trial.access_key_id.is_none()
                                && trial.access_key_secret.is_none()
                                && trial.security_token.is_none()
                                && trial.api_key.is_none()
                        );
                        assert_eq!(config.ai.active_model.as_deref(), Some("auto"));
                        assert_eq!(
                            config.ai.providers["mine"].api_key.as_deref(),
                            Some("preserved-user-key")
                        );
                        assert!(crate::auth::trial_preferences::load().unwrap());
                        let disk = std::fs::read_to_string(home.join(".copilot-shell/config.toml"))
                            .unwrap();
                        let restored: CoreConfig = toml::from_str(&disk).unwrap();
                        assert_eq!(restored.ai.active_provider.as_deref(), Some(id.as_str()));
                        assert_eq!(
                            query(
                                &mut config,
                                "activate",
                                serde_json::json!({"provider_id":"mine"})
                            )
                            .await["success"],
                            true
                        );
                        assert_eq!(config.ai.active_model.as_deref(), Some("own-model"));
                        crate::auth::trial_preferences::save(false).unwrap();
                        assert_eq!(
                            query(&mut config, "activate_trial", Value::Null).await["success"],
                            true
                        );
                        assert_eq!(config.ai.active_provider.as_deref(), Some(id.as_str()));
                        assert_eq!(config.ai.providers.len(), 2);
                        assert!(crate::auth::trial_preferences::load().unwrap());
                    },
                )
                .await;
        })
    });
}

#[test]
fn ecs_builtin_activation_does_not_overwrite_a_colliding_provider_name() {
    isolated(|_| {
        run(async {
            let server = Metadata::start(true).await;
            let mut config = configured();
            config
                .ai
                .providers
                .insert("ecs-free-trial".into(), config.ai.providers["mine"].clone());
            config.user_ai = config.ai.clone();
            TEST_ECS_PROBE
                .scope(
                    (Ok(server.available), Arc::clone(&server.requests)),
                    async {
                        let result = query(&mut config, "activate_trial", Value::Null).await;
                        assert_eq!(result["success"], true, "{result}");
                        assert_ne!(config.ai.active_provider.as_deref(), Some("ecs-free-trial"));
                        assert_eq!(
                            config.ai.providers["ecs-free-trial"].api_key.as_deref(),
                            Some("preserved-user-key")
                        );
                    },
                )
                .await;
        })
    });
}

#[test]
fn ecs_builtin_activation_fails_closed_when_preferences_are_invalid() {
    isolated(|home| {
        run(async {
            let server = Metadata::start(true).await;
            let mut config = configured();
            let path = home.join(".copilot-shell/cosh/preferences.toml");
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path.parent().unwrap())
                .unwrap();
            std::fs::write(&path, "[invalid").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            TEST_ECS_PROBE
                .scope(
                    (Ok(server.available), Arc::clone(&server.requests)),
                    async {
                        let result = query(&mut config, "activate_trial", Value::Null).await;
                        assert_eq!(result["success"], false);
                        assert_eq!(result["data"]["error_code"], "trial_preferences_invalid");
                        assert_eq!(config.ai.active_provider.as_deref(), Some("mine"));
                        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[invalid");
                        assert!(!home.join(".copilot-shell/config.toml").exists());
                    },
                )
                .await;
        })
    });
}

#[test]
fn ecs_builtin_failed_config_save_restores_disabled_preference_and_active_provider() {
    isolated(|home| {
        run(async {
            let server = Metadata::start(true).await;
            let mut config = configured();
            crate::auth::trial_preferences::save(false).unwrap();
            std::fs::create_dir(home.join(".copilot-shell/config.toml")).unwrap();
            TEST_ECS_PROBE
                .scope(
                    (Ok(server.available), Arc::clone(&server.requests)),
                    async {
                        let result = query(&mut config, "activate_trial", Value::Null).await;
                        assert_eq!(result["success"], false);
                        assert_eq!(result["data"]["error_code"], "trial_activation_save_failed");
                        assert_eq!(config.ai.active_provider.as_deref(), Some("mine"));
                        assert!(!crate::auth::trial_preferences::load().unwrap());
                        assert_eq!(config.ai.providers.len(), 1);
                    },
                )
                .await;
        })
    });
}

#[test]
fn ecs_builtin_first_save_uses_the_fixed_displayed_model() {
    isolated(|_| {
        run(async {
            let server = Metadata::start(true).await;
            let mut config = CoreConfig::default();
            config.ai.active_model = Some("chosen-trial-model".into());
            TEST_ECS_PROBE
                .scope((Ok(true), Arc::clone(&server.requests)), async {
                    let state = query(
                        &mut config,
                        "state",
                        serde_json::json!({"probe_ecs_trial":true}),
                    )
                    .await;
                    assert_eq!(state["data"]["ecs_trial"]["model"], "auto");
                    assert_eq!(
                        query(&mut config, "activate_trial", Value::Null).await["success"],
                        true
                    );
                    let id = config.ai.active_provider.as_ref().unwrap();
                    assert_eq!(config.ai.active_model.as_deref(), Some("auto"));
                    assert_eq!(config.user_ai.providers[id].model.as_deref(), Some("auto"));
                })
                .await;
        })
    });
}

#[test]
fn ecs_builtin_default_provider_model_cannot_override_automatic_access() {
    isolated(|_| {
        run(async {
            let server = Metadata::start(true).await;
            let mut config = CoreConfig::default();
            config.ai.providers.insert(
                "default".into(),
                ProviderConfig {
                    model: Some("generic-trial-model".into()),
                    ..Default::default()
                },
            );
            config.user_ai = config.ai.clone();
            TEST_ECS_PROBE
                .scope((Ok(true), Arc::clone(&server.requests)), async {
                    let state = query(
                        &mut config,
                        "state",
                        serde_json::json!({"probe_ecs_trial":true}),
                    )
                    .await;
                    assert_eq!(state["data"]["ecs_trial"]["model"], "auto");
                    assert_eq!(
                        query(&mut config, "activate_trial", Value::Null).await["success"],
                        true
                    );
                    assert_eq!(config.ai.active_model.as_deref(), Some("auto"));
                })
                .await;
        })
    });
}

#[test]
fn identity_stability_registry_preserves_probe_error_without_committing() {
    isolated(|home| {
        run(async {
            for error in [
                ProbeError::AccessDenied,
                ProbeError::InvalidResponse,
                ProbeError::Timeout,
                ProbeError::Http,
            ] {
                let mut config = configured();
                crate::config::persist_config(&config).unwrap();
                crate::auth::trial_preferences::save(false).unwrap();
                let before = std::fs::read(home.join(".copilot-shell/config.toml")).unwrap();
                TEST_ECS_PROBE
                    .scope(
                        (Err(error), Arc::new(std::sync::atomic::AtomicUsize::new(0))),
                        async {
                            let result = query(&mut config, "activate_trial", Value::Null).await;
                            assert_eq!(result["data"]["error_code"], error.code(), "{result}");
                            assert_eq!(result["success"], false);
                            assert_eq!(config.ai.active_provider.as_deref(), Some("mine"));
                            assert_eq!(
                                std::fs::read(home.join(".copilot-shell/config.toml")).unwrap(),
                                before
                            );
                            assert!(!crate::auth::trial_preferences::load().unwrap());
                        },
                    )
                    .await;
            }
        });
    });
}

#[test]
fn explicit_reenable_normalizes_implicit_and_saved_models_to_auto() {
    for saved in [false, true] {
        isolated(|home| {
            run(async {
                let mut config = CoreConfig::default();
                config.ai.active_model = Some("chosen-trial-model".into());
                if saved {
                    config.ai.active_provider = Some("trial".into());
                    config.ai.providers.insert(
                        "trial".into(),
                        ProviderConfig {
                            provider_type: Some("aliyun".into()),
                            auth_source: Some("ecs_identity".into()),
                            model: Some("previous-model".into()),
                            ..Default::default()
                        },
                    );
                }
                config.user_ai = config.ai.clone();
                crate::auth::trial_preferences::save(false).unwrap();
                TEST_ECS_PROBE
                    .scope(
                        (Ok(true), Arc::new(std::sync::atomic::AtomicUsize::new(0))),
                        async {
                            let result = query(&mut config, "activate_trial", Value::Null).await;
                            assert_eq!(result["success"], true, "{result}");
                            assert_eq!(config.ai.active_model.as_deref(), Some("auto"));
                            let id = config.ai.active_provider.as_ref().unwrap();
                            assert_eq!(config.user_ai.providers[id].model.as_deref(), Some("auto"));
                            let stored: CoreConfig = toml::from_str(
                                &std::fs::read_to_string(home.join(".copilot-shell/config.toml"))
                                    .unwrap(),
                            )
                            .unwrap();
                            assert_eq!(stored.ai.active_model.as_deref(), Some("auto"));
                            assert!(crate::auth::trial_preferences::load().unwrap());
                        },
                    )
                    .await;
            })
        });
    }
}
