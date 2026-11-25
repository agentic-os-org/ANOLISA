use super::{MAX_SCAN_BYTES, scan_prefix};

/// Bounded opening + tail sniff on the content's head and trailing window.
/// Deliberately inspects both ends rather than parsing the full document: a
/// JSON payload larger than [`MAX_SCAN_BYTES`] still matches, while the JSON
/// compressor stays the authority and degrades malformed inputs to passthrough.
/// Rejecting only clearly non-JSON openings avoids misrouting bracketed build
/// logs (such as `[1/N] ... [-Wunused-variable]`), while treating an exhausted
/// head sample or concatenated JSON Lines containers as eligible for JSON
/// passthrough.
pub(super) fn is_json_like(content: &str) -> bool {
    let scan = scan_prefix(content);
    let head = trim_start_json_ws(scan.as_bytes());
    let tail = trim_end_json_ws(tail_window(content).as_bytes());
    let head_truncated = scan.len() < content.len();
    match (head.first(), tail.last()) {
        (Some(b'{'), Some(b'}')) => {
            let rest = trim_start_json_ws(&head[1..]);
            match rest.first() {
                None => head_truncated,
                Some(b'"') => true,
                Some(b'}') => is_json_tail_or_next_record(&rest[1..], head_truncated),
                _ => false,
            }
        }
        (Some(b'['), Some(b']')) => looks_like_json_array_start(&head[1..], head_truncated),
        _ => false,
    }
}

fn looks_like_json_array_start(after_open: &[u8], head_truncated: bool) -> bool {
    let rest = trim_start_json_ws(after_open);
    let after_tok = match rest.first() {
        None => return head_truncated,
        Some(b'{' | b'[' | b'"') => return true,
        Some(b']') => return is_json_tail_or_next_record(&rest[1..], head_truncated),
        Some(b't' | b'f' | b'n') => {
            let kw = match rest[0] {
                b't' => b"true".as_slice(),
                b'f' => b"false".as_slice(),
                _ => b"null".as_slice(),
            };
            if head_truncated && kw.starts_with(rest) {
                return true;
            }
            rest.strip_prefix(kw)
        }
        Some(b'-' | b'0'..=b'9') => consume_json_number(rest, head_truncated),
        _ => return false,
    };
    let Some(after_tok) = after_tok else {
        return false;
    };
    let after_ws = trim_start_json_ws(after_tok);
    match after_ws.first() {
        None => head_truncated,
        Some(b',') => true,
        Some(b']') => is_json_tail_or_next_record(&after_ws[1..], head_truncated),
        _ => false,
    }
}

fn is_json_tail_or_next_record(after_close: &[u8], head_truncated: bool) -> bool {
    let rest = trim_start_json_ws(after_close);
    if rest.is_empty() {
        return true;
    }
    matches!(rest.first(), Some(b'{' | b'[' | b'"'))
        || [b"true".as_slice(), b"false", b"null"].iter().any(|kw| {
            (head_truncated && kw.starts_with(rest))
                || rest
                    .strip_prefix(*kw)
                    .is_some_and(starts_with_json_ws_or_eof)
        })
        || consume_json_number(rest, head_truncated).is_some_and(starts_with_json_ws_or_eof)
}

fn starts_with_json_ws_or_eof(bytes: &[u8]) -> bool {
    bytes.first().is_none_or(|&b| is_json_ws(b))
}

fn consume_json_number(bytes: &[u8], head_truncated: bool) -> Option<&[u8]> {
    let mut rest = bytes.strip_prefix(b"-").unwrap_or(bytes);
    match rest.first() {
        None => return head_truncated.then_some(rest),
        Some(b'0') => rest = &rest[1..],
        Some(b'1'..=b'9') => rest = consume_digits(&rest[1..]),
        Some(_) => return None,
    }
    if let Some(after_dot) = rest.strip_prefix(b".") {
        if after_dot.is_empty() && head_truncated {
            return Some(after_dot);
        }
        if !after_dot.first().is_some_and(u8::is_ascii_digit) {
            return None;
        }
        rest = consume_digits(&after_dot[1..]);
    }
    if let Some((&exp, after_exp)) = rest.split_first()
        && matches!(exp, b'e' | b'E')
    {
        if after_exp.is_empty() && head_truncated {
            return Some(after_exp);
        }
        let exp_rest = after_exp
            .strip_prefix(b"+")
            .or_else(|| after_exp.strip_prefix(b"-"))
            .unwrap_or(after_exp);
        if exp_rest.is_empty() && head_truncated {
            return Some(exp_rest);
        }
        if !exp_rest.first().is_some_and(u8::is_ascii_digit) {
            return None;
        }
        rest = consume_digits(&exp_rest[1..]);
    }
    Some(rest)
}

fn consume_digits(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_digit) {
        bytes = &bytes[1..];
    }
    bytes
}

fn is_json_ws(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

fn trim_start_json_ws(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(|&b| is_json_ws(b)) {
        bytes = &bytes[1..];
    }
    bytes
}

fn trim_end_json_ws(mut bytes: &[u8]) -> &[u8] {
    while bytes.last().is_some_and(|&b| is_json_ws(b)) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

/// The trailing counterpart of `scan_prefix`: at most [`MAX_SCAN_BYTES`]
/// final bytes, cut at a char boundary.
fn tail_window(content: &str) -> &str {
    if content.len() <= MAX_SCAN_BYTES {
        return content;
    }
    let mut start = content.len() - MAX_SCAN_BYTES;
    while !content.is_char_boundary(start) {
        start += 1;
    }
    &content[start..]
}
