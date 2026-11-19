//! Configuration, normalized probabilities and fixture identity contracts.

use std::collections::BTreeMap;
use std::time::Duration;

use semantic_arbiter::config::{BackendConfig, ModelConfig, Thresholds, TransportMode};
use semantic_arbiter::model::{
    BackendKind, BackendMode, FixtureBackend, ModelBackend, ModelError, ModelReply, ModelRequest,
    NoulProbability, NoulQuestion,
};
use semantic_arbiter::protocol::{ContractError, MAX_MESSAGE_BYTES};
use serde_json::{json, Value};
use tokio::time::Instant;

fn config_value() -> Value {
    json!({
        "config_revision": "policy-v1",
        "backend": {"kind": "bailian", "endpoint_url": "https://example.invalid/exact/path"},
        "timeout_ms": 1000,
        "max_response_bytes": MAX_MESSAGE_BYTES
    })
}

fn decode(value: Value) -> Result<ModelConfig, ContractError> {
    ModelConfig::decode(&serde_json::to_vec(&value).unwrap())
}

#[test]
fn defaults_are_nonsecret_and_fixtures_are_not_configurable() {
    let config = decode(config_value()).unwrap();
    assert_eq!(config.thresholds, Thresholds::default());
    assert_eq!(config.transport, TransportMode::Https);
    assert_eq!(config.identity().kind, BackendKind::Bailian);
    assert_eq!(config.identity().mode, BackendMode::Remote);
    assert_eq!(config.identity().model, "decision-model-preview");
    assert!(
        matches!(config.backend, BackendConfig::Bailian { ref key_env, .. } if key_env == "DASHSCOPE_API_KEY")
    );
    let mut value = config_value();
    value["backend"] = json!({"kind":"typesafe"});
    let config = decode(value.clone()).unwrap();
    assert_eq!(config.identity().kind, BackendKind::Typesafe);
    assert_eq!(config.identity().model, "jev-latest");
    assert!(
        matches!(config.backend, BackendConfig::Typesafe { ref base_url, ref key_env, .. } if base_url == "https://api.typesafe.ai" && key_env == "TYPESAFE_API_KEY")
    );
    value["backend"] = json!({"kind":"fixture"});
    assert!(decode(value).is_err());
}

#[test]
fn rejects_unknown_fields_credentials_and_noninteger_limits() {
    for (field, value) in [
        ("key", json!("synthetic-credential")),
        ("headers", json!({"authorization":"synthetic"})),
        ("timeout_ms", json!(1.0)),
        ("timeout_ms", json!("1000")),
        ("max_response_bytes", json!(1000.0)),
    ] {
        let mut config = config_value();
        config[field] = value;
        assert!(decode(config).is_err(), "{field}");
    }
    for field in ["api_key", "headers", "transport"] {
        let mut config = config_value();
        config["backend"][field] = json!("not-permitted");
        assert!(decode(config).is_err());
    }
    let bytes = br#"{"config_revision":"v1","backend":{"kind":"typesafe"},"timeout_ms":1e0,"max_response_bytes":1000}"#;
    assert!(ModelConfig::decode(bytes).is_err());
}

#[test]
fn validates_bounds_and_env_names_without_reading_credentials() {
    for (field, value) in [
        ("timeout_ms", json!(0)),
        ("timeout_ms", json!(120_001)),
        ("max_response_bytes", json!(0)),
        ("max_response_bytes", json!(MAX_MESSAGE_BYTES + 1)),
        ("config_revision", json!("")),
        ("config_revision", json!("x".repeat(129))),
    ] {
        let mut config = config_value();
        config[field] = value;
        assert!(decode(config).is_err(), "{field}");
    }
    for name in [
        "",
        "1KEY",
        "KEY-NAME",
        "KEY=VALUE",
        "key.name",
        "KEY NAME",
        "密钥",
    ] {
        let mut config = config_value();
        config["backend"]["key_env"] = json!(name);
        assert!(decode(config).is_err());
    }
    for name in ["_KEY", "key_123", "SEMANTIC_ARBITER_UNSET_TEST_VARIABLE"] {
        let mut config = config_value();
        config["backend"]["key_env"] = json!(name);
        assert!(decode(config).is_ok());
    }
    for model in [
        String::new(),
        " ".into(),
        "x".repeat(257),
        "alias\nheader".into(),
    ] {
        let mut config = config_value();
        config["backend"]["model"] = json!(model);
        assert!(decode(config).is_err());
    }
    for threshold in [0, 500_000, 1_000_001] {
        for field in ["preservation_ppm", "task_scope_ppm"] {
            let mut config = config_value();
            config["thresholds"] = json!({field: threshold});
            assert!(decode(config).is_err());
        }
    }
    for threshold in [500_001, 900_000, 1_000_000] {
        let mut config = config_value();
        config["thresholds"] = json!({"preservation_ppm": threshold, "task_scope_ppm": threshold});
        assert!(decode(config).is_ok());
    }
    let mut config = config_value();
    config["timeout_ms"] = json!(120_000);
    assert!(decode(config).is_ok());
}

