//! Request-scoped trial selection leaves persisted provider configuration untouched.

use std::sync::Arc;

use tokio::sync::OnceCell;

use crate::auth::trial_policy::{self, ProviderIntent, TrialDecision};
use crate::core::{provider_access, CoshCore};
use crate::protocol::{ecs_trial::TrialIntent, EcsTrialContext};
use crate::provider::{
    ContentGenerator, DispatchState, GenerateConfig, GenerateEvent, GenerateStream, Message,
    ProviderFailure, ProviderFailureKind, ToolDeclaration,
};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Prepared {
    Existing,
    Trial,
}

pub(super) async fn prepare(
    engine: &mut CoshCore,
    capable: bool,
    context: Option<EcsTrialContext>,
) -> Result<Prepared, &'static str> {
    let intent = ProviderIntent::from_env();
    let existing = trial_policy::decide(&engine.config, &intent, false, false, false)
        == TrialDecision::Existing;
    let preference = if capable && !existing {
        tokio::task::spawn_blocking(crate::auth::trial_preferences::load)
            .await
            .unwrap_or(Err(crate::auth::trial_preferences::Error::Worker))
    } else {
        Ok(false)
    };
    let result = apply_policy(
        engine,
        capable,
        context,
        &intent,
        preference.unwrap_or(false),
    );
    result.map_err(|code| {
        if code == "ModelNotConfigured" {
            preference.err().map(|error| error.code()).unwrap_or(code)
        } else {
            code
        }
    })
}

fn apply_policy(
    engine: &mut CoshCore,
    capable: bool,
    context: Option<EcsTrialContext>,
    intent: &ProviderIntent,
    persistent_auto_use: bool,
) -> Result<Prepared, &'static str> {
    let was_trial =
        engine.trial_access.take().is_some() || engine.trial_identity_provider.is_some();
    let context = context.filter(|context| capable && context.valid());
    let resolved = engine.config.resolve_provider();
    let decision = if capable {
        trial_policy::decide(
            &engine.config,
            intent,
            context
                .as_ref()
                .is_some_and(|context| context.intent == TrialIntent::UserInitiated),
            context
                .as_ref()
                .is_some_and(|context| context.auto_use_allowed),
            persistent_auto_use,
        )
    } else if resolved.auth_source.as_deref() == Some("ecs_identity") {
        engine.provider = super::auth::unconfigured_provider();
        return Err("EcsTrialInteractionRequired");
    } else {
        TrialDecision::Existing
    };
    match decision {
        TrialDecision::Existing => {
            if was_trial {
                engine.provider = crate::create_provider(&engine.config);
                engine.model = resolved.model;
                engine.extra_params = resolved.extra_params;
            }
            engine.trial_identity_provider = None;
            Ok(Prepared::Existing)
        }
        TrialDecision::Disabled => {
            engine.provider = super::auth::unconfigured_provider();
            Err("ModelNotConfigured")
        }
        TrialDecision::EcsIdentity => {
            let selected_name = engine.config.ai.active_provider.as_deref().or_else(|| {
                (engine.config.ai.providers.len() == 1)
                    .then(|| engine.config.ai.providers.keys().next().map(String::as_str))
                    .flatten()
            });
            let resolved = selected_name
                .map(|name| engine.config.resolve_named_provider(name))
                .unwrap_or(resolved);
            engine.model = trial_policy::MODEL.to_string();
            engine.extra_params = resolved.extra_params;
            let endpoint = resolved.sysom_endpoint;
            let cached = engine
                .trial_identity_provider
                .as_ref()
                .filter(|(previous, _)| previous == &endpoint)
                .map(|(_, provider)| Arc::clone(provider));
            let provider = cached.unwrap_or_else(|| {
                Arc::new(IdentifiedProvider {
                    inner: Arc::new(
                        crate::provider::sysom::SysomProvider::from_ecs_identity_with_admission(
                            &endpoint,
                            Arc::new(|| crate::auth::trial_preferences::load().unwrap_or(false)),
                        ),
                    ),
                    identified: OnceCell::new(),
                })
            });
            engine.trial_identity_provider = Some((endpoint, Arc::clone(&provider)));
            let (provider, access) =
                provider_access::wrap(provider, context.ok_or("ModelNotConfigured")?);
            engine.provider = provider;
            engine.trial_access = Some(access);
            Ok(Prepared::Trial)
        }
    }
}

