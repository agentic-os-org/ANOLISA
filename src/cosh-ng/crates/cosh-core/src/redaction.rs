//! Central secret redaction for provider, hook, and session persistence boundaries.

use std::sync::OnceLock;

use regex::Regex;
use serde::Serialize;
use serde_json::Value;

use crate::provider::{Message, MessageContent, MessageContentBlock};

pub(crate) fn redact_text(text: &str) -> String {
    let (mut redacted, _) = redact_private_key_blocks(text);
    for (pattern, replacement) in [
        (cookie_header_pattern(), "$prefix<redacted>"),
        (authorization_pattern(), "$prefix$scheme <redacted>"),
        (bearer_pattern(), "$prefix<redacted>"),
        (url_password_pattern(), "$prefix<redacted>@"),
        (sensitive_flag_pattern(), "$prefix<redacted>"),
        (sensitive_assignment_pattern(), "$prefix<redacted>"),
        (github_token_pattern(), "<redacted>"),
        (opaque_token_pattern(), "<redacted>"),
        (jwt_pattern(), "<redacted>"),
        (aws_access_key_pattern(), "$prefix<redacted>"),
        (alibaba_access_key_pattern(), "<redacted>"),
    ] {
        redacted = pattern.replace_all(&redacted, replacement).into_owned();
    }
    redacted
}

/// Returns whether text matches a redaction rule or private-key end marker.
pub(crate) fn contains_sensitive_text(text: &str) -> bool {
    text.lines()
        .any(|line| private_key_marker_range(line, "-----END ").is_some())
        || redact_text(text) != text
}

