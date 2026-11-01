//! Shipped regexes plus v1 context scoring and validators.

use crate::aliyun;
use crate::models::{Candidate, ScanError, Severity, Span, check_deadline};
use crate::python_unicode::{DECIMAL_RANGES, WORD_RANGES};
use crate::validators;
use fancy_regex::{Captures, Match, Regex, RegexBuilder};
use serde_json::json;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::ops::Range;
use std::time::Instant;

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
const EMAIL_SENTENCE_DELIMITERS: &str = "\"')]}>,;:!?，。；：！？、）］｝】〕》〉」』”’";
// Overlapping assignments may require revisiting the same bytes. Stop with a
// scan error before adversarial nesting can consume unbounded work.
const ALIYUN_NESTED_SCAN_LIMIT: usize = 32 * 1024 * 1024;

// Frozen WORD_RANGES intersected with Unicode 16 Han/Hiragana/Katakana/Hangul
// Script_Extensions from the locked regex-syntax 0.8.11 tables. Keep other
// Unicode letters as token continuations so international addresses are not sliced.
const CJK_WORD_RANGES: &[(char, char)] = &[
    ('\u{1100}', '\u{11ff}'),
    ('\u{3005}', '\u{3007}'),
    ('\u{3021}', '\u{3029}'),
    ('\u{3031}', '\u{3035}'),
    ('\u{3038}', '\u{303c}'),
    ('\u{3041}', '\u{3096}'),
    ('\u{309d}', '\u{309f}'),
    ('\u{30a1}', '\u{30fa}'),
    ('\u{30fc}', '\u{30ff}'),
    ('\u{3131}', '\u{318e}'),
    ('\u{3192}', '\u{3195}'),
    ('\u{31f0}', '\u{31ff}'),
    ('\u{3220}', '\u{3229}'),
    ('\u{3280}', '\u{3289}'),
    ('\u{3400}', '\u{4dbf}'),
    ('\u{4e00}', '\u{9fff}'),
    ('\u{a960}', '\u{a97c}'),
    ('\u{ac00}', '\u{d7a3}'),
    ('\u{d7b0}', '\u{d7c6}'),
    ('\u{d7cb}', '\u{d7fb}'),
    ('\u{f900}', '\u{fa6d}'),
    ('\u{fa70}', '\u{fad9}'),
    ('\u{ff66}', '\u{ffbe}'),
    ('\u{ffc2}', '\u{ffc7}'),
    ('\u{ffca}', '\u{ffcf}'),
    ('\u{ffd2}', '\u{ffd7}'),
    ('\u{ffda}', '\u{ffdc}'),
    ('\u{16fe3}', '\u{16fe3}'),
    ('\u{1aff0}', '\u{1aff3}'),
    ('\u{1aff5}', '\u{1affb}'),
    ('\u{1affd}', '\u{1affe}'),
    ('\u{1b000}', '\u{1b122}'),
    ('\u{1b150}', '\u{1b152}'),
    ('\u{1b164}', '\u{1b167}'),
    ('\u{1d360}', '\u{1d371}'),
    ('\u{20000}', '\u{2a6df}'),
    ('\u{2a700}', '\u{2b738}'),
    ('\u{2b740}', '\u{2b81d}'),
    ('\u{2b820}', '\u{2cea1}'),
    ('\u{2ceb0}', '\u{2ebe0}'),
    ('\u{2f800}', '\u{2fa1d}'),
    ('\u{30000}', '\u{3134a}'),
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
        let start = span.start.saturating_sub(64);
        let end = (span.end + 64).min(self.chars.len());
        let context = self.slice(start, end).to_lowercase();
        let mut score = base;
        let positive_context = context.replace('-', "_");
        if POSITIVE.iter().any(|s| positive_context.contains(s)) {
            score += 0.12;
        }
        if !email
            && (context.contains(".invalid")
                || ["example", "dummy", "test", "sample"].iter().any(|marker| {
                    self.chars[start..end]
                        .windows(marker.len())
                        .enumerate()
                        .any(|(offset, candidate)| {
                            let index = start + offset;
                            // Clipping the context must not turn part of a word into a marker.
                            candidate
                                .iter()
                                .zip(marker.chars())
                                .all(|(actual, expected)| actual.eq_ignore_ascii_case(&expected))
                                && (index == 0 || !word(self.chars[index - 1]))
                                && self
                                    .chars
                                    .get(index + marker.len())
                                    .is_none_or(|c| !word(*c))
                        })
                }))
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
        let mut source: BTreeMap<String, String> =
            serde_json::from_str(PATTERNS).map_err(|_| ScanError::InvalidBuiltin)?;
        // Built from the alias table so the recognized names and their roles
        // cannot drift apart; see `aliyun::ALIASES`.
        source.insert("_ALIYUN_ENV_RE".to_owned(), aliyun::pattern());
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
                        // Canonical CI/CD and cloud token prefixes alongside the
                        // generic sk/pk/rk shapes: GitHub fine-grained personal
                        // access tokens, GitLab personal access tokens, PyPI
                        // upload tokens, npm tokens and AWS access key ids.
                        // The api_key matcher extends every candidate to the
                        // token's word boundary, so the {16} floors are
                        // minima, not exact lengths. The github_pat_ tail is
                        // bounded above as well: a rejected candidate advances
                        // the scan only to prefix.start()+1, so an unbounded
                        // greedy tail would re-scan the remaining word once
                        // per embedded prefix (quadratic work); the bound caps
                        // each attempt while the extension still covers the
                        // full accepted token.
                        r"(?:sk|pk|rk|gh[pousr]|xox[baprs]|glpat|npm|pypi)[-_][A-Za-z0-9_=-]{16}|github_pat_[A-Za-z0-9_]{16,64}|AKIA[0-9A-Z]{16}"
                            .into()
                    }
                    "_EMAIL_RE" => r"[A-Za-z0-9._%+-]{1,64}@[A-Za-z0-9.-]+\.[A-Za-z]{2,63}".into(),
                    // Keep candidate search linear and check detector-specific
                    // boundaries only where the body matches.
                    "_BEARER_RE" => python_pattern(&pattern.replacen(r"(?i)\b", "(?i)", 1)),
                    "_ALIYUN_ACCESS_KEY_ID_RE" => {
                        python_pattern(pattern.trim_start_matches(r"\b").trim_end_matches(r"\b"))
                    }
                    "_CN_ID_RE" | "_PHONE_CN_RE" => python_pattern(
                        pattern
                            .trim_start_matches(r"(?<!\d)")
                            .trim_end_matches(r"(?!\d)")
                            .trim_end_matches(r"(?!\w)"),
                    ),
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

    pub(crate) fn detect<'a>(&'a self, input: &'a str, deadline: Option<Instant>) -> Matches<'a> {
        Matches {
            detector: self,
            text: Text::new(input),
            deadline,
            failed: false,
            cursors: [
                "_ALIYUN_ENV_RE",
                "_PRIVATE_KEY_RE",
                "_BEARER_RE",
                "_SECRET_FIELD_RE",
                "_API_KEY_RE",
                "_JWT_RE",
                "_CREDIT_CARD_RE",
                "_CN_ID_RE",
                "_PHONE_CN_RE",
                "_EMAIL_RE",
            ]
            .map(|id| Cursor {
                id,
                position: 0,
                command_scanned_to: 0,
                command_start: 0,
                handoff_position: 0,
                handoff_next: None,
                handoff_done: false,
                provider_covered_until: 0,
                handoff_covered_until: 0,
                provider_nested_work: 0,
                handoff_nested_work: 0,
                next: None,
                done: false,
            }),
        }
    }
}

