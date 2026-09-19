//! Shipped regexes plus v1 context scoring and validators.

use crate::models::{Candidate, ScanError, Severity, Span};
use crate::python_unicode::{DECIMAL_RANGES, WORD_RANGES};
use crate::validators;
use fancy_regex::{Captures, Match, Regex, RegexBuilder};
use serde_json::json;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::ops::Range;

pub(crate) const PATTERNS: &str = include_str!("builtin_patterns.json");
pub(crate) const TYPES: &[&str] = &[
    "aliyun_access_key_id",
    "aliyun_access_key_secret",
    "api_key",
    "bearer_token",
    "cn_id",
    "credit_card",
    "email",
    "generic_secret_field",
    "jwt",
    "phone_cn",
    "private_key",
];
const POSITIVE: &[&str] = &[
    "password",
    "secret",
    "token",
    "api_key",
    "apikey",
    "authorization",
    "bearer",
    "accesskeysecret",
    "access_key_secret",
    "密码",
    "口令",
    "密钥",
    "令牌",
    "授权",
    "访问密钥",
];
const RESERVED: &[&str] = &[
    "example",
    "example.com",
    "example.net",
    "example.org",
    "invalid",
    "localhost",
    "test",
];

pub(crate) struct BuiltinDetector {
    patterns: BTreeMap<String, Regex>,
}

struct Text<'a> {
    input: &'a str,
    offsets: Vec<usize>,
    chars: Vec<char>,
}

impl<'a> Text<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            offsets: input
                .char_indices()
                .map(|(i, _)| i)
                .chain(std::iter::once(input.len()))
                .collect(),
            chars: input.chars().collect(),
        }
    }
    fn span(&self, start: usize, end: usize) -> Span {
        Span {
            start: self.offsets.partition_point(|i| *i < start),
            end: self.offsets.partition_point(|i| *i < end),
        }
    }
    fn slice(&self, start: usize, end: usize) -> &str {
        &self.input[self.offsets[start]..self.offsets[end]]
    }
    fn score(&self, span: Span, base: f64, email: bool) -> f64 {
        let context = self
            .slice(
                span.start.saturating_sub(64),
                (span.end + 64).min(self.chars.len()),
            )
            .to_lowercase()
            .replace('-', "_");
        let mut score = base;
        if POSITIVE.iter().any(|s| context.contains(s)) {
            score += 0.12;
        }
        if !email
            && ["example", "dummy", "test", "sample", ".invalid"]
                .iter()
                .any(|s| context.contains(s))
        {
            score -= 0.35;
        }
        score.clamp(0.0, 1.0)
    }
}

fn character_class(ranges: &[(char, char)]) -> String {
    let mut class = String::from("[");
    for (start, end) in ranges {
        // String's writer and integer formatters are infallible.
        write!(
            class,
            r"\x{{{:x}}}-\x{{{:x}}}",
            u32::from(*start),
            u32::from(*end)
        )
        .unwrap_or_else(|_| unreachable!("formatting integers into a String cannot fail"));
    }
    class.push(']');
    class
}

// Freeze builtin classes to Python 3.11, including characters unassigned in
// Unicode 14. Rust's newer tables would suppress findings at their boundaries.
fn python_pattern(pattern: &str) -> String {
    let mut pattern = pattern.to_owned();
    if pattern.starts_with("(?i)") {
        // Python also folds dotted/dotless I into ASCII i. Expand the shipped
        // keywords explicitly without rewriting regex capture-group names.
        for keyword in ["api", "client", "authorization", "git"] {
            pattern = pattern.replace(keyword, &keyword.replace('i', "[iİı]"));
        }
        pattern = pattern.replace("[A-Za-z", "[İıA-Za-z");
    }
    let word = character_class(WORD_RANGES);
    let decimal = character_class(DECIMAL_RANGES);
    let boundary_word = format!("(?-i:{word})");
    pattern
        .replace(r"[\s\S]", "(?s:.)")
        // Case folding must not reintroduce letters added after Unicode 14.
        .replace(r"(?<![\w\u4e00-\u9fff])", &format!("(?<!{boundary_word})"))
        .replace(r"(?<![\w+.-])", &format!("(?<!(?-i:[{word}+.-]))"))
        .replace(
            r"\b",
            &format!(r"(?:(?<={boundary_word})(?!{boundary_word})|(?<!{boundary_word})(?={boundary_word}))"),
        )
        .replace(r"\w", &word)
        .replace(r"\d", &decimal)
        .replace(r"\s", r"[\s\x1c-\x1f]")
}