pub(crate) fn redact_value(value: &mut Value) {
    match value {
        Value::Object(values) => {
            for (key, value) in values {
                if is_sensitive_key(key) {
                    *value = Value::String("<redacted>".to_string());
                } else {
                    redact_value(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_value(value);
            }
        }
        Value::String(text) => *text = redact_text(text),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

pub(crate) fn to_redacted_json<T: Serialize>(value: &T) -> String {
    let Ok(mut value) = serde_json::to_value(value) else {
        return String::new();
    };
    redact_value(&mut value);
    serde_json::to_string(&value).unwrap_or_default()
}

/// Hook payload subtrees holding tool declarations. These are JSON Schema —
/// a property named `api_key` is a *declaration*, not a secret, so key-based
/// redaction would replace the property object with `"<redacted>"` and hand
/// the model (or the hook) a corrupt schema. Both the canonical
/// `llm_request.config.tools` and the legacy `llm_request.tools` positions are
/// listed so hook output written against either survives intact.
pub(crate) const TOOL_DECLARATION_PATHS: &[&[&str]] = &[
    &["llm_request", "config", "tools"],
    &["llm_request", "tools"],
];

/// Like [`to_redacted_json`], but applies structure-preserving redaction to the
/// subtrees at `schema_paths` instead of key-based redaction. Paths that are
/// absent are ignored, so this is safe to use for every hook event.
pub(crate) fn to_redacted_json_with_schemas<T: Serialize>(
    value: &T,
    schema_paths: &[&[&str]],
) -> String {
    let Ok(mut value) = serde_json::to_value(value) else {
        return String::new();
    };
    redact_value_with_schemas(&mut value, schema_paths);
    serde_json::to_string(&value).unwrap_or_default()
}

pub(crate) fn redact_value_with_schemas(value: &mut Value, schema_paths: &[&[&str]]) {
    let detached: Vec<Option<Value>> = schema_paths
        .iter()
        .map(|path| detach_path(value, path))
        .collect();

    redact_value(value);

    for (path, subtree) in schema_paths.iter().zip(detached) {
        if let Some(mut subtree) = subtree {
            redact_schema_value(&mut subtree);
            reattach_path(value, path, subtree);
        }
    }
}

/// Structure-preserving redaction: scrubs secret *shapes* out of string leaves
/// but never replaces a value because its key name looks sensitive.
fn redact_schema_value(value: &mut Value) {
    match value {
        Value::Object(values) => {
            for value in values.values_mut() {
                redact_schema_value(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_schema_value(value);
            }
        }
        Value::String(text) => *text = redact_text(text),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// Removes the subtree at `path`, leaving `Null` behind so [`reattach_path`]
/// can find the slot again after the surrounding value has been redacted.
fn detach_path(value: &mut Value, path: &[&str]) -> Option<Value> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.as_object_mut()?.get_mut(*key)?;
    }
    (!cursor.is_null()).then(|| cursor.take())
}

fn reattach_path(value: &mut Value, path: &[&str], subtree: Value) {
    let mut cursor = value;
    for key in path {
        let Some(next) = cursor.as_object_mut().and_then(|map| map.get_mut(*key)) else {
            return;
        };
        cursor = next;
    }
    *cursor = subtree;
}

pub(crate) fn redact_messages(messages: &mut [Message]) {
    for message in messages {
        redact_message(message);
    }
}

fn redact_message(message: &mut Message) {
    match &mut message.content {
        MessageContent::Text(text) => *text = redact_text(text),
        MessageContent::Blocks(blocks) => {
            for block in blocks {
                match block {
                    MessageContentBlock::Text { text } => *text = redact_text(text),
                    MessageContentBlock::ToolResult { content, .. } => {
                        *content = redact_text(content);
                    }
                }
            }
        }
    }

    if let Some(tool_calls) = &mut message.tool_calls {
        for tool_call in tool_calls {
            tool_call.function.arguments = redact_json_or_text(&tool_call.function.arguments);
        }
    }
}

pub(crate) fn redact_json_or_text(text: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(text) else {
        return redact_text(text);
    };
    redact_value(&mut value);
    serde_json::to_string(&value).unwrap_or_else(|_| redact_text(text))
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "password"
            | "passwd"
            | "passphrase"
            | "token"
            | "accesstoken"
            | "refreshtoken"
            | "idtoken"
            | "secret"
            | "clientsecret"
            | "apikey"
            | "accesskeyid"
            | "accesskeysecret"
            | "securitytoken"
            | "awssecretaccesskey"
            | "openaiapikey"
            | "dashscopeapikey"
            | "githubtoken"
            | "authorization"
            | "cookie"
            | "setcookie"
    )
}

fn redact_private_key_blocks(text: &str) -> (String, bool) {
    let mut output = String::with_capacity(text.len());
    let mut in_private_key = false;
    let mut changed = false;

    for line in text.split_inclusive('\n') {
        if in_private_key {
            changed = true;
            if let Some((_, end)) = private_key_marker_range(line, "-----END ") {
                output.push_str(&line[end..]);
                in_private_key = false;
            }
            continue;
        }
        if let Some((start, end)) = private_key_marker_range(line, "-----BEGIN ") {
            output.push_str(&line[..start]);
            output.push_str("<redacted private key block>");
            let remainder = &line[end..];
            if let Some((_, private_key_end)) = private_key_marker_range(remainder, "-----END ") {
                output.push_str(&remainder[private_key_end..]);
            } else {
                in_private_key = true;
            }
            changed = true;
            continue;
        }
        output.push_str(line);
    }

    (output, changed)
}

fn private_key_marker_range(line: &str, marker: &str) -> Option<(usize, usize)> {
    let upper = line.to_ascii_uppercase();
    let start = upper.find(marker)?;
    let marker_end = upper[start..].find("PRIVATE KEY-----")?;
    Some((start, start + marker_end + "PRIVATE KEY-----".len()))
}

fn cookie_header_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // Unanchored, mirroring cosh-platform/src/audit/redact.rs: a Cookie
        // header passed as `curl -H "Cookie: …"` never sits at a line start,
        // so the old `^`-anchored pattern let the whole header through.
        // The pattern is a compile-time constant covered by the tests below.
        Regex::new(r"(?im)(?P<prefix>\b(?:set-cookie|cookie)\s*:\s*)[^\r\n]*")
            .unwrap_or_else(|_| unreachable!("static cookie pattern must compile"))
    })
}

fn authorization_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // The pattern is a compile-time constant covered by the tests below.
        Regex::new(
            r"(?i)(?P<prefix>\bauthorization\s*(?::|=)\s*)(?P<scheme>bearer|basic|token)?\s*(?P<value>[^\s,;&]+)",
        )
        .unwrap_or_else(|_| unreachable!("static authorization pattern must compile"))
    })
}

fn bearer_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // The pattern is a compile-time constant covered by the tests below.
        Regex::new(r"(?i)(?P<prefix>\bbearer\s+)[A-Za-z0-9._~+/=-]+")
            .unwrap_or_else(|_| unreachable!("static bearer pattern must compile"))
    })
}

fn url_password_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // The pattern is a compile-time constant covered by the tests below.
        Regex::new(r"(?i)(?P<prefix>\b[a-z][a-z0-9+.-]*://[^/\s:@]+:)[^@/\s]+@")
            .unwrap_or_else(|_| unreachable!("static URL password pattern must compile"))
    })
}