// One lookahead per shipped detector bounds pending matches independently of
// input density. All streams are monotone in (span, type).
pub(crate) struct Matches<'a> {
    detector: &'a BuiltinDetector,
    text: Text<'a>,
    cursors: [Cursor<'a>; 10],
    deadline: Option<Instant>,
    failed: bool,
}

struct Cursor<'a> {
    id: &'static str,
    position: usize,
    command_scanned_to: usize,
    command_start: usize,
    handoff_position: usize,
    handoff_next: Option<(usize, Range<usize>)>,
    handoff_done: bool,
    provider_covered_until: usize,
    handoff_covered_until: usize,
    provider_nested_work: usize,
    handoff_nested_work: usize,
    next: Option<Candidate<'a>>,
    done: bool,
}

impl<'a> Iterator for Matches<'a> {
    type Item = Result<Candidate<'a>, ScanError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        for cursor in &mut self.cursors {
            if cursor.done || cursor.next.is_some() {
                continue;
            }
            match cursor.advance(self.detector, &self.text, self.deadline) {
                Ok(next) => {
                    cursor.done = next.is_none();
                    cursor.next = next;
                }
                Err(error) => {
                    self.failed = true;
                    return Some(Err(error));
                }
            }
        }
        let index = self
            .cursors
            .iter()
            .enumerate()
            .filter_map(|(index, cursor)| cursor.next.as_ref().map(|next| (index, next)))
            .min_by(|(_, a), (_, b)| a.compare(b))?
            .0;
        self.cursors[index].next.take().map(Ok)
    }
}