impl BuiltinDetector {
    pub(crate) fn new() -> Result<Self, ScanError> {
        let source: BTreeMap<String, String> =
            serde_json::from_str(PATTERNS).map_err(|_| ScanError::InvalidBuiltin)?;
        // Shipped patterns use a larger fixed allowance so ordinary multi-MiB
        // inputs remain compatible. Administrator-supplied rules use 1M instead.
        let patterns = source
            .into_iter()
            .map(|(id, pattern)| {
                // These unbounded tokens need linear candidate matching; their
                // lookarounds otherwise overflow the VM stack on large inputs.
                let pattern = match id.as_str() {
                    "_JWT_RE" => r"[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{3,}\.[A-Za-z0-9_-]{8,}".into(),
                    "_API_KEY_RE" => {
                        r"(?:sk|pk|rk|gh[pousr]|xox[baprs])[-_][A-Za-z0-9_=-]{16}".into()
                    }
                    "_EMAIL_RE" => r"[A-Za-z0-9._%+-]{1,64}@[A-Za-z0-9.-]+\.[A-Za-z]{2,63}".into(),
                    _ => python_pattern(&pattern),
                };
                RegexBuilder::new(&pattern)
                    .backtrack_limit(32_000_000)
                    .build()
                    .map(|regex| (id, regex))
                    .map_err(|_| ScanError::InvalidBuiltin)
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { patterns })
    }

    pub(crate) fn detect(&self, input: &str) -> Result<Vec<Candidate>, ScanError> {
        let text = Text::new(input);
        let mut candidates = Vec::new();
        for id in [
            "_PRIVATE_KEY_RE",
            "_BEARER_RE",
            "_SECRET_FIELD_RE",
            "_API_KEY_RE",
            "_ALIYUN_ACCESS_KEY_ID_RE",
            "_JWT_RE",
            "_CREDIT_CARD_RE",
            "_CN_ID_RE",
            "_PHONE_CN_RE",
            "_EMAIL_RE",
        ] {
            let pattern = self.patterns.get(id).ok_or(ScanError::InvalidBuiltin)?;
            if id == "_API_KEY_RE" {
                candidates.extend(api_keys(&text, pattern)?);
                continue;
            }
            if id == "_EMAIL_RE" {
                let remote_uri = self
                    .patterns
                    .get("_REMOTE_EMAIL_URI_RE")
                    .ok_or(ScanError::InvalidBuiltin)?;
                candidates.extend(emails(&text, pattern, remote_uri)?);
                continue;
            }
            for captures in pattern.captures_iter(input) {
                let captures = captures.map_err(|_| ScanError::Matching)?;
                let candidate = if id == "_SECRET_FIELD_RE" {
                    secret(&text, &captures)?
                } else {
                    basic(&text, id, &captures)?
                };
                if let Some(candidate) = candidate {
                    candidates.push(candidate);
                }
            }
        }
        Ok(candidates)
    }
}

fn candidate(
    text: &Text<'_>,
    matched: Range<usize>,
    kind: &str,
    base: f64,
    mut metadata: BTreeMap<String, serde_json::Value>,
) -> Candidate {
    let span = text.span(matched.start, matched.end);
    let personal = matches!(kind, "email" | "phone_cn" | "credit_card" | "cn_id");
    let value = if kind == "private_key" && span.end - span.start > 16_384 {
        metadata.insert("evidence_omitted".into(), json!(true));
        "[PRIVATE_KEY_OMITTED]".to_owned()
    } else {
        text.input[matched].to_owned()
    };
    metadata.insert("detector".into(), json!("regex"));
    metadata.insert("engine".into(), json!("regex_v2"));
    Candidate {
        kind: kind.into(),
        category: if personal {
            "personal_data"
        } else {
            "credential"
        }
        .into(),
        severity: if personal {
            Severity::Warn
        } else {
            Severity::Deny
        },
        confidence: if kind == "private_key" {
            base
        } else {
            text.score(span, base, kind == "email")
        },
        value,
        span,
        metadata,
    }
}

fn basic(
    text: &Text<'_>,
    id: &str,
    captures: &Captures<'_>,
) -> Result<Option<Candidate>, ScanError> {
    let matched = captures
        .get(usize::from(id == "_BEARER_RE"))
        .ok_or(ScanError::Matching)?;
    let value = matched.as_str();
    let mut metadata = BTreeMap::new();
    let (kind, base, validator) = match id {
        "_PRIVATE_KEY_RE" => ("private_key", 1.0, Some("pem_private_key")),
        "_BEARER_RE" => {
            metadata.insert("context".into(), json!("bearer"));
            ("bearer_token", 0.92, None)
        }
        "_ALIYUN_ACCESS_KEY_ID_RE" => ("aliyun_access_key_id", 0.92, None),
        "_JWT_RE" if jwt_boundaries(text.input, matched) && validators::jwt(value) => {
            ("jwt", 0.94, Some("jwt_structure"))
        }
        "_CREDIT_CARD_RE" if validators::luhn(value) => ("credit_card", 0.92, Some("luhn")),
        "_CN_ID_RE" if validators::cn_id(value) => ("cn_id", 0.93, Some("cn_id_checksum")),
        "_PHONE_CN_RE" => ("phone_cn", 0.78, None),
        _ => return Ok(None),
    };
    if let Some(validator) = validator {
        metadata.insert("validator".into(), json!(validator));
    }
    Ok(Some(candidate(text, matched.range(), kind, base, metadata)))
}

fn word(c: char) -> bool {
    let index = WORD_RANGES.partition_point(|(_, end)| *end < c);
    WORD_RANGES.get(index).is_some_and(|(start, _)| *start <= c)
}

fn api_keys(text: &Text<'_>, pattern: &Regex) -> Result<Vec<Candidate>, ScanError> {
    let mut candidates = Vec::new();
    let mut cursor = 0;
    while let Some(prefix) = pattern
        .find_from_pos(text.input, cursor)
        .map_err(|_| ScanError::Matching)?
    {
        // An invalid prefix must not consume a later valid prefix within its value.
        cursor = prefix.start() + 1;
        if text.input[..prefix.start()]
            .chars()
            .next_back()
            .is_some_and(word)
        {
            continue;
        }
        let mut end = prefix.end();
        while text
            .input
            .as_bytes()
            .get(end)
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'=' | b'-'))
        {
            end += 1;
        }
        // V1 greedily backs up to the last Python word boundary. A trailing '-'
        // before Chinese text is a boundary too, so trimming punctuation is wrong.
        while end >= prefix.end() {
            let before = text.input.as_bytes()[end - 1];
            let before_word = before.is_ascii_alphanumeric() || before == b'_';
            let after_word = text.input[end..].chars().next().is_some_and(word);
            if before_word != after_word {
                candidates.push(candidate(
                    text,
                    prefix.start()..end,
                    "api_key",
                    0.86,
                    BTreeMap::from([("pattern".into(), json!("token_prefix"))]),
                ));
                cursor = end;
                break;
            }
            end -= 1;
        }
    }
    Ok(candidates)
}

