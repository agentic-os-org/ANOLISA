/// Whether the content opens as a complete HTML document.
///
/// A leading byte-order mark has already been stripped by `detect()` — for
/// every domain at once, so the HTML detector does not strip a second one
/// here: only the first mark is a BOM, a second is content, and a doctype
/// behind content no longer opens the document.
pub(super) fn is_html_document(scan: &str) -> bool {
    let head = scan.trim_start().as_bytes();
    starts_with_ignore_ascii_case(head, b"<!doctype html")
        || starts_with_ignore_ascii_case(head, b"<html")
}

fn starts_with_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len() && haystack[..needle.len()].eq_ignore_ascii_case(needle)
}
