//! Sensitive-field redaction for audit log entries.
//!
//! Redaction happens at log-write time, not at PEP→PDP boundary — see
//! `docs/audit-design.md` §8.4. The PDP may legitimately need to see raw
//! values (e.g. "deny if api_key is empty"), but those values must not
//! land in the on-disk JSONL log.
//!
//! # Completeness contract
//!
//! When `redact_action` reports a change (so `LogEntry.redacted = true`),
//! **every** serialized field of the action must be consistently scrubbed —
//! `operation`, a free-string `subsystem` (`ActionSubsystem::Other`), `target`,
//! `raw`, and both halves of each `args` pair. An earlier implementation
//! only rewrote the `args` value when its key named a secret, leaving the
//! same secret in plaintext inside `raw`/`target` (GitHub #1618). Detection is
//! now pattern-based (Alibaba `LTAI…` / AWS `AKIA…` access keys, `eyJ…` JWT /
//! bearer tokens, PEM private-key blocks, `?token=`/`--password=` style
//! key/value secrets) rather than a length or context heuristic, and is
//! applied uniformly to every field.
//!
//! # Ownership
//!
//! `cosh-platform::audit` owns this write-boundary scanner. Split the token
//! state machine from the pattern catalog before adding another detector
//! family, so security semantics remain reviewable as the module grows.

use std::sync::OnceLock;

use cosh_types::audit::{Action, ActionSubsystem};
use regex::Regex;

/// Argument-key needles whose *value* is always a secret regardless of the
/// value's shape. Matched against the *normalized* key (separators dropped,
/// lowercased — see `normalize_key`), so a single needle covers every
/// separator/case variant: `apikey` matches `api_key`/`api-key`/`API_KEY`,
/// `accesskey` matches `access_key`/`AccessKeyId`/`access-key-secret`, etc.
/// Used both for structured `--arg-key/--arg-value` input and to recognize a
/// `--flag value` whose secret rides in the adjacent positional token.
const SENSITIVE_KEY_NEEDLES: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "apikey",
    "accesskey",
    "credential",
    "privatekey",
    "authorization",
    "bearer",
    "cookie",
];

const REDACTED_VALUE: &str = "<redacted>";
const REDACTED_PEM: &str = "<redacted-pem>";

/// Redact sensitive content in `action`. Returns true if any change was made
/// — caller should set `LogEntry.redacted = true` accordingly.
///
/// Every string-bearing field is scrubbed with the same secret-pattern scanner
/// so a secret can never survive in one field while being masked in another:
/// `operation`, a free-string `subsystem` (`ActionSubsystem::Other`), `target`,
/// `raw`, and both halves of each `args` pair.
pub fn redact_action(action: &mut Action) -> bool {
    // Action fields do not retain enough ordering information to reconstruct a
    // PEM span after parsing. Once any field carries private-key armor, redact
    // every user-controlled field rather than guessing which fragments belong
    // to the body.
    if action_has_pem(action) {
        return redact_pem_action(action);
    }

    let mut changed = false;

    // Preserve flag/value relationships before per-field scrubbing destroys
    // token context. This also covers quoted values split across several args.
    let contextual_redaction = redact_secret_value_tokens(action);
    changed |= contextual_redaction;

    for (k, v) in action.args.iter_mut() {
        if is_sensitive_key(k) && !v.is_empty() && v != REDACTED_VALUE {
            *v = REDACTED_VALUE.to_string();
            changed = true;
        }
        changed |= scrub_action_field(k);
        changed |= scrub_action_field(v);
    }

    changed |= scrub_action_field(&mut action.operation);
    if let ActionSubsystem::Other(name) = &mut action.subsystem {
        changed |= scrub_action_field(name);
    }
    if let Some(target) = action.target.as_mut() {
        changed |= scrub_action_field(target);
    }
    if let Some(raw) = action.raw.as_mut() {
        // The raw command cannot be mapped reliably back to mutated Action
        // tokens. Once contextual scanning finds a secret, fail closed rather
        // than risk leaving a spelling variant that the regex scanner misses.
        changed |= if contextual_redaction {
            replace_nonempty(raw, REDACTED_VALUE)
        } else {
            scrub_action_field(raw)
        };
    }

    changed
}

/// Redact positional tokens that carry values for sensitive flags or continue
/// a quoted sensitive assignment.
fn redact_secret_value_tokens(action: &mut Action) -> bool {
    let mut redact = vec![false; action.args.len()];
    let mut redact_operation = false;
    let mut redact_target = false;
    let duplicated_target = matches!(action.subsystem, ActionSubsystem::Shell)
        && action.target.as_ref().is_some_and(|target| {
            action
                .args
                .first()
                .is_some_and(|(key, value)| key == target && value.is_empty())
        });

    let mut positions =
        Vec::with_capacity(action.args.len() + usize::from(action.target.is_some()) + 1);
    positions.push((ActionToken::Operation, action.operation.as_str()));
    if let Some(target) = &action.target {
        positions.push((ActionToken::Target, target.as_str()));
    }
    for (index, (token, _)) in action.args.iter().enumerate() {
        if duplicated_target && index == 0 {
            continue;
        }
        positions.push((ActionToken::Arg(index), token.as_str()));
    }

    let mut context = None;
    let mut position = 0;
    while position < positions.len() {
        let (_, token) = positions[position];
        match context {
            Some(SecretContext::Authorization) if is_authorization_scheme(token) => {
                context = Some(SecretContext::Value);
            }
            Some(_) => {
                if let Some(next_context) = explicit_secret_context(token) {
                    // An incomplete marker must not consume a later marker and
                    // expose the value that belongs to the latter.
                    context = Some(next_context);
                    position += 1;
                    continue;
                }
                let end = mark_action_value_span(
                    &positions,
                    position,
                    token,
                    &mut redact_operation,
                    &mut redact_target,
                    &mut redact,
                );
                context = None;
                position = end;
            }
            None => {
                context = secret_context(token);
                if context.is_none()
                    && positions
                        .get(position + 1)
                        .is_some_and(|(_, separator)| is_assignment_separator(separator))
                {
                    // Whitespace tokenization can separate `password: value`
                    // into three tokens. Consume the separator while keeping
                    // the sensitive context for the following value.
                    context = split_assignment_context(token);
                    if context.is_some() {
                        position += 1;
                    }
                } else if context.is_none() {
                    if let Some(value) = sensitive_inline_value(token) {
                        let end = mark_action_value_span(
                            &positions,
                            position,
                            value,
                            &mut redact_operation,
                            &mut redact_target,
                            &mut redact,
                        );
                        position = end;
                    }
                }
            }
        }
        position += 1;
    }

    let mut changed = false;
    if redact_operation {
        changed |= replace_nonempty(&mut action.operation, REDACTED_VALUE);
    }
    if redact_target {
        if let Some(target) = action.target.as_mut() {
            changed |= replace_nonempty(target, REDACTED_VALUE);
        }
        if duplicated_target {
            redact[0] = true;
        }
    }

    for ((key, value), should_redact) in action.args.iter_mut().zip(redact) {
        if should_redact {
            changed |= replace_nonempty(key, REDACTED_VALUE);
            changed |= replace_nonempty(value, REDACTED_VALUE);
        }
    }
    changed
}