fn jwt_boundaries(input: &str, matched: Match<'_>) -> bool {
    let token_byte = |b: &u8| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-');
    !matched
        .start()
        .checked_sub(1)
        .and_then(|i| input.as_bytes().get(i))
        .is_some_and(token_byte)
        && !input.as_bytes().get(matched.end()).is_some_and(token_byte)
}

fn secret(text: &Text<'_>, captures: &Captures<'_>) -> Result<Option<Candidate>, ScanError> {
    let field = captures.name("name").ok_or(ScanError::Matching)?.as_str();
    let value = captures
        .name("double_value")
        .or_else(|| captures.name("single_value"))
        .or_else(|| captures.name("bare_value"));
    let Some(value) = value else {
        return Ok(None);
    };
    if value.as_str().chars().count() < 12 && !field.to_lowercase().starts_with("accesskey") {
        return Ok(None);
    }
    let matched = captures.name("quoted_value").ok_or(ScanError::Matching)?;
    let mut metadata = BTreeMap::new();
    metadata.insert("field".into(), json!(field));
    let normalized = field.to_lowercase().replace(['-', '_'], "");
    let kind = match normalized.as_str() {
        "accesskeysecret" => "aliyun_access_key_secret",
        "apikey" => "api_key",
        _ => "generic_secret_field",
    };
    Ok(Some(candidate(text, matched.range(), kind, 0.82, metadata)))
}