impl<'a> Cursor<'a> {
    fn advance(
        &mut self,
        detector: &BuiltinDetector,
        text: &Text<'a>,
        deadline: Option<Instant>,
    ) -> Result<Option<Candidate<'a>>, ScanError> {
        check_deadline(deadline)?;
        let pattern = detector
            .patterns
            .get(self.id)
            .ok_or(ScanError::InvalidBuiltin)?;
        let result = match self.id {
            "_API_KEY_RE" => api_key(text, pattern, &mut self.position, deadline),
            "_ALIYUN_ENV_RE" => aliyun_env(
                text,
                pattern,
                &mut self.position,
                &mut self.provider_covered_until,
                &mut self.provider_nested_work,
                deadline,
            ),
            "_EMAIL_RE" => {
                let remote = detector
                    .patterns
                    .get("_REMOTE_EMAIL_URI_RE")
                    .ok_or(ScanError::InvalidBuiltin)?;
                next_email(text, pattern, remote, self, deadline)
            }
            id => loop {
                check_deadline(deadline)?;
                let Some(captures) = pattern
                    .captures_from_pos(text.input, self.position)
                    .map_err(|_| ScanError::Matching)?
                else {
                    break Ok(None);
                };
                check_deadline(deadline)?;
                let whole = captures.get(0).ok_or(ScanError::Matching)?;
                if !candidate_boundaries(id, text.input, whole) {
                    // A rejected country prefix can contain a valid local phone number.
                    let first = whole.as_str().chars().next().ok_or(ScanError::Matching)?;
                    self.position = whole.start() + first.len_utf8();
                    continue;
                }
                self.position = whole.end();
                let found = if id == "_SECRET_FIELD_RE" {
                    let field = captures.name("full_name").ok_or(ScanError::Matching)?;
                    let value = captures
                        .name("double_value")
                        .or_else(|| captures.name("single_value"))
                        .or_else(|| captures.name("bare_value"));
                    let handoff = if aliyun::is_official(field.as_str()) {
                        match value {
                            Some(value) => self.provider_covers(
                                detector,
                                text,
                                field.start(),
                                value.range(),
                                deadline,
                            )?,
                            None => false,
                        }
                    } else {
                        false
                    };
                    secret(text, &captures, handoff)?
                } else {
                    basic(text, id, &captures)?
                };
                if found.is_some() {
                    break Ok(found);
                }
            },
        };
        check_deadline(deadline)?;
        result
    }

    // Mirror the provider cursor with a single lookahead. A local syntax check
    // cannot tell whether a preceding provider match consumed this assignment.
    fn provider_covers(
        &mut self,
        detector: &BuiltinDetector,
        text: &Text<'_>,
        field_start: usize,
        value: Range<usize>,
        deadline: Option<Instant>,
    ) -> Result<bool, ScanError> {
        let pattern = detector
            .patterns
            .get("_ALIYUN_ENV_RE")
            .ok_or(ScanError::InvalidBuiltin)?;
        loop {
            check_deadline(deadline)?;
            if let Some((start, range)) = &self.handoff_next {
                if *start >= field_start {
                    return Ok(*start == field_start
                        && range.start <= value.start
                        && range.end >= value.end);
                }
                self.handoff_next = None;
            }
            if self.handoff_done {
                return Ok(false);
            }
            let Some((alias_start, _, matched)) = next_aliyun_assignment(
                text.input,
                pattern,
                &mut self.handoff_position,
                &mut self.handoff_covered_until,
                &mut self.handoff_nested_work,
                deadline,
            )?
            else {
                self.handoff_done = true;
                return Ok(false);
            };
            self.handoff_next = Some((alias_start, matched));
        }
    }
}

