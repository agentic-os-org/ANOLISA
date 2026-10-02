use std::sync::OnceLock;

use regex::Regex;

use crate::tools::{classify_command_interaction, OutputStability};
use crate::types::{CommandBlock, CommandStatus};

mod shell_word;

use shell_word::redact_shell_cookie_headers;

const PROVIDER_COMMAND_MAX_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCommandFacts {
    pub id: String,
    pub command: String,
    pub cwd: String,
    pub end_cwd: String,
    pub status: &'static str,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub output_bytes: u64,
    pub output_id: String,
    pub output_stability: &'static str,
}

pub fn redact_provider_command_text(command: &str) -> String {
    truncate_provider_command(redact_sensitive_text(command).0)
}

/// Provider-facing text for a handoff command the provider itself authored
/// (#2142 D-6): the original text already sits in the model transcript, so
/// re-redacting it here only breaks result-to-call correlation. Length is
/// still bounded; durable surfaces keep the redacted form.
pub fn truncate_provider_authored_command_text(command: &str) -> String {
    truncate_provider_command(command.to_string())
}

fn truncate_provider_command(mut command: String) -> String {
    const MARKER: &str = " ... <truncated>";
    if command.len() <= PROVIDER_COMMAND_MAX_BYTES {
        return command;
    }

    let mut end = PROVIDER_COMMAND_MAX_BYTES - MARKER.len();
    while !command.is_char_boundary(end) {
        end -= 1;
    }
    command.truncate(end);
    command.push_str(MARKER);
    command
}

pub fn terminal_output_id(shell_session_id: &str, command_id: &str) -> String {
    format!("terminal-output://{shell_session_id}/{command_id}")
}

pub fn provider_safe_command_facts(block: &CommandBlock) -> ProviderCommandFacts {
    let status = match block.status {
        CommandStatus::Completed => "completed",
        CommandStatus::Failed => "failed",
    };
    let output_id = if block.output.terminal_output_ref.is_some() {
        terminal_output_id(&block.session_id, &block.id)
    } else {
        "<missing>".to_string()
    };
    let output_stability = match classify_command_interaction(&block.command).output_stability {
        OutputStability::StableSnapshot => "stable_snapshot",
        OutputStability::UnstableInteractive => "unstable_interactive",
    };
    ProviderCommandFacts {
        id: block.id.clone(),
        command: redact_provider_command_text(&block.command),
        cwd: redact_sensitive_text(&redact_home_path(&block.cwd)).0,
        end_cwd: redact_sensitive_text(&redact_home_path(&block.end_cwd)).0,
        status,
        exit_code: block.exit_code,
        duration_ms: block.duration_ms,
        output_bytes: block.output.terminal_output_bytes,
        output_id,
        output_stability,
    }
}

fn redact_home_path(value: &str) -> String {
    let Ok(home) = std::env::var("HOME") else {
        return value.to_string();
    };
    if home.is_empty() {
        value.to_string()
    } else {
        value.replace(&home, "~")
    }
}

pub(crate) fn redact_sensitive_text(text: &str) -> (String, bool) {
    let (mut redacted, mut changed) = redact_private_key_blocks(text);
    let next = conservative_cookie_header_pattern()
        .replace_all(&redacted, "$prefix<redacted>")
        .into_owned();
    changed |= next != redacted;
    redacted = next;
    let (next, cookie_changed) = redact_shell_cookie_headers(&redacted);
    redacted = next;
    changed |= cookie_changed;
    for (pattern, replacement) in [
        (authorization_pattern(), "$prefix$scheme <redacted>"),
        (bearer_pattern(), "$prefix<redacted>"),
        (url_password_pattern(), "$prefix<redacted>@"),
        (sensitive_flag_pattern(), "$prefix<redacted>"),
        (sensitive_cookie_assignment_pattern(), "$prefix<redacted>"),
        (sensitive_assignment_pattern(), "$prefix<redacted>"),
        (github_token_pattern(), "<redacted>"),
        (opaque_token_pattern(), "<redacted>"),
        (jwt_pattern(), "<redacted>"),
        (aws_access_key_pattern(), "$prefix<redacted>"),
        (alibaba_access_key_pattern(), "<redacted>"),
    ] {
        let next = pattern.replace_all(&redacted, replacement).into_owned();
        changed |= next != redacted;
        redacted = next;
    }
    (redacted, changed)
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

fn conservative_cookie_header_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r#"(?im)(?P<prefix>(?:^(?:set-cookie|cookie)\s*:\s*|\s(?:set-cookie|cookie)\s*:\s+))[^=\s;,]+\s*=\s*[^\r\n]*"#,
        )
        .unwrap_or_else(|_| unreachable!("static cookie pattern must compile"))
    })
}