fn email(
    text: &Text<'_>,
    matched: Match<'_>,
    command_start: usize,
    remote_uri: &Regex,
) -> Result<Option<Candidate>, ScanError> {
    if !validators::email(matched.as_str()) {
        return Ok(None);
    }
    let span = text.span(matched.start(), matched.end());
    let mut metadata = BTreeMap::new();
    metadata.insert("validator".into(), json!("email_syntax"));
    let mut base = 0.82;
    if remote_identity(text, span, command_start, remote_uri)? {
        base = 0.35;
        metadata.insert("context".into(), json!("remote_identity"));
    } else {
        let domain = matched
            .as_str()
            .rsplit('@')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if RESERVED
            .iter()
            .any(|r| domain == *r || domain.ends_with(&format!(".{r}")))
        {
            base = 0.35;
            metadata.insert("context".into(), json!("reserved_domain"));
        }
    }
    Ok(Some(candidate(
        text,
        matched.range(),
        "email",
        base,
        metadata,
    )))
}

fn emails(
    text: &Text<'_>,
    pattern: &Regex,
    remote_uri: &Regex,
) -> Result<Vec<Candidate>, ScanError> {
    let mut candidates = Vec::new();
    let mut cursor = 0;
    let mut command_cursor = 0;
    let mut command_start = 0;
    while let Some(whole) = pattern
        .find_from_pos(text.input, cursor)
        .map_err(|_| ScanError::Matching)?
    {
        cursor = whole.start() + 1;
        let before = text.input[..whole.start()].chars().next_back();
        let after = text.input[whole.end()..].chars().next();
        if before.is_some_and(|c| word(c) || matches!(c, '.' | '+' | '-'))
            || after.is_some_and(|c| word(c) || matches!(c, '.' | '-'))
        {
            continue;
        }
        cursor = whole.end();
        let span = text.span(whole.start(), whole.end());
        for offset in command_cursor..span.start {
            if matches!(text.chars[offset], '\n' | ';' | '|' | '&') {
                command_start = offset + 1;
            }
        }
        command_cursor = span.end;
        if let Some(candidate) = email(text, whole, command_start, remote_uri)? {
            candidates.push(candidate);
        }
    }
    Ok(candidates)
}

fn space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

// V1 recognizes the literal .git suffix; this is not a filesystem extension lookup.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn remote_identity(
    text: &Text<'_>,
    span: Span,
    command_start: usize,
    uri: &Regex,
) -> Result<bool, ScanError> {
    if uri
        .is_match(text.slice(span.start.saturating_sub(64), span.start))
        .map_err(|_| ScanError::Matching)?
    {
        return Ok(true);
    }
    if text.chars.get(span.end) == Some(&':')
        && text.chars.get(span.end + 1).is_some_and(|c| !space(*c))
    {
        let path = text
            .slice(span.end + 1, (span.end + 1 + 1024).min(text.chars.len()))
            .split(space)
            .next()
            .unwrap_or("");
        let is_uri = path.split_once("://").is_some_and(|(scheme, _)| {
            scheme
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
                && scheme
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"+.-".contains(&c))
        });
        if !is_uri && (path.starts_with(['/', '~']) || path.contains('/') || path.ends_with(".git"))
        {
            return Ok(true);
        }
    }
    let mut start = span.start;
    let opening = start
        .checked_sub(1)
        .and_then(|i| text.chars.get(i))
        .copied()
        .filter(|c| matches!(c, '\'' | '"'));
    let closing = text
        .chars
        .get(span.end)
        .copied()
        .filter(|c| matches!(c, '\'' | '"'));
    if opening.is_some() || closing.is_some() {
        if opening.is_none() || opening != closing {
            return Ok(false);
        }
        if text
            .chars
            .get(span.end + 1)
            .is_some_and(|c| !space(*c) && !matches!(c, ';' | '|' | '&'))
        {
            return Ok(false);
        }
        start -= 1;
    }
    if start <= command_start || !space(text.chars[start - 1]) || start - command_start > 4096 {
        return Ok(false);
    }
    Ok(remote_command(text.slice(command_start, start)))
}

