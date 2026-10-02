//! Extraction of the factual claims a message can be held to.
//!
//! Grounding only applies to statements that can be checked, so this layer
//! pulls out the parts of an agent message that carry verifiable weight —
//! counts, dates, URLs, paths, versions, identifiers — and leaves prose alone.
//!
//! Nothing here relates to LLM tokens; a claim is a factual assertion the
//! evaluator may later be asked to trace back to a tool observation.
//!
//! Two rules keep the extractor from manufacturing work. Numbers need at least
//! three significant digits, which is what stops arithmetic like `1+1=2` from
//! ever being treated as a claim needing external evidence. And numeric
//! comparison happens on the parsed value rather than the surrounding text, so
//! `242391`, `242,391` and `24.2万` are recognised as the same fact.

/// Smallest count of significant digits a bare number needs before it is worth
/// grounding. Below this the number is either arithmetic or an index, and
/// demanding a source for it is the over-flagging this pipeline exists to stop.
const MIN_SIGNIFICANT_DIGITS: usize = 3;

/// Shortest quoted span worth tracking. Short quotes are usually field names or
/// flags rather than factual content.
const MIN_QUOTED_LEN: usize = 8;

/// Characters that glue a value to surrounding JSON structure, escaping, or CJK
/// prose. Splitting on them is what makes a number inside a serialized payload
/// or a Chinese sentence visible; without it every figure a tool returned, or
/// the agent stated, reads as sourceless.
///
/// `|` earns its place from a live capture: sqlite3 delimits columns with it by
/// default, so `complete|119` hid the 119 an agent then correctly reported, and
/// the claim was called sourceless and pinned on an unrelated failed call.
///
/// The CJK marks earn theirs the same way: Chinese prose puts no spaces around
/// punctuation, so "端口通常是 8081。其配置文件名为" leaves the number welded to the
/// rest of the sentence. Edge-trimming cannot reach it and no ASCII separator
/// appears, so a fabricated port went unchecked by the layer built to check it.
const STRUCTURAL_SEPARATORS: &[char] = &[
    '=', ':', ',', '\\', '{', '}', '[', ']', '|', '。', '，', '、', '：', '；', '！', '？', '（',
    '）', '「', '」', '【', '】',
];

/// What kind of fact a claim asserts. The class decides how strict grounding is
/// and which tool could have supplied it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClaimClass {
    /// A count, size or measurement.
    Number,
    /// A calendar date or a marked year.
    Date,
    /// An absolute URL.
    Url,
    /// A filesystem path.
    Path,
    /// A semantic version.
    Version,
    /// A code-like identifier (`Foo::bar`, `snake_case_name`).
    Identifier,
    /// A quoted span long enough to be content.
    Quoted,
}

/// One extracted fact: its surface form, its class, and the numeric value when
/// it has one. `value` is what comparison uses, so a reformatted number still
/// matches its source.
#[derive(Debug, Clone, PartialEq)]
pub struct Claim {
    pub text: String,
    pub class: ClaimClass,
    pub value: Option<f64>,
}

impl Claim {
    /// Whether this claim may anchor an ungrounded-onset finding.
    ///
    /// Quoted spans and identifiers are excluded: paraphrase and translation
    /// legitimately change them, so a mismatch there is weak evidence and would
    /// mostly produce false accusations.
    pub fn can_anchor_finding(&self) -> bool {
        matches!(
            self.class,
            ClaimClass::Number
                | ClaimClass::Date
                | ClaimClass::Url
                | ClaimClass::Path
                | ClaimClass::Version
        )
    }
}

/// Chinese and SI magnitude suffixes, largest first so that `亿` is not read as
/// a digit run followed by noise.
const MAGNITUDES: &[(&str, f64)] = &[
    ("亿", 100_000_000.0),
    ("万", 10_000.0),
    ("b", 1_000_000_000.0),
    ("m", 1_000_000.0),
    ("k", 1_000.0),
];

/// Remove commas that group digits, leaving every other comma in place.
///
/// A comma directly between two digits is a thousands separator, not structure.
/// Splicing those out before the structural split is what lets `数**：244,781`
/// keep its number: that token cannot pass the stands-alone guard because of its
/// prefix, and splitting it on the comma would invent a claim of "244". A comma
/// doing real work, as between two JSON fields, is untouched.
fn splice_digit_groups(word: &str) -> String {
    let chars: Vec<char> = word.chars().collect();
    let mut out = String::with_capacity(word.len());
    for (i, &c) in chars.iter().enumerate() {
        let grouping = matches!(c, ',' | '，')
            && i > 0
            && chars[i - 1].is_ascii_digit()
            && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit());
        if !grouping {
            out.push(c);
        }
    }
    out
}