#[derive(Clone, Copy)]
enum ActionToken {
    Operation,
    Target,
    Arg(usize),
}

#[derive(Clone, Copy)]
enum SecretContext {
    Value,
    Authorization,
}

/// Return the context introduced by a token whose secret is in a later token.
fn secret_context(token: &str) -> Option<SecretContext> {
    if let Some(context) = explicit_secret_context(token) {
        return Some(context);
    }

    let trimmed = token.trim_matches(['"', '\'']);
    if normalize_key(trimmed) == "bearer" {
        return Some(SecretContext::Value);
    }
    let (key, value) = trimmed
        .split_once('=')
        .or_else(|| trimmed.split_once(':'))?;
    if !is_sensitive_key(key) {
        return None;
    }

    if normalize_key(key) == "authorization" {
        if value.is_empty() || is_authorization_scheme(value) {
            Some(SecretContext::Authorization)
        } else {
            None
        }
    } else if value.is_empty() {
        Some(SecretContext::Value)
    } else {
        None
    }
}

/// Return the context introduced by an explicit flag or empty assignment.
fn explicit_secret_context(token: &str) -> Option<SecretContext> {
    if is_sensitive_value_flag(token) {
        let key = token.trim_start_matches('-');
        return Some(if normalize_key(key) == "authorization" {
            SecretContext::Authorization
        } else {
            SecretContext::Value
        });
    }

    let trimmed = token.trim_matches(['"', '\'']);
    let (key, value) = trimmed
        .split_once('=')
        .or_else(|| trimmed.split_once(':'))?;
    if !value.is_empty() || !is_sensitive_key(key) {
        return None;
    }
    Some(if normalize_key(key) == "authorization" {
        SecretContext::Authorization
    } else {
        SecretContext::Value
    })
}

/// Return whether a standalone token is an assignment separator.
fn is_assignment_separator(token: &str) -> bool {
    matches!(token.trim_matches(['"', '\'']), ":" | "=")
}

/// Return the context for a sensitive key whose assignment separator is a
/// separate token.
fn split_assignment_context(token: &str) -> Option<SecretContext> {
    let key = token.trim_matches(['"', '\'']);
    if !is_sensitive_key(key) {
        return None;
    }
    if normalize_key(key) == "authorization" {
        Some(SecretContext::Authorization)
    } else {
        Some(SecretContext::Value)
    }
}

/// Authentication schemes may sit between `Authorization:` and its value.
fn is_authorization_scheme(token: &str) -> bool {
    matches!(
        token
            .trim_matches(['"', '\''])
            .to_ascii_lowercase()
            .as_str(),
        "bearer" | "basic" | "token"
    )
}

/// Mark one semantic value token and any quote- or escape-continued tokens.
fn mark_action_value_span(
    positions: &[(ActionToken, &str)],
    start: usize,
    first_fragment: &str,
    redact_operation: &mut bool,
    redact_target: &mut bool,
    redact_args: &mut [bool],
) -> usize {
    let mark = |location: ActionToken,
                redact_operation: &mut bool,
                redact_target: &mut bool,
                redact_args: &mut [bool]| {
        match location {
            ActionToken::Operation => *redact_operation = true,
            ActionToken::Target => *redact_target = true,
            ActionToken::Arg(index) => redact_args[index] = true,
        }
    };

    let (location, _) = positions[start];
    mark(location, redact_operation, redact_target, redact_args);

    let mut end = start;
    if let Some(quote) = first_fragment
        .chars()
        .next()
        .filter(|c| matches!(c, '"' | '\''))
    {
        if has_unescaped_quote(&first_fragment[quote.len_utf8()..], quote) {
            return end;
        }
        for (index, (location, token)) in positions.iter().enumerate().skip(start + 1) {
            mark(*location, redact_operation, redact_target, redact_args);
            end = index;
            if has_unescaped_quote(token, quote) {
                break;
            }
        }
    } else if has_trailing_escape(first_fragment) {
        for (index, (location, token)) in positions.iter().enumerate().skip(start + 1) {
            mark(*location, redact_operation, redact_target, redact_args);
            end = index;
            if !has_trailing_escape(token) {
                break;
            }
        }
    }
    end
}

/// Replace a non-empty field unless it already contains the placeholder.
fn replace_nonempty(value: &mut String, replacement: &str) -> bool {
    if value.is_empty() || value == replacement {
        return false;
    }
    value.clear();
    value.push_str(replacement);
    true
}