fn remote_command(prefix: &str) -> bool {
    let tokens: Vec<_> = prefix.split(space).filter(|s| !s.is_empty()).collect();
    let Some(command) = tokens.first().and_then(|s| s.rsplit('/').next()) else {
        return false;
    };
    let (values, flags) = match command.to_ascii_lowercase().as_str() {
        "ssh" => ("BbcDEeFIiJLlmOoPpRSWw".to_owned(), "46AaCfGgKkMNnqsTtvXxYy"),
        "sftp" => ("BbcFiJloPRSsX".to_owned(), "46AaCfNpqrv"),
        _ => return false,
    };
    let mut index = 1;
    while index < tokens.len() {
        let option = tokens[index];
        if option == "--" {
            return index + 1 == tokens.len();
        }
        let chars: Vec<_> = option.chars().collect();
        if chars.len() < 2 || chars[0] != '-' {
            return false;
        }
        if chars.len() == 2 && flags.contains(chars[1]) {
            index += 1;
        } else if values.contains(chars[1]) {
            index += if chars.len() == 2 { 2 } else { 1 };
            if index > tokens.len() {
                return false;
            }
        } else if chars.len() > 2 && chars[1..].iter().all(|c| flags.contains(*c)) {
            index += 1;
        } else {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_python_classes_cover_every_unicode_scalar() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/v1.json")).unwrap();
        let scalars: Vec<char> = (0..=0x10_ffff).filter_map(char::from_u32).collect();
        let input: String = scalars.iter().collect();
        let decimal_values: Vec<u8> = scalars
            .iter()
            .map(|c| u8::try_from(validators::decimal(*c).unwrap_or(255)).unwrap())
            .collect();
        assert_eq!(
            crate::scanner::digest(decimal_values),
            fixture["character_classes_sha256"]["decimal_values"]
                .as_str()
                .unwrap(),
        );
        for (name, ranges) in [("word", WORD_RANGES), ("decimal", DECIMAL_RANGES)] {
            let pattern = Regex::new(&character_class(ranges)).unwrap();
            let mut classified = vec![0_u8; 0x11_0000];
            for found in pattern.find_iter(&input) {
                let found = found.unwrap();
                let character = found.as_str().chars().next().unwrap();
                assert_eq!(found.as_str().chars().count(), 1);
                classified[character as usize] = 1;
            }
            let values: Vec<u8> = scalars.iter().map(|c| classified[*c as usize]).collect();
            assert_eq!(
                crate::scanner::digest(&values),
                fixture["character_classes_sha256"][name].as_str().unwrap(),
                "Python 3.11 character class {name}",
            );
            if name == "word" {
                let boundaries: Vec<u8> = scalars.iter().map(|c| u8::from(word(*c))).collect();
                assert_eq!(boundaries, values);
            }
        }
    }

    #[test]
    fn every_shipped_pattern_compiles_with_python_boundaries() {
        let patterns: BTreeMap<String, String> = serde_json::from_str(PATTERNS).unwrap();
        for (name, pattern) in patterns {
            RegexBuilder::new(&python_pattern(&pattern))
                .build()
                .unwrap_or_else(|error| panic!("{name}: {error}"));
        }
    }
}