#[cfg(test)]
mod resource_tests {
    use super::*;

    #[test]
    fn deadline_is_checked_between_matches() {
        let detector = BuiltinDetector::new().unwrap();
        let mut matches = detector.detect("alice@company.cn bob@company.cn", None);
        assert!(matches.next().unwrap().is_ok());
        matches.deadline = Some(Instant::now());
        assert!(matches!(
            matches.next(),
            Some(Err(ScanError::DeadlineExceeded))
        ));
        assert!(matches.next().is_none());
    }

    #[test]
    fn escaped_bytes_cannot_skip_deadline_checkpoints() {
        let escaped = r"\a".repeat(3_000);
        let start = 1;
        let mut offset = start;
        let mut next_check = start + 4096;
        let deadline = Some(Instant::now());
        for pair in escaped.as_bytes().chunks_exact(2) {
            assert_eq!(pair, b"\\a");
            if matches!(
                check_deadline_at_offset(offset, &mut next_check, deadline),
                Err(ScanError::DeadlineExceeded)
            ) {
                assert_eq!(offset, start + 4096);
                return;
            }
            offset += 2;
        }
        panic!("escaped input crossed the deadline checkpoint without checking it");
    }

    #[test]
    fn nested_provider_work_limit_has_an_exact_failure_boundary() {
        let detector = BuiltinDetector::new().unwrap();
        let pattern = detector.patterns.get("_ALIYUN_ENV_RE").unwrap();
        let input = "ALICLOUD_ACCESS_KEY_SECRET=secret42";
        for (initial, succeeds) in [
            (ALIYUN_NESTED_SCAN_LIMIT - 8, true),
            (ALIYUN_NESTED_SCAN_LIMIT - 7, false),
        ] {
            let mut cursor = 0;
            let mut covered = input.len();
            let mut work = initial;
            let result =
                next_aliyun_assignment(input, pattern, &mut cursor, &mut covered, &mut work, None);
            if succeeds {
                assert!(result.unwrap().is_some());
            } else {
                assert!(matches!(result, Err(ScanError::Matching)));
            }
        }
    }

    #[test]
    fn typical_nested_provider_assignments_stay_below_the_work_limit() {
        let detector = BuiltinDetector::new().unwrap();
        let pattern = detector.patterns.get("_ALIYUN_ENV_RE").unwrap();
        let input = format!(
            "{}ALICLOUD_ACCESS_KEY_SECRET=short123{}",
            "ALICLOUD_ACCESS_KEY_ID=$(echo ".repeat(32),
            ")".repeat(32)
        );
        let mut cursor = 0;
        let mut covered = 0;
        let mut work = 0;
        let mut count = 0;
        while next_aliyun_assignment(
            input.as_str(),
            pattern,
            &mut cursor,
            &mut covered,
            &mut work,
            None,
        )
        .unwrap()
        .is_some()
        {
            count += 1;
        }
        assert_eq!(count, 33);
        assert!(work > 0 && work < 20_000, "nested byte visits: {work}");
    }

    #[test]
    fn email_sentence_period_scan_checks_deadline() {
        let input = format!("alice@company.cn{} ", ".".repeat(8193));
        assert!(matches!(
            email_right_boundary(&input, "alice@company.cn".len(), Some(Instant::now())),
            Err(ScanError::DeadlineExceeded)
        ));
    }
}

fn candidate<'a>(
    text: &Text<'a>,
    matched: Range<usize>,
    kind: &'a str,
    base: f64,
    mut metadata: BTreeMap<String, serde_json::Value>,
) -> Candidate<'a> {
    let span = text.span(matched.start, matched.end);
    let personal = matches!(kind, "email" | "phone_cn" | "credit_card" | "cn_id");
    let value = if kind == "private_key" && span.end - span.start > 16_384 {
        metadata.insert("evidence_omitted".into(), json!(true));
        "[PRIVATE_KEY_OMITTED]"
    } else {
        &text.input[matched.clone()]
    };
    metadata.insert("detector".into(), json!("regex"));
    metadata.insert("engine".into(), json!("regex_v2"));
    Candidate {
        kind,
        category: if personal {
            "personal_data"
        } else {
            "credential"
        },
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
        bytes: matched,
        metadata,
        mask: None,
    }
}

