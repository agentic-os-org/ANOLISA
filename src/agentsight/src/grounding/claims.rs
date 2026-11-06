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
    '=', ':', ',', ';', '\\', '{', '}', '[', ']', '|', '。', '，', '、', '：', '；', '！', '？',
    '（', '）', '「', '」', '【', '】',
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

fn is_decoration(c: char) -> bool {
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
            | '\\'
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
            | '（'
            | '）'
            | '【'
            | '】'
    )
}

fn trim_decoration(word: &str) -> &str {
    word.trim_matches(is_decoration)
}

fn is_url_terminator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>' | '[' | ']')
}

fn is_url_trailing_punctuation(c: char) -> bool {
    matches!(c, '.' | ',' | ')' | ']' | '。' | '，')
}

fn single_quote_opens(chars: &[char], index: usize) -> bool {
    let decade_apostrophe = chars.get(index + 1).is_some_and(|c| c.is_ascii_digit())
        && chars.get(index + 2).is_some_and(|c| c.is_ascii_digit())
        && chars.get(index + 3).is_some_and(|c| matches!(c, 's' | 'S'));
    !decade_apostrophe
        && (index == 0
            || chars[index - 1].is_whitespace()
            || matches!(chars[index - 1], '[' | '{' | '(' | ':' | '=' | ',' | '，'))
}

fn single_quote_closes(chars: &[char], index: usize) -> bool {
    index + 1 == chars.len()
        || chars[index + 1].is_whitespace()
        || matches!(
            chars[index + 1],
            ']' | '}' | ')' | ',' | '，' | ':' | ';' | '.' | '!' | '?'
        )
}

fn quote_closer_table(closer: char) -> Option<usize> {
    match closer {
        '"' => Some(0),
        '\'' => Some(1),
        '”' => Some(2),
        '’' => Some(3),
        '」' => Some(4),
        _ => None,
    }
}

fn quote_closers(chars: &[char]) -> [Vec<usize>; 5] {
    let mut closers: [Vec<usize>; 5] = Default::default();
    let mut escaped = false;
    for (index, c) in chars.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if *c == '\\' {
            escaped = true;
            continue;
        }
        let Some(table) = quote_closer_table(*c) else {
            continue;
        };
        if *c != '\'' || single_quote_closes(chars, index) {
            closers[table].push(index);
        }
    }
    closers
}

fn closing_quote(closers: &[Vec<usize>; 5], open: usize, closer: char) -> Option<usize> {
    let positions = &closers[quote_closer_table(closer)?];
    let next = positions.partition_point(|close| *close <= open);
    positions.get(next).copied()
}

fn escaped_by_backslash(chars: &[char], index: usize) -> bool {
    let mut start = index;
    while start > 0 && chars[start - 1] == '\\' {
        start -= 1;
    }
    (index - start) % 2 == 1
}

fn closing_escaped_double_quote(chars: &[char], open: usize) -> Option<usize> {
    chars
        .iter()
        .enumerate()
        .skip(open + 1)
        .find_map(|(index, c)| (*c == '"' && escaped_by_backslash(chars, index)).then_some(index))
}

fn quoted_chars(chars: &[char]) -> Vec<bool> {
    let mut quoted = vec![false; chars.len()];
    let closers = quote_closers(chars);
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '"' && escaped_by_backslash(chars, index) {
            if let Some(close) = closing_escaped_double_quote(chars, index) {
                quoted[index - 1..=close].fill(true);
                index = close + 1;
            } else {
                index += 1;
            }
            continue;
        }
        let closer = match chars[index] {
            '"' => Some('"'),
            '\'' if single_quote_opens(chars, index) => Some('\''),
            '“' => Some('”'),
            '‘' => Some('’'),
            '「' => Some('」'),
            _ => None,
        };
        if let Some(closer) = closer
            && let Some(close) = closing_quote(&closers, index, closer)
        {
            quoted[index..=close].fill(true);
            index = close + 1;
            continue;
        }
        index += 1;
    }
    quoted
}

fn decorated_url_end(text: &str, start: usize) -> usize {
    let rest = &text[start..];
    let bold = text[..start].ends_with("**");
    let parenthesized = text[..start].ends_with('(');
    let mut paren_depth = usize::from(parenthesized);
    for (offset, c) in rest.char_indices() {
        if is_url_terminator(c) {
            return offset;
        }
        if bold && c == '*' && rest[offset..].starts_with("**") {
            return offset;
        }
        if parenthesized {
            match c {
                '(' => paren_depth += 1,
                ')' => {
                    paren_depth -= 1;
                    if paren_depth == 0 {
                        return offset;
                    }
                }
                _ => {}
            }
        }
    }
    rest.len()
}