struct IdentifiedProvider {
    inner: Arc<dyn ContentGenerator>,
    identified: OnceCell<()>,
}

fn classify_identity_probe(
    result: Result<
        Option<crate::provider::sysom::EcsAuthChallenge>,
        crate::provider::sysom::ProbeError,
    >,
) -> Result<(), ProviderFailure> {
    let code = match result {
        Ok(Some(_)) => return Ok(()),
        Ok(None) => "EcsIdentityUnavailable",
        Err(error) => error.code(),
    };
    Err(ProviderFailure {
        code: code.to_string(),
        kind: ProviderFailureKind::Identity,
        dispatch: DispatchState::NotDispatched,
        request_id: None,
    })
}

#[async_trait::async_trait]
impl ContentGenerator for IdentifiedProvider {
    async fn generate(
        &self,
        messages: &[Message],
        tools: &[ToolDeclaration],
        config: &GenerateConfig,
    ) -> Result<GenerateStream, String> {
        let identity = self
            .identified
            .get_or_try_init(|| async {
                classify_identity_probe(crate::provider::sysom::detect_ecs_auth_challenge().await)
            })
            .await;
        if let Err(failure) = identity {
            return Ok(Box::pin(futures::stream::iter(vec![
                GenerateEvent::ProviderFailure(failure),
            ])));
        }
        self.inner.generate(messages, tools, config).await
    }

