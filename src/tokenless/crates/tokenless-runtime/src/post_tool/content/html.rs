/// Whether the content opens as a complete HTML document.
///
/// One byte-order mark at the very start of the stream is skipped first: the
/// HTML tokenizer ignores it, so a page served with `EF BB BF` is the same
/// document and the renderer reads it into the same view. `trim_start` cannot
/// do this because U+FEFF is a format character, not whitespace. Only the
/// first mark is a BOM; a second one is content, and a doctype behind content
/// no longer opens the document.
pub(super) fn is_html_document(scan: &str) -> bool {
    let head = scan
        .strip_prefix('\u{feff}')
        .unwrap_or(scan)
        .trim_start()
        .as_bytes();
    starts_with_ignore_ascii_case(head, b"<!doctype html")
        || starts_with_ignore_ascii_case(head, b"<html")
}

fn starts_with_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len() && haystack[..needle.len()].eq_ignore_ascii_case(needle)
}