fn trim_url_trailing_punctuation(url: &str) -> &str {
    let mut matched_closes = vec![false; url.len()];
    let mut open_parens = Vec::new();
    for (index, c) in url.char_indices() {
        match c {
            '(' => open_parens.push(index),
            ')' => {
                if open_parens.pop().is_some() {
                    matched_closes[index] = true;
                }
            }
            _ => {}
        }
    }

    let mut end = url.len();
    while let Some((index, c)) = url[..end].char_indices().next_back() {
        if (c == ')' && matched_closes[index]) || !is_url_trailing_punctuation(c) {
            break;
        }
        end = index;
    }
    &url[..end]
}

fn trim_attached_decoration(value: &str) -> &str {
    let mut start = 0;
    for (index, c) in value.char_indices() {
        let relative_path =
            c == '.' && (value[index..].starts_with("./") || value[index..].starts_with("../"));
        if relative_path || !is_decoration(c) {
            break;
        }
        start = index + c.len_utf8();
    }
    let value = &value[start..];

    let mut matched_closes = vec![false; value.len()];
    let mut open_brackets = Vec::new();
    for (index, c) in value.char_indices() {
        match c {
            '[' => open_brackets.push(index),
            ']' => {
                if let Some(open) = open_brackets.pop() {
                    matched_closes[index] = open + 1 < index;
                }
            }
            _ => {}
        }
    }

    let mut end = value.len();
    let preserve_path_brackets = looks_like_path(value);
    while let Some((index, c)) = value[..end].char_indices().next_back() {
        if !is_decoration(c) {
            break;
        }
        if c == ']' && matched_closes[index] && preserve_path_brackets {
            break;
        }
        end = index;
    }
    &value[..end]
}

fn split_attached_value(word: &str) -> Option<(&str, &str)> {
    let mut equals = None;
    for (index, c) in word.char_indices() {
        if c == '=' {
            equals = Some(index);
            break;
        }
        if STRUCTURAL_SEPARATORS.contains(&c) {
            return None;
        }
    }
    let key = &word[..equals?];
    let value = &word[key.len() + 1..];
    if key.is_empty() || key.contains("://") || key.contains('/') {
        return None;
    }
    Some((key, value))
}

fn attached_field_boundaries(value: &str) -> Vec<(usize, usize)> {
    let chars: Vec<char> = value.chars().collect();
    let quoted = quoted_chars(&chars);
    let mut depth_deltas = vec![0i32; value.len() + 1];
    let mut open_brackets = Vec::new();
    for (char_index, (index, c)) in value.char_indices().enumerate() {
        if quoted[char_index] {
            continue;
        }
        match c {
            '[' => open_brackets.push(index),
            ']' => {
                if let Some(open) = open_brackets.pop() {
                    depth_deltas[open + 1] += 1;
                    depth_deltas[index] -= 1;
                }
            }
            _ => {}
        }
    }

    let mut boundaries = Vec::new();
    let mut bracket_depth = 0;
    for (char_index, (index, c)) in value.char_indices().enumerate() {
        bracket_depth += depth_deltas[index];
        if quoted[char_index] {
            continue;
        }
        match c {
            ';' | '|' | '；' | '。' if bracket_depth == 0 => {
                boundaries.push((index, c.len_utf8()));
            }
            '\\' if bracket_depth == 0 && value[index + 1..].starts_with('n') => {
                boundaries.push((index, c.len_utf8() + 1));
            }
            ',' | '，' | ':' | '：' if bracket_depth == 0 => {
                let suffix = &value[index + c.len_utf8()..];
                if split_attached_value(suffix).is_some() || suffix.starts_with('"') {
                    boundaries.push((index, c.len_utf8()));
                }
            }
            _ => {}
        }
    }
    boundaries
}