/// Return the value fragment from a sensitive inline `key=value` token.
fn sensitive_inline_value(token: &str) -> Option<&str> {
    for (separator, ch) in token
        .char_indices()
        .filter(|(_, ch)| matches!(ch, '=' | ':'))
    {
        let prefix = &token[..separator];
        let key_start = prefix
            .char_indices()
            .rev()
            .find(|(_, ch)| matches!(ch, '?' | '&' | ';' | ','))
            .map_or(0, |(index, ch)| index + ch.len_utf8());
        let key = prefix[key_start..].trim_matches(['"', '\'']);
        if is_sensitive_key(key) {
            return Some(&token[separator + ch.len_utf8()..]);
        }
    }
    None
}

/// True when `text` contains `quote` that is not escaped by a backslash.
fn has_unescaped_quote(text: &str, quote: char) -> bool {
    let mut escaped = false;
    for ch in text.chars() {
        if ch == quote && !escaped {
            return true;
        }
        if ch == '\\' {
            escaped = !escaped;
        } else {
            escaped = false;
        }
    }
    false
}

/// True when a token ends with an odd number of backslashes.
fn has_trailing_escape(token: &str) -> bool {
    token.chars().rev().take_while(|c| *c == '\\').count() % 2 == 1
}

/// True when the action carries a PEM private-key header in any field.
fn action_has_pem(action: &Action) -> bool {
    let mut joined = String::new();
    let mut append = |field: &str| {
        if !field.is_empty() {
            if !joined.is_empty() {
                joined.push(' ');
            }
            joined.push_str(field);
        }
    };

    if let ActionSubsystem::Other(name) = &action.subsystem {
        append(name);
    }
    append(&action.operation);
    if let Some(target) = &action.target {
        append(target);
    }
    for (key, value) in &action.args {
        append(key);
        append(value);
    }
    if let Some(raw) = &action.raw {
        append(raw);
    }

    contains_pem_header(&joined)
}

/// Redact all user-controlled fields in an action carrying private-key armor.
fn redact_pem_action(action: &mut Action) -> bool {
    let mut changed = replace_nonempty(&mut action.operation, REDACTED_PEM);
    if let ActionSubsystem::Other(name) = &mut action.subsystem {
        changed |= replace_nonempty(name, REDACTED_PEM);
    }
    if let Some(target) = action.target.as_mut() {
        changed |= replace_nonempty(target, REDACTED_PEM);
    }
    for (key, value) in &mut action.args {
        changed |= replace_nonempty(key, REDACTED_PEM);
        changed |= replace_nonempty(value, REDACTED_PEM);
    }
    if let Some(raw) = action.raw.as_mut() {
        changed |= replace_nonempty(raw, REDACTED_PEM);
    }
    changed
}

/// True when a string contains a private-key PEM begin marker.
fn contains_pem_header(text: &str) -> bool {
    let mut normalized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' && chars.peek().is_some_and(|next| next.is_whitespace()) {
            continue;
        }
        normalized.push(ch);
    }
    marker_range(&normalized, "-----BEGIN ").is_some()
}

/// Normalize a key for needle matching: drop every non-alphanumeric byte and
/// lowercase the rest, so `access_key`, `access-key`, and `AccessKey` all
/// collapse to `accesskey`.
fn normalize_key(key: &str) -> String {
    let bytes = key.as_bytes();
    let mut normalized = String::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = if bytes[index] == b'%' && index + 2 < bytes.len() {
            match (hex_value(bytes[index + 1]), hex_value(bytes[index + 2])) {
                (Some(high), Some(low)) => {
                    index += 3;
                    high * 16 + low
                }
                _ => {
                    index += 1;
                    b'%'
                }
            }
        } else {
            let byte = bytes[index];
            index += 1;
            byte
        };
        if byte.is_ascii_alphanumeric() {
            normalized.push(byte.to_ascii_lowercase() as char);
        }
    }
    normalized
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// True when a key names a secret (see `SENSITIVE_KEY_NEEDLES`).
fn is_sensitive_key(key: &str) -> bool {
    let normalized = normalize_key(key);
    SENSITIVE_KEY_NEEDLES
        .iter()
        .any(|needle| normalized.contains(needle))
}

/// True when `token` is a `--flag` naming a secret and carrying no inline
/// `=value` — i.e. its value is the following positional token. `-`/`_`
/// separators and case are normalized (`--api-key`, `--access_key`, …).
fn is_sensitive_value_flag(token: &str) -> bool {
    let stripped = token.trim_start_matches('-');
    stripped.len() < token.len() && !stripped.contains('=') && is_sensitive_key(stripped)
}

/// Scrub one serialized field, falling back to whole-field redaction when its
/// normalized assignment key is broader than the display-oriented regexes.
fn scrub_action_field(field: &mut String) -> bool {
    if contains_contextual_secret(field) {
        replace_nonempty(field, REDACTED_VALUE)
    } else {
        scrub(field)
    }
}

/// Detect a semantic secret relationship within one serialized field.
fn contains_contextual_secret(field: &str) -> bool {
    let tokens: Vec<&str> = field.split_whitespace().collect();
    let mut context = None;
    let mut position = 0;
    while position < tokens.len() {
        let token = tokens[position];
        match context {
            Some(SecretContext::Authorization) if is_authorization_scheme(token) => {
                context = Some(SecretContext::Value);
            }
            Some(_) => {
                if let Some(next_context) = explicit_secret_context(token) {
                    context = Some(next_context);
                } else {
                    return true;
                }
            }
            None => {
                context = secret_context(token);
                if context.is_none()
                    && tokens
                        .get(position + 1)
                        .is_some_and(|separator| is_assignment_separator(separator))
                {
                    context = split_assignment_context(token);
                    if context.is_some() {
                        position += 1;
                    }
                } else if context.is_none() && sensitive_inline_value(token).is_some() {
                    return true;
                }
            }
        }
        position += 1;
    }
    false
}