fn sensitive_flag_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // The pattern is a compile-time constant covered by the tests below.
        // The key catalog and the value-span alternation mirror
        // cosh-shell/src/evidence/redaction.rs and
        // cosh-platform/src/audit/redact.rs — the three copies must stay in
        // sync. Values are matched as complete shell words: a quoted secret
        // runs to its closing quote (or end of input — fail-closed), an
        // escaped continuation runs to the first unescaped separator, so a
        // multi-word password can never be half redacted.
        // The pattern is a compile-time constant covered by the tests below.
        Regex::new(
            r#"(?ix)
            (?P<prefix>
                (?:^|\s)
                --(?:password|passwd|passphrase|token|access[_-]?token|refresh[_-]?token|
                     id[_-]?token|secret|secret[_-]?key|secret[_-]?access[_-]?key|
                     private[_-]?key|auth[_-]?token|session[_-]?token|credentials?|bearer|
                     client[_-]?secret|api[_-]?key|apikey|access[_-]?key|
                     access[_-]?key[_-]?secret|security[_-]?token|authorization)
                (?:=|\s+)
            )
            (?:
                "(?:\\(?:\r?\n|[^"\\\r\n])|[^"\\])*(?:"|$)|
                '[^']*(?:'|$)|
                \\(?:\r?\n|[^\r\n])|
                [^\s;&|()<>"'\\]
            )+
            "#,
        )
        .unwrap_or_else(|_| unreachable!("static sensitive flag pattern must compile"))
    })
}

fn sensitive_assignment_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // The pattern is a compile-time constant covered by the tests below.
        // Same key catalog and value-span alternation as the flag pattern
        // (mirrors cosh-shell/cosh-platform; see the note there).
        // The pattern is a compile-time constant covered by the tests below.
        Regex::new(
            r#"(?ix)
            (?P<prefix>
                (?:^|[\s"'])
                (?:alibaba[_-]?cloud[_-]?access[_-]?key[_-]?id|
                   aws[_-]?access[_-]?key[_-]?id|access[_-]?key[_-]?id|
                   aws[_-]?secret[_-]?access[_-]?key|access[_-]?key[_-]?secret|
                   secret[_-]?access[_-]?key|secret[_-]?key|private[_-]?key|
                   auth[_-]?token|session[_-]?token|credentials?|bearer|
                   dashscope[_-]?api[_-]?key|openai[_-]?api[_-]?key|
                   client[_-]?secret|security[_-]?token|refresh[_-]?token|
                   access[_-]?token|github[_-]?token|id[_-]?token|
                   password|passphrase|passwd|api[_-]?key|apikey|token|secret)
                \s*(?:=|:)\s*
            )
            (?:
                "(?:\\(?:\r?\n|[^"\\\r\n])|[^"\\])*(?:"|$)|
                '[^']*(?:'|$)|
                \\(?:\r?\n|[^\r\n])|
                [^\s;&|()<>"'\\]
            )+
            "#,
        )
        .unwrap_or_else(|_| unreachable!("static sensitive assignment pattern must compile"))
    })
}

fn github_token_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // The pattern is a compile-time constant covered by the tests below.
        Regex::new(r"\b(?:gh[pousr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{20,})\b")
            .unwrap_or_else(|_| unreachable!("static GitHub token pattern must compile"))
    })
}

fn opaque_token_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // The pattern is a compile-time constant covered by the tests below.
        Regex::new(
            r"\b(?:sk-[A-Za-z0-9_-]{10,}|sk_(?:live|test)_[A-Za-z0-9]{10,}|glpat-[A-Za-z0-9_-]{10,}|npm_[A-Za-z0-9]{20,}|hf_[A-Za-z0-9]{20,}|AIza[A-Za-z0-9_-]{20,}|xox[baprs]-[A-Za-z0-9-]{10,})\b",
        )
        .unwrap_or_else(|_| unreachable!("static opaque token pattern must compile"))
    })
}

fn jwt_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // The pattern is a compile-time constant covered by the tests below.
        Regex::new(r"\beyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\b")
            .unwrap_or_else(|_| unreachable!("static JWT pattern must compile"))
    })
}

fn aws_access_key_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // The pattern is a compile-time constant covered by the tests below.
        Regex::new(r"\b(?P<prefix>AKIA|ASIA)[A-Z0-9]{16}\b")
            .unwrap_or_else(|_| unreachable!("static AWS access key pattern must compile"))
    })
}