/// Parse a human-written number into its value.
///
/// Accepts thousands separators and magnitude suffixes, because agents restate
/// retrieved counts in whichever form reads best and a literal comparison would
/// then call a grounded number ungrounded.
pub fn parse_number(raw: &str) -> Option<f64> {
    // Approximation markers lead the number ("约240k", "~1.2m"), so they are
    // stripped up front rather than per magnitude branch.
    let raw = raw.trim_start_matches(['约', '~', '≈', '+']);
    // `，` is a digit-group separator in Chinese prose exactly as `,` is in
    // English. Parsing through it is what lets `244，618` stand alone as one
    // number, so the separator split cannot break it into a bogus `244`.
    let cleaned: String = raw
        .chars()
        .filter(|c| !matches!(c, ',' | '_' | ' ' | '\u{a0}' | '，' | '\u{3000}'))
        .collect();
    let lower = cleaned.to_lowercase();

    for (suffix, factor) in MAGNITUDES {
        if let Some(head) = lower.strip_suffix(suffix) {
            if let Ok(base) = head.parse::<f64>() {
                return Some(base * factor);
            }
        }
    }
    lower.parse::<f64>().ok()
}

/// Whether two numbers name the same fact.
///
/// Rounded restatements ("about 240k" for 242,391) must still count as
/// grounded, so agreement is relative rather than exact.
pub fn numbers_agree(claim: f64, evidence: f64) -> bool {
    if claim == evidence {
        return true;
    }
    let scale = claim.abs().max(evidence.abs());
    if scale == 0.0 {
        return false;
    }
    // 2% covers "24.2万" and "about 240k" for 242,391 without letting
    // genuinely different counts pass.
    (claim - evidence).abs() / scale <= 0.02
}

/// Extract every verifiable claim from a message.
pub fn extract_claims(text: &str) -> Vec<Claim> {
    let mut claims = Vec::new();
    claims.extend(extract_urls(text));
    claims.extend(extract_quoted(text));
    for word in text.split_whitespace() {
        // Strip decoration, not content. Agents habitually wrap paths and
        // identifiers in markdown (`/var/log`, **error**), and a claim carrying
        // those marks would never match the plain form sitting in the evidence —
        // a path the user typed themselves would be reported as sourceless.
        let word = word.trim_matches(|c: char| {
            matches!(
                c,
                '(' | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | ','
                    | ':'
                    | ';'
                    | '`'
                    | '*'
                    | '"'
                    | '\''
                    | '<'
                    | '>'
                    | '.'
                    | '!'
                    | '?'
                    | '。'
                    | '，'
                    | '、'
                    | '：'
                    | '；'
                    | '「'
                    | '」'
                    | '“'
                    | '”'
                    | '‘'
                    | '’'
            )
        });
        if word.is_empty() {
            continue;
        }
        let whole_word_claim = classify_word(word);
        let word_stands_alone = whole_word_claim.is_some();
        if let Some(claim) = whole_word_claim {
            claims.push(claim);
        }
        // Tool output is JSON far more often than prose, and its structure glues
        // values to their surroundings: `stargazers_count=242391`, or
        // `\"stargazers_count\": 244618,\n` where the escaped newline leaves
        // `244618,\n` as one whitespace-delimited word that parses as nothing.
        // Splitting on the structural characters exposes such a value.
        //
        // Digit-group separators are spliced out first. The stands-alone guard
        // below only spares a word that is *entirely* a number, so `数**：244,781`
        // fails it on its prefix and would then break at the thousands
        // separator, inventing a "244" no evidence supports. A comma doing real
        // structural work, as between two JSON fields, is left to separate.
        if !word_stands_alone && !word.starts_with("http") && word.contains(STRUCTURAL_SEPARATORS) {
            let degrouped = splice_digit_groups(word);
            for segment in degrouped.split(STRUCTURAL_SEPARATORS) {
                if let Some(claim) = classify_word(segment) {
                    claims.push(claim);
                }
            }
        }
    }
    dedup(claims)
}

/// Classify a single whitespace-delimited word, if it carries a fact.
fn classify_word(word: &str) -> Option<Claim> {
    if word.starts_with("http://") || word.starts_with("https://") {
        // Handled by `extract_urls`, which keeps punctuation out of the URL.
        return None;
    }
    if let Some(date) = as_date(word) {
        return Some(date);
    }
    if let Some(version) = as_version(word) {
        return Some(version);
    }
    if let Some(path) = as_path(word) {
        return Some(path);
    }
    if let Some(number) = as_number(word) {
        return Some(number);
    }
    as_identifier(word)
}