#[test]
fn production_requires_https_and_clean_urls() {
    for url in [
        "http://example.invalid/api",
        "ftp://example.invalid/api",
        "https://user:pass@example.invalid/api",
        "https://user@example.invalid",
        "https://@example.invalid",
        "https://example.invalid/?key=value",
        "https://example.invalid/?",
        "https://example.invalid/#",
        " https://example.invalid",
        "https://example.invalid/\napi",
        "https://example.invalid\\@other.invalid",
        "https:///missing-authority",
    ] {
        let mut config = config_value();
        config["backend"]["endpoint_url"] = json!(url);
        assert!(decode(config).is_err(), "{url}");
    }
    for url in [
        "https://example.invalid/full/path/",
        "https://[::1]:443/api",
        "https://example.invalid/a%20b",
    ] {
        let mut config = config_value();
        config["backend"]["endpoint_url"] = json!(url);
        assert!(decode(config).is_ok());
    }
}

#[test]
fn loopback_mode_requires_literal_loopback_not_dns_or_normalized_shorthand() {
    for (url, accepted) in [
        ("http://127.0.0.1:8080/api", true),
        ("http://127.1.2.3/api", true),
        ("http://[::1]:8080/api", true),
        ("http://localhost/api", false),
        ("http://127.1/api", false),
        ("http://2130706433/api", false),
        ("http://0177.0.0.1/api", false),
        ("http://0x7f000001/api", false),
        ("http://127.0.0.1.evil.invalid/api", false),
        ("http://192.168.1.2/api", false),
        ("http://0.0.0.0/api", false),
        ("https://127.0.0.1/api", false),
        ("http://[::ffff:127.0.0.1]/api", false),
    ] {
        let mut config = config_value();
        config["transport"] = json!("loopback_test");
        config["backend"]["endpoint_url"] = json!(url);
        let result = decode(config);
        assert_eq!(result.is_ok(), accepted, "{url}");
        if let Ok(config) = result {
            assert_eq!(config.identity().mode, BackendMode::LoopbackTest);
        }
    }
}

#[test]
fn config_hash_binds_policy_but_not_env_contents() {
    let config = decode(config_value()).unwrap();
    let digest = config.digest().unwrap();
    assert_eq!(digest.len(), 71);
    assert!(digest.starts_with("sha256:"));
    let reparsed = ModelConfig::decode(&serde_json::to_vec(&config).unwrap()).unwrap();
    assert_eq!(digest, reparsed.digest().unwrap());
    let mut changed = config.clone();
    changed.thresholds.preservation_ppm += 1;
    assert_ne!(digest, changed.digest().unwrap());
    changed = config.clone();
    changed.config_revision.push('2');
    assert_ne!(digest, changed.digest().unwrap());
    changed = config.clone();
    changed.timeout_ms += 1;
    assert_ne!(digest, changed.digest().unwrap());
    changed = config.clone();
    changed.backend = BackendConfig::Typesafe {
        base_url: "https://example.invalid".into(),
        model: "jev-latest".into(),
        key_env: "UNSET_VARIABLE".into(),
    };
    assert_ne!(digest, changed.digest().unwrap());
}