fn alibaba_access_key_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // The pattern is a compile-time constant covered by the tests below.
        Regex::new(r"\bLTAI[A-Za-z0-9]{12,32}\b")
            .unwrap_or_else(|_| unreachable!("static Alibaba access key pattern must compile"))
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{redact_text, redact_value};

    #[test]
    fn redacts_common_secret_shapes() {
        let input = concat!(
            "Bearer bearer-value ",
            "ALIBABA_CLOUD_ACCESS_KEY_ID=LTAIexampleaccesskey ",
            "OPENAI_API_KEY=sk-example-secret ",
            "--password hunter2 ",
            "AKIA1234567890ABCDEF ",
            "LTAI5tExampleAccessKey ",
            "ghp_abcdefghijklmnopqrstuvwxyz123456 ",
            "https://user:url-password@example.test/path"
        );

        let redacted = redact_text(input);

        for secret in [
            "bearer-value",
            "LTAIexampleaccesskey",
            "sk-example-secret",
            "hunter2",
            "AKIA1234567890ABCDEF",
            "LTAI5tExampleAccessKey",
            "ghp_",
            "url-password",
        ] {
            assert!(!redacted.contains(secret), "{redacted}");
        }
    }

    #[test]
    fn redacts_sensitive_json_keys_and_nested_strings() {
        let mut value = json!({
            "api_key": "short-value",
            "nested": {
                "command": "curl --token command-secret",
                "safe": "visible"
            }
        });

        redact_value(&mut value);

        assert_eq!(value["api_key"], "<redacted>");
        assert_eq!(value["nested"]["safe"], "visible");
        assert!(!value.to_string().contains("short-value"));
        assert!(!value.to_string().contains("command-secret"));
    }

    #[test]
    fn removes_private_key_body() {
        let input = concat!(
            "before\n",
            "-----BEGIN PRIVATE KEY-----\n",
            "private-body\n",
            "-----END PRIVATE KEY-----\n",
            "after"
        );

        assert_eq!(
            redact_text(input),
            "before\n<redacted private key block>\nafter"
        );
    }

    #[test]
    fn preserves_content_after_private_key_end_marker() {
        let input = concat!(
            "-----BEGIN PRIVATE KEY-----\n",
            "private-body\n",
            "-----END PRIVATE KEY-----' --token token-value"
        );

        let redacted = redact_text(input);

        assert_eq!(redacted, "<redacted private key block>' --token <redacted>");
    }

    #[test]
    fn redacts_quoted_multi_word_flag_and_assignment_values() {
        // A quoted secret runs to its closing quote: multi-word passwords
        // were half redated before (only up to the first space).
        let out = redact_text(r#"run --password "correct horse battery staple""#);
        assert_eq!(out, "run --password <redacted>");
        for word in ["correct", "horse", "battery", "staple"] {
            assert!(!out.contains(word), "leaked {word}: {out}");
        }

        let out = redact_text("mysql --password='swordfish hunter two'");
        assert_eq!(out, "mysql --password=<redacted>");
        for word in ["swordfish", "hunter", "two"] {
            assert!(!out.contains(word), "leaked {word}: {out}");
        }
    }

    #[test]
    fn redacts_escaped_continuation_values() {
        // An escaped continuation runs to the first unescaped separator.
        let out = redact_text(r"pkg install --password=head\ LEAKSTRUCTESCTAIL");
        assert_eq!(out, "pkg install --password=<redacted>");
        assert!(!out.contains("LEAKSTRUCTESCTAIL"), "leaked tail: {out}");

        // The redaction must stop at the separator and not eat later flags.
        let out = redact_text(r"tool --token=alpha\ beta --region cn");
        assert!(!out.contains("alpha"), "leaked token: {out}");
        assert!(out.contains("--region cn"), "later flags kept: {out}");
    }

    #[test]
    fn redacts_unterminated_quoted_value_to_end_of_input() {
        // Fail-closed: no closing quote means the rest of the input is the
        // secret.
        let out = redact_text(r#"tool --password "unterminated secret tail"#);
        assert_eq!(out, "tool --password <redacted>");
        assert!(!out.contains("unterminated"), "leaked: {out}");
    }

    #[test]
    fn redacts_platform_secret_key_names() {
        // The platform key catalog (issue #1618 / PR #1765) ported to core.
        for (input, secret) in [
            (
                "SECRET_KEY=django-insecure-1a2b3c4d",
                "django-insecure-1a2b3c4d",
            ),
            ("private_key=rawbase64secret", "rawbase64secret"),
            ("--secret-key sk-live-abcdef", "sk-live-abcdef"),
            ("--auth-token tok123", "tok123"),
            ("--session-token sess456", "sess456"),
            ("--credentials cred789", "cred789"),
        ] {
            let out = redact_text(input);
            assert!(!out.contains(secret), "leaked {secret} from {input}: {out}");
        }
    }

    #[test]
    fn redacts_mid_line_cookie_headers() {
        // A Cookie header passed as a curl -H argument never sits at a line
        // start; the old ^-anchored pattern let the whole header through.
        let out = redact_text(r#"curl -H "Cookie: SESSIONID=deadbeefcafe1234" https://x"#);
        assert!(!out.contains("deadbeefcafe1234"), "leaked cookie: {out}");

        let out = redact_text("printf 'Set-Cookie: csrf=tok123; Path=/'");
        assert!(!out.contains("tok123"), "leaked set-cookie: {out}");
    }
}