fn authorization_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // The pattern is a compile-time constant covered by the tests below.
        Regex::new(
            r"(?i)(?P<prefix>(?-u:\b)authorization\s*(?::|=)\s*)(?P<scheme>bearer|basic|token)?\s*(?P<value>[^\s,;&]+)",
        )
        .unwrap_or_else(|_| unreachable!("static authorization pattern must compile"))
    })
}

fn bearer_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // The pattern is a compile-time constant covered by the tests below.
        Regex::new(r"(?i)(?P<prefix>(?-u:\b)bearer\s+)[A-Za-z0-9._~+/=-]+")
            .unwrap_or_else(|_| unreachable!("static bearer pattern must compile"))
    })
}

fn url_password_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // The pattern is a compile-time constant covered by the tests below.
        Regex::new(r"(?i)(?P<prefix>(?-u:\b)[a-z][a-z0-9+.-]*://[^/\s:@]*:)[^@/\s]+@")
            .unwrap_or_else(|_| unreachable!("static URL password pattern must compile"))
    })
}

fn sensitive_flag_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // The pattern is a compile-time constant covered by the tests below.
        Regex::new(
            r#"(?ix)
            (?P<prefix>
                (?:^|\s)
                --(?:password|passwd|passphrase|token|access[_-]?token|refresh[_-]?token|
                     id[_-]?token|secret|client[_-]?secret|api[_-]?key|apikey|
                     access[_-]?key[_-]?(?:id|secret)|security[_-]?token|authorization|
                     cookie|set[_-]?cookie)
                (?:=|\s+)
            )
            (?:
                "(?:\\(?:\r?\n|[^\r\n])|[^"\\])*(?:"|$)|
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
        Regex::new(
            r#"(?ix)
            (?P<prefix>
                ["']?
                (?:alibaba[_-]?cloud[_-]?access[_-]?key[_-]?id|
                   aws[_-]?access[_-]?key[_-]?id|access[_-]?key[_-]?id|
                   aws[_-]?secret[_-]?access[_-]?key|access[_-]?key[_-]?secret|
                   dashscope[_-]?api[_-]?key|openai[_-]?api[_-]?key|
                   client[_-]?secret|security[_-]?token|refresh[_-]?token|
                   access[_-]?token|github[_-]?token|id[_-]?token|
                   password|passphrase|passwd|api[_-]?key|apikey|token|secret)
                ["']?
                \s*(?:=|:)\s*
            )
            (?:
                "(?:\\(?:\r?\n|[^\r\n])|[^"\\])*(?:"|$)|
                '[^']*(?:'|$)|
                \\(?:\r?\n|[^\r\n])|
                [^\s;&|()<>"'\\]
            )+
            "#,
        )
        .unwrap_or_else(|_| unreachable!("static sensitive assignment pattern must compile"))
    })
}

fn sensitive_cookie_assignment_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // Cookie colons are reserved for structured JSON or name=value headers.
        Regex::new(
            r#"(?ix)
            (?P<prefix>
                (?:^|[^A-Za-z0-9_-])["']?(?:cookie|set[_-]?cookie)["']?\s*=\s*|
                ["'](?:cookie|set[_-]?cookie)["']\s*:\s*
            )
            (?:
                "(?:\\(?:\r?\n|[^\r\n])|[^"\\])*(?:"|$)|
                '[^']*(?:'|$)|
                \\(?:\r?\n|[^\r\n])|
                [^\s;&|()<>"'\\]
            )+
            "#,
        )
        .unwrap_or_else(|_| unreachable!("static cookie assignment pattern must compile"))
    })
}

fn github_token_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // The pattern is a compile-time constant covered by the tests below.
        Regex::new(r"(?-u:\b)(?:gh[pousr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{20,})(?-u:\b)")
            .unwrap_or_else(|_| unreachable!("static GitHub token pattern must compile"))
    })
}

fn opaque_token_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // A five-character sk- suffix needs a digit to exclude sk-hynix prose.
        Regex::new(
            r"(?-u:\b)(?:sk-(?:[A-Za-z0-9_-]{6,}|[0-9][A-Za-z0-9_-]{4}|[A-Za-z0-9_-][0-9][A-Za-z0-9_-]{3}|[A-Za-z0-9_-]{2}[0-9][A-Za-z0-9_-]{2}|[A-Za-z0-9_-]{3}[0-9][A-Za-z0-9_-]|[A-Za-z0-9_-]{4}[0-9])|sk_(?:live|test)_[A-Za-z0-9]{10,}|glpat-[A-Za-z0-9_-]{10,}|npm_[A-Za-z0-9]{20,}|hf_[A-Za-z0-9]{20,}|AIza[A-Za-z0-9_-]{20,}|(?i:xox.)-[A-Za-z0-9-]{10,})(?-u:\b)",
        )
        .unwrap_or_else(|_| unreachable!("static opaque token pattern must compile"))
    })
}

