use super::MAX_SCAN_BYTES;

/// Bracket sniff on the first and last non-whitespace bytes. Deliberately
/// consults the content's tail, not just the leading scan window: the closing
/// bracket of a JSON document larger than the window lies beyond it, and a
/// misroute degrades to passthrough because the JSON compressor is the
/// authority that parses. Cost stays bounded: the leading trim is covered by
/// `detect()`'s emptiness check on the scan prefix, and the trailing trim
/// inspects at most [`MAX_SCAN_BYTES`] final bytes — deeper whitespace
/// padding is not JSON.
pub(super) fn is_json_like(content: &str) -> bool {
    let first = content.trim_start().as_bytes().first();
    let last = tail_window(content).trim_end().as_bytes().last();
    matches!(
        (first, last),
        (Some(b'{'), Some(b'}')) | (Some(b'['), Some(b']'))
    )
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