#[test]
fn rejects_duplicate_config_keys_including_nested_and_escaped_keys() {
    for bytes in [
        br#"{"config_revision":"v1","config_revision":"v2","backend":{"kind":"typesafe"},"timeout_ms":1,"max_response_bytes":1000}"#.as_slice(),
        br#"{"config_revision":"v1","backend":{"kind":"typesafe","kind":"typesafe"},"timeout_ms":1,"max_response_bytes":1000}"#,
        br#"{"config_revision":"v1","backend":{"kind":"typesafe"},"timeout_ms":1,"max_response_bytes":1000,"thresholds":{"preservation_ppm":900000,"preservation_\u0070pm":999999}}"#,
    ] {
        assert!(matches!(ModelConfig::decode(bytes), Err(ContractError::InvalidJson)));
    }
    assert!(matches!(
        ModelConfig::decode(&vec![b' '; MAX_MESSAGE_BYTES + 1]),
        Err(ContractError::MessageTooLarge)
    ));
}

#[test]
fn probabilities_are_independent_finite_and_conservative() {
    for raw in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.01, 1.000001] {
        assert_eq!(NoulProbability::new(raw), Err(ModelError::InvalidResponse));
    }
    // Injected floats use their JSON decimal spelling, not binary subtraction.
    for (raw, yes, no) in [
        (0.0, 0, 1_000_000),
        (0.1, 100_000, 900_000),
        (0.5, 500_000, 500_000),
        (0.9, 900_000, 100_000),
        (0.999999, 999_999, 1),
        (1.0, 1_000_000, 0),
    ] {
        let probability = NoulProbability::new(raw).unwrap();
        assert_eq!(probability.raw(), raw);
        assert_eq!(probability.yes_ppm(), yes);
        assert_eq!(probability.no_ppm(), no);
        assert_eq!(
            probability,
            NoulProbability::from_json_number(&serde_json::to_string(&raw).unwrap()).unwrap()
        );
        assert!(probability.yes_ppm() + probability.no_ppm() <= 1_000_000);
        let bytes = serde_json::to_vec(&probability).unwrap();
        assert_eq!(
            serde_json::from_slice::<NoulProbability>(&bytes).unwrap(),
            probability
        );
    }
    let just_below = f64::from_bits(0.9_f64.to_bits() - 1);
    assert!(!NoulProbability::new(just_below)
        .unwrap()
        .supports_yes(900_000));
    assert!(NoulProbability::new(0.9).unwrap().supports_yes(900_000));
    assert!(NoulProbability::new(0.1).unwrap().supports_no(900_000));
    let just_above = f64::from_bits(0.1_f64.to_bits() + 1);
    assert!(!NoulProbability::new(just_above)
        .unwrap()
        .supports_no(900_000));
    assert!(!NoulProbability::new(0.0).unwrap().supports_no(u32::MAX));
    assert!(!NoulProbability::new(0.100001).unwrap().supports_no(900_000));
    assert!(!NoulProbability::new(0.5).unwrap().supports_yes(900_000));
    assert!(!NoulProbability::new(0.5).unwrap().supports_no(900_000));
    assert!(NoulProbability::new(1.0).unwrap().supports_yes(1_000_000));
    assert!(NoulProbability::new(0.0).unwrap().supports_no(1_000_000));
    let reply = ModelReply {
        answers: BTreeMap::from([
            ("preservation".into(), NoulProbability::new(0.99).unwrap()),
            ("task_scope".into(), NoulProbability::new(0.99).unwrap()),
        ]),
    };
    assert!(reply.answers.values().all(|p| p.supports_yes(900_000)));
}

