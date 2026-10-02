use super::non_empty_lines;

const SOURCE_MARKERS: &[&str] = &[
    "fn ",
    "pub ",
    "def ",
    "class ",
    "#include",
    "import ",
    "use ",
    "impl ",
    "function ",
    "package ",
];

/// Source code needs strong signals: a shebang, or several lines opening
/// with declaration keywords. Weak matches fall through to plain text —
/// misclassifying prose as code is worse than the reverse while the
/// source-code compressor is experimental and Read stays skipped.
pub(super) fn is_source_code(scan: &str) -> bool {
    if scan.starts_with("#!") {
        return true;
    }
    non_empty_lines(scan)
        .filter(|l| {
            let t = l.trim_start();
            SOURCE_MARKERS.iter().any(|m| t.starts_with(m))
        })
        .count()
        >= 5
}