fn jwt_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // The pattern is a compile-time constant covered by the tests below.
        Regex::new(r"(?-u:\b)eyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}(?-u:\b)")
            .unwrap_or_else(|_| unreachable!("static JWT pattern must compile"))
    })
}

fn aws_access_key_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // The pattern is a compile-time constant covered by the tests below.
        Regex::new(r"(?-u:\b)(?P<prefix>AKIA|ASIA)[A-Z0-9]{16}(?-u:\b)")
            .unwrap_or_else(|_| unreachable!("static AWS access key pattern must compile"))
    })
}

fn alibaba_access_key_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // The pattern is a compile-time constant covered by the tests below.
        Regex::new(r"(?-u:\b)LTAI[A-Za-z0-9]{12,32}(?-u:\b)")
            .unwrap_or_else(|_| unreachable!("static Alibaba access key pattern must compile"))
    })
}

pub fn provider_safe_command_fact_line(block: &CommandBlock) -> String {
    let facts = provider_safe_command_facts(block);
    format!(
        "command_id={id}; command={command}; cwd={cwd}; end_cwd={end_cwd}; status={status}; exit_code={exit_code}; duration_ms={duration_ms}; output_bytes={output_bytes}; output_id={output_id}; output_stability={output_stability}",
        id = facts.id,
        command = facts.command,
        cwd = facts.cwd,
        end_cwd = facts.end_cwd,
        status = facts.status,
        exit_code = facts.exit_code,
        duration_ms = facts.duration_ms,
        output_bytes = facts.output_bytes,
        output_id = facts.output_id,
        output_stability = facts.output_stability,
    )
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_command_redacts_private_key_blocks_before_token_redaction() {
        let command = "printf '%s' '-----BEGIN PRIVATE KEY-----\nsuper-secret-key-material\n-----END PRIVATE KEY-----' --token token-value";

        let redacted = redact_provider_command_text(command);

        assert!(redacted.contains("<redacted private key block>"));
        assert!(redacted.contains("--token <redacted>"));
        assert!(!redacted.contains("super-secret-key-material"));
        assert!(!redacted.contains("token-value"));
    }

    #[test]
    fn provider_command_is_utf8_safely_bounded() {
        let command = format!("tool {} secret={}", "你".repeat(2_000), "x".repeat(8_000));

        let redacted = redact_provider_command_text(&command);

        assert!(redacted.len() <= PROVIDER_COMMAND_MAX_BYTES);
        assert!(redacted.ends_with(" ... <truncated>"));
        assert!(!redacted.contains(&"x".repeat(100)));
    }

    #[test]
    fn redacts_assignments_flags_headers_and_urls() {
        let input = concat!(
            "ALIBABA_CLOUD_ACCESS_KEY_ID=LTAIexampleaccesskey ",
            "OPENAI_API_KEY=sk-example-secret ",
            "curl --password 'hunter2' --access-token=token-value ",
            "--access-key-id flag-id --cookie flag-cookie ",
            "--set-cookie=flag-set-cookie authorization=assignment-auth ",
            "cookie=assignment-cookie set-cookie=assignment-set-cookie ",
            "'https://example.test/?client_secret=query-value&next=ok'\n",
            "https://user:url-password@example.test/path\n",
            r#"{"api_key":"json-value","password":"json-password","cookie":"json-cookie-value"}"#,
            "\nAuthorization: Basic dXNlcjpwYXNz\n",
            "Cookie: session=private; other=value\n",
            "Set-Cookie: csrf=set-cookie-header-value; Path=/\n",
        );

        let (redacted, changed) = redact_sensitive_text(input);

        assert!(changed);
        for secret in [
            "sk-example-secret",
            "LTAIexampleaccesskey",
            "hunter2",
            "token-value",
            "flag-id",
            "flag-cookie",
            "flag-set-cookie",
            "assignment-auth",
            "assignment-cookie",
            "assignment-set-cookie",
            "query-value",
            "url-password",
            "json-value",
            "json-password",
            "json-cookie-value",
            "dXNlcjpwYXNz",
            "session=private",
            "other=value",
            "set-cookie-header-value",
        ] {
            assert!(!redacted.contains(secret), "{redacted}");
        }
        assert!(redacted.contains("next=ok"), "{redacted}");
        assert!(
            redacted.contains("Authorization: Basic <redacted>"),
            "{redacted}"
        );
    }

    #[test]
    fn redacts_complete_quoted_flag_and_assignment_values() {
        let input = concat!(
            "tool --password 'correct horse battery staple' ",
            "--token=\"alpha beta\" password='gamma delta' --region cn"
        );

        let (redacted, changed) = redact_sensitive_text(input);

        assert!(changed);
        assert_eq!(
            redacted,
            "tool --password <redacted> --token=<redacted> password=<redacted> --region cn"
        );
    }

    #[test]
    fn redacts_escaped_quotes_and_whitespace_in_credential_values() {
        let input = concat!(
            r#"tool --password "correct horse\" battery staple" "#,
            r#"--token=alpha\ beta password='gamma\' delta' "#,
            r#"--secret escaped\ space --region cn; "#,
            r#"tool --password "joined secret"tail deploy billing-api; "#,
            "tool --token=line\\\ncontinued-tail deploy reports-api",
            "; tool --password correct,horse|kubectl get pods",
        );

        let (redacted, changed) = redact_sensitive_text(input);

        assert!(changed);
        for secret in [
            "battery staple",
            "gamma\\' delta",
            "escaped\\ space",
            "tail deploy billing-api",
            "continued-tail",
            ",horse",
        ] {
            assert!(!redacted.contains(secret), "{redacted}");
        }
        assert!(redacted.contains("--region cn"), "{redacted}");
        assert!(redacted.contains("billing-api"), "{redacted}");
        assert!(redacted.contains("reports-api"), "{redacted}");
        assert!(redacted.contains("|kubectl get pods"), "{redacted}");

        let (unterminated, changed) =
            redact_sensitive_text("tool --password \"unterminated secret tail");
        assert!(changed);
        assert_eq!(unterminated, "tool --password <redacted>");
    }

    #[test]
    fn redacts_multiple_opaque_token_shapes() {
        let slack_token = ["xoxb", "1234567890", "abcdefghijklmnop"].join("-");
        let marker_wildcard_slack_token = "xoxc-123456789012345";
        let input = format!(
            "{}{slack_token} {marker_wildcard_slack_token} {}",
            concat!(
                "Bearer first.secret.value and bearer second-secret ",
                "ghp_abcdefghijklmnopqrstuvwxyz123456 ",
                "github_pat_abcdefghijklmnopqrstuvwxyz123456 ",
                "sk-abcdefghijklmnopqrstuvwxyz ",
                "AKIA1234567890ABCDEF ",
                "ASIA1234567890ABCDEF ",
                "LTAI5tExampleAccessKey ",
                "glpat-abcdefghijklmnopqrstuvwxyz ",
                "npm_abcdefghijklmnopqrstuvwxyz123456 ",
                "hf_abcdefghijklmnopqrstuvwxyz123456 ",
            ),
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.signature",
        );

        let (redacted, changed) = redact_sensitive_text(&input);

        assert!(changed);
        for secret in [
            "first.secret.value",
            "second-secret",
            "ghp_",
            "github_pat_",
            "sk-abcdefghijklmnopqrstuvwxyz",
            "AKIA1234567890ABCDEF",
            "ASIA1234567890ABCDEF",
            "LTAI5tExampleAccessKey",
            "glpat-",
            "npm_",
            "hf_",
            "xoxb-",
            marker_wildcard_slack_token,
            "eyJhbGciOiJIUzI1NiJ9",
        ] {
            assert!(!redacted.contains(secret), "{redacted}");
        }
        assert_eq!(
            redacted
                .to_ascii_lowercase()
                .matches("bearer <redacted>")
                .count(),
            2
        );
    }

    #[test]
    fn redacts_complete_private_key_blocks() {
        let input = concat!(
            "before\n",
            "-----BEGIN PRIVATE KEY-----\n",
            "private-key-body-must-not-survive\n",
            "-----END PRIVATE KEY-----\n",
            "after\n",
        );

        let (redacted, changed) = redact_sensitive_text(input);

        assert!(changed);
        assert_eq!(redacted, "before\n<redacted private key block>\nafter\n");
    }

    #[test]
    fn leaves_non_secret_text_unchanged() {
        let input = "cargo test --package cosh-shell\n";
        assert_eq!(redact_sensitive_text(input), (input.to_string(), false));
    }
}
