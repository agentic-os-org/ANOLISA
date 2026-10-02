use super::scan_lines;

pub(super) fn is_diff(scan: &str) -> bool {
    if scan.starts_with("diff --git ") || scan.contains("\ndiff --git ") {
        return true;
    }
    // Unified diff without git framing: a ---/+++ header pair followed by at
    // least one hunk header.
    let mut minus_header = false;
    let mut plus_header = false;
    let mut hunks = 0;
    for line in scan_lines(scan) {
        if line.starts_with("--- ") {
            minus_header = true;
        } else if line.starts_with("+++ ") {
            plus_header = true;
        } else if line.starts_with("@@ -") {
            hunks += 1;
        }
    }
    minus_header && plus_header && hunks >= 1
}
