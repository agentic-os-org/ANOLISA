use std::sync::OnceLock;

use regex::Regex;

use crate::evidence::redact_sensitive_text;

use super::personal_model::{RedactionKind, RedactionReport};

const COMMAND_MAX_BYTES: usize = 1024;
const REQUEST_MAX_BYTES: usize = 4 * 1024;
const TRUNCATION_MARKER: &str = "<truncated>";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SanitizedText {
    pub(crate) text: String,
    pub(crate) report: RedactionReport,
}

pub(crate) fn sanitize_shell_command(input: &str) -> Result<SanitizedText, String> {
    sanitize(input, COMMAND_MAX_BYTES)
}

pub(crate) fn sanitize_agent_request(input: &str) -> Result<SanitizedText, String> {
    sanitize(input, REQUEST_MAX_BYTES)
}

pub(crate) fn contains_hard_secret(input: &str) -> bool {
    sanitize(input, input.len().saturating_add(1))
        .map(|sanitized| sanitized.text != input)
        .unwrap_or(true)
}

fn sanitize(input: &str, max_bytes: usize) -> Result<SanitizedText, String> {
    let mut replacements = Vec::new();
    let mut text = input.to_string();

    text = replace_pattern(
        &text,
        credential_header_pattern(),
        "$prefix<authorization>",
        RedactionKind::Authorization,
        &mut replacements,
    );
    text = replace_pattern(
        &text,
        inline_cookie_pattern(),
        "$prefix<authorization>",
        RedactionKind::Authorization,
        &mut replacements,
    );
    text = replace_pattern(
        &text,
        encoded_credential_pattern(),
        "$prefix<credential>",
        RedactionKind::Credential,
        &mut replacements,
    );
    text = replace_pattern(
        &text,
        curl_user_pattern(),
        "$prefix<credential>",
        RedactionKind::Credential,
        &mut replacements,
    );
    text = replace_pattern(
        &text,
        mysql_password_pattern(),
        "$prefix<credential>",
        RedactionKind::Credential,
        &mut replacements,
    );
    text = replace_pattern(
        &text,
        docker_password_pattern(),
        "$prefix<credential>",
        RedactionKind::Credential,
        &mut replacements,
    );

    let (base_redacted, base_changed) = redact_sensitive_text(&text);
    if base_changed {
        push_unique(&mut replacements, classify_base_redaction(&text));
    }
    text = base_redacted;

    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() && text.contains(&home) {
            text = text.replace(&home, "$HOME");
            push_unique(&mut replacements, RedactionKind::HomePath);
        }
    }

    let (text, truncated) = truncate_middle(&text, max_bytes);
    let (rescanned, changed_after_truncation) = redact_sensitive_text(&text);
    if changed_after_truncation {
        push_unique(&mut replacements, RedactionKind::Secret);
    }

    Ok(SanitizedText {
        text: rescanned,
        report: RedactionReport {
            replacements,
            truncated,
            sanitizer_version: 1,
        },
    })
}

fn replace_pattern(
    input: &str,
    pattern: &Regex,
    replacement: &str,
    kind: RedactionKind,
    replacements: &mut Vec<RedactionKind>,
) -> String {
    let output = pattern.replace_all(input, replacement).into_owned();
    if output != input {
        push_unique(replacements, kind);
    }
    output
}

fn classify_base_redaction(input: &str) -> RedactionKind {
    let lowercase = input.to_ascii_lowercase();
    if lowercase.contains("-----begin ") && lowercase.contains("private key-----") {
        RedactionKind::PrivateKey
    } else if lowercase.contains("authorization")
        || lowercase.contains("bearer ")
        || lowercase.contains("cookie:")
    {
        RedactionKind::Authorization
    } else {
        RedactionKind::Secret
    }
}

fn push_unique(values: &mut Vec<RedactionKind>, value: RedactionKind) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn truncate_middle(input: &str, max_bytes: usize) -> (String, bool) {
    if input.len() <= max_bytes {
        return (input.to_string(), false);
    }
    if max_bytes <= TRUNCATION_MARKER.len() {
        return (TRUNCATION_MARKER[..max_bytes].to_string(), true);
    }

    let available = max_bytes - TRUNCATION_MARKER.len();
    let head_budget = available * 3 / 4;
    let tail_budget = available - head_budget;
    let head_end = floor_char_boundary(input, head_budget);
    let tail_start = ceil_char_boundary(input, input.len().saturating_sub(tail_budget));
    let mut output = String::with_capacity(max_bytes);
    output.push_str(&input[..head_end]);
    output.push_str(TRUNCATION_MARKER);
    output.push_str(&input[tail_start..]);
    (output, true)
}

fn floor_char_boundary(input: &str, mut index: usize) -> usize {
    index = index.min(input.len());
    while !input.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(input: &str, mut index: usize) -> usize {
    index = index.min(input.len());
    while index < input.len() && !input.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn credential_header_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r#"(?i)(?P<prefix>\b(?:x-api-key|proxy-authorization)\s*:\s*)[^\s'";]+"#)
            .expect("static credential header regex")
    })
}

fn inline_cookie_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r#"(?i)(?P<prefix>\b(?:set-cookie|cookie)\s*:\s*)[^\r\n'\"]+"#)
            .expect("static cookie header regex")
    })
}

fn encoded_credential_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r#"(?i)(?P<prefix>\b(?:password|passwd|token|access[_-]?token|refresh[_-]?token|client[_-]?secret|api[_-]?key|access[_-]?key[_-]?secret)%3d)[^\s'\"]+"#,
        )
        .expect("static encoded credential regex")
    })
}

fn curl_user_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r#"(?i)(?P<prefix>\bcurl\b[^\n;]*?\s(?:-u|--user)(?:=|\s+))(?:"(?:\\(?:\r?\n|[^\r\n])|[^"\\])*(?:"|$)|'[^']*(?:'|$)|\\(?:\r?\n|[^\r\n])|[^\s;&|()<>"'\\])+"#,
        )
            .expect("static curl credential regex")
    })
}

fn mysql_password_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r#"(?i)(?P<prefix>\b(?:mysql|mariadb)\b[^\n;]*?\s-p)(?:"(?:\\(?:\r?\n|[^\r\n])|[^"\\])*(?:"|$)|'[^']*(?:'|$)|\\(?:\r?\n|[^\r\n])|[^\s;&|()<>"'\\])+"#,
        )
        .expect("static mysql credential regex")
    })
}

fn docker_password_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r#"(?i)(?P<prefix>\bdocker\s+login\b[^\n;]*?\s(?:-p|--password)(?:=|\s+))(?:"(?:\\(?:\r?\n|[^\r\n])|[^"\\])*(?:"|$)|'[^']*(?:'|$)|\\(?:\r?\n|[^\r\n])|[^\s;&|()<>"'\\])+"#,
        )
            .expect("static docker credential regex")
    })
}
