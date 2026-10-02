use crate::evidence::redact_sensitive_text;

#[test]
fn redacts_sensitive_patterns_adjacent_to_cjk_text() {
    for secret in [
        "ghp_abcdefghijklmnopqrstuvwxyz123456",
        "sk-abcdefghijklmnopqrstuvwxyz",
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.signature",
        "AKIA1234567890ABCDEF",
        "LTAI5tExampleAccessKey",
    ] {
        let input = format!("前缀{secret}后缀");
        let (redacted, changed) = redact_sensitive_text(&input);

        assert!(changed, "{input}");
        assert!(!redacted.contains(secret), "{redacted}");
    }

    for (input, secret) in [
        (
            "请求头authorization: abc123def456ghi789",
            "abc123def456ghi789",
        ),
        ("令牌bearer abc123def456ghi789", "abc123def456ghi789"),
        ("看一下https://user:p4ssw0rd@example.com/x", "p4ssw0rd"),
        ("看一下http://:s3cr3t42@example.test/", "s3cr3t42"),
    ] {
        let (redacted, changed) = redact_sensitive_text(input);

        assert!(changed, "{input}");
        assert!(!redacted.contains(secret), "{redacted}");
    }
}

#[test]
fn redacts_short_sk_tokens_without_matching_prose() {
    for secret in ["sk-abc123", "sk-fbaa6"] {
        for input in [
            format!("key是{secret}"),
            format!("（{secret}）"),
            format!("密钥为{secret}"),
            format!("key：{secret}"),
        ] {
            let (redacted, changed) = redact_sensitive_text(&input);
            assert!(changed, "short token not detected: {input}");
            assert!(!redacted.contains(secret), "{redacted}");
        }
    }

    for input in [
        "sk-hynix 内存条什么价格",
        "npm_package_version 怎么读",
        "cookie: 头是干什么的",
        "cookie: header meaning",
        "bearer 是什么认证方式",
        "shf_test 文件在哪",
        "xsk-abc123",
        "http://:@example.test/",
    ] {
        assert_eq!(
            redact_sensitive_text(input),
            (input.to_string(), false),
            "safe control rejected: {input}"
        );
    }
}