fn basic<'a>(
    text: &Text<'a>,
    id: &str,
    captures: &Captures<'_>,
) -> Result<Option<Candidate<'a>>, ScanError> {
    let matched = captures
        .get(usize::from(id == "_BEARER_RE"))
        .ok_or(ScanError::Matching)?;
    // The cursor consumes the whole numeric expression; only trailing punctuation
    // lies outside the card's evidence and redaction span.
    let value = if id == "_CREDIT_CARD_RE" {
        matched.as_str().trim_end_matches([' ', '-'])
    } else {
        matched.as_str()
    };
    let mut metadata = BTreeMap::new();
    let (kind, base, validator) = match id {
        "_PRIVATE_KEY_RE" => ("private_key", 1.0, Some("pem_private_key")),
        "_BEARER_RE" => {
            metadata.insert("context".into(), json!("bearer"));
            ("bearer_token", 0.92, None)
        }
        "_JWT_RE" if jwt_boundaries(text.input, matched) && validators::jwt(value) => {
            ("jwt", 0.94, Some("jwt_structure"))
        }
        "_CREDIT_CARD_RE" if validators::credit_card(value) => ("credit_card", 0.92, Some("luhn")),
        "_CN_ID_RE" if validators::cn_id(value) => ("cn_id", 0.93, Some("cn_id_checksum")),
        "_PHONE_CN_RE" => ("phone_cn", 0.78, None),
        _ => return Ok(None),
    };
    if let Some(validator) = validator {
        metadata.insert("validator".into(), json!(validator));
    }
    Ok(Some(candidate(
        text,
        matched.start()..matched.start() + value.len(),
        kind,
        base,
        metadata,
    )))
}

fn word(c: char) -> bool {
    let index = WORD_RANGES.partition_point(|(_, end)| *end < c);
    WORD_RANGES.get(index).is_some_and(|(start, _)| *start <= c)
}

fn cjk_word(c: char) -> bool {
    let index = CJK_WORD_RANGES.partition_point(|(_, end)| *end < c);
    CJK_WORD_RANGES
        .get(index)
        .is_some_and(|(start, _)| *start <= c)
}

fn token_word(c: char) -> bool {
    word(c) && !cjk_word(c)
}

fn candidate_boundaries(id: &str, input: &str, matched: Match<'_>) -> bool {
    let before = || input[..matched.start()].chars().next_back();
    let after = || input[matched.end()..].chars().next();
    let decimal = |c| validators::decimal(c).is_some();
    match id {
        "_BEARER_RE" => !before().is_some_and(token_word),
        "_ALIYUN_ACCESS_KEY_ID_RE" => {
            !before().is_some_and(token_word) && !after().is_some_and(token_word)
        }
        "_CN_ID_RE" => !before().is_some_and(decimal) && !after().is_some_and(token_word),
        "_PHONE_CN_RE" => !before().is_some_and(decimal) && !after().is_some_and(decimal),
        _ => true,
    }
}

fn api_key<'a>(
    text: &Text<'a>,
    pattern: &Regex,
    cursor: &mut usize,
    deadline: Option<Instant>,
) -> Result<Option<Candidate<'a>>, ScanError> {
    while let Some(prefix) = pattern
        .find_from_pos(text.input, *cursor)
        .map_err(|_| ScanError::Matching)?
    {
        // An invalid prefix must not consume a later valid prefix within its value.
        check_deadline(deadline)?;
        *cursor = prefix.start() + 1;
        if text.input[..prefix.start()]
            .chars()
            .next_back()
            .is_some_and(token_word)
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
            if end % 4096 == 0 {
                check_deadline(deadline)?;
            }
        }
        // CJK prose ends the complete token, including trailing '-'/'=' bytes.
        // Other contexts retain V1's greedy rollback to a Python word boundary.
        while end >= prefix.end() {
            let before = text.input.as_bytes()[end - 1];
            let before_word = before.is_ascii_alphanumeric() || before == b'_';
            let after = text.input[end..].chars().next();
            let after_word = after.is_some_and(word);
            if after.is_some_and(cjk_word) || before_word != after_word {
                *cursor = end;
                return Ok(Some(candidate(
                    text,
                    prefix.start()..end,
                    "api_key",
                    0.86,
                    BTreeMap::from([("pattern".into(), json!("token_prefix"))]),
                )));
            }
            end -= 1;
            if end % 4096 == 0 {
                check_deadline(deadline)?;
            }
        }
    }
    Ok(None)
}