/// Scrub non-PEM secrets from a single string in place.
fn scrub(s: &mut String) -> bool {
    if s.is_empty() {
        return false;
    }

    let mut out = s.clone();
    for (pattern, replacement) in pattern_replacements() {
        out = pattern.replace_all(&out, *replacement).into_owned();
    }

    if &out != s {
        *s = out;
        true
    } else {
        false
    }
}

/// Locate a `<marker>…PRIVATE KEY-----` span in `line`, returning its byte
/// range. Case-insensitive on the marker text.
fn marker_range(line: &str, marker: &str) -> Option<(usize, usize)> {
    let upper = line.to_ascii_uppercase();
    let start = upper.find(marker)?;
    let key_end = upper[start..].find("PRIVATE KEY-----")?;
    Some((start, start + key_end + "PRIVATE KEY-----".len()))
}

/// Ordered (pattern, replacement) list applied to every scrubbed string.
/// The set mirrors the central provider/session redactor in
/// `cosh-core::redaction`, extended so bare URL-query / CLI-flag secret keys
/// (`access_key`, `token`, `secret`, `password`, …) are covered.
fn pattern_replacements() -> &'static [(Regex, &'static str)] {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            (cookie_header_pattern(), "${prefix}<redacted>"),
            (authorization_pattern(), "${prefix}${scheme} <redacted>"),
            (bearer_pattern(), "${prefix}<redacted>"),
            (url_password_pattern(), "${prefix}<redacted>@"),
            (sensitive_flag_pattern(), "${prefix}<redacted>"),
            (sensitive_assignment_pattern(), "${prefix}<redacted>"),
            (github_token_pattern(), "<redacted>"),
            (opaque_token_pattern(), "<redacted>"),
            (jwt_pattern(), "<redacted>"),
            (aws_access_key_pattern(), "${prefix}<redacted>"),
            (alibaba_access_key_pattern(), "<redacted>"),
        ]
    })
}

fn cookie_header_pattern() -> Regex {
    Regex::new(r"(?i)(?P<prefix>\b(?:set-cookie|cookie)\s*:\s*)[^\r\n]*")
        .unwrap_or_else(|_| unreachable!("static cookie pattern must compile"))
}

fn authorization_pattern() -> Regex {
    Regex::new(
        r"(?i)(?P<prefix>\bauthorization\s*(?::|=)\s*)(?P<scheme>bearer|basic|token)?\s*(?P<value>[^\s,;&]+)",
    )
    .unwrap_or_else(|_| unreachable!("static authorization pattern must compile"))
}

fn bearer_pattern() -> Regex {
    Regex::new(r"(?i)(?P<prefix>\bbearer\s+)[A-Za-z0-9._~+/=-]+")
        .unwrap_or_else(|_| unreachable!("static bearer pattern must compile"))
}

fn url_password_pattern() -> Regex {
    Regex::new(r"(?i)(?P<prefix>\b[a-z][a-z0-9+.-]*://[^/\s:@]+:)[^@/\s]+@")
        .unwrap_or_else(|_| unreachable!("static URL password pattern must compile"))
}

/// `--password value` / `--token=value` style CLI flags.
fn sensitive_flag_pattern() -> Regex {
    Regex::new(
        r#"(?ix)
        (?P<prefix>
            (?:^|\s)
            --(?:password|passwd|passphrase|token|access[_-]?token|refresh[_-]?token|
                 id[_-]?token|secret|client[_-]?secret|api[_-]?key|apikey|
                 access[_-]?key[_-]?secret|access[_-]?key[_-]?id|access[_-]?key|
                 secret[_-]?key|security[_-]?token|authorization|bearer|credential)
            (?:=|\s+)
        )
        (?:
            "(?:\\[^\r\n]|[^"\\])*(?:"|$)|
            '[^']*(?:'|$)|
            \\[^\r\n]|
            [^\s;&|()<>"'\\]
        )+
        "#,
    )
    .unwrap_or_else(|_| unreachable!("static sensitive flag pattern must compile"))
}