fn has_following_attached_field(value: &str) -> bool {
    let boundaries = attached_field_boundaries(value);
    boundaries
        .iter()
        .enumerate()
        .any(|(position, (index, len))| {
            if !matches!(
                value[*index..].chars().next(),
                Some(';' | '|' | '；' | '。')
            ) {
                return true;
            }
            let start = index + len;
            let end = boundaries
                .get(position + 1)
                .map_or(value.len(), |(index, _)| *index);
            let suffix = trim_decoration(&value[start..end]);
            split_attached_value(suffix).is_some()
                || suffix.starts_with('"')
                || classify_structural_segment(suffix)
                    .is_some_and(|claim| claim.can_anchor_finding())
        })
}

fn attached_field_segments(word: &str) -> Option<Vec<&str>> {
    let (_, raw_value) = split_attached_value(word)?;
    let value = trim_attached_decoration(raw_value);
    let url_end = if value.starts_with("http://") || value.starts_with("https://") {
        let value_start = word.len() - raw_value.len() + raw_value.find(value)?;
        Some(value_start + decorated_url_end(word, value_start))
    } else {
        None
    };
    let boundaries: Vec<_> = attached_field_boundaries(word)
        .into_iter()
        .filter(|(index, _)| url_end.is_none_or(|end| *index >= end))
        .collect();
    if boundaries.is_empty() {
        return None;
    }
    let mut segments = Vec::new();
    let mut start = 0;
    for (index, len) in boundaries {
        if start < index {
            segments.push(&word[start..index]);
        }
        start = index + len;
    }
    if start < word.len() {
        segments.push(&word[start..]);
    }
    Some(segments)
}

fn has_numeric_colon_suffix(word: &str) -> bool {
    word.rsplit_once(':')
        .is_some_and(|(_, suffix)| parse_number(trim_decoration(suffix)).is_some())
}

fn has_unmatched_closing_bracket_before_path(word: &str) -> bool {
    let mut bracket_depth = 0usize;
    for c in word.chars() {
        match c {
            '/' => return false,
            '[' => bracket_depth += 1,
            ']' if bracket_depth == 0 => return true,
            ']' => bracket_depth -= 1,
            _ => {}
        }
    }
    false
}

fn classify_complete_word(word: &str) -> Option<Claim> {
    let claim = classify_word(word)?;
    let attached = split_attached_value(word).is_some();
    if (claim.class == ClaimClass::Path && (attached || has_numeric_colon_suffix(word)))
        || has_unmatched_closing_bracket_before_path(word)
    {
        return None;
    }
    if (claim.class != ClaimClass::Path || attached) && has_following_attached_field(word) {
        return None;
    }
    Some(claim)
}

fn classify_attached_value(word: &str) -> Option<Claim> {
    let (_, value) = split_attached_value(word)?;
    let trimmed = trim_attached_decoration(value);
    let claim = classify_word(trimmed)?;
    if claim.class == ClaimClass::Date
        && claim
            .text
            .chars()
            .any(|c| STRUCTURAL_SEPARATORS.contains(&c))
    {
        return None;
    }
    if claim.class == ClaimClass::Path
        && (has_unmatched_closing_bracket_before_path(trimmed) || has_numeric_colon_suffix(trimmed))
    {
        return None;
    }
    if has_following_attached_field(value) {
        return None;
    }
    Some(claim)
}

fn classify_structural_segment(segment: &str) -> Option<Claim> {
    let claim = classify_word(segment);
    if segment.contains('/') {
        let trimmed = trim_attached_decoration(segment);
        if trimmed.len() != segment.len()
            && let Some(path) =
                classify_word(trimmed).filter(|claim| claim.class == ClaimClass::Path)
        {
            return Some(path);
        }
    }
    let trimmed = trim_decoration(segment);
    if trimmed.len() == segment.len()
        || claim
            .as_ref()
            .is_some_and(|claim| claim.class != ClaimClass::Date)
    {
        return claim;
    }
    classify_word(trimmed).or(claim)
}

