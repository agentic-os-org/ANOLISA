//! Finds static Cookie header shell-word boundaries without executing shell syntax.

#[derive(Clone, Copy)]
enum InitialContext {
    Unquoted,
    Quoted(u8),
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ShellContext {
    Unquoted,
    SingleQuoted,
    DoubleQuoted,
}

enum ScanResult {
    Precise {
        value_len: usize,
        quote_closed: bool,
    },
    Dynamic,
}

struct CookieHeaderMatch {
    start: usize,
    prefix_end: usize,
    value_start: usize,
    prefix_quote: Option<u8>,
    initial_context: InitialContext,
}

enum HeaderSearchResult {
    Match(CookieHeaderMatch),
    Dynamic,
    None,
}

enum HeaderParseResult {
    Match(CookieHeaderMatch),
    Dynamic,
    NoMatch,
}

struct StaticChar {
    byte: u8,
    raw_start: usize,
    raw_end: usize,
    context_before: ShellContext,
    context_after: ShellContext,
}

enum StaticCharResult {
    Char(StaticChar),
    Dynamic,
    End(ShellContext),
}

enum CandidateStartResult {
    Match(ShellContext),
    Dynamic,
    NoMatch,
}

enum StaticOptionPrefix {
    Decoded {
        bytes: Vec<u8>,
        context: ShellContext,
    },
    Dynamic,
}

/// Redacts static Cookie header values through their complete shell word.
///
/// Dynamic shell syntax is redacted as a whole because finding its boundary
/// requires the complete Bash and Zsh grammars.
pub(super) fn redact_shell_cookie_headers(text: &str) -> (String, bool) {
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut changed = false;

    loop {
        let header = match find_cookie_header(text, cursor) {
            HeaderSearchResult::Match(header) => header,
            HeaderSearchResult::Dynamic => return ("<redacted>".to_string(), true),
            HeaderSearchResult::None => break,
        };
        let ScanResult::Precise {
            value_len,
            quote_closed,
        } = shell_word_value_len(&text[header.value_start..], header.initial_context)
        else {
            return ("<redacted>".to_string(), true);
        };

        output.push_str(&text[cursor..header.start]);
        output.push_str(&text[header.start..header.prefix_end]);
        output.push_str("<redacted>");
        if let Some(prefix_quote) = header.prefix_quote {
            let value_keeps_prefix_quote = matches!(header.initial_context, InitialContext::Quoted(quote) if quote == prefix_quote);
            if !value_keeps_prefix_quote || quote_closed {
                output.push(prefix_quote as char);
            }
        }
        cursor = header.value_start + value_len;
        changed = true;
    }
    output.push_str(&text[cursor..]);

    (output, changed)
}

fn find_cookie_header(text: &str, search_start: usize) -> HeaderSearchResult {
    let bytes = text.as_bytes();
    for start in search_start..bytes.len() {
        if !could_start_cookie_header(bytes[start]) {
            continue;
        }
        match is_cookie_header_candidate_start(bytes, start) {
            CandidateStartResult::Match(context) => {
                match parse_cookie_header(bytes, start, context) {
                    HeaderParseResult::Match(header) => {
                        return HeaderSearchResult::Match(header);
                    }
                    HeaderParseResult::Dynamic => return HeaderSearchResult::Dynamic,
                    HeaderParseResult::NoMatch => {}
                }
            }
            CandidateStartResult::Dynamic => {
                for context in [
                    ShellContext::Unquoted,
                    ShellContext::SingleQuoted,
                    ShellContext::DoubleQuoted,
                ] {
                    if !matches!(
                        parse_cookie_header(bytes, start, context),
                        HeaderParseResult::NoMatch
                    ) {
                        return HeaderSearchResult::Dynamic;
                    }
                }
            }
            CandidateStartResult::NoMatch => {}
        }
    }

    HeaderSearchResult::None
}

fn could_start_cookie_header(byte: u8) -> bool {
    matches!(byte, b'c' | b'C' | b's' | b'S' | b'\'' | b'"' | b'\\')
}

fn is_cookie_header_candidate_start(bytes: &[u8], start: usize) -> CandidateStartResult {
    if start == 0 || is_shell_word_boundary(bytes[start - 1]) {
        return CandidateStartResult::Match(ShellContext::Unquoted);
    }

    let word_start = shell_word_start(bytes, start);
    match decode_static_option_prefix(&bytes[word_start..start]) {
        StaticOptionPrefix::Decoded {
            bytes: option_prefix,
            context,
        } if option_prefix == b"--header=" || is_attached_short_header_option(&option_prefix) => {
            CandidateStartResult::Match(context)
        }
        StaticOptionPrefix::Dynamic => CandidateStartResult::Dynamic,
        StaticOptionPrefix::Decoded { .. } => CandidateStartResult::NoMatch,
    }
}

fn shell_word_start(bytes: &[u8], end: usize) -> usize {
    let mut word_start = 0;
    let mut cursor = 0;
    while cursor < end {
        if bytes[cursor] == b'$' && bytes.get(cursor + 1) == Some(&b'{') {
            cursor = parameter_expansion_end(bytes, cursor + 2, end);
            continue;
        }
        if is_shell_word_boundary(bytes[cursor]) {
            word_start = cursor + 1;
        }
        cursor += 1;
    }
    word_start
}

fn parameter_expansion_end(bytes: &[u8], mut cursor: usize, end: usize) -> usize {
    let mut depth = 1usize;
    while cursor < end {
        match bytes[cursor] {
            b'$' if bytes.get(cursor + 1) == Some(&b'{') => {
                depth += 1;
                cursor += 2;
            }
            // Quotes and nested substitutions require the complete shell
            // grammar to prove which brace closes this expansion. Consume
            // the remaining prefix so the caller treats it as dynamic.
            b'\'' | b'"' | b'`' => return end,
            b'$' if bytes.get(cursor + 1) == Some(&b'(') => return end,
            b'<' | b'>' if bytes.get(cursor + 1) == Some(&b'(') => return end,
            b'}' => {
                depth -= 1;
                cursor += 1;
                if depth == 0 {
                    return cursor;
                }
            }
            b'\\' => cursor = skip_shell_escape(bytes, cursor),
            _ => cursor += 1,
        }
    }
    end
}

fn decode_static_option_prefix(raw_prefix: &[u8]) -> StaticOptionPrefix {
    let mut decoded = Vec::with_capacity(raw_prefix.len());
    let mut cursor = 0;
    let mut context = ShellContext::Unquoted;

    loop {
        match next_static_char(raw_prefix, cursor, context) {
            StaticCharResult::Char(character) => {
                decoded.push(character.byte);
                cursor = character.raw_end;
                context = character.context_after;
            }
            StaticCharResult::Dynamic => return StaticOptionPrefix::Dynamic,
            StaticCharResult::End(final_context) => {
                return StaticOptionPrefix::Decoded {
                    bytes: decoded,
                    context: final_context,
                };
            }
        }
    }
}

fn is_attached_short_header_option(option_prefix: &[u8]) -> bool {
    option_prefix.starts_with(b"-")
        && !option_prefix.starts_with(b"--")
        && option_prefix.ends_with(b"H")
        && option_prefix[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'#')
}

fn parse_cookie_header(
    bytes: &[u8],
    start: usize,
    initial_context: ShellContext,
) -> HeaderParseResult {
    for expected_prefix in [b"set-cookie:".as_slice(), b"cookie:".as_slice()] {
        let mut cursor = start;
        let mut context = initial_context;
        if !consume_static_literal(bytes, &mut cursor, &mut context, expected_prefix) {
            continue;
        }

        match consume_static_whitespace(bytes, &mut cursor, &mut context) {
            StaticCharResult::Dynamic => return HeaderParseResult::Dynamic,
            StaticCharResult::Char(_) | StaticCharResult::End(_) => {}
        }
        let prefix_end = cursor;
        let prefix_quote = context_quote(context);
        let mut name_seen = false;

        loop {
            match next_static_char(bytes, cursor, context) {
                StaticCharResult::Char(character) if character.byte == b'=' && name_seen => {
                    return cookie_header_match(start, prefix_end, prefix_quote, character);
                }
                StaticCharResult::Char(character)
                    if character.byte.is_ascii_whitespace() && name_seen =>
                {
                    match consume_static_whitespace(bytes, &mut cursor, &mut context) {
                        StaticCharResult::Dynamic => return HeaderParseResult::Dynamic,
                        StaticCharResult::Char(_) | StaticCharResult::End(_) => {}
                    }
                    return match next_static_char(bytes, cursor, context) {
                        StaticCharResult::Char(character) if character.byte == b'=' => {
                            cookie_header_match(start, prefix_end, prefix_quote, character)
                        }
                        StaticCharResult::Dynamic => HeaderParseResult::Dynamic,
                        StaticCharResult::Char(_) | StaticCharResult::End(_) => {
                            HeaderParseResult::NoMatch
                        }
                    };
                }
                StaticCharResult::Char(character)
                    if matches!(character.byte, b'=' | b';' | b',') =>
                {
                    return HeaderParseResult::NoMatch;
                }
                StaticCharResult::Char(character) => {
                    name_seen = true;
                    cursor = character.raw_end;
                    context = character.context_after;
                }
                StaticCharResult::Dynamic => return HeaderParseResult::Dynamic,
                StaticCharResult::End(_) => return HeaderParseResult::NoMatch,
            }
        }
    }

    HeaderParseResult::NoMatch
}

fn cookie_header_match(
    start: usize,
    prefix_end: usize,
    prefix_quote: Option<u8>,
    equals: StaticChar,
) -> HeaderParseResult {
    HeaderParseResult::Match(CookieHeaderMatch {
        start,
        prefix_end,
        value_start: equals.raw_end,
        prefix_quote,
        initial_context: initial_context(equals.context_after),
    })
}

fn consume_static_literal(
    bytes: &[u8],
    cursor: &mut usize,
    context: &mut ShellContext,
    expected: &[u8],
) -> bool {
    for expected_byte in expected {
        let StaticCharResult::Char(character) = next_static_char(bytes, *cursor, *context) else {
            return false;
        };
        if !character.byte.eq_ignore_ascii_case(expected_byte) {
            return false;
        }
        *cursor = character.raw_end;
        *context = character.context_after;
    }

    true
}

fn consume_static_whitespace(
    bytes: &[u8],
    cursor: &mut usize,
    context: &mut ShellContext,
) -> StaticCharResult {
    loop {
        match next_static_char(bytes, *cursor, *context) {
            StaticCharResult::Char(character) if character.byte.is_ascii_whitespace() => {
                *cursor = character.raw_end;
                *context = character.context_after;
            }
            StaticCharResult::Char(character) => {
                *cursor = character.raw_start;
                *context = character.context_before;
                return StaticCharResult::Char(character);
            }
            other => return other,
        }
    }
}

fn next_static_char(
    bytes: &[u8],
    mut cursor: usize,
    mut context: ShellContext,
) -> StaticCharResult {
    loop {
        let Some(&byte) = bytes.get(cursor) else {
            return StaticCharResult::End(context);
        };
        match context {
            ShellContext::Unquoted => match byte {
                b'\'' => {
                    context = ShellContext::SingleQuoted;
                    cursor += 1;
                }
                b'"' => {
                    context = ShellContext::DoubleQuoted;
                    cursor += 1;
                }
                b'\\' => match bytes.get(cursor + 1) {
                    Some(b'\r') if bytes.get(cursor + 2) == Some(&b'\n') => cursor += 3,
                    Some(b'\n') => cursor += 2,
                    Some(&escaped) => {
                        return StaticCharResult::Char(StaticChar {
                            byte: escaped,
                            raw_start: cursor,
                            raw_end: cursor + 2,
                            context_before: context,
                            context_after: context,
                        });
                    }
                    None => return StaticCharResult::End(context),
                },
                b'$' | b'`' => return StaticCharResult::Dynamic,
                boundary if is_shell_word_boundary(boundary) => {
                    return StaticCharResult::End(context);
                }
                _ => {
                    return StaticCharResult::Char(StaticChar {
                        byte,
                        raw_start: cursor,
                        raw_end: cursor + 1,
                        context_before: context,
                        context_after: context,
                    });
                }
            },
            ShellContext::SingleQuoted => {
                if byte == b'\'' {
                    context = ShellContext::Unquoted;
                    cursor += 1;
                } else {
                    return StaticCharResult::Char(StaticChar {
                        byte,
                        raw_start: cursor,
                        raw_end: cursor + 1,
                        context_before: context,
                        context_after: context,
                    });
                }
            }
            ShellContext::DoubleQuoted => match byte {
                b'"' => {
                    context = ShellContext::Unquoted;
                    cursor += 1;
                }
                b'\\' => match bytes.get(cursor + 1) {
                    Some(b'\r') if bytes.get(cursor + 2) == Some(&b'\n') => cursor += 3,
                    Some(b'\n') => cursor += 2,
                    Some(&escaped) if matches!(escaped, b'$' | b'`' | b'"' | b'\\') => {
                        return StaticCharResult::Char(StaticChar {
                            byte: escaped,
                            raw_start: cursor,
                            raw_end: cursor + 2,
                            context_before: context,
                            context_after: context,
                        });
                    }
                    Some(_) => {
                        return StaticCharResult::Char(StaticChar {
                            byte,
                            raw_start: cursor,
                            raw_end: cursor + 1,
                            context_before: context,
                            context_after: context,
                        });
                    }
                    None => return StaticCharResult::End(context),
                },
                b'$' | b'`' => return StaticCharResult::Dynamic,
                _ => {
                    return StaticCharResult::Char(StaticChar {
                        byte,
                        raw_start: cursor,
                        raw_end: cursor + 1,
                        context_before: context,
                        context_after: context,
                    });
                }
            },
        }
    }
}

fn initial_context(context: ShellContext) -> InitialContext {
    match context {
        ShellContext::Unquoted => InitialContext::Unquoted,
        ShellContext::SingleQuoted => InitialContext::Quoted(b'\''),
        ShellContext::DoubleQuoted => InitialContext::Quoted(b'"'),
    }
}

fn context_quote(context: ShellContext) -> Option<u8> {
    match context {
        ShellContext::Unquoted => None,
        ShellContext::SingleQuoted => Some(b'\''),
        ShellContext::DoubleQuoted => Some(b'"'),
    }
}

fn shell_word_value_len(value: &str, initial_context: InitialContext) -> ScanResult {
    let bytes = value.as_bytes();
    let (mut cursor, quote_closed) = match initial_context {
        InitialContext::Unquoted => (0, false),
        InitialContext::Quoted(outer_quote) => match scan_quoted_segment(bytes, 0, outer_quote) {
            ScanResult::Precise {
                value_len,
                quote_closed,
            } => (value_len, quote_closed),
            ScanResult::Dynamic => return ScanResult::Dynamic,
        },
    };

    if matches!(initial_context, InitialContext::Quoted(_)) && !quote_closed {
        return ScanResult::Precise {
            value_len: cursor,
            quote_closed: false,
        };
    }

    while cursor < bytes.len() && !is_shell_word_boundary(bytes[cursor]) {
        match bytes[cursor] {
            b'\'' | b'"' => match scan_quoted_segment(bytes, cursor + 1, bytes[cursor]) {
                ScanResult::Precise { value_len, .. } => cursor = value_len,
                ScanResult::Dynamic => return ScanResult::Dynamic,
            },
            b'\\' => cursor = skip_shell_escape(bytes, cursor),
            b'$' | b'`' => return ScanResult::Dynamic,
            _ => cursor += 1,
        }
    }

    if bytes.get(cursor) == Some(&b'(')
        || (matches!(bytes.get(cursor), Some(b'<') | Some(b'>'))
            && bytes.get(cursor + 1) == Some(&b'('))
    {
        return ScanResult::Dynamic;
    }
    if matches!(initial_context, InitialContext::Unquoted)
        && starts_additional_cookie_pair(bytes, cursor)
    {
        return ScanResult::Dynamic;
    }

    ScanResult::Precise {
        value_len: cursor,
        quote_closed,
    }
}

fn starts_additional_cookie_pair(bytes: &[u8], cursor: usize) -> bool {
    if bytes.get(cursor) != Some(&b';') {
        return false;
    }

    let mut next = cursor + 1;
    while bytes
        .get(next)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        next += 1;
    }
    let name_start = next;
    while bytes
        .get(next)
        .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(byte, b'=' | b';' | b','))
    {
        next += 1;
    }
    if next == name_start {
        return false;
    }
    while bytes
        .get(next)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        next += 1;
    }

    bytes.get(next) == Some(&b'=')
}

fn scan_quoted_segment(bytes: &[u8], mut cursor: usize, quote: u8) -> ScanResult {
    while cursor < bytes.len() {
        match bytes[cursor] {
            byte if byte == quote => {
                return ScanResult::Precise {
                    value_len: cursor + 1,
                    quote_closed: true,
                };
            }
            b'\\' if quote == b'"' => cursor = skip_shell_escape(bytes, cursor),
            b'$' | b'`' if quote == b'"' => return ScanResult::Dynamic,
            _ => cursor += 1,
        }
    }

    ScanResult::Precise {
        value_len: cursor,
        quote_closed: false,
    }
}

fn skip_shell_escape(bytes: &[u8], cursor: usize) -> usize {
    match bytes.get(cursor + 1) {
        Some(b'\r') if bytes.get(cursor + 2) == Some(&b'\n') => cursor + 3,
        Some(_) => cursor + 2,
        None => cursor + 1,
    }
}

fn is_shell_word_boundary(byte: u8) -> bool {
    byte.is_ascii_whitespace() || matches!(byte, b';' | b'&' | b'|' | b'(' | b')' | b'<' | b'>')
}
