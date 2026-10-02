use super::non_empty_lines;

/// `path:line[:col]:text` listings: at least 3 non-empty lines, and at least
/// 80% of the inspected ones carry a path-like prefix and a line number.
pub(super) fn is_search_results(scan: &str) -> bool {
    let mut total = 0usize;
    let mut hits = 0usize;
    for line in non_empty_lines(scan).take(50) {
        total += 1;
        if line_is_search_hit(line) {
            hits += 1;
        }
    }
    total >= 3 && hits * 10 >= total * 8
}

fn line_is_search_hit(line: &str) -> bool {
    let Some(colon) = line.find(':') else {
        return false;
    };
    let path = &line[..colon];
    // Require a path-looking prefix so `12:30:00` timestamps do not match.
    if path.is_empty() || !(path.contains('/') || path.contains('.')) {
        return false;
    }
    let rest = &line[colon + 1..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    digits >= 1 && rest.as_bytes().get(digits) == Some(&b':')
}
