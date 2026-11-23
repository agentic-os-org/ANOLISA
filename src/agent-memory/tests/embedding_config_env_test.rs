//! Config-file embedding providers honor field overrides without a backend selector.

use agent_memory::config::AppConfig;
use agent_memory::embedding::{EmbeddingConfig, build_provider};
use std::process::Command;

const OPENAI: &str = r#"
[memory.embedding]
backend = "openai"
api_key = "file-only-key"
model = "file-model"
base_url = "http://127.0.0.1:18080/file"
"#;

const OLLAMA: &str = r#"
[memory.embedding]
backend = "ollama"
model = "file-ollama-model"
base_url = "http://127.0.0.1:18081/file"
"#;

fn isolated(
    name: &str,
    toml: &str,
    variables: &[(&str, &str)],
    check: impl FnOnce(EmbeddingConfig),
) {
    // Each scenario gets a fresh process: neither the operator's credentials nor
    // concurrent tests may supply an unintended fallback or change precedence.
    if std::env::var("ANOLISA_CONFIG_TEST_CHILD").as_deref() != Ok(name) {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture"])
            .env_clear()
            .env("ANOLISA_CONFIG_TEST_CHILD", name)
            .envs(variables.iter().copied())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success()
                && stdout.contains("test result: ok. 1 passed; 0 failed; 0 ignored;"),
            "isolated config case {name} did not run or failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("memory.toml");
    std::fs::write(&path, toml).unwrap();
    check(AppConfig::load(Some(&path)).unwrap().memory.embedding);
}

#[test]
fn file_openai_accepts_prefixed_key_without_backend_env() {
    isolated(
        "file_openai_accepts_prefixed_key_without_backend_env",
        &OPENAI.replace("file-only-key", ""),
        &[("MEMORY_OPENAI_API_KEY", "synthetic-env-key")],
        |config| {
            assert!(build_provider(&config).unwrap().is_some());
            match config {
                EmbeddingConfig::OpenAI {
                    api_key,
                    model,
                    base_url,
                } => {
                    assert_eq!(api_key, "synthetic-env-key");
                    assert_eq!(model, "file-model");
                    assert_eq!(base_url.as_deref(), Some("http://127.0.0.1:18080/file"));
                }
                _ => panic!("configured OpenAI backend was replaced"),
            }
        },
    );
}

#[test]
fn file_openai_field_overrides_take_precedence() {
    isolated(
        "file_openai_field_overrides_take_precedence",
        OPENAI,
        &[
            ("MEMORY_OPENAI_API_KEY", "synthetic-env-key"),
            ("MEMORY_OPENAI_MODEL", "env-model"),
            ("MEMORY_OPENAI_BASE_URL", "http://127.0.0.1:18082/override"),
        ],
        |config| {
            assert!(build_provider(&config).unwrap().is_some());
            match config {
                EmbeddingConfig::OpenAI {
                    api_key,
                    model,
                    base_url,
                } => {
                    assert_eq!(api_key, "synthetic-env-key");
                    assert_eq!(model, "env-model");
                    assert_eq!(base_url.as_deref(), Some("http://127.0.0.1:18082/override"));
                }
                _ => panic!("configured OpenAI backend was replaced"),
            }
        },
    );
}

#[test]
fn file_ollama_field_overrides_take_precedence() {
    isolated(
        "file_ollama_field_overrides_take_precedence",
        OLLAMA,
        &[
            ("MEMORY_OLLAMA_MODEL", "env-ollama-model"),
            ("MEMORY_OLLAMA_BASE_URL", "http://127.0.0.1:18083/override"),
        ],
        |config| {
            assert!(build_provider(&config).unwrap().is_some());
            match config {
                EmbeddingConfig::Ollama { model, base_url } => {
                    assert_eq!(model, "env-ollama-model");
                    assert_eq!(base_url, "http://127.0.0.1:18083/override");
                }
                _ => panic!("configured Ollama backend was replaced"),
            }
        },
    );
}

#[test]
fn absent_overrides_preserve_file_settings() {
    isolated(
        "absent_overrides_preserve_file_settings",
        OPENAI,
        &[],
        |config| {
            assert!(build_provider(&config).unwrap().is_some());
            match config {
                EmbeddingConfig::OpenAI {
                    api_key,
                    model,
                    base_url,
                } => {
                    assert_eq!(api_key, "file-only-key");
                    assert_eq!(model, "file-model");
                    assert_eq!(base_url.as_deref(), Some("http://127.0.0.1:18080/file"));
                }
                _ => panic!("configured OpenAI backend was replaced"),
            }
        },
    );
}

#[test]
fn generic_key_fallback_remains_available() {
    isolated(
        "generic_key_fallback_remains_available",
        &OPENAI.replace("file-only-key", ""),
        &[("OPENAI_API_KEY", "synthetic-generic-key")],
        |config| assert!(build_provider(&config).unwrap().is_some()),
    );
}

#[test]
fn provider_fields_do_not_enable_disabled_backend() {
    isolated(
        "provider_fields_do_not_enable_disabled_backend",
        "[memory.embedding]\nbackend = 'none'\n",
        &[
            ("MEMORY_OPENAI_API_KEY", "synthetic-env-key"),
            ("MEMORY_OPENAI_MODEL", "env-model"),
            ("MEMORY_OLLAMA_MODEL", "env-ollama-model"),
        ],
        |config| {
            assert!(matches!(config, EmbeddingConfig::None));
            assert!(build_provider(&config).unwrap().is_none());
        },
    );
}

#[test]
fn explicit_backend_keeps_existing_reset_defaults() {
    isolated(
        "explicit_backend_keeps_existing_reset_defaults",
        OPENAI,
        &[
            ("MEMORY_EMBEDDING_BACKEND", "openai"),
            ("MEMORY_OPENAI_API_KEY", "synthetic-env-key"),
        ],
        |config| match config {
            EmbeddingConfig::OpenAI {
                api_key,
                model,
                base_url,
            } => {
                assert_eq!(api_key, "synthetic-env-key");
                assert_eq!(model, "text-embedding-3-small");
                assert_eq!(base_url, None);
            }
            _ => panic!("explicit OpenAI selector was not applied"),
        },
    );
}

#[test]
fn unknown_backend_keeps_existing_file_settings() {
    isolated(
        "unknown_backend_keeps_existing_file_settings",
        OLLAMA,
        &[
            ("MEMORY_EMBEDDING_BACKEND", "unknown-provider"),
            ("MEMORY_OLLAMA_MODEL", "env-ollama-model"),
        ],
        |config| match config {
            EmbeddingConfig::Ollama { model, base_url } => {
                assert_eq!(model, "file-ollama-model");
                assert_eq!(base_url, "http://127.0.0.1:18081/file");
            }
            _ => panic!("unknown selector replaced the configured provider"),
        },
    );
}

#[test]
fn empty_prefixed_key_clears_file_key() {
    isolated(
        "empty_prefixed_key_clears_file_key",
        OPENAI,
        &[("MEMORY_OPENAI_API_KEY", "")],
        |config| {
            let EmbeddingConfig::OpenAI { api_key, .. } = &config else {
                panic!("configured OpenAI backend was replaced");
            };
            assert!(api_key.is_empty());
            assert!(build_provider(&config).unwrap().is_none());
        },
    );
}

#[test]
fn empty_prefixed_key_uses_generic_fallback() {
    isolated(
        "empty_prefixed_key_uses_generic_fallback",
        OPENAI,
        &[
            ("MEMORY_OPENAI_API_KEY", ""),
            ("OPENAI_API_KEY", "synthetic-generic-key"),
        ],
        |config| {
            let EmbeddingConfig::OpenAI { api_key, .. } = &config else {
                panic!("configured OpenAI backend was replaced");
            };
            assert!(api_key.is_empty());
            assert!(build_provider(&config).unwrap().is_some());
        },
    );
}

#[test]
fn empty_openai_fields_replace_file_values() {
    isolated(
        "empty_openai_fields_replace_file_values",
        OPENAI,
        &[("MEMORY_OPENAI_MODEL", ""), ("MEMORY_OPENAI_BASE_URL", "")],
        |config| {
            let EmbeddingConfig::OpenAI {
                model, base_url, ..
            } = &config
            else {
                panic!("configured OpenAI backend was replaced");
            };
            assert!(model.is_empty());
            assert_eq!(base_url.as_deref(), Some(""));
            assert!(build_provider(&config).unwrap().is_some());
        },
    );
}

#[test]
fn empty_ollama_fields_replace_file_values() {
    isolated(
        "empty_ollama_fields_replace_file_values",
        OLLAMA,
        &[("MEMORY_OLLAMA_MODEL", ""), ("MEMORY_OLLAMA_BASE_URL", "")],
        |config| {
            let EmbeddingConfig::Ollama { model, base_url } = &config else {
                panic!("configured Ollama backend was replaced");
            };
            assert!(model.is_empty());
            assert!(base_url.is_empty());
            assert!(build_provider(&config).unwrap().is_some());
        },
    );
}

#[test]
fn empty_backend_selector_keeps_file_and_ignores_fields() {
    isolated(
        "empty_backend_selector_keeps_file_and_ignores_fields",
        &OPENAI.replace("file-only-key", ""),
        &[
            ("MEMORY_EMBEDDING_BACKEND", ""),
            ("MEMORY_OPENAI_API_KEY", "synthetic-env-key"),
            ("MEMORY_OPENAI_MODEL", "env-model"),
        ],
        |config| {
            let EmbeddingConfig::OpenAI {
                api_key,
                model,
                base_url,
            } = &config
            else {
                panic!("empty selector replaced the configured provider");
            };
            assert!(api_key.is_empty());
            assert_eq!(model, "file-model");
            assert_eq!(base_url.as_deref(), Some("http://127.0.0.1:18080/file"));
            assert!(build_provider(&config).unwrap().is_none());
        },
    );
}

#[test]
fn file_openai_ignores_ollama_fields() {
    isolated(
        "file_openai_ignores_ollama_fields",
        OPENAI,
        &[
            ("MEMORY_OLLAMA_MODEL", "env-ollama-model"),
            ("MEMORY_OLLAMA_BASE_URL", "http://127.0.0.1:18083/override"),
        ],
        |config| {
            let EmbeddingConfig::OpenAI {
                api_key,
                model,
                base_url,
            } = &config
            else {
                panic!("Ollama fields replaced the configured OpenAI backend");
            };
            assert_eq!(api_key, "file-only-key");
            assert_eq!(model, "file-model");
            assert_eq!(base_url.as_deref(), Some("http://127.0.0.1:18080/file"));
            assert!(build_provider(&config).unwrap().is_some());
        },
    );
}

#[test]
fn file_ollama_ignores_openai_fields() {
    isolated(
        "file_ollama_ignores_openai_fields",
        OLLAMA,
        &[
            ("MEMORY_OPENAI_API_KEY", "synthetic-env-key"),
            ("MEMORY_OPENAI_MODEL", "env-model"),
            ("MEMORY_OPENAI_BASE_URL", "http://127.0.0.1:18082/override"),
        ],
        |config| {
            let EmbeddingConfig::Ollama { model, base_url } = &config else {
                panic!("OpenAI fields replaced the configured Ollama backend");
            };
            assert_eq!(model, "file-ollama-model");
            assert_eq!(base_url, "http://127.0.0.1:18081/file");
            assert!(build_provider(&config).unwrap().is_some());
        },
    );
}