/// Extract every verifiable claim from a message.
pub fn extract_claims(text: &str) -> Vec<Claim> {
    let mut claims = Vec::new();
    claims.extend(extract_urls(text));
    claims.extend(extract_quoted(text));
    for json_word in text.split_whitespace() {
        let attached_value_claim = classify_attached_value(json_word);
        // Strip decoration, not content. Agents habitually wrap paths and
        // identifiers in markdown (`/var/log`, **error**), and a claim carrying
        // those marks would never match the plain form sitting in the evidence —
        // a path the user typed themselves would be reported as sourceless.
        let undecorated = trim_decoration(json_word);
        let word = if looks_like_path(undecorated) {
            trim_attached_decoration(json_word)
        } else {
            undecorated
        };
        if word.is_empty() {
            continue;
        }
        let whole_word_claim = attached_value_claim.or_else(|| classify_complete_word(word));
        let whole_word_claim = whole_word_claim.filter(|claim| {
            claim.class != ClaimClass::Path
                || json_word == word
                || !has_following_attached_field(json_word)
        });
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
        // The sparing guard matches `classify_word`'s URL check exactly:
        // a bare `http`-prefixed word that is not a URL (`http_code=242391`,
        // curl -w style) is a key=value fact token, and sparing it hid its
        // value from the grounding check entirely.
        if !word_stands_alone {
            if let Some(fields) = attached_field_segments(json_word) {
                for field in fields {
                    let complete_path = || {
                        classify_complete_word(trim_attached_decoration(field))
                            .filter(|claim| claim.class == ClaimClass::Path)
                    };
                    if let Some(claim) = classify_attached_value(field).or_else(complete_path) {
                        claims.push(claim);
                    } else {
                        let degrouped = splice_digit_groups(field);
                        for segment in degrouped.split(STRUCTURAL_SEPARATORS) {
                            if let Some(claim) = classify_structural_segment(segment) {
                                claims.push(claim);
                            }
                        }
                    }
                }
            } else if !json_word.starts_with("http://")
                && !json_word.starts_with("https://")
                && json_word.contains(STRUCTURAL_SEPARATORS)
            {
                let degrouped = splice_digit_groups(json_word);
                for segment in degrouped.split(STRUCTURAL_SEPARATORS) {
                    if let Some(claim) = classify_structural_segment(segment) {
                        claims.push(claim);
                    }
                }
            }
        }
    }
    // In valid JSON, a comma separates values rather than grouping digits.
    // Replace only numeric claims so the existing literal/quote extraction
    // remains intact; string values still use the human-text numeric rules.
    if let Ok(value @ (serde_json::Value::Array(_) | serde_json::Value::Object(_))) =
        serde_json::from_str::<serde_json::Value>(text)
    {
        claims.retain(|claim| claim.class != ClaimClass::Number);
        let mut pending = vec![&value];
        while let Some(value) = pending.pop() {
            match value {
                serde_json::Value::Number(number) => {
                    if let Some(claim) = as_number(&number.to_string()) {
                        claims.push(claim);
                    }
                }
                serde_json::Value::String(text) => {
                    claims.extend(
                        extract_claims(text)
                            .into_iter()
                            .filter(|claim| claim.class == ClaimClass::Number),
                    );
                }
                serde_json::Value::Array(values) => pending.extend(values.iter().rev()),
                serde_json::Value::Object(values) => {
                    // Keys are payload too: a histogram keyed by status code
                    // carries its codes in the keys, and dropping them loses
                    // facts the text pass this walk replaces did read.
                    claims.extend(
                        values
                            .keys()
                            .flat_map(|key| extract_claims(key))
                            .filter(|claim| claim.class == ClaimClass::Number),
                    );
                    pending.extend(values.values().rev());
                }
                _ => {}
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
    if !looks_like_path(word) {
        return None;
    }
    Some(Claim {
        text: word.to_string(),
        class: ClaimClass::Path,
        value: None,
    })
}

/// Whether a token names a file rather than merely containing a slash.
///
/// A bare `contains('/')` made prose like `CI/CD`, `3/4` or `MB/s` a `Path`,
/// which is a hard class: it can anchor a finding, so an answer that merely
/// mentions one could be reported as fabricated. A single separator only
/// counts when the tail looks like a file name, and two separators mean a
/// nested relative path on their own.
fn looks_like_path(word: &str) -> bool {
    if word.len() <= 2 || word.contains("://") {
        return false;
    }
    if word.starts_with('/') || word.starts_with("./") || word.starts_with("~/") {
        return true;
    }
    let separators = word.matches('/').count();
    let tail = word.rsplit('/').next().unwrap_or(word);
    separators >= 2 || (separators == 1 && has_extension(tail))
}

/// Whether a path segment ends in a short alphanumeric extension.
fn has_extension(segment: &str) -> bool {
    let Some((stem, extension)) = segment.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && !extension.is_empty()
        && extension.len() <= 6
        && extension.chars().all(|c| c.is_ascii_alphanumeric())
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
            let end = decorated_url_end(text, start);
            let url = trim_url_trailing_punctuation(&rest[..end]);
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