#[test]
fn exact_decimal_tokens_preserve_bounds_floors_and_complementary_thresholds() {
    assert_eq!(
        semantic_arbiter::model::NORMALIZATION_VERSION,
        "noul-floor-ppm/v2"
    );
    for (token, yes, no) in [
        ("0", 0, 1_000_000),
        ("-0.000e+9999", 0, 1_000_000),
        ("1", 1_000_000, 0),
        ("1.00000000000000000000", 1_000_000, 0),
        ("10E-1", 1_000_000, 0),
        ("0.89999999999999999999", 899_999, 100_000),
        ("8.9999999999999999999e-1", 899_999, 100_000),
        ("0.9", 900_000, 100_000),
        ("9.000000E-1", 900_000, 100_000),
        ("0.90000000000000000001", 900_000, 99_999),
        ("0.09999999999999999999", 99_999, 900_000),
        ("0.1", 100_000, 900_000),
        ("100000e-6", 100_000, 900_000),
        ("0.10000000000000000001", 100_000, 899_999),
        ("1.0000000000000000001e-1", 100_000, 899_999),
        ("0.99999999999999999999", 999_999, 0),
        ("1e-999", 0, 999_999),
        ("1e-9999999999999999999999999", 0, 999_999),
        ("1E-6", 1, 999_999),
        ("0.00000099999999999999999999", 0, 999_999),
        ("1234567e-7", 123_456, 876_543),
        ("0.5", 500_000, 500_000),
    ] {
        let probability = NoulProbability::from_json_number(token).unwrap();
        assert_eq!(probability.raw_json_number(), token);
        assert_eq!(probability.yes_ppm(), yes, "{token}");
        assert_eq!(probability.no_ppm(), no, "{token}");
        for threshold in [
            0,
            1,
            100_000,
            500_001,
            900_000,
            999_999,
            1_000_000,
            u32::MAX,
        ] {
            assert_eq!(
                probability.supports_yes(threshold),
                yes >= threshold,
                "{token}"
            );
            assert_eq!(
                probability.supports_no(threshold),
                no >= threshold,
                "{token}"
            );
        }
        let encoded = serde_json::to_string(&probability).unwrap();
        assert_eq!(
            encoded,
            format!(r#"{{"raw":{token},"yes_ppm":{yes},"no_ppm":{no}}}"#)
        );
        assert_eq!(
            serde_json::from_str::<NoulProbability>(&encoded).unwrap(),
            probability
        );
        assert_eq!(
            NoulProbability::from_json_number(probability.raw_json_number()).unwrap(),
            probability
        );
    }
}

#[test]
fn lossy_float_access_cannot_be_used_to_revalidate_provider_tokens() {
    for (token, approximate) in [
        ("0.89999999999999999999", 0.9),
        ("0.10000000000000000001", 0.1),
        ("0.99999999999999999999", 1.0),
        ("1e-999", 0.0),
    ] {
        let probability = NoulProbability::from_json_number(token).unwrap();
        assert_eq!(probability.raw(), approximate);
        assert_ne!(
            NoulProbability::new(probability.raw()).unwrap(),
            probability
        );
    }
    for raw in [f64::from_bits(1), -0.0] {
        let probability = NoulProbability::new(raw).unwrap();
        assert_eq!(probability.raw().to_bits(), raw.to_bits());
        assert_eq!(
            NoulProbability::new(probability.raw()).unwrap(),
            probability
        );
    }
}

#[test]
fn decimal_parser_rejects_non_numbers_and_exact_out_of_range_values() {
    for token in [
        "",
        " ",
        " 0",
        "0 ",
        "+0",
        "00",
        "01",
        ".1",
        "1.",
        "1.e0",
        "0x0",
        "1e",
        "1e+",
        "1e--1",
        "1e+-1",
        "0e0e0",
        "0..1",
        "--0",
        "NaN",
        "Infinity",
        "true",
        "false",
        "null",
        "[]",
        "{}",
        "\"0.5\"",
        "0,1",
        "0\n0",
        "零",
        "1.00000000000000000001",
        "100000000000000000001e-20",
        "1.00000000000000000001e+0",
        "1e999999999999999999999",
        "-0.00000000000000000001",
        "-1e-999",
        "-1e-999999999999999999999",
    ] {
        assert_eq!(
            NoulProbability::from_json_number(token),
            Err(ModelError::InvalidResponse),
            "{token}"
        );
    }
}

#[test]
fn decimal_parser_bounds_work_without_expanding_large_exponents() {
    for token in [
        format!("1{}e-400", "0".repeat(400)),
        format!("0.{}1e401", "0".repeat(400)),
    ] {
        let probability = NoulProbability::from_json_number(&token).unwrap();
        assert_eq!(
            (probability.yes_ppm(), probability.no_ppm()),
            (1_000_000, 0)
        );
    }
    let huge_exponent = "9".repeat(400);
    for (token, yes, no) in [
        (format!("1e-{huge_exponent}"), 0, 999_999),
        (format!("-0e+{huge_exponent}"), 0, 1_000_000),
    ] {
        let probability = NoulProbability::from_json_number(&token).unwrap();
        assert_eq!((probability.yes_ppm(), probability.no_ppm()), (yes, no));
    }
    let at_limit = format!("0.{}1", "0".repeat(MAX_MESSAGE_BYTES - 3));
    let probability = NoulProbability::from_json_number(&at_limit).unwrap();
    assert_eq!((probability.yes_ppm(), probability.no_ppm()), (0, 999_999));
    assert_eq!(
        NoulProbability::from_json_number(&format!("{at_limit}0")),
        Err(ModelError::InvalidResponse)
    );
}

#[test]
fn deserialization_cannot_round_or_forge_exact_normalization() {
    for encoded in [
        r#"{"raw":0.89999999999999999999,"yes_ppm":900000,"no_ppm":100000}"#,
        r#"{"raw":0.10000000000000000001,"yes_ppm":100000,"no_ppm":900000}"#,
        r#"{"raw":1.00000000000000000001,"yes_ppm":1000000,"no_ppm":0}"#,
        r#"{"raw":-1e-999,"yes_ppm":0,"no_ppm":1000000}"#,
        r#"{"raw":1e-999,"yes_ppm":0,"no_ppm":1000000}"#,
        r#"{"raw":0.99999999999999999999,"yes_ppm":1000000,"no_ppm":0}"#,
        r#"{"raw":0.9,"yes_ppm":900000,"no_ppm":99999}"#,
        r#"{"raw":0.9,"yes_ppm":900000,"no_ppm":100000,"approximate":1}"#,
        r#"{"raw":0.9,"raw":1,"yes_ppm":1000000,"no_ppm":0}"#,
    ] {
        assert!(
            serde_json::from_str::<NoulProbability>(encoded).is_err(),
            "{encoded}"
        );
    }
}

#[test]
fn deserialization_cannot_forge_normalized_support() {
    for value in [
        json!({"raw": 0.5, "yes_ppm": 900000, "no_ppm": 500000}),
        json!({"raw": 0.5, "yes_ppm": 500000, "no_ppm": 900000}),
        json!({"raw": "0.5", "yes_ppm": 500000, "no_ppm": 500000}),
        json!({"raw": true, "yes_ppm": 500000, "no_ppm": 500000}),
        json!({"raw": null, "yes_ppm": 500000, "no_ppm": 500000}),
        json!({"raw": 1.1, "yes_ppm": 1000000, "no_ppm": 0}),
        json!({"raw": 0.5, "yes_ppm": 500000.0, "no_ppm": 500000}),
        json!({"raw": 0.5, "yes_ppm": 500000, "no_ppm": 500000, "actions": []}),
    ] {
        assert!(
            serde_json::from_slice::<NoulProbability>(&serde_json::to_vec(&value).unwrap())
                .is_err()
        );
    }
}

#[tokio::test]
async fn fixture_identity_is_explicit_and_expired_calls_fail() {
    let reply = ModelReply {
        answers: BTreeMap::from([("q".into(), NoulProbability::new(0.9).unwrap())]),
    };
    let fixture = FixtureBackend::new(Ok(reply.clone()));
    assert_eq!(fixture.identity().kind, BackendKind::Fixture);
    assert_eq!(fixture.identity().mode, BackendMode::Fixture);
    assert_eq!(fixture.identity().model, "fixture");
    let question = NoulQuestion::new("Is this in scope?".into());
    assert_eq!(
        serde_json::to_value(&question).unwrap(),
        json!({"type":"noul","instructions":"Is this in scope?"})
    );
    let request = ModelRequest {
        state: json!({}),
        questions: BTreeMap::from([("q".into(), question)]),
    };
    assert_eq!(
        fixture
            .infer(&request, Instant::now() + Duration::from_secs(1))
            .await,
        Ok(reply)
    );
    assert_eq!(
        fixture.infer(&request, Instant::now()).await,
        Err(ModelError::Timeout)
    );
    let failure = FixtureBackend::new(Err(ModelError::RateLimited));
    assert_eq!(
        failure
            .infer(&request, Instant::now() + Duration::from_secs(1))
            .await,
        Err(ModelError::RateLimited)
    );
}