/// Matches one documented Alibaba Cloud environment assignment.
///
/// The credential is the right-hand side only: the variable name and any
/// `export` keyword stay in the surrounding text and in `redacted_text`, so the
/// report still shows which credential was found.
fn aliyun_env<'a>(
    text: &Text<'a>,
    pattern: &Regex,
    cursor: &mut usize,
    covered_until: &mut usize,
    nested_work: &mut usize,
    deadline: Option<Instant>,
) -> Result<Option<Candidate<'a>>, ScanError> {
    while let Some((_, role, range)) = next_aliyun_assignment(
        text.input,
        pattern,
        cursor,
        covered_until,
        nested_work,
        deadline,
    )? {
        let value = &text.input[range.clone()];
        let bytes = text.input.as_bytes();
        let single_quoted = range.start > 0
            && bytes.get(range.start - 1) == Some(&b'\'')
            && bytes.get(range.end) == Some(&b'\'');
        if aliyun::is_placeholder(value, single_quoted) {
            continue;
        }
        // A compound value can contain another credential; partial ID evidence
        // must not disclose any part of that embedded secret. An ID too short
        // for the identifier mask to conceal anything needs the same treatment:
        // an enclosing generic finding would otherwise keep the whole value in
        // its tail-four suffix, and the audit projection copies it there too.
        let mask = if role == aliyun::Role::AccessKeyId
            && (!value.bytes().all(|byte| byte.is_ascii_alphanumeric())
                || value.chars().count() <= aliyun::SHORT_ID_MAX_LEN)
        {
            Some("[REDACTED_ACCESS_KEY_ID]")
        } else {
            role.mask()
        };
        return Ok(Some(Candidate {
            kind: role.kind(),
            category: "credential",
            severity: role.severity(),
            // There is no scoring component here: the provider comes from a
            // documented name, so evidence does not have to be corroborated.
            confidence: 1.0,
            value,
            span: text.span(range.start, range.end),
            bytes: range,
            metadata: BTreeMap::from([
                ("detector".into(), json!("regex")),
                ("engine".into(), json!("regex_v2")),
                ("provider".into(), json!("alibaba_cloud")),
                ("credential_kind".into(), json!(role.credential_kind())),
            ]),
            mask,
        }));
    }
    Ok(None)
}

