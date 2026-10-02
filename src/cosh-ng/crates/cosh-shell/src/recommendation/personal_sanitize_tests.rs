use super::personal_model::RedactionKind;
use super::personal_sanitize::{sanitize_agent_request, sanitize_shell_command};

#[test]
fn sanitizer_removes_hard_secrets_and_keeps_business_context() {
    let input = concat!(
        "kubectl logs payment-api-abc -n production && ",
        "curl -H 'X-Api-Key: secret-header' -u alice:hunter2 ",
        "'https://api.example/payments?access_token=query-secret&region=cn'"
    );

    let sanitized = sanitize_shell_command(input).expect("sanitize command");

    for secret in ["secret-header", "hunter2", "query-secret"] {
        assert!(!sanitized.text.contains(secret), "{}", sanitized.text);
    }
    for business in [
        "payment-api-abc",
        "production",
        "api.example/payments",
        "region=cn",
    ] {
        assert!(sanitized.text.contains(business), "{}", sanitized.text);
    }
    assert!(sanitized
        .report
        .replacements
        .contains(&RedactionKind::Authorization));
}

#[test]
fn known_short_password_flags_are_redacted_but_generic_p_is_preserved() {
    let input = "mysql -uroot -pdb-secret app; docker login -u bob -p registry-secret registry.example; cargo test -p cosh-shell";

    let sanitized = sanitize_shell_command(input).expect("sanitize command");

    assert!(!sanitized.text.contains("db-secret"));
    assert!(!sanitized.text.contains("registry-secret"));
    assert!(sanitized.text.contains("cargo test -p cosh-shell"));
}

#[test]
fn quoted_cli_credentials_are_fully_redacted() {
    let input = concat!(
        "curl -u 'alice:curl-secret-one curl-secret-two' https://api.example/payments; ",
        "docker login --password \"docker-secret-one docker-secret-two\" registry.example; ",
        "mysql -p'mysql-secret-one mysql-secret-two' payments"
    );

    let sanitized = sanitize_shell_command(input).expect("sanitize quoted credentials");

    for secret in [
        "curl-secret-one",
        "curl-secret-two",
        "docker-secret-one",
        "docker-secret-two",
        "mysql-secret-one",
        "mysql-secret-two",
    ] {
        assert!(!sanitized.text.contains(secret), "{}", sanitized.text);
    }
    for business in ["api.example/payments", "registry.example", "payments"] {
        assert!(sanitized.text.contains(business), "{}", sanitized.text);
    }
}

#[test]
fn escaped_cli_credentials_are_fully_redacted() {
    let input = concat!(
        r#"tool --password correct\ horse\ battery deploy payment-api; "#,
        r#"curl -u alice:curl\ secret\ tail https://api.example/payments; "#,
        r#"docker login --password docker\ secret\ tail registry.example; "#,
        r#"mysql -pmysql\ secret\ tail payments; "#,
        r#"tool --token "alpha\" beta tail" deploy checkout-api; "#,
        r#"curl -u "alice:curl\" quoted tail" https://curl.example; "#,
        r#"docker login --password "docker\" quoted tail" docker.example; "#,
        r#"mysql -p"mysql\" quoted tail" reports"#,
        r#"; curl -u "alice:joined secret"tail https://joined.example"#,
        "; docker login --password=line\\\ncontinued-tail continuation.example",
        "; curl -u alice:comma,secret|kubectl get svc payment-api",
    );

    let sanitized = sanitize_shell_command(input).expect("sanitize escaped credentials");

    for secret in [
        "horse\\ battery",
        "curl\\ secret\\ tail",
        "docker\\ secret\\ tail",
        "mysql\\ secret\\ tail",
        "beta tail",
        "curl\\\" quoted tail",
        "docker\\\" quoted tail",
        "mysql\\\" quoted tail",
        "tail https://joined.example",
        "continued-tail",
        "comma,secret",
    ] {
        assert!(!sanitized.text.contains(secret), "{}", sanitized.text);
    }
    for business in [
        "payment-api",
        "api.example/payments",
        "registry.example",
        "payments",
        "checkout-api",
        "curl.example",
        "docker.example",
        "reports",
        "joined.example",
        "continuation.example",
        "kubectl get svc payment-api",
    ] {
        assert!(sanitized.text.contains(business), "{}", sanitized.text);
    }
}

#[test]
fn utf8_truncation_is_bounded_and_scanned_again() {
    let input = format!("分析 payment-api {} token=tail-secret", "你".repeat(2_000));

    let sanitized = sanitize_agent_request(&input).expect("sanitize request");

    assert!(sanitized.text.len() <= 4 * 1024);
    assert!(sanitized.report.truncated);
    assert!(!sanitized.text.contains("tail-secret"));
    assert!(sanitized.text.contains("payment-api"));
    assert!(sanitized.text.contains("<truncated>"));
}

#[test]
fn private_key_and_credential_url_never_survive() {
    let input = "-----BEGIN PRIVATE KEY-----\nprivate-sentinel\n-----END PRIVATE KEY-----\nredis://user:redis-secret@cache.internal/0";

    let sanitized = sanitize_agent_request(input).expect("sanitize request");

    assert!(!sanitized.text.contains("private-sentinel"));
    assert!(!sanitized.text.contains("redis-secret"));
    assert!(sanitized.text.contains("cache.internal/0"));
}

#[test]
fn inline_cookie_and_url_encoded_credentials_are_removed() {
    let input = concat!(
        "curl -H 'Cookie: session_id=cookie-secret' ",
        "'https://api.example/payments?access_token%3Dencoded-secret%26region%3Dcn'"
    );

    let sanitized = sanitize_shell_command(input).expect("sanitize encoded credentials");

    assert!(!sanitized.text.contains("cookie-secret"));
    assert!(!sanitized.text.contains("encoded-secret"));
    assert!(sanitized.text.contains("api.example/payments"));
}