#[test]
fn redacts_embedded_cookie_headers_and_keeps_bare_token_prose() {
    for (input, secret, expected) in [
        (
            "curl -H 'Cookie: session=abc123def'",
            "session=abc123def",
            "curl -H 'Cookie: <redacted>'",
        ),
        (
            "2026-08-31 req Cookie: session=abc123def; Path=/",
            "session=abc123def",
            "2026-08-31 req Cookie: <redacted>",
        ),
        (
            "curl -H Cookie:session=abc123def --region cn",
            "session=abc123def",
            "curl -H Cookie:<redacted> --region cn",
        ),
        (
            r"curl -H Cookie:\ session=quoted-cookie-value-42 --region cn",
            "quoted-cookie-value-42",
            r"curl -H Cookie:\ <redacted> --region cn",
        ),
        (
            r#"curl -H"Cookie: session=quoted-cookie-value-42" --region cn"#,
            "quoted-cookie-value-42",
            r#"curl -H"Cookie: <redacted>" --region cn"#,
        ),
        (
            "curl -H'Cookie: session=quoted-cookie-value-42' --region cn",
            "quoted-cookie-value-42",
            "curl -H'Cookie: <redacted>' --region cn",
        ),
        (
            "curl -HCookie:session=quoted-cookie-value-42 --region cn",
            "quoted-cookie-value-42",
            "curl -HCookie:<redacted> --region cn",
        ),
        (
            r#"curl --header="Set-Cookie: csrf=quoted-cookie-value-42" --region cn"#,
            "quoted-cookie-value-42",
            r#"curl --header="Set-Cookie: <redacted>" --region cn"#,
        ),
        (
            r"curl -HCookie\:session=quoted-cookie-value-42 --region cn",
            "quoted-cookie-value-42",
            r"curl -HCookie\:<redacted> --region cn",
        ),
        (
            "curl -sH'Cookie: session=quoted-cookie-value-42' --region cn",
            "quoted-cookie-value-42",
            "curl -sH'Cookie: <redacted>' --region cn",
        ),
        (
            r#"curl -fsSLH"Cookie: session=quoted-cookie-value-42" --region cn"#,
            "quoted-cookie-value-42",
            r#"curl -fsSLH"Cookie: <redacted>" --region cn"#,
        ),
        (
            r"curl -sHCookie\:session=quoted-cookie-value-42 --region cn",
            "quoted-cookie-value-42",
            r"curl -sHCookie\:<redacted> --region cn",
        ),
        (
            "curl -s'H'Cookie:session=quoted-cookie-value-42 --region cn",
            "quoted-cookie-value-42",
            "curl -s'H'Cookie:<redacted> --region cn",
        ),
        (
            r"curl -s\HCookie:session=quoted-cookie-value-42 --region cn",
            "quoted-cookie-value-42",
            r"curl -s\HCookie:<redacted> --region cn",
        ),
        (
            r#"curl "-HCookie: session=whole-option-secret-42" --region cn"#,
            "whole-option-secret-42",
            r#"curl "-HCookie: <redacted>" --region cn"#,
        ),
        (
            "curl '-HCookie: session=whole-option-secret-42' --region cn",
            "whole-option-secret-42",
            "curl '-HCookie: <redacted>' --region cn",
        ),
        (
            r"curl -H Cookie\:session=quoted-cookie-value-42 --region cn",
            "quoted-cookie-value-42",
            r"curl -H Cookie\:<redacted> --region cn",
        ),
        (
            "curl -H Cookie':'session=quoted-cookie-value-42 --region cn",
            "quoted-cookie-value-42",
            "curl -H Cookie':'<redacted> --region cn",
        ),
        (
            "curl -H Coo'kie:'session=quoted-cookie-value-42 --region cn",
            "quoted-cookie-value-42",
            "curl -H Coo'kie:'<redacted> --region cn",
        ),
        (
            "curl -H 'Cookie: session = quoted-cookie-value-42' --region cn",
            "quoted-cookie-value-42",
            "curl -H 'Cookie: <redacted>' --region cn",
        ),
        (
            "curl -H Cookie:'session'=quoted-cookie-value-42 --region cn",
            "quoted-cookie-value-42",
            "curl -H Cookie:'<redacted>' --region cn",
        ),
        (
            r"curl -H Set\-Cookie\:csrf=quoted-cookie-value-42 --region cn",
            "quoted-cookie-value-42",
            r"curl -H Set\-Cookie\:<redacted> --region cn",
        ),
        (
            "Cookie:session=abc123def; other=second-cookie-value\nsafe",
            "second-cookie-value",
            "Cookie:<redacted>\nsafe",
        ),
        (
            r#"curl -H "Cookie: session=literal\$value" --region cn"#,
            r#"literal\$value"#,
            r#"curl -H "Cookie: <redacted>" --region cn"#,
        ),
        (
            r#"curl -H 'Cookie: session="quoted-cookie-value-42"'"#,
            "quoted-cookie-value-42",
            "curl -H 'Cookie: <redacted>'",
        ),
        (
            r#"curl -H "Set-Cookie: csrf='quoted-cookie-value-42'; Path=/""#,
            "quoted-cookie-value-42",
            "curl -H \"Set-Cookie: <redacted>\"",
        ),
        (
            r#"Cookie: session="quoted-cookie-value-42""#,
            "quoted-cookie-value-42",
            "Cookie: <redacted>",
        ),
        (
            r#"curl -H "Cookie: session=\"quoted-cookie-value-42\"""#,
            "quoted-cookie-value-42",
            "curl -H \"Cookie: <redacted>\"",
        ),
        (
            "curl -H 'Cookie: session='quoted-cookie-value-42",
            "quoted-cookie-value-42",
            "curl -H 'Cookie: <redacted>'",
        ),
        (
            "curl -H 'Cookie: session='quoted-cookie-value-42 --region cn",
            "quoted-cookie-value-42",
            "curl -H 'Cookie: <redacted>' --region cn",
        ),
        (
            r#"curl -H "Cookie: session="'quoted-cookie-value-42'"#,
            "quoted-cookie-value-42",
            "curl -H \"Cookie: <redacted>\"",
        ),
        (
            r#"curl -H 'Set-Cookie: csrf='"quoted cookie value 42""#,
            "quoted cookie value 42",
            "curl -H 'Set-Cookie: <redacted>'",
        ),
        (
            "curl -H 'Cookie: session='$(printf quoted-cookie-value-42)",
            "quoted-cookie-value-42",
            "<redacted>",
        ),
        (
            "curl -H 'Cookie: session='$(printf $(true) quoted-cookie-value-42) --region cn",
            "quoted-cookie-value-42",
            "<redacted>",
        ),
        (
            "curl -H 'Cookie: session='$(printf '%s' \\\n  quoted-cookie-value-42)",
            "quoted-cookie-value-42",
            "<redacted>",
        ),
        (
            r#"curl -H 'Cookie: session='$(printf quoted-cookie-value-42) --region "$(uname)""#,
            "quoted-cookie-value-42",
            "<redacted>",
        ),
        (
            "curl -H 'Cookie: session='$(case x in x) printf '%s' quoted-cookie-value-42;; esac)",
            "quoted-cookie-value-42",
            "<redacted>",
        ),
        (
            "curl -H 'Cookie: session='$(printf case) --region cn",
            "case",
            "<redacted>",
        ),
        (
            r#"curl -H "Set-Cookie: csrf="`printf 'quoted cookie value 42'`"#,
            "quoted cookie value 42",
            "<redacted>",
        ),
    ] {
        let (redacted, changed) = redact_sensitive_text(input);

        assert!(changed, "embedded Cookie header not detected: {input}");
        assert!(
            !redacted.contains(secret),
            "cookie value leaked: {redacted}"
        );
        assert_eq!(redacted, expected);
    }

    for input in [
        "curl -H 'Cookie: session='$(if true; then case x in x) printf '%s' quoted-cookie-value-42;; esac; fi) --region cn",
        "curl -H 'Cookie: session='$(printf '%s' start # )\nprintf '%s' quoted-cookie-value-42) --region cn",
        "curl -H 'Cookie: session='$(cat <<'EOF'\nquoted-cookie-value-42)\nEOF\n) --region cn",
        "curl -H Cookie:session=$COOKIE_VALUE --region cn",
        "curl -s${HEADER_OPTION}Cookie:session=quoted-cookie-value-42 --region cn",
        r#"curl ${HEADER_OPTION}"Cookie: session=dynamic-option-secret-42" --region cn"#,
        r#"curl ${HEADER_OPTION#*;}"Cookie: session=parameter-pattern-secret-42" https://example.test"#,
        r#"curl ${HEADER_OPTION#*|}"Cookie: session=parameter-pipe-secret-42" https://example.test"#,
        r#"curl ${HEADER_OPTION:-$(printf '}' >/dev/null; printf -- -H)}"Cookie: session=parameter-nested-secret-42""#,
        "curl -H 'Cookie: session='<(printf quoted-cookie-value-42) --region cn",
        "request Cookie:session=quoted-cookie-value-42; other=second-cookie-value",
        "Cookie: first=first-cookie-value\ncurl -H 'Cookie: session='$(printf quoted-cookie-value-42)",
    ] {
        let (redacted, changed) = redact_sensitive_text(input);

        assert!(changed, "dynamic Cookie header not detected: {input}");
        assert_eq!(redacted, "<redacted>");
    }

    // A header-shaped cookie requires name=value to avoid redacting prose.
    for input in [
        "Cookie: abc123baretoken",
        "Set-Cookie: abc123baretoken",
        r"curl -H Cookie\:header-meaning",
        r"curl -H Crookie\:session=quoted-cookie-value-42",
        r#"tool x-H"Cookie: session=quoted-cookie-value-42""#,
        r#"tool x-sH"Cookie: session=quoted-cookie-value-42""#,
        r#"curl --sH"Cookie: session=quoted-cookie-value-42""#,
        r#"tool -s'Hx'Cookie:session=quoted-cookie-value-42"#,
        r#"curl --header-label="Cookie: session=quoted-cookie-value-42""#,
        r#"curl ${HEADER_OPTION#*;}"Crookie: session=parameter-pattern-secret-42""#,
        r#"curl ${HEADER_OPTION:-$(printf '}' >/dev/null; printf -- -H)}"Crookie: session=parameter-nested-secret-42""#,
    ] {
        assert_eq!(
            redact_sensitive_text(input),
            (input.to_string(), false),
            "bare token boundary changed: {input}"
        );
    }
}