// Parse one shell word in linear time. Quotes and substitutions can contain
// whitespace; adjacent fragments form one value and must be masked together.
fn next_aliyun_assignment(
    input: &str,
    pattern: &Regex,
    cursor: &mut usize,
    covered_until: &mut usize,
    nested_work: &mut usize,
    deadline: Option<Instant>,
) -> Result<Option<(usize, aliyun::Role, Range<usize>)>, ScanError> {
    while let Some(captures) = pattern
        .captures_from_pos(input, *cursor)
        .map_err(|_| ScanError::Matching)?
    {
        check_deadline(deadline)?;
        let whole = captures.get(0).ok_or(ScanError::Matching)?;
        let alias = captures.name("alias").ok_or(ScanError::Matching)?;
        let role = aliyun::role(alias.as_str()).ok_or(ScanError::Matching)?;
        let start = whole.end();
        let inside_covered = alias.start() < *covered_until;
        let bytes = input.as_bytes();
        // Angle-bracket placeholders remain recognized even though an
        // unquoted '<' otherwise starts a shell redirection.
        if let Some(end) = angle_placeholder_end(bytes, start, deadline)? {
            *cursor = start + 1;
            return Ok(Some((alias.start(), role, start..end)));
        }
        let mut end = start;
        let mut next_deadline_check = start.saturating_add(4096);
        let mut stack = Vec::new();
        let mut first_quote_end = None;
        while end < bytes.len() {
            if inside_covered {
                *nested_work += 1;
                if *nested_work > ALIYUN_NESTED_SCAN_LIMIT {
                    return Err(ScanError::Matching);
                }
            }
            check_deadline_at_offset(end, &mut next_deadline_check, deadline)?;
            let byte = bytes[end];
            if matches!(byte, b'\r' | b'\n') && inside_covered {
                break;
            }
            match stack.last().copied() {
                Some(b'\'') => {
                    if byte == b'\'' {
                        stack.pop();
                    }
                }
                Some(b'`') => {
                    if byte == b'\\' && end + 1 < bytes.len() {
                        end += 1;
                    } else if byte == b'`' {
                        stack.pop();
                    }
                }
                context => {
                    if byte == b'\\' && end + 1 < bytes.len() {
                        end += 1;
                    } else if byte == b'$' && bytes.get(end + 1) == Some(&b'(') {
                        stack.push(b'(');
                        end += 1;
                    } else if byte == b'$' && bytes.get(end + 1) == Some(&b'{') {
                        // A parameter expansion can carry whitespace in its
                        // word — `${NAME:-prefix value}` — which would end the
                        // value early and leave the word unredacted.
                        stack.push(b'{');
                        end += 1;
                    } else if context == Some(b'(') && byte == b'(' {
                        stack.push(b'(');
                    } else if (context == Some(b'(') && byte == b')')
                        || (context == Some(b'{') && byte == b'}')
                        || (context == Some(b'"') && byte == b'"')
                    {
                        stack.pop();
                    } else if (context != Some(b'"') && matches!(byte, b'\'' | b'"' | b'`'))
                        || (context == Some(b'"') && byte == b'`')
                    {
                        stack.push(byte);
                    } else if context.is_none()
                        && (byte.is_ascii_whitespace()
                            || matches!(byte, b';' | b'&' | b'|' | b'<' | b'>' | b'(' | b')'))
                    {
                        break;
                    }
                }
            }
            end += 1;
            if first_quote_end.is_none()
                && stack.is_empty()
                && end > start
                && matches!(bytes[start], b'\'' | b'"')
            {
                first_quote_end = Some(end);
            }
        }
        // Revisit every value for nested aliases. The explicit work limit
        // bounds repeated scans of overlapping values.
        if end > start {
            *covered_until = (*covered_until).max(end);
            *cursor = start;
        } else {
            *cursor = end;
        }
        if end == start {
            continue;
        }
        let range = if first_quote_end == Some(end) {
            start + 1..end - 1
        } else {
            start..end
        };
        return Ok(Some((alias.start(), role, range)));
    }
    Ok(None)
}

fn check_deadline_at_offset(
    offset: usize,
    next_check: &mut usize,
    deadline: Option<Instant>,
) -> Result<(), ScanError> {
    if offset >= *next_check {
        check_deadline(deadline)?;
        *next_check = offset.saturating_add(4096);
    }
    Ok(())
}

fn angle_placeholder_end(
    bytes: &[u8],
    start: usize,
    deadline: Option<Instant>,
) -> Result<Option<usize>, ScanError> {
    if bytes.get(start) != Some(&b'<') {
        return Ok(None);
    }
    let mut close = start + 1;
    while close < bytes.len()
        && !bytes[close].is_ascii_whitespace()
        && !matches!(bytes[close], b'<' | b'>' | b';' | b'&' | b'|')
    {
        if close.is_multiple_of(4096) {
            check_deadline(deadline)?;
        }
        close += 1;
    }
    Ok((close > start + 1 && bytes.get(close) == Some(&b'>')).then_some(close + 1))
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

fn secret<'a>(
    text: &Text<'a>,
    captures: &Captures<'_>,
    handoff: bool,
) -> Result<Option<Candidate<'a>>, ScanError> {
    let field_match = captures.name("full_name").ok_or(ScanError::Matching)?;
    let field = field_match.as_str();
    let value = captures
        .name("double_value")
        .or_else(|| captures.name("single_value"))
        .or_else(|| captures.name("bare_value"));
    let Some(value) = value else {
        return Ok(None);
    };
    let matched = captures.name("quoted_value").ok_or(ScanError::Matching)?;
    // Only a match actually consumed by the provider can own this value.
    if handoff {
        return Ok(None);
    }
    if value.as_str().chars().count() < 12 && !field.to_lowercase().starts_with("accesskey") {
        return Ok(None);
    }
    let mut metadata = BTreeMap::new();
    metadata.insert("field".into(), json!(field_label(captures)?));
    let normalized = field.to_lowercase().replace(['-', '_'], "");
    // `accessKeySecret` without a documented provider name does not establish
    // Alibaba Cloud ownership, so it keeps the generic type and the denial.
    // It still gets a whole-value mask: the retained leading and trailing
    // characters would otherwise narrow a credential that is only a few
    // characters longer than the shortest accepted value.
    let kind = match normalized.as_str() {
        "apikey" => "api_key",
        _ => "generic_secret_field",
    };
    let mut found = candidate(text, matched.range(), kind, 0.82, metadata);
    found.mask = aliyun::role(field)
        .and_then(aliyun::Role::mask)
        .or((normalized == "accesskeysecret").then_some("[REDACTED_SECRET]"));
    Ok(Some(found))
}