    fn cancel(&self) {
        self.inner.cancel();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::auth::trial_policy::ProviderIntent;
    use crate::config::CoreConfig;
    use crate::core::CoshCore;
    use crate::protocol::EcsTrialContext;
    use crate::provider::{mock::MockProvider, ContentGenerator};
    use crate::tool::ToolRegistry;

    fn context(allowed: bool) -> EcsTrialContext {
        serde_json::from_value(serde_json::json!({
            "schema_version":1,"shell_session_id":"shell-one",
            "intent":"user_initiated","auto_use_allowed":allowed
        }))
        .unwrap()
    }

    #[test]
    fn ecs_trial_selects_its_default_model_without_changing_configuration() {
        let mut core = CoshCore::new_legacy(
            CoreConfig::default(),
            Box::new(MockProvider::history_echo()),
            ToolRegistry::new(),
        );
        assert!(super::apply_policy(
            &mut core,
            true,
            Some(context(true)),
            &ProviderIntent::default(),
            true,
        )
        .is_ok());
        assert_eq!(core.model, "auto");
        assert_eq!(core.config.ai.active_model, None);
    }

    #[test]
    fn singleton_identity_provider_preserves_endpoint_and_params_with_fixed_model() {
        let endpoint = "https://sysom.cn-hangzhou.aliyuncs.com";
        let extra = serde_json::json!({"enable_thinking":false});
        let mut config = CoreConfig::default();
        config.ai.providers.insert(
            "trial".into(),
            crate::config::ProviderConfig {
                provider_type: Some("aliyun".into()),
                auth_source: Some("ecs_identity".into()),
                sysom_endpoint: Some(endpoint.into()),
                model: Some("selected-model".into()),
                extra_params: Some(extra.clone()),
                ..Default::default()
            },
        );
        let mut core = CoshCore::new_legacy(
            config,
            Box::new(MockProvider::history_echo()),
            ToolRegistry::new(),
        );
        assert_eq!(
            super::apply_policy(
                &mut core,
                true,
                Some(context(true)),
                &ProviderIntent::default(),
                true
            )
            .unwrap(),
            super::Prepared::Trial
        );
        assert_eq!(core.trial_identity_provider.as_ref().unwrap().0, endpoint);
        assert_eq!(core.model, "auto");
        assert_eq!(core.extra_params, Some(extra));
        assert_eq!(core.config.ai.active_provider, None);
        assert_eq!(core.config.user_ai.active_provider, None);
    }

    #[test]
    fn ecs_trial_ignores_an_explicit_model_override() {
        let mut config = CoreConfig::default();
        config.ai.active_model = Some("custom-model".to_string());
        let mut core = CoshCore::new_legacy(
            config,
            Box::new(MockProvider::history_echo()),
            ToolRegistry::new(),
        );
        assert!(super::apply_policy(
            &mut core,
            true,
            Some(context(true)),
            &ProviderIntent::default(),
            true,
        )
        .is_ok());
        assert_eq!(core.model, "auto");
    }

    #[test]
    fn ecs_trial_ignores_a_provider_model_override() {
        for name in ["default", "generic"] {
            let mut config = CoreConfig::default();
            config.ai.providers.insert(
                name.to_string(),
                crate::config::ProviderConfig {
                    model: Some("provider-model".to_string()),
                    ..Default::default()
                },
            );
            let mut core = CoshCore::new_legacy(
                config,
                Box::new(MockProvider::history_echo()),
                ToolRegistry::new(),
            );
            assert!(super::apply_policy(
                &mut core,
                true,
                Some(context(true)),
                &ProviderIntent::default(),
                true,
            )
            .is_ok());
            assert_eq!(core.model, "auto", "{name}");
        }
    }

    #[test]
    fn cached_identity_cannot_bypass_a_new_users_suppression() {
        let mut core = CoshCore::new_legacy(
            CoreConfig::default(),
            Box::new(MockProvider::history_echo()),
            ToolRegistry::new(),
        );
        let cached: Arc<dyn ContentGenerator> = Arc::new(MockProvider::history_echo());
        core.trial_identity_provider = Some((String::new(), cached.clone()));
        assert!(super::apply_policy(
            &mut core,
            true,
            Some(context(true)),
            &ProviderIntent::default(),
            true
        )
        .is_ok());
        assert!(core.trial_access.is_some());
        assert!(Arc::ptr_eq(
            &core.trial_identity_provider.as_ref().unwrap().1,
            &cached
        ));
        assert_eq!(
            super::apply_policy(
                &mut core,
                true,
                Some(context(false)),
                &ProviderIntent::default(),
                true
            )
            .unwrap_err(),
            "ModelNotConfigured"
        );
        assert!(core.trial_access.is_none());
    }

    #[test]
    fn cached_identity_cannot_bypass_changed_persistent_preference() {
        let mut core = CoshCore::new_legacy(
            CoreConfig::default(),
            Box::new(MockProvider::history_echo()),
            ToolRegistry::new(),
        );
        core.trial_identity_provider =
            Some((String::new(), Arc::new(MockProvider::history_echo())));
        assert_eq!(
            super::apply_policy(
                &mut core,
                true,
                Some(context(true)),
                &ProviderIntent::default(),
                false
            )
            .unwrap_err(),
            "ModelNotConfigured"
        );
        assert!(core.trial_access.is_none());
    }

    #[test]
    fn unavailable_or_rejected_metadata_never_becomes_activation_required() {
        use crate::provider::sysom::ProbeError;
        use crate::provider::{DispatchState, ProviderFailureKind};

        for (probe, code) in [
            (Ok(None), "EcsIdentityUnavailable"),
            (Err(ProbeError::AccessDenied), "metadata_access_denied"),
            (
                Err(ProbeError::InvalidResponse),
                "invalid_metadata_response",
            ),
            (Err(ProbeError::Timeout), "metadata_timeout"),
        ] {
            let failure = super::classify_identity_probe(probe).unwrap_err();
            assert_eq!(failure.code, code);
            assert_eq!(failure.kind, ProviderFailureKind::Identity);
            assert_eq!(failure.dispatch, DispatchState::NotDispatched);
            assert_eq!(failure.request_id, None);
        }
    }
    #[test]
    fn ecs_builtin_activation_updates_live_model_without_replaying_a_question() {
        use super::super::{process_input_line, CliArgs, HeadlessExtensionRuntime, ProtocolState};
        use clap::Parser;
        use tokio::io::AsyncBufReadExt;

        let _lock = crate::sls::tests::ENV_TEST_MUTEX
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for (resumed, fail_save) in [(false, false), (true, false), (false, true)] {
            let home = tempfile::tempdir().unwrap();
            let _home = crate::sls::tests::EnvVarGuard::set("HOME", home.path().to_str().unwrap());
            tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            let (root, mut runtime, mut engine) =
                super::super::live_state_tests::background_compaction_fixture();
            engine.config.ai.active_provider = Some("mine".into());
            engine.config.ai.providers.insert("mine".into(),crate::config::ProviderConfig {
                provider_type: Some("mock".into()), model: Some("own-model".into()), ..Default::default()
            });
            engine.config.user_ai = engine.config.ai.clone();
            engine.model = "own-model".into();
            if resumed {
                engine.config.ai = Default::default();
                engine.config.user_ai = Default::default();
                engine.set_session_resumed(true);
                engine.model = "resumed-trial-model".into();
            }
            let expected_model = "auto";
            let configured_model_before = engine.config.ai.active_model.clone();
            let messages_before = serde_json::to_value(&engine.messages).unwrap();
            runtime.persist(&engine).unwrap();
            if fail_save {
                runtime.record.generation += 1;
            }
            let args = CliArgs::try_parse_from(["cosh-core","--headless","--bare","--cosh-shell-transport"]).unwrap();
            let live = crate::registry::LiveExtensionRuntime::new(engine.extension_generation.clone(),false,false,None);
            let mut manager = crate::extension::ExtensionManager::new_isolated(
                home.path().to_path_buf(), Some(home.path().join("extensions")), Some(home.path().join("system-extensions")));
            let mut extensions = HeadlessExtensionRuntime { manager: Some(&mut manager), live: &live };
            let mut protocol = ProtocolState::new(args.execution_profile);
            let mut lines = tokio::io::BufReader::new(&b""[..]).lines();
            let mut output = Vec::new();
            let line = serde_json::json!({"type":"registry_request","request_id":"switch-trial","domain":"auth","action":"activate_trial","params":null}).to_string();
            crate::registry::TEST_ECS_PROBE.scope((Ok(true),Arc::new(std::sync::atomic::AtomicUsize::new(0))),async {
                let state_line = serde_json::json!({"type":"registry_request","request_id":"trial-state","domain":"auth","action":"state","params":{"probe_ecs_trial":true}}).to_string();
                process_input_line(&state_line,&mut engine,&mut lines,&mut output,&args,&mut runtime,&mut extensions,&mut protocol).await;
                let response: serde_json::Value = serde_json::from_slice(output.strip_suffix(b"\n").unwrap_or(&output)).unwrap();
                assert_eq!(response["data"]["ecs_trial"]["model"],expected_model);
                assert_eq!(engine.config.ai.active_model,configured_model_before);
                output.clear();
                process_input_line(&line,&mut engine,&mut lines,&mut output,&args,&mut runtime,&mut extensions,&mut protocol).await;
            }).await;
            let events: Vec<serde_json::Value> = String::from_utf8(output).unwrap().lines().map(|line|serde_json::from_str(line).unwrap()).collect();
            if fail_save {
                let response = events.iter().find(|event| event["type"] == "registry_response").unwrap();
                assert_eq!(response["success"],false,"{response}");
                assert_eq!(response["data"]["provider_updated"],true,"{response}");
                assert!(response["error"].as_str().unwrap().contains("session model could not be saved"));
                assert_eq!(engine.model,expected_model);
                return;
            }
            assert!(events.iter().any(|event|event["type"]=="registry_response" && event["success"]==true),"{events:?}");
            assert_eq!(engine.model,expected_model);
            assert_eq!(serde_json::to_value(&engine.messages).unwrap(),messages_before);
            assert!(!events.iter().any(|event|matches!(event["type"].as_str(),Some("assistant"|"result"|"control_request"))));
            let saved = runtime.store.as_ref().unwrap().load(&runtime.record.session_id).unwrap();
            assert_eq!(saved.model, expected_model, "activation must persist before the next question");
            let shutdown = serde_json::json!({"type":"control_request","request_id":"shutdown","request":{"subtype":"shutdown"}}).to_string();
            process_input_line(&shutdown,&mut engine,&mut lines,&mut Vec::new(),&args,&mut runtime,&mut extensions,&mut protocol).await;
            let session_id = runtime.record.session_id.to_string();
            let mut resume_config = engine.config.clone();
            resume_config.session.persist_dir = root.path().join("sessions").to_str().unwrap().into();
            drop(runtime);
            drop(engine);
            let args = CliArgs::try_parse_from(["cosh-core","--headless","--bare","--cosh-shell-transport",
                "--workspace",root.path().to_str().unwrap(),"--resume",&session_id]).unwrap();
            let mut runtime = super::super::SessionRuntime::initialize(&args,&resume_config).unwrap();
            runtime.finalize_model(&resume_config.resolve_provider().model,false);
            let mut engine = CoshCore::new_legacy(resume_config,Box::new(MockProvider::history_echo()),ToolRegistry::new());
            engine.model = runtime.record.model.clone();
            engine.set_session_resumed(runtime.resumed);
            super::apply_policy(&mut engine,true,Some(context(true)),&ProviderIntent::default(),true).unwrap();
            assert_eq!(engine.model,expected_model);
        });
        }
    }

    #[tokio::test]
    async fn trial_normalizes_legacy_session_and_cli_models_to_auto() {
        use super::super::{CliArgs, SessionRuntime};
        use clap::Parser;

        for (saved_model, configured_model, cli_model) in [
            ("custom-model", None, None),
            ("qwen-max", None, None),
            ("custom-model", Some("configured-model"), None),
            ("custom-model", Some("configured-model"), Some("cli-model")),
        ] {
            let (root, mut runtime, mut engine) =
                super::super::live_state_tests::background_compaction_fixture();
            assert_eq!(
                super::apply_policy(
                    &mut engine,
                    true,
                    Some(context(true)),
                    &ProviderIntent::default(),
                    true,
                )
                .unwrap(),
                super::Prepared::Trial
            );
            engine.model = saved_model.to_string();
            runtime.persist(&engine).unwrap();
            let session_id = runtime.record.session_id.to_string();
            drop(runtime);
            drop(engine);

            let mut resume_args = vec![
                "cosh-core",
                "--headless",
                "--bare",
                "--cosh-shell-transport",
                "--workspace",
                root.path().to_str().unwrap(),
                "--resume",
                &session_id,
            ];
            if let Some(model) = cli_model {
                resume_args.extend(["--model", model]);
            }
            let args = CliArgs::try_parse_from(resume_args).unwrap();
            let mut config = CoreConfig::default();
            config.session.persist_dir = root.path().join("sessions").to_str().unwrap().into();
            config.ai.active_model = cli_model.or(configured_model).map(str::to_string);
            let mut runtime = SessionRuntime::initialize(&args, &config).unwrap();
            assert_eq!(runtime.record.model, saved_model);
            runtime.finalize_model(&config.resolve_provider().model, args.model.is_some());
            let mut engine = CoshCore::new_legacy(
                config,
                Box::new(MockProvider::history_echo()),
                ToolRegistry::new(),
            );
            engine.session_id = session_id;
            engine.set_session_resumed(runtime.resumed);
            engine.model = runtime.record.model.clone();
            for _ in 0..2 {
                assert_eq!(
                    super::apply_policy(
                        &mut engine,
                        true,
                        Some(context(true)),
                        &ProviderIntent::default(),
                        true,
                    )
                    .unwrap(),
                    super::Prepared::Trial
                );
                assert_eq!(engine.model, "auto");
            }
            assert_eq!(engine.config.user_ai.active_model, None);
            runtime.persist(&engine).unwrap();
            let saved = runtime
                .store
                .as_ref()
                .unwrap()
                .load(&runtime.record.session_id)
                .unwrap();
            assert_eq!(saved.model, "auto");
        }
    }

    #[test]
    fn trial_model_switch_cannot_change_automatic_access() {
        use super::super::{process_input_line, CliArgs, HeadlessExtensionRuntime, ProtocolState};
        use clap::Parser;
        use tokio::io::AsyncBufReadExt;

        struct ProviderEnvironment(Vec<(&'static str, Option<std::ffi::OsString>)>);

        impl Drop for ProviderEnvironment {
            fn drop(&mut self) {
                for (key, value) in &self.0 {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }

        let _lock = crate::sls::tests::ENV_TEST_MUTEX
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let provider_env = [
            "COSH_AI_PROVIDER",
            "OPENAI_BASE_URL",
            "COSH_SYSOM_ENDPOINT",
            "COSH_SYSOM_VPC_PROXY_HOST",
            "DASHSCOPE_API_KEY",
            "OPENAI_API_KEY",
            "ALIBABA_CLOUD_ACCESS_KEY_ID",
            "ALIBABA_CLOUD_ACCESS_KEY_SECRET",
            "ALIBABA_CLOUD_SECURITY_TOKEN",
        ];
        let expected_environment = provider_env.map(|key| (key, std::env::var_os(key)));
        let environment = ProviderEnvironment(expected_environment.to_vec());
        for key in provider_env {
            std::env::remove_var(key);
        }
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                for (model, already_trial) in [
                    ("custom-model", false),
                    ("qwen-max", false),
                    ("custom-model", true),
                ] {
                    let (_root, mut runtime, mut engine) =
                        super::super::live_state_tests::background_compaction_fixture();
                    let args = CliArgs::try_parse_from([
                        "cosh-core",
                        "--headless",
                        "--bare",
                        "--cosh-shell-transport",
                    ])
                    .unwrap();
                    let live = crate::registry::LiveExtensionRuntime::new(
                        engine.extension_generation.clone(),
                        false,
                        false,
                        None,
                    );
                    let mut extensions = HeadlessExtensionRuntime {
                        manager: None,
                        live: &live,
                    };
                    let mut protocol = ProtocolState::new(args.execution_profile);
                    let mut lines = tokio::io::BufReader::new(&b""[..]).lines();
                    let mut output = Vec::new();
                    let context = || {
                        serde_json::from_value(serde_json::json!({
                            "schema_version":1,"shell_session_id":"model-switch-test",
                            "intent":"user_initiated","auto_use_allowed":true
                        }))
                        .unwrap()
                    };
                    if already_trial {
                        assert_eq!(
                            super::apply_policy(
                                &mut engine,
                                true,
                                Some(context()),
                                &ProviderIntent::default(),
                                true
                            )
                            .unwrap(),
                            super::Prepared::Trial
                        );
                    }
                    let line =
                        serde_json::json!({"type":"control_request","request_id":"model-switch",
            "request":{"subtype":"switch_model","model":model}})
                        .to_string();
                    process_input_line(
                        &line,
                        &mut engine,
                        &mut lines,
                        &mut output,
                        &args,
                        &mut runtime,
                        &mut extensions,
                        &mut protocol,
                    )
                    .await;
                    if already_trial {
                        assert!(engine.config.ai.active_model.is_none());
                        assert!(String::from_utf8_lossy(&output).contains("EcsTrialModelFixed"));
                    } else {
                        assert_eq!(engine.config.ai.active_model.as_deref(), Some(model));
                    }
                    for _ in 0..2 {
                        assert_eq!(
                            super::apply_policy(
                                &mut engine,
                                true,
                                Some(context()),
                                &ProviderIntent::default(),
                                true
                            )
                            .unwrap(),
                            super::Prepared::Trial
                        );
                        assert_eq!(engine.model, "auto");
                    }
                    assert_eq!(engine.config.user_ai.active_model, None);
                    engine.config.ai.active_provider = Some("custom".into());
                    engine.config.ai.providers.insert(
                        "custom".into(),
                        crate::config::ProviderConfig {
                            provider_type: Some("mock".into()),
                            ..Default::default()
                        },
                    );
                    output.clear();
                    process_input_line(
                        &line,
                        &mut engine,
                        &mut lines,
                        &mut output,
                        &args,
                        &mut runtime,
                        &mut extensions,
                        &mut protocol,
                    )
                    .await;
                    assert_eq!(engine.model, model);
                    assert!(String::from_utf8_lossy(&output).contains("model_switched"));
                    assert_eq!(
                        super::apply_policy(
                            &mut engine,
                            true,
                            Some(context()),
                            &ProviderIntent::default(),
                            true
                        )
                        .unwrap(),
                        super::Prepared::Existing
                    );
                    assert_eq!(engine.model, model);
                }
            });
        drop(environment);
        for (key, expected) in expected_environment {
            assert_eq!(std::env::var_os(key), expected, "{key}");
        }
    }
}