/// ISO-like dates, and years only when carrying a 年/月/日 marker so that a
/// bare `1999` stays a number.
fn as_date(word: &str) -> Option<Claim> {
    let trimmed = word.trim_end_matches(['年', '月', '日']);
    let digits = trimmed.chars().filter(|c| c.is_ascii_digit()).count();
    let dashes = trimmed.matches('-').count();

    let is_iso = dashes >= 2 && digits >= 6 && trimmed.starts_with(|c: char| c.is_ascii_digit());
    let is_marked_year = trimmed.len() == 4
        && digits == 4
        && matches!(&trimmed[..2], "19" | "20" | "21")
        && trimmed != word;

    if is_iso || is_marked_year {
        return Some(Claim {
            text: trimmed.to_string(),
            class: ClaimClass::Date,
            value: None,
        });
    }
    None
}

/// `1.2.3` / `v1.2.3` style versions.
fn as_version(word: &str) -> Option<Claim> {
    let body = word.strip_prefix('v').unwrap_or(word);
    let parts: Vec<&str> = body.split('.').collect();
    if parts.len() >= 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    {
        return Some(Claim {
            text: word.to_string(),
            class: ClaimClass::Version,
            value: None,
        });
    }
    None
}

fn as_path(word: &str) -> Option<Claim> {
    let looks_like_path = (word.starts_with('/') || word.starts_with("./") || word.contains('/'))
        && word.len() > 2
        && !word.contains("://");
    if looks_like_path {
        return Some(Claim {
            text: word.to_string(),
            class: ClaimClass::Path,
            value: None,
        });
    }
    None
}

fn as_number(word: &str) -> Option<Claim> {
    let candidate = word.trim_start_matches(['约', '~', '≈', '+']);
    if !candidate.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let significant = candidate.chars().filter(|c| c.is_ascii_digit()).count();
    let value = parse_number(candidate)?;
    // A magnitude suffix means the written form is deliberately abbreviated, so
    // its digit count understates the precision — judge those on value instead.
    let abbreviated = MAGNITUDES
        .iter()
        .any(|(s, _)| candidate.to_lowercase().ends_with(s));
    if significant < MIN_SIGNIFICANT_DIGITS && !abbreviated {
        return None;
    }
    Some(Claim {
        text: candidate.to_string(),
        class: ClaimClass::Number,
        value: Some(value),
    })
}

/// Code-like names: qualified paths or multi-word snake/camel identifiers.
fn as_identifier(word: &str) -> Option<Claim> {
    let qualified = word.contains("::") || (word.contains('.') && !word.ends_with('.'));
    let snake = word.contains('_') && word.len() > 4;
    let camel = word.len() > 4
        && word.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && word.chars().skip(1).any(|c| c.is_ascii_uppercase());
    if (qualified || snake || camel)
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '.' | '-'))
    {
        return Some(Claim {
            text: word.to_string(),
            class: ClaimClass::Identifier,
            value: None,
        });
    }
    None
}

/// URLs, trimmed of trailing sentence punctuation.
fn extract_urls(text: &str) -> Vec<Claim> {
    let mut out = Vec::new();
    for scheme in ["https://", "http://"] {
        let mut cursor = 0;
        while let Some(rel) = text[cursor..].find(scheme) {
            let start = cursor + rel;
            let rest = &text[start..];
            let end = rest
                .find(|c: char| {
                    // `[` and `]` cannot appear unencoded in a URL, so they mark
                    // markdown link syntax. Without them `[title](target)`
                    // scanned into one glued pseudo-URL that matched no evidence
                    // and was reported as fabricated. Parentheses are excluded:
                    // they occur inside real URLs and are trimmed below.
                    c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>' | '[' | ']')
                })
                .unwrap_or(rest.len());
            let url = rest[..end].trim_end_matches(['.', ',', ')', ']', '。', '，']);
            if url.len() > scheme.len() {
                out.push(Claim {
                    text: url.to_string(),
                    class: ClaimClass::Url,
                    value: None,
                });
            }
            cursor = start + end.max(1);
        }
    }
    out
}

/// Double-quoted spans of at least [`MIN_QUOTED_LEN`] chars.
fn extract_quoted(text: &str) -> Vec<Claim> {
    let mut out = Vec::new();
    let mut cursor = 0;
    while let Some(rel) = text[cursor..].find('"') {
        let open = cursor + rel + 1;
        let Some(close_rel) = text[open..].find('"') else {
            break;
        };
        let inner = &text[open..open + close_rel];
        if inner.chars().count() >= MIN_QUOTED_LEN {
            out.push(Claim {
                text: inner.to_string(),
                class: ClaimClass::Quoted,
                value: None,
            });
        }
        // Resume past the closing quote so quotes pair up rather than overlap.
        cursor = open + close_rel + 1;
    }
    out
}

/// Drop repeats, keeping first-appearance order so the earliest mention of a
/// fact is the one a finding points at.
fn dedup(claims: Vec<Claim>) -> Vec<Claim> {
    let mut seen = std::collections::HashSet::new();
    claims
        .into_iter()
        .filter(|c| seen.insert((c.class, c.text.clone())))
        .collect()
}

#[cfg(test)]
#[path = "claims_tests.rs"]
mod tests;