// The client report retains metadata.field verbatim, so the label must stay
// inside the fixed keyword vocabulary. A compound prefix is input-derived and
// can itself embed a live credential (sk_live_..._PASSWORD) or an adversarial
// megabyte run; only the matched keyword is labeled, behind an elision marker.
fn field_label(captures: &Captures<'_>) -> Result<String, ScanError> {
    if let Some(name) = captures.name("name") {
        return Ok(name.as_str().to_owned());
    }
    let keyword = captures
        .name("keyword")
        .ok_or(ScanError::Matching)?
        .as_str();
    Ok(format!("..._{keyword}"))
}

fn email<'a>(
    text: &Text<'a>,
    matched: Match<'_>,
    command_start: usize,
    remote_uri: &Regex,
) -> Result<Option<Candidate<'a>>, ScanError> {
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

fn next_email<'a>(
    text: &Text<'a>,
    pattern: &Regex,
    remote_uri: &Regex,
    cursor: &mut Cursor<'a>,
    deadline: Option<Instant>,
) -> Result<Option<Candidate<'a>>, ScanError> {
    while let Some(whole) = pattern
        .find_from_pos(text.input, cursor.position)
        .map_err(|_| ScanError::Matching)?
    {
        check_deadline(deadline)?;
        cursor.position = whole.start() + 1;
        let before = text.input[..whole.start()].chars().next_back();
        if before.is_some_and(|c| token_word(c) || matches!(c, '.' | '+' | '-'))
            || !email_right_boundary(text.input, whole.end(), deadline)?
        {
            continue;
        }
        cursor.position = whole.end();
        let span = text.span(whole.start(), whole.end());
        for offset in cursor.command_scanned_to..span.start {
            if offset % 4096 == 0 {
                check_deadline(deadline)?;
            }
            if matches!(text.chars[offset], '\n' | ';' | '|' | '&') {
                cursor.command_start = offset + 1;
            }
        }
        cursor.command_scanned_to = span.end;
        if let Some(candidate) = email(text, whole, cursor.command_start, remote_uri)? {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn email_right_boundary(
    input: &str,
    mut end: usize,
    deadline: Option<Instant>,
) -> Result<bool, ScanError> {
    let Some(after) = input[end..].chars().next() else {
        return Ok(true);
    };
    if after != '.' {
        return Ok(!token_word(after) && after != '-');
    }
    // A period run is punctuation only when it cannot continue the domain.
    let start = end;
    while input.as_bytes().get(end) == Some(&b'.') {
        if (end - start).is_multiple_of(4096) {
            check_deadline(deadline)?;
        }
        end += 1;
    }
    Ok(input[end..]
        .chars()
        .next()
        .is_none_or(|c| space(c) || EMAIL_SENTENCE_DELIMITERS.contains(c)))
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
    fn cjk_boundaries_match_frozen_word_script_extensions() {
        let pattern = Regex::new(&format!(
            r"[{}&&[\p{{scx:Han}}\p{{scx:Hiragana}}\p{{scx:Katakana}}\p{{scx:Hangul}}]]",
            character_class(WORD_RANGES)
        ))
        .unwrap();
        let scalars: Vec<char> = (0..=0x10_ffff).filter_map(char::from_u32).collect();
        let input: String = scalars.iter().collect();
        let expected: Vec<char> = pattern
            .find_iter(&input)
            .map(|found| found.unwrap().as_str().chars().next().unwrap())
            .collect();
        let actual: Vec<char> = scalars.into_iter().filter(|c| cjk_word(*c)).collect();
        assert_eq!(actual, expected);
    }

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