/// `key=value` / `key:value` assignments, including URL query parameters
/// (`?access_key=…`, `&token=…`) and env-style bindings.
fn sensitive_assignment_pattern() -> Regex {
    Regex::new(
        r#"(?ix)
        (?P<prefix>
            ["']?
            (?:alibaba[_-]?cloud[_-]?access[_-]?key[_-]?id|
               aws[_-]?access[_-]?key[_-]?id|
               aws[_-]?secret[_-]?access[_-]?key|
               access[_-]?key[_-]?secret|access[_-]?key[_-]?id|access[_-]?key|
               secret[_-]?access[_-]?key|secret[_-]?key|
               dashscope[_-]?api[_-]?key|openai[_-]?api[_-]?key|
               client[_-]?secret|security[_-]?token|session[_-]?token|
               refresh[_-]?token|access[_-]?token|auth[_-]?token|
               github[_-]?token|id[_-]?token|private[_-]?key|
               api[_-]?key|apikey|credentials?|authorization|
               password|passphrase|passwd|bearer|secret|token)
            ["']?
            \s*(?:=|:)\s*
        )
        (?:
            "(?:\\[^\r\n]|[^"\\])*(?:"|$)|
            '[^']*(?:'|$)|
            \\[^\r\n]|
            [^\s;&|()<>"'\\]
        )+
        "#,
    )
    .unwrap_or_else(|_| unreachable!("static sensitive assignment pattern must compile"))
}

fn github_token_pattern() -> Regex {
    Regex::new(r"\b(?:gh[pousr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{20,})\b")
        .unwrap_or_else(|_| unreachable!("static GitHub token pattern must compile"))
}

fn opaque_token_pattern() -> Regex {
    Regex::new(
        r"\b(?:sk-[A-Za-z0-9_-]{10,}|sk_(?:live|test)_[A-Za-z0-9]{10,}|glpat-[A-Za-z0-9_-]{10,}|npm_[A-Za-z0-9]{20,}|hf_[A-Za-z0-9]{20,}|AIza[A-Za-z0-9_-]{20,}|xox[baprs]-[A-Za-z0-9-]{10,})\b",
    )
    .unwrap_or_else(|_| unreachable!("static opaque token pattern must compile"))
}

fn jwt_pattern() -> Regex {
    Regex::new(r"\beyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\b")
        .unwrap_or_else(|_| unreachable!("static JWT pattern must compile"))
}

fn aws_access_key_pattern() -> Regex {
    Regex::new(r"(?P<prefix>\b)(?:AKIA|ASIA)[A-Z0-9]{16}\b")
        .unwrap_or_else(|_| unreachable!("static AWS access key pattern must compile"))
}

fn alibaba_access_key_pattern() -> Regex {
    Regex::new(r"\bLTAI[A-Za-z0-9]{12,32}\b")
        .unwrap_or_else(|_| unreachable!("static Alibaba access key pattern must compile"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::parse_action_string;
    use cosh_types::audit::ActionSubsystem;

    fn pkg_action_with_args(args: Vec<(&str, &str)>) -> Action {
        Action {
            subsystem: ActionSubsystem::Pkg,
            operation: "install".to_string(),
            target: Some("nginx".to_string()),
            args: args
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            raw: None,
        }
    }

    /// Assert `needle` appears in no string-bearing field of `action`.
    fn assert_no_leak(action: &Action, needle: &str) {
        assert!(
            !action.subsystem.as_str().contains(needle),
            "leaked in subsystem: {}",
            action.subsystem.as_str()
        );
        assert!(
            !action.operation.contains(needle),
            "leaked in operation: {}",
            action.operation
        );
        if let Some(raw) = &action.raw {
            assert!(!raw.contains(needle), "leaked in raw: {raw}");
        }
        if let Some(target) = &action.target {
            assert!(!target.contains(needle), "leaked in target: {target}");
        }
        for (k, v) in &action.args {
            assert!(!k.contains(needle), "leaked in arg key: {k}");
            assert!(!v.contains(needle), "leaked in arg value: {v}");
        }
    }

    // -- existing behavior (arg key needle) ---------------------------------

    #[test]
    fn redacts_password_key() {
        let mut a = pkg_action_with_args(vec![("password", "hunter2")]);
        assert!(redact_action(&mut a));
        assert_eq!(a.args[0].1, "<redacted>");
    }

    #[test]
    fn redacts_api_key_case_insensitive() {
        let mut a = pkg_action_with_args(vec![("API_KEY", "abcdef")]);
        assert!(redact_action(&mut a));
        assert_eq!(a.args[0].1, "<redacted>");

        let mut a = pkg_action_with_args(vec![("apikey", "xyz")]);
        assert!(redact_action(&mut a));
        assert_eq!(a.args[0].1, "<redacted>");
    }

    #[test]
    fn redacts_token_substring() {
        let mut a = pkg_action_with_args(vec![("auth_token", "bearer-xyz")]);
        assert!(redact_action(&mut a));
        assert_eq!(a.args[0].1, "<redacted>");
    }

    #[test]
    fn does_not_change_unrelated_keys() {
        let mut a = pkg_action_with_args(vec![("name", "nginx"), ("version", "1.25")]);
        assert!(!redact_action(&mut a));
        assert_eq!(a.args[0].1, "nginx");
        assert_eq!(a.args[1].1, "1.25");
    }

    #[test]
    fn idempotent_when_already_redacted() {
        let mut a = pkg_action_with_args(vec![("password", "<redacted>")]);
        assert!(!redact_action(&mut a));
    }

    // -- completeness across raw / target / args (GitHub #1618) -------------

    #[test]
    fn url_query_access_key_scrubbed_in_all_fields() {
        // Long AK — matches the Alibaba access-key pattern *and* the query
        // param assignment.
        let mut a = parse_action_string("curl http://x.com/?access_key=LTAI4SECRETKEY123").unwrap();
        assert!(redact_action(&mut a));
        assert_no_leak(&a, "LTAI4SECRETKEY123");
        assert_eq!(a.operation, "curl");
    }

    #[test]
    fn url_query_short_secret_scrubbed_by_key() {
        // Value too short for the AK pattern; caught by the query-param key.
        let mut a =
            parse_action_string("curl http://example.com/?access_key=LTAI4ABCDEFG").unwrap();
        assert!(redact_action(&mut a));
        assert_no_leak(&a, "LTAI4ABCDEFG");
    }

    #[test]
    fn cli_literal_access_key_value_scrubbed() {
        let mut a = parse_action_string("echo AK=LTAI4GHIJKLMNOPQRSTUVWXYZ").unwrap();
        assert!(redact_action(&mut a));
        assert_no_leak(&a, "LTAI4GHIJKLMNOPQRSTUVWXYZ");
    }

    #[test]
    fn bare_alibaba_access_key_prefix_variants_are_scrubbed() {
        for access_key in ["LTAI5tExampleAccessKey", "LTAI6tExampleAccessKey"] {
            let mut action = parse_action_string(&format!("echo {access_key}")).unwrap();

            assert!(redact_action(&mut action), "no redaction for {access_key}");
            assert_no_leak(&action, access_key);
        }
    }

    #[test]
    fn bearer_assignment_scrubbed() {
        let mut a = parse_action_string("echo bearer=eyJABCDEFGHIJK").unwrap();
        assert!(redact_action(&mut a));
        assert_no_leak(&a, "eyJABCDEFGHIJK");
    }

    #[test]
    fn jwt_token_scrubbed() {
        let mut a =
            parse_action_string("echo eyJhbGciOiJIUzI1.eyJzdWIiOiIxMjM.SflKxwRJSMeK").unwrap();
        assert!(redact_action(&mut a));
        assert_no_leak(&a, "eyJhbGciOiJIUzI1");
        assert_no_leak(&a, "SflKxwRJSMeK");
    }

    #[test]
    fn cli_flag_secrets_scrubbed() {
        for (cmd, secret) in [
            ("echo --password=hunter2supersecret", "hunter2supersecret"),
            ("echo --token=abctokenvalue123", "abctokenvalue123"),
            ("echo --secret=topsecretvalue", "topsecretvalue"),
            ("echo --api-key=myapikeyvalue", "myapikeyvalue"),
        ] {
            let mut a = parse_action_string(cmd).unwrap();
            assert!(redact_action(&mut a), "no redaction for {cmd}");
            assert_no_leak(&a, secret);
        }
    }

    #[test]
    fn cli_flag_space_separated_secret_scrubbed() {
        // `--flag value` parses the secret into the positional token *after*
        // the flag; per-field scrubbing alone cannot see the association, so
        // the Action-level flag→value pass must catch it (Codex review PoC #1).
        for (cmd, secret) in [
            ("echo --password hunter2supersecret", "hunter2supersecret"),
            ("echo --token abctokenvalue123", "abctokenvalue123"),
            ("echo --secret topsecretvalue", "topsecretvalue"),
            ("echo --api-key myapikeyvalue", "myapikeyvalue"),
            ("echo --access-key myaccesskeyvalue", "myaccesskeyvalue"),
        ] {
            let mut a = parse_action_string(cmd).unwrap();
            assert!(redact_action(&mut a), "no redaction for {cmd}");
            assert_no_leak(&a, secret);
        }
    }

    #[test]
    fn structured_secret_arg_scrubbed_by_normalized_key() {
        // `--arg-key access_key --arg-value <secret>` builds a single
        // `(key, value)` pair; the normalized key needle must recognize the
        // separator/case variants `access_key` / `api-key` (Codex review PoC #1).
        for key in ["access_key", "api-key", "AccessKeyId", "client-secret"] {
            let mut a = Action {
                subsystem: ActionSubsystem::Shell,
                operation: "echo".to_string(),
                target: None,
                args: vec![(key.to_string(), "hunter2supersecret".to_string())],
                raw: None,
            };
            assert!(redact_action(&mut a), "no redaction for key {key}");
            assert_eq!(a.args[0].1, "<redacted>", "key {key}");
            assert_no_leak(&a, "hunter2supersecret");
        }
    }

    #[test]
    fn operation_secret_scrubbed() {
        // `operation` is serialized into the log too (Codex review #1).
        let mut a = Action {
            subsystem: ActionSubsystem::Shell,
            operation: "LTAI4OPERATIONSECRET123".to_string(),
            target: None,
            args: vec![],
            raw: None,
        };
        assert!(redact_action(&mut a));
        assert_eq!(a.operation, "<redacted>");
    }

    #[test]
    fn free_string_subsystem_secret_scrubbed() {
        // `ActionSubsystem::Other(String)` round-trips a caller-supplied string.
        let mut a = Action {
            subsystem: ActionSubsystem::from_token("LTAI4SUBSYSTEMSECRET99"),
            operation: "status".to_string(),
            target: None,
            args: vec![],
            raw: None,
        };
        assert!(matches!(a.subsystem, ActionSubsystem::Other(_)));
        assert!(redact_action(&mut a));
        assert_eq!(a.subsystem.as_str(), "<redacted>");
    }

    #[test]
    fn structured_subsystem_flag_value_across_target_boundary_scrubbed() {
        // pkg/svc/checkpoint/cosh put the 3rd token in `target` and the rest in
        // `args`, so `--flag value` straddles the target→args[0] boundary
        // (Codex review #2).
        let mut a = parse_action_string("pkg install --password hunter2structuredsecret").unwrap();
        assert_eq!(a.target.as_deref(), Some("--password"));
        assert!(redact_action(&mut a));
        assert_no_leak(&a, "hunter2structuredsecret");
    }

    #[test]
    fn sensitive_operation_redacts_target() {
        let secret = "hunter2OPERATIONTARGET";
        let mut a = parse_action_string(&format!("pkg --password {secret}")).unwrap();
        assert_eq!(a.operation, "--password");
        assert_eq!(a.target.as_deref(), Some(secret));

        assert!(redact_action(&mut a));
        assert_eq!(a.target.as_deref(), Some(REDACTED_VALUE));
        assert_no_leak(&a, secret);
    }

    #[test]
    fn split_secret_context_redacts_following_positional_value() {
        for (cmd, secret) in [
            (
                "echo Authorization: Bearer eyJAUTHHEADERSECRET",
                "eyJAUTHHEADERSECRET",
            ),
            (
                "echo password: hunter2SPLITASSIGNMENT",
                "hunter2SPLITASSIGNMENT",
            ),
            ("echo Bearer eyJBARETOKEN", "eyJBARETOKEN"),
            ("echo Cookie: session=COOKIEVALUE", "session=COOKIEVALUE"),
        ] {
            let mut a = parse_action_string(cmd).unwrap();

            assert!(redact_action(&mut a), "no redaction for {cmd}");
            assert_no_leak(&a, secret);
            assert!(
                a.target.as_deref() == Some(REDACTED_VALUE)
                    || a.args
                        .iter()
                        .any(|(key, value)| { key == REDACTED_VALUE || value == REDACTED_VALUE }),
                "context redaction did not reach target/args for {cmd}: {a:?}"
            );
        }
    }

    #[test]
    fn split_assignment_separator_propagates_sensitive_context() {
        for (cmd, secret) in [
            (
                "echo password : hunter2SPACEDSEPARATOR",
                "hunter2SPACEDSEPARATOR",
            ),
            (
                "pkg password : hunter2OPERATIONSEPARATOR",
                "hunter2OPERATIONSEPARATOR",
            ),
            (
                "echo Authorization = Bearer eyJSPACEDAUTHVALUE",
                "eyJSPACEDAUTHVALUE",
            ),
            (
                "echo Authorization:Bearer eyJINLINEAUTHVALUE",
                "eyJINLINEAUTHVALUE",
            ),
        ] {
            let mut a = parse_action_string(cmd).unwrap();
            assert!(redact_action(&mut a), "no redaction for {cmd}");
            assert_no_leak(&a, secret);
        }
    }

    #[test]
    fn sensitive_target_scrubs_both_halves_of_structured_arg_pair() {
        let mut a = Action {
            subsystem: ActionSubsystem::Shell,
            operation: "echo".to_string(),
            target: Some("--password".to_string()),
            args: vec![("value".to_string(), "hunter2CROSSFIELD".to_string())],
            raw: None,
        };

        assert!(redact_action(&mut a));
        assert_eq!(
            a.args[0],
            (REDACTED_VALUE.to_string(), REDACTED_VALUE.to_string())
        );
        assert_no_leak(&a, "hunter2CROSSFIELD");
    }

    #[test]
    fn structured_subsystem_flag_value_various_forms() {
        for cmd in [
            "svc start --token abctokenstructured123",
            "checkpoint create --api-key mystructuredapikey",
            "cosh run --secret structuredsecretvalue",
        ] {
            let secret = cmd.rsplit(' ').next().unwrap();
            let mut a = parse_action_string(cmd).unwrap();
            assert!(redact_action(&mut a), "no redaction for {cmd}");
            assert_no_leak(&a, secret);
        }
    }

    #[test]
    fn pem_private_key_scrubbed_in_all_fields() {
        // A realistic single-line PEM: the whitespace-split parser scatters the
        // base64 body across `target`/`args`, so the PEM-context blob scrubber
        // must reach every field.
        let body = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQ";
        let raw = format!("echo -----BEGIN PRIVATE KEY-----{body}-----END PRIVATE KEY-----");
        let mut a = parse_action_string(&raw).unwrap();
        assert!(redact_action(&mut a));
        assert_no_leak(&a, body);
        assert_no_leak(&a, "BEGIN PRIVATE KEY");
    }

    #[test]
    fn pem_header_in_operation_scrubs_residue_from_other_fields() {
        let body = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQ";
        let mut a = Action {
            subsystem: ActionSubsystem::Shell,
            operation: "-----BEGIN PRIVATE KEY-----".to_string(),
            target: Some(body.to_string()),
            args: vec![(body.to_string(), String::new())],
            raw: None,
        };

        assert!(redact_action(&mut a));
        assert_no_leak(&a, body);
        assert_no_leak(&a, "BEGIN PRIVATE KEY");
    }

    #[test]
    fn pem_header_in_custom_subsystem_scrubs_residue_from_other_fields() {
        let body = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQ";
        let mut a = Action {
            subsystem: ActionSubsystem::Other("-----BEGIN PRIVATE KEY-----".to_string()),
            operation: "import".to_string(),
            target: Some(body.to_string()),
            args: vec![],
            raw: Some(body.to_string()),
        };

        assert!(redact_action(&mut a));
        assert_no_leak(&a, body);
        assert_no_leak(&a, "BEGIN PRIVATE KEY");
    }

    #[test]
    fn encrypted_pkcs8_private_key_is_redacted() {
        let body = "MIIJrTBXBgkqhkiG9w0BBQ0wSjApBgkqhkiG9w0BBQwwHAQI";
        let raw = format!(
            "echo -----BEGIN ENCRYPTED PRIVATE KEY----- {body} \
             -----END ENCRYPTED PRIVATE KEY-----"
        );
        let mut a = parse_action_string(&raw).unwrap();

        assert!(redact_action(&mut a));
        assert_eq!(a.operation, REDACTED_PEM);
        assert_eq!(a.target.as_deref(), Some(REDACTED_PEM));
        assert_eq!(a.raw.as_deref(), Some(REDACTED_PEM));
        assert_no_leak(&a, body);
        assert_no_leak(&a, "ENCRYPTED PRIVATE KEY");
    }

    #[test]
    fn private_key_header_split_across_fields_still_sets_pem_context() {
        let body = "MIIEvQIBADANBgkqhkiG";
        let mut a = Action {
            subsystem: ActionSubsystem::Shell,
            operation: "-----BEGIN".to_string(),
            target: Some("ENCRYPTED".to_string()),
            args: vec![
                ("PRIVATE".to_string(), "KEY-----".to_string()),
                (body.to_string(), String::new()),
            ],
            raw: None,
        };

        assert!(redact_action(&mut a));
        assert_eq!(a.operation, REDACTED_PEM);
        assert_eq!(a.target.as_deref(), Some(REDACTED_PEM));
        assert_eq!(
            a.args[0],
            (REDACTED_PEM.to_string(), REDACTED_PEM.to_string())
        );
        assert_no_leak(&a, body);
        assert_no_leak(&a, "ENCRYPTED");
    }

    #[test]
    fn pem_context_redacts_body_split_into_short_chunks() {
        let chunks = [
            "ABCDEFGHIJKLMNOPQRST",
            "abcdefghijklmnopqrst",
            "0123456789ABCDEFGHIJ",
        ];
        let raw = format!(
            "echo -----BEGIN PRIVATE KEY----- {} -----END PRIVATE KEY-----",
            chunks.join(" ")
        );
        let mut a = parse_action_string(&raw).unwrap();

        assert!(redact_action(&mut a));
        for chunk in chunks {
            assert_no_leak(&a, chunk);
        }
    }

    #[test]
    fn cli_secret_values_with_flag_like_or_delimited_content_are_fully_scrubbed() {
        for (cmd, secret_fragments) in [
            ("echo --password -hunter2", ["hunter2", "-hunter2"]),
            (
                "echo --password \"correct horse battery staple\"",
                ["correct", "staple"],
            ),
            ("echo --password=correct,horse", ["correct", "horse"]),
            ("echo password='gamma delta'", ["gamma", "delta"]),
        ] {
            let mut a = parse_action_string(cmd).unwrap();

            assert!(redact_action(&mut a), "no redaction for {cmd}");
            for fragment in secret_fragments {
                assert_no_leak(&a, fragment);
            }
        }
    }

    #[test]
    fn inline_secret_continuations_cross_operation_target_and_args() {
        for (cmd, secret) in [
            (
                "pkg install --password=\"head LEAKSTRUCTQUOTETAIL\"",
                "LEAKSTRUCTQUOTETAIL",
            ),
            (
                "pkg install --password='head LEAKSTRUCTSINGLETAIL'",
                "LEAKSTRUCTSINGLETAIL",
            ),
            (
                "pkg install --password=head\\ LEAKSTRUCTESCTAIL",
                "LEAKSTRUCTESCTAIL",
            ),
            ("pkg --password=\"head LEAKOPQUOTETAIL\"", "LEAKOPQUOTETAIL"),
        ] {
            let mut action = parse_action_string(cmd).unwrap();

            assert!(redact_action(&mut action), "no redaction for {cmd}");
            assert_no_leak(&action, secret);
        }
    }

    #[test]
    fn authorization_flag_preserves_scheme_context() {
        let secret = "LEAKAUTHFLAG123";
        let mut action =
            parse_action_string(&format!("echo --authorization Bearer {secret}")).unwrap();

        assert!(redact_action(&mut action));
        assert_no_leak(&action, secret);
    }

    #[test]
    fn adjacent_sensitive_markers_do_not_expose_later_values() {
        for (cmd, secret) in [
            ("echo --password --token LEAKADJFLAG", "LEAKADJFLAG"),
            (
                "echo --password= --token LEAKEMPTYASSIGN123",
                "LEAKEMPTYASSIGN123",
            ),
            ("echo password: token: LEAKADJASSIGN", "LEAKADJASSIGN"),
        ] {
            let mut action = parse_action_string(cmd).unwrap();

            assert!(redact_action(&mut action), "no redaction for {cmd}");
            assert_no_leak(&action, secret);
        }

        let mut action = parse_action_string("echo password: SECRET1 token: SECRET2").unwrap();
        assert!(redact_action(&mut action));
        assert_no_leak(&action, "SECRET1");
        assert_no_leak(&action, "SECRET2");
    }

    #[test]
    fn normalized_sensitive_keys_redact_structured_and_raw_fields() {
        for (cmd, secret) in [
            (
                "echo pass_word=LEAKNORMALIZEDKEY123",
                "LEAKNORMALIZEDKEY123",
            ),
            ("echo pass\\word=LEAKESCAPEDKEY123", "LEAKESCAPEDKEY123"),
            ("echo --pass\\word LEAKESCAPEDFLAG123", "LEAKESCAPEDFLAG123"),
        ] {
            let mut action = parse_action_string(cmd).unwrap();

            assert!(redact_action(&mut action), "no redaction for {cmd}");
            assert_no_leak(&action, secret);
        }
    }

    #[test]
    fn contextual_detector_covers_independently_serialized_fields() {
        let secret = "LEAKINCONSISTENTACTION123";
        let mut action = Action {
            subsystem: ActionSubsystem::Other(format!("pass_word={secret}")),
            operation: "echo".to_string(),
            target: Some("safe".to_string()),
            args: vec![("note".to_string(), format!("pass\\word={secret}"))],
            raw: Some(format!("echo --pass\\word {secret}")),
        };

        assert!(redact_action(&mut action));
        assert_no_leak(&action, secret);
    }

    #[test]
    fn shell_escaped_pem_header_triggers_action_redaction() {
        let body = "MIIEPEMLEAK1234567890";
        let raw =
            format!("echo -----BEGIN\\ PRIVATE\\ KEY-----{body}-----END\\ PRIVATE\\ KEY-----");
        let mut action = parse_action_string(&raw).unwrap();

        assert!(redact_action(&mut action));
        assert_no_leak(&action, body);
        assert_no_leak(&action, "BEGIN\\ PRIVATE\\ KEY");
    }

    #[test]
    fn percent_encoded_url_query_key_is_redacted() {
        let secret = "LEAKPERCENTURL123";
        let mut action =
            parse_action_string(&format!("curl http://x.test/?access%5Fkey={secret}")).unwrap();

        assert!(redact_action(&mut action));
        assert_no_leak(&action, secret);

        let later_secret = "LEAKLATERPERCENTURL123";
        let url = format!("http://x.test/?region=cn&access%5Fkey={later_secret}");
        let mut structured_action = Action {
            subsystem: ActionSubsystem::Shell,
            operation: "curl".to_string(),
            target: Some(url.clone()),
            args: vec![(url.clone(), String::new())],
            raw: Some(format!("curl '{url}'")),
        };

        assert!(redact_action(&mut structured_action));
        assert_no_leak(&structured_action, later_secret);
    }

    #[test]
    fn redacts_pem_in_raw() {
        let mut a = Action {
            subsystem: ActionSubsystem::Shell,
            operation: "echo".to_string(),
            target: None,
            args: vec![],
            raw: Some(
                "echo -----BEGIN PRIVATE KEY-----\nMIIE...\n-----END PRIVATE KEY-----".to_string(),
            ),
        };
        assert!(redact_action(&mut a));
        let raw = a.raw.as_deref().unwrap();
        assert!(raw.contains("<redacted-pem>"), "got {raw}");
        assert!(!raw.contains("MIIE"), "got {raw}");
    }

    #[test]
    fn does_not_over_redact_normal_command() {
        let mut a = parse_action_string("git push origin main").unwrap();
        assert!(!redact_action(&mut a));
        assert_eq!(a.raw.as_deref(), Some("git push origin main"));
        assert_eq!(a.target.as_deref(), Some("push"));
    }

    #[test]
    fn does_not_over_redact_normal_url() {
        let mut a = parse_action_string("curl http://example.com/?q=hello").unwrap();
        assert!(!redact_action(&mut a));
        assert_no_leak(&a, "<redacted>");
    }
}
