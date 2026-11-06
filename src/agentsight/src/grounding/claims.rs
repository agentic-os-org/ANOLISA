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

use unicode_casefold::UnicodeCaseFold;

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

/// Whether a comma joins two digits and is therefore numeric grouping rather
/// than structure. Path-owned brackets mask this test during structural splitting.
fn is_digit_group_separator(chars: &[char], index: usize) -> bool {
    matches!(chars[index], ',' | '，')
        && index > 0
        && chars[index - 1].is_ascii_digit()
        && chars
            .get(index + 1)
            .is_some_and(|next| next.is_ascii_digit())
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

fn url_scheme_len(chars: &[char], index: usize) -> Option<usize> {
    const HTTP: &[char] = &['h', 't', 't', 'p', ':', '/', '/'];
    const HTTPS: &[char] = &['h', 't', 't', 'p', 's', ':', '/', '/'];
    if chars[index..].starts_with(HTTPS) {
        Some(HTTPS.len())
    } else if chars[index..].starts_with(HTTP) {
        Some(HTTP.len())
    } else {
        None
    }
}

fn decorated_url_end_chars(chars: &[char], index: usize, scheme_len: usize) -> usize {
    let bold = index >= 2 && chars[index - 2..index] == ['*', '*'];
    let parenthesized = index > 0 && chars[index - 1] == '(';
    let mut paren_depth = usize::from(parenthesized);
    let mut end = index + scheme_len;
    while end < chars.len() {
        let c = chars[end];
        if is_url_terminator(c) || (bold && c == '*' && chars.get(end + 1) == Some(&'*')) {
            break;
        }
        if parenthesized {
            match c {
                '(' => paren_depth += 1,
                ')' => {
                    paren_depth -= 1;
                    if paren_depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
        end += 1;
    }
    end
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

fn literal_commas(chars: &[char], quoted: &[bool]) -> Vec<bool> {
    let mut literal = vec![false; chars.len()];
    let mut index = 0;
    while index < chars.len() {
        let Some(scheme_len) = url_scheme_len(chars, index) else {
            index += 1;
            continue;
        };
        let end = decorated_url_end_chars(chars, index, scheme_len);
        let mut content_end = end;
        while content_end > index && is_url_trailing_punctuation(chars[content_end - 1]) {
            content_end -= 1;
        }
        literal[index..content_end].fill(true);
        index = end;
    }

    for (index, c) in chars.iter().enumerate() {
        if quoted[index] && matches!(c, ',' | '，') {
            literal[index] = true;
        }
    }
    literal
}

fn matching_bracket_pairs(chars: &[char], quoted: &[bool]) -> Vec<(usize, usize)> {
    let mut matches = Vec::new();
    let mut open_brackets = Vec::new();
    for (index, c) in chars.iter().enumerate() {
        if quoted[index] {
            continue;
        }
        match c {
            '[' => open_brackets.push(index),
            ']' => {
                if let Some(open) = open_brackets.pop() {
                    matches.push((open, index));
                }
            }
            _ => {}
        }
    }
    matches
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ListItemCompleteness {
    Empty,
    Complete,
    Incomplete,
}

struct PathItemIndex<'a> {
    chars: &'a [char],
    byte_offsets: Vec<usize>,
    next_content: Vec<usize>,
    next_equals: Vec<usize>,
    previous_content: Vec<usize>,
    date_ends: Vec<usize>,
    last_slash: Vec<usize>,
    slashes: Vec<usize>,
    digits: Vec<usize>,
    dashes: Vec<usize>,
    schemes: Vec<usize>,
    equals: Vec<usize>,
    structural_separators: Vec<usize>,
    brackets: Vec<usize>,
    numeric_group_blockers: Vec<usize>,
    list_separators: Vec<bool>,
    list_item_separators: Vec<bool>,
    path_commas: Vec<bool>,
    list_item_starts: Vec<usize>,
    list_item_ends: Vec<usize>,
    item_starts: Vec<usize>,
    item_ends: [Vec<usize>; 2],
}

impl<'a> PathItemIndex<'a> {
    fn new(chars: &'a [char]) -> Self {
        let len = chars.len();
        let mut byte_offsets = vec![0; len + 1];
        let mut attached_equals = vec![false; len];
        let mut token_start = 0;
        let mut segment_start = 0;
        let mut token_attached = false;
        let mut seen_equals = false;
        let mut last_slash = vec![usize::MAX; len + 1];
        let mut slashes = vec![0; len + 1];
        let mut digits = vec![0; len + 1];
        let mut dashes = vec![0; len + 1];
        let mut schemes = vec![0; len + 1];
        let mut equals = vec![0; len + 1];
        let mut structural_separators = vec![0; len + 1];
        let mut brackets = vec![0; len + 1];
        let mut numeric_group_blockers = vec![0; len + 1];
        let list_separators = vec![false; len];
        let list_item_separators = vec![false; len];
        let path_commas = vec![false; len];
        let mut matching_opens = vec![None; len];
        let mut matching_closes = vec![None; len];
        let mut open_brackets = Vec::new();
        for (index, c) in chars.iter().enumerate() {
            byte_offsets[index + 1] = byte_offsets[index] + c.len_utf8();
            if c.is_whitespace() {
                token_start = index + 1;
                segment_start = token_start;
                token_attached = false;
                seen_equals = false;
            }
            match c {
                '=' => {
                    if !seen_equals {
                        token_attached = slashes[index] == slashes[token_start];
                        seen_equals = true;
                    }
                    attached_equals[index] =
                        token_attached && slashes[index] == slashes[segment_start];
                    if attached_equals[index] {
                        segment_start = index + 1;
                    }
                }
                ',' | '，' | ';' | '|' if token_attached => segment_start = index + 1,
                _ => {}
            }
            last_slash[index + 1] = if *c == '/' { index } else { last_slash[index] };
            slashes[index + 1] = slashes[index] + usize::from(*c == '/');
            digits[index + 1] = digits[index] + usize::from(c.is_ascii_digit());
            dashes[index + 1] = dashes[index] + usize::from(*c == '-');
            schemes[index + 1] =
                schemes[index] + usize::from(chars[index..].starts_with(&[':', '/', '/']));
            equals[index + 1] = equals[index] + usize::from(*c == '=');
            structural_separators[index + 1] =
                structural_separators[index] + usize::from(STRUCTURAL_SEPARATORS.contains(c));
            brackets[index + 1] = brackets[index] + usize::from(matches!(c, '[' | ']'));
            numeric_group_blockers[index + 1] = numeric_group_blockers[index]
                + usize::from(
                    !c.is_ascii_digit()
                        && !matches!(c, ',' | '_' | ' ' | '\u{a0}' | '，' | '\u{3000}'),
                );
            match c {
                '[' => open_brackets.push(index),
                ']' => {
                    if let Some(open) = open_brackets.pop() {
                        matching_opens[index] = Some(open);
                        matching_closes[open] = Some(index);
                    }
                }
                _ => {}
            }
        }

        let mut item_starts = vec![0; len + 1];
        for start in 1..=len {
            let previous = start - 1;
            item_starts[start] = match chars[previous] {
                c if c.is_whitespace() || matches!(c, '[' | ',' | '，' | '"' | '\'') => start,
                '=' if attached_equals[previous] => start,
                ']' => matching_opens[previous].map_or(start, |open| item_starts[open]),
                _ => item_starts[previous],
            };
        }
        let mut item_ends = [vec![len; len + 1], vec![len; len + 1]];
        for end in (0..len).rev() {
            for attached in [false, true] {
                let table = usize::from(attached);
                let c = chars[end];
                let boundary = c.is_whitespace()
                    || matches!(c, ']' | ',' | '，' | '"' | '\'')
                    || (attached && (matches!(c, ';' | '|') || attached_equals[end]));
                let next = if boundary {
                    end
                } else if c == '[' {
                    matching_closes[end].map_or(end, |close| item_ends[table][close + 1])
                } else {
                    item_ends[table][end + 1]
                };
                item_ends[table][end] = next;
            }
        }

        let quoted = quoted_chars(chars);
        let mut list_matching_opens = vec![None; len];
        let mut list_matching_closes = vec![None; len];
        let mut list_open_brackets = Vec::new();
        for (index, c) in chars.iter().enumerate() {
            if quoted[index] {
                continue;
            }
            match c {
                '[' => list_open_brackets.push(index),
                ']' => {
                    if let Some(open) = list_open_brackets.pop() {
                        list_matching_opens[index] = Some(open);
                        list_matching_closes[open] = Some(index);
                    }
                }
                _ => {}
            }
        }
        let mut list_item_starts = vec![0; len + 1];
        for start in 1..=len {
            let previous = start - 1;
            list_item_starts[start] = match chars[previous] {
                c if !quoted[previous] && matches!(c, '[' | ',' | '，') => start,
                ']' if !quoted[previous] => {
                    list_matching_opens[previous].map_or(start, |open| list_item_starts[open])
                }
                _ => list_item_starts[previous],
            };
        }
        let mut list_item_ends = vec![len; len + 1];
        for end in (0..len).rev() {
            list_item_ends[end] = match chars[end] {
                c if !quoted[end] && matches!(c, ']' | ',' | '，') => end,
                '[' if !quoted[end] => {
                    list_matching_closes[end].map_or(end, |close| list_item_ends[close + 1])
                }
                _ => list_item_ends[end + 1],
            };
        }

        let mut next_content = vec![len; len + 1];
        let mut next_equals = vec![len; len + 1];
        for index in (0..len).rev() {
            if !is_decoration(chars[index]) {
                next_content[index] = index;
            } else {
                next_content[index] = next_content[index + 1];
            }
            next_equals[index] = if chars[index] == '=' {
                index
            } else {
                next_equals[index + 1]
            };
        }
        let mut previous_content = vec![0; len + 1];
        let mut date_ends = vec![0; len + 1];
        for (index, c) in chars.iter().enumerate() {
            previous_content[index + 1] = if is_decoration(*c) {
                previous_content[index]
            } else {
                index + 1
            };
            date_ends[index + 1] = if matches!(c, '年' | '月' | '日') {
                date_ends[index]
            } else {
                index + 1
            };
        }
        let mut paths = Self {
            chars,
            byte_offsets,
            next_content,
            next_equals,
            previous_content,
            date_ends,
            last_slash,
            slashes,
            digits,
            dashes,
            schemes,
            equals,
            structural_separators,
            brackets,
            numeric_group_blockers,
            list_separators,
            list_item_separators,
            path_commas,
            list_item_starts,
            list_item_ends,
            item_starts,
            item_ends,
        };
        let mut completeness_cache = std::collections::HashMap::new();
        let mut detected_item_starts = vec![0; len + 1];
        let mut open_brackets = Vec::new();
        for (index, c) in chars.iter().enumerate() {
            if !quoted[index] {
                match c {
                    '[' => open_brackets.push(index),
                    ']' => {
                        open_brackets.pop();
                    }
                    ',' | '，' if !open_brackets.is_empty() => {
                        let left = (detected_item_starts[index], index);
                        let right = (index + 1, paths.list_item_ends[index + 1]);
                        let combined = paths.list_item_content_range(left.0, right.1);
                        if combined.0 < combined.1
                            && paths.classifies_as_path(combined.0, combined.1)
                        {
                            let left = list_item_completeness(
                                &paths,
                                left.0,
                                left.1,
                                &mut completeness_cache,
                            );
                            let right = list_item_completeness(
                                &paths,
                                right.0,
                                right.1,
                                &mut completeness_cache,
                            );
                            paths.path_commas[index] = !(left == ListItemCompleteness::Complete
                                && right == ListItemCompleteness::Complete);
                        }
                    }
                    _ => {}
                }
            }
            detected_item_starts[index + 1] = match c {
                '[' if !quoted[index] => index + 1,
                ',' | '，' if !quoted[index] && !paths.path_commas[index] => index + 1,
                ']' if !quoted[index] => {
                    list_matching_opens[index].map_or(index + 1, |open| detected_item_starts[open])
                }
                _ => detected_item_starts[index],
            };
        }

        for start in 1..=len {
            let previous = start - 1;
            paths.item_starts[start] = match chars[previous] {
                c if c.is_whitespace() || matches!(c, '[' | '"' | '\'') => start,
                c if matches!(c, ',' | '，') && !paths.path_commas[previous] => start,
                '=' if attached_equals[previous] => start,
                ']' => matching_opens[previous].map_or(start, |open| paths.item_starts[open]),
                _ => paths.item_starts[previous],
            };
        }
        for end in (0..len).rev() {
            for attached in [false, true] {
                let table = usize::from(attached);
                let c = chars[end];
                let path_comma = matches!(c, ',' | '，') && paths.path_commas[end];
                let boundary = c.is_whitespace()
                    || matches!(c, ']' | '"' | '\'')
                    || (matches!(c, ',' | '，') && !path_comma)
                    || (attached && (matches!(c, ';' | '|') || attached_equals[end]));
                let next = if boundary {
                    end
                } else if c == '[' {
                    matching_closes[end].map_or(end, |close| paths.item_ends[table][close + 1])
                } else {
                    paths.item_ends[table][end + 1]
                };
                paths.item_ends[table][end] = next;
            }
        }

        for start in 1..=len {
            let previous = start - 1;
            let next = match chars[previous] {
                c if !quoted[previous] && c == '[' => start,
                c if !quoted[previous]
                    && matches!(c, ',' | '，')
                    && !paths.path_commas[previous] =>
                {
                    start
                }
                ']' if !quoted[previous] => {
                    list_matching_opens[previous].map_or(start, |open| paths.list_item_starts[open])
                }
                _ => paths.list_item_starts[previous],
            };
            paths.list_item_starts[start] = next;
        }
        for end in (0..len).rev() {
            let next = match chars[end] {
                c if !quoted[end] && c == ']' => end,
                c if !quoted[end] && matches!(c, ',' | '，') && !paths.path_commas[end] => end,
                '[' if !quoted[end] => {
                    list_matching_closes[end].map_or(end, |close| paths.list_item_ends[close + 1])
                }
                _ => paths.list_item_ends[end + 1],
            };
            paths.list_item_ends[end] = next;
        }

        let mut open_brackets = Vec::new();
        for (index, c) in chars.iter().enumerate() {
            if quoted[index] {
                continue;
            }
            match c {
                '[' => open_brackets.push(index),
                ']' => {
                    if let Some(open) = open_brackets.pop()
                        && paths.list_separators[open]
                        && let Some(&parent) = open_brackets.last()
                    {
                        paths.list_separators[parent] = true;
                    }
                }
                ',' | '，' if !open_brackets.is_empty() && !paths.path_commas[index] => {
                    let separates = if !is_digit_group_separator(chars, index) {
                        true
                    } else {
                        let (left, right) = list_item_ranges_around_comma(&paths, index);
                        let left =
                            list_item_completeness(&paths, left.0, left.1, &mut completeness_cache);
                        let right = list_item_completeness(
                            &paths,
                            right.0,
                            right.1,
                            &mut completeness_cache,
                        );
                        left != ListItemCompleteness::Incomplete
                            && right != ListItemCompleteness::Incomplete
                            && (left == ListItemCompleteness::Complete
                                || right == ListItemCompleteness::Complete)
                    };
                    paths.list_item_separators[index] = separates;
                    if let Some(&open) = open_brackets.last() {
                        paths.list_separators[open] |= separates;
                    }
                }
                _ => {}
            }
        }
        paths
    }

    fn count(prefix: &[usize], start: usize, end: usize) -> usize {
        prefix[end] - prefix[start]
    }

    fn classification_range(&self, start: usize, end: usize) -> (usize, usize) {
        (self.next_content[start], self.previous_content[end])
    }

    fn list_item_content_range(&self, mut start: usize, mut end: usize) -> (usize, usize) {
        while start < end && self.chars[start].is_whitespace() {
            start += 1;
        }
        while start < end && self.chars[end - 1].is_whitespace() {
            end -= 1;
        }
        if start == end {
            return (0, 0);
        }
        let (start, end) = self.classification_range(start, end);
        if start < end { (start, end) } else { (0, 0) }
    }

    fn has_equals(&self, start: usize, end: usize) -> bool {
        Self::count(&self.equals, start, end) > 0
    }

    fn has_slash(&self, start: usize, end: usize) -> bool {
        Self::count(&self.slashes, start, end) > 0
    }

    fn has_list_separator(&self, open: usize) -> bool {
        self.list_separators[open]
    }

    fn has_attached_key(&self, start: usize, end: usize) -> bool {
        let equals = self.next_equals[start];
        equals > start
            && equals < end
            && Self::count(&self.structural_separators, start, equals) == 0
            && !self.has_slash(start, equals)
            && Self::count(&self.schemes, start, equals) == 0
    }

    fn classifies_as_path(&self, start: usize, end: usize) -> bool {
        let (start, end) = self.classification_range(start, end);
        if start >= end || self.byte_offsets[end] - self.byte_offsets[start] <= 2 {
            return false;
        }
        let slashes = Self::count(&self.slashes, start, end);
        if slashes == 0 || url_scheme_len(self.chars, start).is_some() {
            return false;
        }

        let date_end = self.date_ends[end].max(start);
        let digits = Self::count(&self.digits, start, date_end);
        let dashes = Self::count(&self.dashes, start, date_end);
        let is_iso = dashes >= 2 && digits >= 6 && self.chars[start].is_ascii_digit();
        let is_marked_year = self.byte_offsets[date_end] - self.byte_offsets[start] == 4
            && digits == 4
            && matches!(
                (self.chars[start], self.chars[start + 1]),
                ('1', '9') | ('2', '0') | ('2', '1')
            )
            && date_end != end;
        if is_iso || is_marked_year || Self::count(&self.schemes, start, end) > 0 {
            return false;
        }

        if self.chars[start] == '/'
            || (self.chars[start] == '.' && self.chars[start + 1] == '/')
            || (self.chars[start] == '~' && self.chars[start + 1] == '/')
            || slashes >= 2
        {
            return true;
        }

        let tail_start = self.last_slash[end] + 1;
        let scan_start = end.saturating_sub(7).max(tail_start);
        let Some(dot) = (scan_start..end)
            .rev()
            .find(|index| self.chars[*index] == '.')
        else {
            return false;
        };
        dot > tail_start
            && dot + 1 < end
            && self.byte_offsets[end] - self.byte_offsets[dot + 1] <= 6
            && self.chars[dot + 1..end]
                .iter()
                .all(|c| c.is_ascii_alphanumeric())
    }

    fn classifies_as_attached_path(&self, start: usize, end: usize) -> bool {
        if !self.has_attached_key(start, end) {
            return false;
        }
        let mut value_start = self.next_equals[start] + 1;
        while value_start < end {
            let c = self.chars[value_start];
            let relative_path = c == '.'
                && (self.chars[value_start..].starts_with(&['.', '/'])
                    || self.chars[value_start..].starts_with(&['.', '.', '/']));
            if relative_path || !is_decoration(c) {
                break;
            }
            value_start += 1;
        }
        self.classifies_as_path(value_start, end)
    }
}

fn path_item_start(paths: &PathItemIndex<'_>, open: usize) -> usize {
    paths.item_starts[open]
}

fn path_item_end(paths: &PathItemIndex<'_>, item_start: usize, close: usize) -> usize {
    let attached = usize::from(item_start > 0 && paths.chars[item_start - 1] == '=');
    paths.item_ends[attached][close + 1]
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

fn has_empty_list_prefix(value: &str) -> bool {
    let mut depth = 0usize;
    let mut matched = false;
    for c in value.chars() {
        if !is_decoration(c) {
            break;
        }
        match c {
            '[' => depth += 1,
            ']' if depth > 0 => {
                depth -= 1;
                matched = true;
            }
            _ => {}
        }
    }
    matched
}

fn is_list_wrapper_decoration(c: char) -> bool {
    matches!(
        c,
        ')' | '}' | '`' | '*' | '"' | '\'' | '>' | '”' | '’' | '」' | '）' | '】'
    )
}

fn attached_field_after_closing_bracket_at<'a>(
    value: &'a str,
    value_char_start: usize,
    quoted: &[bool],
) -> Option<(&'a str, usize)> {
    if !value.contains(']') {
        return None;
    }
    let mut bracket_depth = 0usize;
    let mut decorated_prefix = true;
    for (char_index, (index, c)) in value.char_indices().enumerate() {
        let prefix_decoration = decorated_prefix && is_decoration(c);
        if quoted[value_char_start + char_index] && !prefix_decoration {
            decorated_prefix = false;
            continue;
        }
        if !is_decoration(c) {
            decorated_prefix = false;
        }
        match c {
            '[' => bracket_depth += 1,
            ']' => {
                bracket_depth = bracket_depth.saturating_sub(1);
                if bracket_depth == 0 {
                    let start = index + c.len_utf8();
                    let suffix = &value[start..];
                    let attached = suffix.trim_start_matches(is_list_wrapper_decoration);
                    if split_attached_value(attached).is_some() {
                        let decorations = suffix[..suffix.len() - attached.len()].chars().count();
                        return Some((attached, value_char_start + char_index + 1 + decorations));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn attached_field_after_closing_bracket(value: &str) -> Option<&str> {
    let chars: Vec<char> = value.chars().collect();
    let quoted = quoted_chars(&chars);
    attached_field_after_closing_bracket_at(value, 0, &quoted).map(|(attached, _)| attached)
}

fn attached_value_after_list_residue<'a>(
    value: &'a str,
    value_char_start: usize,
    original: &str,
    quoted_original: &mut Option<Vec<bool>>,
) -> Option<(&'a str, usize)> {
    if let Some(suffix) = value.strip_prefix("[]") {
        let attached = suffix.trim_start_matches(is_list_wrapper_decoration);
        if split_attached_value(attached).is_some() {
            let decorations = suffix[..suffix.len() - attached.len()].chars().count();
            return Some((attached, value_char_start + 2 + decorations));
        }
    }
    if !value
        .trim_start_matches(|c| is_decoration(c) && c != '[' && c != ']')
        .starts_with('[')
    {
        return None;
    }
    let quoted = quoted_original.get_or_insert_with(|| {
        let chars: Vec<char> = original.chars().collect();
        quoted_chars(&chars)
    });
    attached_field_after_closing_bracket_at(value, value_char_start, quoted)
}

fn standalone_relative_path_has_equals(key: &str, word: &str, value: &str) -> bool {
    if key == "path"
        || value.starts_with(['/', '.'])
        || value.chars().next().is_some_and(is_decoration)
    {
        return false;
    }
    let Some(slash) = value.find('/') else {
        return false;
    };
    let Some(equals) = value.find('=') else {
        return false;
    };
    slash < equals && classify_word(word).is_some_and(|claim| claim.class == ClaimClass::Path)
}

fn classify_attached_value(mut word: &str) -> Option<Claim> {
    let original = word;
    let mut word_char_start = 0;
    let mut quoted_original = None;
    loop {
        let (key, value) = split_attached_value(word)?;
        if standalone_relative_path_has_equals(key, word, value) {
            return classify_word(word);
        }
        let value_char_start = word_char_start + key.chars().count() + 1;
        if let Some((attached, attached_char_start)) = attached_value_after_list_residue(
            value,
            value_char_start,
            original,
            &mut quoted_original,
        ) {
            word = attached;
            word_char_start = attached_char_start;
            continue;
        }
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
        let glued_path = split_attached_value(trimmed)
            .is_some_and(|(_, nested_value)| !nested_value.contains('='));
        if claim.class == ClaimClass::Path
            && (has_unmatched_closing_bracket_before_path(trimmed)
                || has_numeric_colon_suffix(trimmed)
                || (has_empty_list_prefix(value) && glued_path))
        {
            return None;
        }
        if has_following_attached_field(value) {
            return None;
        }
        return Some(claim);
    }
}

fn bracket_belongs_to_path(
    chars: &[char],
    open: usize,
    close: usize,
    paths: &PathItemIndex<'_>,
    preserve_attached_decoration: bool,
) -> bool {
    let item_start = path_item_start(paths, open);
    if open == item_start {
        if !preserve_attached_decoration || item_start == 0 || chars[item_start - 1] != '=' {
            return false;
        }
        let item_end = path_item_end(paths, item_start, close);
        if !paths.has_slash(item_start, item_end)
            || paths.has_list_separator(open)
            || !paths.classifies_as_path(item_start, item_end)
        {
            return false;
        }
        let mut attached_start = item_start - 1;
        while attached_start > 0 {
            let c = chars[attached_start - 1];
            if c.is_whitespace() || STRUCTURAL_SEPARATORS.contains(&c) {
                break;
            }
            attached_start -= 1;
        }
        return paths.classifies_as_attached_path(attached_start, item_end);
    }
    let item_end = path_item_end(paths, item_start, close);
    if !paths.has_slash(item_start, item_end) {
        return false;
    }
    if !paths.has_slash(item_start, open) && paths.has_list_separator(open) {
        return false;
    }
    if !paths.has_equals(item_start, item_end) {
        return paths.classifies_as_path(item_start, item_end);
    }
    if paths.classifies_as_path(item_start, item_end) {
        return true;
    }
    paths.classifies_as_attached_path(item_start, item_end)
}

fn list_item_ranges_around_comma(
    paths: &PathItemIndex<'_>,
    index: usize,
) -> ((usize, usize), (usize, usize)) {
    let left_start = paths.list_item_starts[index];
    let right_end = paths.list_item_ends[index + 1];
    ((left_start, index), (index + 1, right_end))
}

fn list_item_completeness(
    paths: &PathItemIndex<'_>,
    start: usize,
    end: usize,
    cache: &mut std::collections::HashMap<(usize, usize), ListItemCompleteness>,
) -> ListItemCompleteness {
    let range = paths.list_item_content_range(start, end);
    if range.0 == range.1 {
        return ListItemCompleteness::Empty;
    }
    if paths.classifies_as_path(range.0, range.1) {
        return ListItemCompleteness::Complete;
    }
    let has_equals = paths.has_equals(range.0, range.1);
    if has_equals && !paths.has_attached_key(range.0, range.1) {
        return ListItemCompleteness::Incomplete;
    }
    if paths.has_slash(range.0, range.1) && !has_equals {
        return ListItemCompleteness::Incomplete;
    }
    if !has_equals && PathItemIndex::count(&paths.brackets, range.0, range.1) > 0 {
        return ListItemCompleteness::Incomplete;
    }
    if !has_equals
        && PathItemIndex::count(&paths.numeric_group_blockers, range.0, range.1) == 0
        && PathItemIndex::count(&paths.digits, range.0, range.1) > 0
    {
        return ListItemCompleteness::Complete;
    }
    if let Some(completeness) = cache.get(&range) {
        return *completeness;
    }
    let item: String = paths.chars[range.0..range.1].iter().collect();
    let completeness = if complete_list_item(&item) {
        ListItemCompleteness::Complete
    } else {
        ListItemCompleteness::Incomplete
    };
    cache.insert(range, completeness);
    completeness
}

fn complete_list_item(item: &str) -> bool {
    let item = trim_decoration(item.trim());
    parse_number(item).is_some()
        || as_version(item).is_some()
        || as_date(item).is_some()
        || as_path(item).is_some()
        || classify_attached_value(item).is_some_and(|claim| {
            matches!(
                claim.class,
                ClaimClass::Number | ClaimClass::Version | ClaimClass::Date
            )
        })
}

fn comma_separates_list_items(paths: &PathItemIndex<'_>, index: usize) -> bool {
    paths.list_item_separators[index]
}

fn is_reference_definition(chars: &[char], open: usize, close: usize) -> bool {
    let mut line_start = open;
    while line_start > 0 && chars[line_start - 1] == ' ' && open - line_start < 3 {
        line_start -= 1;
    }
    chars.get(close + 1) == Some(&':') && (line_start == 0 || chars[line_start - 1] == '\n')
}

fn normalize_markdown_reference_label(chars: &[char]) -> Vec<char> {
    let mut normalized = Vec::with_capacity(chars.len());
    let mut pending_space = false;
    for c in chars {
        if c.is_whitespace() {
            pending_space = !normalized.is_empty();
        } else {
            if pending_space {
                normalized.push(' ');
                pending_space = false;
            }
            normalized.extend(c.case_fold());
        }
    }
    normalized
}

/// Separate commas inside matched square brackets before whitespace tokenization.
///
/// Bracketed commas are list structure when they separate complete numeric
/// items. Commas inside quotes, URLs, and a path's own nested brackets remain
/// content, so compact outer lists can mix those values with numbers.
fn separate_bracketed_comma_lists(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let quoted = quoted_chars(&chars);
    let literal_commas = literal_commas(&chars, &quoted);
    let paths = PathItemIndex::new(&chars);
    let serialized_json = serialized_json_container_bytes(text);
    let mut attached_path_suffixes = vec![false; chars.len()];
    for index in 0..chars.len() {
        let serialized_newline = chars[index] == '\\' && chars.get(index + 1) == Some(&'n');
        if quoted[index]
            || (!matches!(chars[index], ';' | '|' | ':' | '：' | '；' | '。')
                && !serialized_newline)
        {
            continue;
        }
        let item = paths.list_item_content_range(paths.list_item_starts[index], index);
        attached_path_suffixes[index] = item.0 < item.1
            && paths.has_slash(item.0, item.1)
            && PathItemIndex::count(&paths.schemes, item.0, item.1) == 0
            && paths.has_attached_key(index + 1, paths.list_item_ends[index + 1]);
    }

    let mut open_brackets = Vec::new();
    let mut matched_brackets = Vec::new();
    let mut depth_deltas = vec![0i32; chars.len() + 1];
    let mut path_deltas = vec![0i32; chars.len() + 1];
    let mut path_item_until = 0;
    let mut path_item_is_path = false;
    let mut rejected_path_item = None;
    for (index, c) in chars.iter().enumerate() {
        if quoted[index] {
            continue;
        }
        match c {
            '[' => open_brackets.push((index, false)),
            ']' => {
                if let Some((open, has_nested)) = open_brackets.pop() {
                    matched_brackets.push((open, index, !has_nested));
                    if let Some(parent) = open_brackets.last_mut() {
                        parent.1 = true;
                    }
                    depth_deltas[open + 1] += 1;
                    depth_deltas[index] -= 1;
                    let follows_attached_path_boundary = open > 0
                        && (matches!(chars[open - 1], ';' | '|' | '；' | '。')
                            || (open > 1 && chars[open - 2..open] == ['\\', 'n']))
                        && paths.has_list_separator(open)
                        && chars.get(index + 1).is_none_or(|c| {
                            c.is_whitespace()
                                || STRUCTURAL_SEPARATORS.contains(c)
                                || is_list_wrapper_decoration(*c)
                        });
                    let path_owned = if index < path_item_until {
                        path_item_is_path
                    } else {
                        let item_start = path_item_start(&paths, open);
                        let item_end = path_item_end(&paths, item_start, index);
                        let cacheable = open != item_start;
                        let item = (item_start, item_end);
                        let path_owned = if follows_attached_path_boundary
                            || (cacheable && rejected_path_item == Some(item))
                        {
                            false
                        } else {
                            bracket_belongs_to_path(&chars, open, index, &paths, true)
                        };
                        rejected_path_item = (cacheable && !path_owned).then_some(item);
                        path_item_until = if path_owned { item_end } else { index + 1 };
                        path_item_is_path = path_owned;
                        path_owned
                    };
                    if path_owned {
                        path_deltas[open + 1] += 1;
                        path_deltas[index] -= 1;
                    }
                }
            }
            _ => {}
        }
    }

    let reference_definitions: std::collections::HashSet<Vec<char>> = matched_brackets
        .iter()
        .filter(|(open, close, innermost)| {
            *innermost && is_reference_definition(&chars, *open, *close)
        })
        .map(|(open, close, _)| normalize_markdown_reference_label(&chars[open + 1..*close]))
        .collect();
    let mut label_deltas = vec![0i32; chars.len() + 1];
    for (open, close, innermost) in matched_brackets {
        let link_follower = chars.get(close + 1).is_some_and(|c| matches!(c, '(' | '['));
        let shortcut_reference = innermost
            && reference_definitions
                .contains(&normalize_markdown_reference_label(&chars[open + 1..close]));
        if (link_follower || shortcut_reference) && open + 1 < close {
            label_deltas[open + 1] += 1;
            label_deltas[close] -= 1;
        }
    }

    let mut label_grouping_whitespace = vec![false; chars.len()];
    for index in 1..chars.len() {
        if !matches!(chars[index], ',' | '，') || !chars[index - 1].is_ascii_digit() {
            continue;
        }
        let mut end = index + 1;
        while end < chars.len() && chars[end].is_whitespace() {
            end += 1;
        }
        if end > index + 1 && end < chars.len() && chars[end].is_ascii_digit() {
            label_grouping_whitespace[index + 1..end].fill(true);
        }
    }

    // Difference ranges make matched nesting linear-time; an unmatched '[' does
    // not change the meaning of later unbracketed thousands separators.
    let mut bracket_depth = 0;
    let mut path_depth = 0;
    let mut label_depth = 0;
    chars
        .iter()
        .enumerate()
        .filter_map(|(index, c)| {
            bracket_depth += depth_deltas[index];
            path_depth += path_deltas[index];
            label_depth += label_deltas[index];
            // An earlier unmatched prose quote must not turn a later valid JSON
            // string into list syntax; removing its commas makes replay impossible.
            if serialized_json[paths.byte_offsets[index]] {
                return Some(*c);
            }
            if label_depth > 0 && label_grouping_whitespace[index] {
                return None;
            }
            if bracket_depth > 0
                && path_depth == 0
                && label_depth == 0
                && attached_path_suffixes[index]
            {
                return Some(' ');
            }
            Some(
                if bracket_depth > 0
                    && path_depth == 0
                    && label_depth == 0
                    && matches!(c, ',' | '，')
                    && !literal_commas[index]
                    && comma_separates_list_items(&paths, index)
                {
                    ' '
                } else {
                    *c
                },
            )
        })
        .collect()
}

/// Split serialized structure without consuming URLs or the brackets and commas
/// that belong to a path. URL terminators delimit the URL, so fields after a
/// quoted URL remain visible to the suffix scan.
fn structural_segments(word: &str) -> Vec<String> {
    let chars: Vec<char> = word.chars().collect();
    let quoted = quoted_chars(&chars);
    let matched_brackets = matching_bracket_pairs(&chars, &quoted);
    let mut quoted_paths = vec![false; chars.len()];
    let mut index = 0;
    while index < chars.len() {
        if !quoted[index] {
            index += 1;
            continue;
        }
        let start = index;
        while index < chars.len() && quoted[index] {
            index += 1;
        }
        let candidate: String = chars[start..index].iter().collect();
        if classify_word(trim_attached_decoration(&candidate))
            .is_some_and(|claim| claim.class == ClaimClass::Path)
        {
            quoted_paths[start..index].fill(true);
        }
    }
    let paths = PathItemIndex::new(&chars);
    let mut path_deltas = vec![0i32; chars.len() + 1];
    let mut path_item_until = 0;
    let mut rejected_path_item = None;
    for (open, close) in matched_brackets {
        if close < path_item_until {
            continue;
        }
        let item_start = path_item_start(&paths, open);
        let item_end = path_item_end(&paths, item_start, close);
        let cacheable = open != item_start;
        let item = (item_start, item_end);
        let path_owned = if cacheable && rejected_path_item == Some(item) {
            false
        } else {
            bracket_belongs_to_path(&chars, open, close, &paths, false)
        };
        rejected_path_item = (cacheable && !path_owned).then_some(item);
        path_item_until = if path_owned { item_end } else { close + 1 };
        if path_owned {
            path_deltas[open] += 1;
            path_deltas[path_item_until] -= 1;
        }
    }

    let mut segments = Vec::new();
    let mut current = String::new();
    let mut current_starts_with_digit = None;
    let mut path_depth = 0;
    let mut index = 0;
    while index < chars.len() {
        path_depth += path_deltas[index];
        let path_char = path_depth > 0 || quoted_paths[index];
        if !path_char && let Some(scheme_len) = url_scheme_len(&chars, index) {
            if !current.is_empty() {
                segments.push(std::mem::take(&mut current));
                current_starts_with_digit = None;
            }
            index = decorated_url_end_chars(&chars, index, scheme_len);
            continue;
        }
        let c = chars[index];
        let digit_group = !path_char && is_digit_group_separator(&chars, index);
        let follows_date = digit_group
            && current_starts_with_digit == Some(true)
            && current
                .get(current.len().saturating_sub(10)..)
                .is_some_and(|suffix| as_date(suffix).is_some());
        if digit_group && !follows_date {
            index += 1;
            continue;
        }
        let identifier_colon = c == ':'
            && (chars.get(index + 1) == Some(&':') || (index > 0 && chars[index - 1] == ':'));
        if !path_char && !identifier_colon && STRUCTURAL_SEPARATORS.contains(&c) {
            if !current.is_empty() {
                segments.push(std::mem::take(&mut current));
                current_starts_with_digit = None;
            }
        } else {
            if current_starts_with_digit.is_none() && !is_decoration(c) {
                current_starts_with_digit = Some(c.is_ascii_digit());
            }
            current.push(c);
        }
        index += 1;
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
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

fn embedded_json_string_prefix(value: &str) -> Option<(usize, serde_json::Value)> {
    let rest = value.strip_prefix('"')?;
    let mut backslashes = 0usize;
    for (offset, c) in rest.char_indices() {
        if c == '"' && backslashes % 2 == 0 {
            let end = offset + 2;
            return serde_json::from_str::<serde_json::Value>(&value[..end])
                .ok()
                .filter(|value| matches!(value, serde_json::Value::String(_)))
                .map(|value| (end, value));
        }
        if c == '\\' {
            backslashes += 1;
        } else {
            backslashes = 0;
        }
    }
    None
}

fn serialized_json_container_bytes(text: &str) -> Vec<bool> {
    let mut protected = vec![false; text.len()];
    let mut cursor = 0;
    let mut backslashes = 0usize;
    while cursor < text.len() {
        let Some(c) = text[cursor..].chars().next() else {
            break;
        };
        if c == '"'
            && backslashes % 2 == 0
            && let Some((end, serde_json::Value::String(decoded))) =
                embedded_json_string_prefix(&text[cursor..])
            && parse_serialized_json_container(&decoded).is_some()
        {
            protected[cursor..cursor + end].fill(true);
            cursor += end;
            backslashes = 0;
            continue;
        }
        if c == '\\' {
            backslashes += 1;
        } else {
            backslashes = 0;
        }
        cursor += c.len_utf8();
    }
    protected
}

enum SerializedJsonNode {
    Number(serde_json::Number),
    String(String),
    Array(Vec<usize>),
    Object(Vec<(String, usize)>),
    Other,
}

enum SerializedJsonFrame {
    Array(usize),
    Object {
        node: usize,
        key: Option<String>,
        positions: std::collections::HashMap<String, usize>,
    },
}

#[derive(Clone, Copy)]
enum SerializedJsonState {
    Value,
    ArrayValueOrEnd,
    ArrayCommaOrEnd,
    ObjectKeyOrEnd,
    ObjectKey,
    ObjectColon,
    ObjectCommaOrEnd,
    End,
}

fn serialized_json_after_value(frames: &[SerializedJsonFrame]) -> SerializedJsonState {
    match frames.last() {
        Some(SerializedJsonFrame::Array(_)) => SerializedJsonState::ArrayCommaOrEnd,
        Some(SerializedJsonFrame::Object { .. }) => SerializedJsonState::ObjectCommaOrEnd,
        None => SerializedJsonState::End,
    }
}

// `serde_json::Value` descends containers recursively. Decoded strings can be
// adversarially deep, so container parsing and replay both use explicit stacks.
fn parse_serialized_json_container(text: &str) -> Option<(Vec<SerializedJsonNode>, usize)> {
    let bytes = text.as_bytes();
    let mut cursor = 0;
    let mut nodes = Vec::new();
    let mut frames = Vec::new();
    let mut root = None;
    let mut state = SerializedJsonState::Value;
    loop {
        while bytes
            .get(cursor)
            .is_some_and(|c| matches!(c, b' ' | b'\n' | b'\r' | b'\t'))
        {
            cursor += 1;
        }
        match state {
            SerializedJsonState::End => {
                if cursor != bytes.len() {
                    return None;
                }
                let root = root?;
                return matches!(
                    nodes.get(root),
                    Some(SerializedJsonNode::Array(_) | SerializedJsonNode::Object(_))
                )
                .then_some((nodes, root));
            }
            SerializedJsonState::ArrayValueOrEnd => {
                if bytes.get(cursor) == Some(&b']') {
                    cursor += 1;
                    if !matches!(frames.pop(), Some(SerializedJsonFrame::Array(_))) {
                        return None;
                    }
                    state = serialized_json_after_value(&frames);
                } else {
                    state = SerializedJsonState::Value;
                }
            }
            SerializedJsonState::ArrayCommaOrEnd => match bytes.get(cursor)? {
                b',' => {
                    cursor += 1;
                    state = SerializedJsonState::Value;
                }
                b']' => {
                    cursor += 1;
                    if !matches!(frames.pop(), Some(SerializedJsonFrame::Array(_))) {
                        return None;
                    }
                    state = serialized_json_after_value(&frames);
                }
                _ => return None,
            },
            SerializedJsonState::ObjectKeyOrEnd | SerializedJsonState::ObjectKey => {
                if matches!(state, SerializedJsonState::ObjectKeyOrEnd)
                    && bytes.get(cursor) == Some(&b'}')
                {
                    cursor += 1;
                    if !matches!(frames.pop(), Some(SerializedJsonFrame::Object { .. })) {
                        return None;
                    }
                    state = serialized_json_after_value(&frames);
                    continue;
                }
                let Some((end, serde_json::Value::String(key))) =
                    embedded_json_string_prefix(&text[cursor..])
                else {
                    return None;
                };
                cursor += end;
                let Some(SerializedJsonFrame::Object { key: slot, .. }) = frames.last_mut() else {
                    return None;
                };
                if slot.replace(key).is_some() {
                    return None;
                }
                state = SerializedJsonState::ObjectColon;
            }
            SerializedJsonState::ObjectColon => {
                if bytes.get(cursor) != Some(&b':') {
                    return None;
                }
                cursor += 1;
                state = SerializedJsonState::Value;
            }
            SerializedJsonState::ObjectCommaOrEnd => match bytes.get(cursor)? {
                b',' => {
                    cursor += 1;
                    state = SerializedJsonState::ObjectKey;
                }
                b'}' => {
                    cursor += 1;
                    if !matches!(frames.pop(), Some(SerializedJsonFrame::Object { .. })) {
                        return None;
                    }
                    state = serialized_json_after_value(&frames);
                }
                _ => return None,
            },
            SerializedJsonState::Value => {
                let (node, container) = match *bytes.get(cursor)? {
                    b'[' => {
                        cursor += 1;
                        let node = nodes.len();
                        nodes.push(SerializedJsonNode::Array(Vec::new()));
                        (node, Some(SerializedJsonFrame::Array(node)))
                    }
                    b'{' => {
                        cursor += 1;
                        let node = nodes.len();
                        nodes.push(SerializedJsonNode::Object(Vec::new()));
                        (
                            node,
                            Some(SerializedJsonFrame::Object {
                                node,
                                key: None,
                                positions: std::collections::HashMap::new(),
                            }),
                        )
                    }
                    b'"' => {
                        let Some((end, serde_json::Value::String(text))) =
                            embedded_json_string_prefix(&text[cursor..])
                        else {
                            return None;
                        };
                        cursor += end;
                        let node = nodes.len();
                        nodes.push(SerializedJsonNode::String(text));
                        (node, None)
                    }
                    b't' if text[cursor..].starts_with("true") => {
                        cursor += 4;
                        let node = nodes.len();
                        nodes.push(SerializedJsonNode::Other);
                        (node, None)
                    }
                    b'f' if text[cursor..].starts_with("false") => {
                        cursor += 5;
                        let node = nodes.len();
                        nodes.push(SerializedJsonNode::Other);
                        (node, None)
                    }
                    b'n' if text[cursor..].starts_with("null") => {
                        cursor += 4;
                        let node = nodes.len();
                        nodes.push(SerializedJsonNode::Other);
                        (node, None)
                    }
                    b'0'..=b'9' | b'-' => {
                        let start = cursor;
                        while bytes.get(cursor).is_some_and(|c| {
                            !matches!(c, b' ' | b'\n' | b'\r' | b'\t' | b',' | b']' | b'}')
                        }) {
                            cursor += 1;
                        }
                        let number =
                            serde_json::from_str::<serde_json::Number>(&text[start..cursor])
                                .ok()?;
                        let node = nodes.len();
                        nodes.push(SerializedJsonNode::Number(number));
                        (node, None)
                    }
                    _ => return None,
                };
                match frames.last_mut() {
                    Some(SerializedJsonFrame::Array(parent)) => {
                        let SerializedJsonNode::Array(values) = &mut nodes[*parent] else {
                            return None;
                        };
                        values.push(node);
                    }
                    Some(SerializedJsonFrame::Object {
                        node: parent,
                        key,
                        positions,
                    }) => {
                        let key = key.take()?;
                        let SerializedJsonNode::Object(values) = &mut nodes[*parent] else {
                            return None;
                        };
                        if let Some(position) = positions.get(&key) {
                            values[*position].1 = node;
                        } else {
                            positions.insert(key.clone(), values.len());
                            values.push((key, node));
                        }
                    }
                    None => {
                        if root.replace(node).is_some() {
                            return None;
                        }
                    }
                }
                match container {
                    Some(frame @ SerializedJsonFrame::Array(_)) => {
                        frames.push(frame);
                        state = SerializedJsonState::ArrayValueOrEnd;
                    }
                    Some(frame @ SerializedJsonFrame::Object { .. }) => {
                        frames.push(frame);
                        state = SerializedJsonState::ObjectKeyOrEnd;
                    }
                    None => state = serialized_json_after_value(&frames),
                }
            }
        }
    }
}

enum JsonTypedInput {
    Value(serde_json::Value),
    Text(String),
    Serialized { tree: usize, node: usize },
}

enum RootJson {
    Value(serde_json::Value),
    Serialized {
        nodes: Vec<SerializedJsonNode>,
        root: usize,
    },
}

fn json_typed_claims(value: serde_json::Value) -> Vec<Claim> {
    json_typed_claims_from(JsonTypedInput::Value(value), Vec::new())
}

fn serialized_json_typed_claims(nodes: Vec<SerializedJsonNode>, root: usize) -> Vec<Claim> {
    json_typed_claims_from(
        JsonTypedInput::Serialized {
            tree: 0,
            node: root,
        },
        vec![nodes],
    )
}

fn json_typed_claims_from(
    input: JsonTypedInput,
    mut trees: Vec<Vec<SerializedJsonNode>>,
) -> Vec<Claim> {
    let mut claims = Vec::new();
    let mut pending = vec![input];
    while let Some(input) = pending.pop() {
        match input {
            JsonTypedInput::Value(serde_json::Value::Number(number)) => {
                if let Some(claim) = as_number(&number.to_string()) {
                    claims.push(claim);
                }
            }
            JsonTypedInput::Value(serde_json::Value::String(text)) => {
                pending.push(JsonTypedInput::Text(text));
            }
            JsonTypedInput::Value(serde_json::Value::Array(values)) => {
                pending.extend(values.into_iter().rev().map(JsonTypedInput::Value));
            }
            JsonTypedInput::Value(serde_json::Value::Object(values)) => {
                let keys: Vec<String> = values.keys().cloned().collect();
                pending.extend(values.into_values().rev().map(JsonTypedInput::Value));
                pending.extend(keys.into_iter().rev().map(JsonTypedInput::Text));
            }
            JsonTypedInput::Value(_) => {}
            JsonTypedInput::Text(text) => {
                let trimmed = text.trim_start();
                if trimmed.starts_with('"') {
                    if let Ok(decoded) = serde_json::from_str::<String>(&text) {
                        pending.push(JsonTypedInput::Text(decoded));
                        continue;
                    }
                } else if matches!(trimmed.as_bytes().first(), Some(b'[' | b'{')) {
                    if let Some((nodes, root)) = parse_serialized_json_container(&text) {
                        let tree = trees.len();
                        trees.push(nodes);
                        pending.push(JsonTypedInput::Serialized { tree, node: root });
                        continue;
                    }
                }
                claims.extend(
                    extract_claims(&text).into_iter().filter(|claim| {
                        matches!(claim.class, ClaimClass::Number | ClaimClass::Path)
                    }),
                );
            }
            JsonTypedInput::Serialized { tree, node } => match &trees[tree][node] {
                SerializedJsonNode::Number(number) => {
                    if let Some(claim) = as_number(&number.to_string()) {
                        claims.push(claim);
                    }
                }
                SerializedJsonNode::String(text) => {
                    pending.push(JsonTypedInput::Text(text.clone()));
                }
                SerializedJsonNode::Array(values) => {
                    pending.extend(
                        values
                            .iter()
                            .rev()
                            .map(|node| JsonTypedInput::Serialized { tree, node: *node }),
                    );
                }
                SerializedJsonNode::Object(values) => {
                    pending.extend(
                        values
                            .iter()
                            .rev()
                            .map(|(_, node)| JsonTypedInput::Serialized { tree, node: *node }),
                    );
                    pending.extend(
                        values
                            .iter()
                            .rev()
                            .map(|(key, _)| key.clone())
                            .map(JsonTypedInput::Text),
                    );
                }
                SerializedJsonNode::Other => {}
            },
        }
    }
    claims
}

/// Extract every verifiable claim from a message.
pub fn extract_claims(text: &str) -> Vec<Claim> {
    let root_json = if matches!(text.trim_start().as_bytes().first(), Some(b'[' | b'{')) {
        parse_serialized_json_container(text)
            .map(|(nodes, root)| RootJson::Serialized { nodes, root })
    } else {
        serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .filter(|value| matches!(value, serde_json::Value::String(_)))
            .map(RootJson::Value)
    };
    let mut claims = Vec::new();
    claims.extend(extract_urls(text));
    claims.extend(extract_quoted(text));
    let normalized = separate_bracketed_comma_lists(text);
    for json_word in normalized.split_whitespace() {
        let attached_after_residue =
            if root_json.is_none() && split_attached_value(json_word).is_none() {
                attached_field_after_closing_bracket(json_word)
            } else {
                None
            };
        let attached_value_claim = if root_json.is_some() {
            None
        } else {
            classify_attached_value(attached_after_residue.unwrap_or(json_word))
        };
        // Strip decoration, not content. Agents habitually wrap paths and
        // identifiers in markdown (`/var/log`, **error**), and a claim carrying
        // those marks would never match the plain form sitting in the evidence —
        // a path the user typed themselves would be reported as sourceless.
        let undecorated = trim_decoration(json_word);
        let word = if root_json.is_some() {
            json_word
        } else if looks_like_path(undecorated) {
            trim_attached_decoration(json_word)
        } else {
            undecorated
        };
        if word.is_empty() {
            continue;
        }
        let whole_word_claim = if root_json.is_some() {
            None
        } else {
            attached_value_claim.or_else(|| classify_complete_word(word))
        };
        let whole_word_claim = whole_word_claim.filter(|claim| {
            claim.class != ClaimClass::Path
                || json_word == word
                || !has_following_attached_field(json_word)
        });
        let word_stands_alone = whole_word_claim.is_some();
        if let Some(claim) = whole_word_claim {
            claims.push(claim);
        }
        if word_stands_alone && let Some(attached) = attached_after_residue {
            let prefix = &json_word[..json_word.len() - attached.len()];
            for segment in structural_segments(prefix) {
                if let Some(claim) = classify_structural_segment(&segment) {
                    claims.push(claim);
                }
            }
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
            let fields = if root_json.is_some() {
                None
            } else {
                attached_field_segments(json_word)
            };
            if let Some(fields) = fields {
                for field in fields {
                    let complete_path = || {
                        classify_complete_word(trim_attached_decoration(field))
                            .filter(|claim| claim.class == ClaimClass::Path)
                    };
                    if let Some(claim) = classify_attached_value(field).or_else(complete_path) {
                        claims.push(claim);
                    } else {
                        for segment in structural_segments(field) {
                            if let Some(claim) = classify_structural_segment(&segment) {
                                claims.push(claim);
                            }
                        }
                    }
                }
            } else if !json_word.starts_with("http://")
                && !json_word.starts_with("https://")
                && json_word.contains(STRUCTURAL_SEPARATORS)
            {
                for segment in structural_segments(json_word) {
                    if let Some(claim) = classify_structural_segment(&segment) {
                        claims.push(claim);
                    }
                }
            }
        }
    }
    // In valid JSON, a comma separates values rather than grouping digits.
    // Replace only numeric claims so the existing literal/quote extraction
    // remains intact; string values still use the human-text numeric rules.
    match root_json {
        Some(RootJson::Value(value)) => {
            claims.retain(|claim| !matches!(claim.class, ClaimClass::Number | ClaimClass::Path));
            claims.extend(json_typed_claims(value));
        }
        Some(RootJson::Serialized { nodes, root }) => {
            claims.retain(|claim| !matches!(claim.class, ClaimClass::Number | ClaimClass::Path));
            claims.extend(serialized_json_typed_claims(nodes, root));
        }
        None => {}
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
