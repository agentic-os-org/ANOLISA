use super::*;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;

fn config(source: &str) -> CoreConfig {
    toml::from_str(source).unwrap()
}

fn allowed(config: &CoreConfig, intent: &ProviderIntent) -> TrialDecision {
    decide(config, intent, true, true, true)
}

#[test]
fn unconfigured_and_model_only_configs_are_trial_candidates() {
    for source in [
        "",
        "[ai]\nactive_model = 'custom-model'\n",
        "[ai]\nactive_provider = 'default'\n",
        "[ai]\nactive_provider = 'generic'\n",
        "[ai.providers.default]\n",
        "[ai.providers.generic]\ntype = 'generic'\nmodel = 'custom-model'\n",
        "[ai]\nactive_provider = 'default'\n[ai.providers.default]\ntype = 'generic'\nmodel = 'custom-model'\n",
    ] {
        assert_eq!(allowed(&config(source), &ProviderIntent::default()), TrialDecision::EcsIdentity, "{source}");
    }
}

#[test]
fn candidate_requires_user_session_and_persistent_permission() {
    for user_initiated in [false, true] {
        for session in [false, true] {
            for persistent in [false, true] {
                let expected = if user_initiated && session && persistent {
                    TrialDecision::EcsIdentity
                } else {
                    TrialDecision::Disabled
                };
                assert_eq!(
                    decide(
                        &CoreConfig::default(),
                        &ProviderIntent::default(),
                        user_initiated,
                        session,
                        persistent
                    ),
                    expected
                );
            }
        }
    }
}

#[test]
fn explicit_other_providers_keep_the_existing_path() {
    for source in [
        "[ai.providers.qwen]\ntype = 'dashscope'\n",
        "[ai.providers.default]\ntype = 'openai'\n",
        "[ai.providers.custom]\nmodel = 'custom-model'\n",
        "[ai]\nactive_provider = 'qwen'\n[ai.providers.qwen]\ntype = 'dashscope'\napi_key = 'rejected'\n",
        "[ai.providers.aliyun]\ntype = 'aliyun'\n",
        "[ai]\nactive_provider = 'off'\n",
    ] {
        let config = config(source);
        assert_eq!(allowed(&config, &ProviderIntent::default()), TrialDecision::Existing, "{source}");
        assert_eq!(decide(&config, &ProviderIntent::default(), false, false, false), TrialDecision::Existing, "{source}");
    }
}

#[test]
fn endpoint_credentials_and_partial_configuration_never_switch_accounts() {
    for field in [
        "api_key = ''",
        "api_key = '${UNRESOLVED_KEY}'",
        "access_key_id = ''",
        "access_key_secret = 'only-secret'",
        "security_token = 'manual-sts'",
        "base_url = ''",
        "base_url = 'https://custom.example/v1'",
        "sysom_endpoint = ''",
        "auth_source = ''",
        "auth_source = 'ecs_ram_role'",
        "type = ''",
        "extra_params = { temperature = 0.7 }",
        "explicit_cache = false",
    ] {
        let config = config(&format!("[ai.providers.default]\n{field}\n"));
        assert_eq!(
            allowed(&config, &ProviderIntent::default()),
            TrialDecision::Existing,
            "{field}"
        );
    }
}

#[test]
fn invalid_active_selection_and_ambiguous_providers_keep_existing_path() {
    for source in [
        "[ai]\nactive_provider = 'missing'\n",
        "[ai]\nactive_provider = ''\n",
        "[ai]\nactive_provider = 'missing'\n[ai.providers.qwen]\napi_key = 'key'\n",
        "[ai]\nactive_provider = 'default'\n[ai.providers.other]\n",
        "[ai.providers.default]\n[ai.providers.generic]\n",
        "[ai.providers.qwen]\n[ai.providers.aliyun]\n",
    ] {
        assert_eq!(
            allowed(&config(source), &ProviderIntent::default()),
            TrialDecision::Existing,
            "{source}"
        );
    }
}

#[test]
fn each_environment_intent_preserves_existing_behavior_even_when_empty_or_invalid() {
    for name in [
        "COSH_AI_PROVIDER",
        "OPENAI_BASE_URL",
        "COSH_SYSOM_ENDPOINT",
        "COSH_SYSOM_VPC_PROXY_HOST",
        "DASHSCOPE_API_KEY",
        "OPENAI_API_KEY",
        "ALIBABA_CLOUD_ACCESS_KEY_ID",
        "ALIBABA_CLOUD_ACCESS_KEY_SECRET",
        "ALIBABA_CLOUD_SECURITY_TOKEN",
    ] {
        for value in [
            OsString::new(),
            OsString::from("value"),
            OsString::from_vec(vec![0xff]),
        ] {
            let intent = ProviderIntent::from_lookup(|key| (key == name).then(|| value.clone()));
            assert!(
                intent.provider_set || intent.endpoint_set || intent.credentials_set,
                "{name}"
            );
            assert_eq!(
                allowed(&CoreConfig::default(), &intent),
                TrialDecision::Existing,
                "{name}"
            );
            assert_eq!(
                decide(&CoreConfig::default(), &intent, false, false, false),
                TrialDecision::Existing,
                "{name}"
            );
        }
    }
}

#[test]
fn model_only_environment_does_not_block_trial_selection() {
    let intent =
        ProviderIntent::from_lookup(|name| (name == "COSH_MODEL").then(|| "custom-model".into()));
    assert_eq!(
        allowed(&CoreConfig::default(), &intent),
        TrialDecision::EcsIdentity
    );
}

#[test]
fn saved_ecs_identity_still_obeys_current_permissions() {
    for prefix in ["", "[ai]\nactive_provider = 'trial'\n"] {
        let config = config(&format!(
            "{prefix}[ai.providers.trial]\ntype = 'aliyun'\nauth_source = 'ecs_identity'\n"
        ));
        for user in [false, true] {
            for session in [false, true] {
                for persistent in [false, true] {
                    let expected = if user && session && persistent {
                        TrialDecision::EcsIdentity
                    } else {
                        TrialDecision::Disabled
                    };
                    for intent in [
                        ProviderIntent::default(),
                        ProviderIntent {
                            provider_set: true,
                            endpoint_set: true,
                            credentials_set: true,
                        },
                    ] {
                        assert_eq!(
                            decide(&config, &intent, user, session, persistent),
                            expected
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn inactive_ecs_identity_does_not_override_selected_legacy_provider() {
    let config = config("[ai]\nactive_provider = 'legacy'\n[ai.providers.legacy]\ntype = 'aliyun'\nauth_source = 'ecs_ram_role'\n[ai.providers.trial]\ntype = 'aliyun'\nauth_source = 'ecs_identity'\n");
    assert_eq!(
        decide(&config, &ProviderIntent::default(), false, false, false),
        TrialDecision::Existing
    );
}

#[test]
fn malformed_ecs_identity_provider_cannot_become_a_trial_candidate() {
    let config = config("[ai.providers.trial]\ntype = 'other'\nauth_source = 'ecs_identity'\n");
    assert_eq!(
        allowed(&config, &ProviderIntent::default()),
        TrialDecision::Existing
    );
    assert_eq!(
        decide(&config, &ProviderIntent::default(), true, false, false),
        TrialDecision::Disabled
    );
}
