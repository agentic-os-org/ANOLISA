//! Browser accessibility-snapshot shape detection.
//!
//! Runs first in the domain chain: snapshots can embed diff, test, or source
//! markers that later detectors would claim, while `[ref=` / `uid=` line
//! anchors are unique to snapshots. Shape only — the compressor parsers are
//! authoritative and fail open.

use std::collections::BTreeSet;

use super::MAX_SCAN_LINES;
use tokenless_compressors::{
    fold_advance, is_cdt_listing, is_pw_element_line, name_continues, pw_element_ref,
};

/// Non-empty lines inspected by the ratio fallbacks.
const MAX_RATIO_LINES: usize = 50;

/// True for Playwright MCP aria snapshots or chrome-devtools-mcp uid listings.
pub(super) fn is_browser_snapshot(scan: &str) -> bool {
    if looks_like_diff(scan) {
        return false;
    }
    is_playwright(scan) || is_browser_find(scan) || is_chrome_devtools(scan)
}

/// A diff of a snapshot file carries the same line anchors in its context
/// rows; the diff domain must keep owning those. Head-anchored so a snapshot
/// merely quoting diff text in a node name is not rejected; the compressor
/// parsers keep a contains-based guard for diffs with a command preamble.
fn looks_like_diff(scan: &str) -> bool {
    scan.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .is_some_and(|first| {
            first.starts_with("diff --git ")
                || first.starts_with("--- ")
                || first.starts_with("+++ ")
                || first.starts_with("@@ ")
        })
}

fn sampled_lines(scan: &str) -> Vec<&str> {
    scan.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(MAX_RATIO_LINES)
        .collect()
}

fn is_playwright(scan: &str) -> bool {
    let lines = sampled_lines(scan);
    let Some(first) = lines.first() else {
        return false;
    };
    // Wrapped snapshot: a column-0 `### Snapshot` section heading plus a fenced
    // YAML aria tree. Playwright can precede it with `### Open tabs`,
    // `### Ran Playwright code`, or `### Page` sections, so anchor on the
    // snapshot section rather than requiring `### Page` on the first line.
    // The heading must start at column 0: a diff of a snapshot file carries
    // ` ### Snapshot` as an indented context row, and the Diff domain owns it.
    if scan.lines().any(|l| l.trim_end() == "### Snapshot")
        && scan.contains("```yaml")
        && scan.contains("[ref=")
    {
        return true;
    }
    // Bare snapshot (e.g. a saved .yml body quoted into a tool result). The
    // first line must itself be a tree row so command preambles (a diff
    // echoed after its shell command) never reach the ratio fallback.
    if lines.len() < 3 || !is_pw_line(first) {
        return false;
    }
    let pw_lines = lines.iter().filter(|l| is_pw_line(l)).count();
    let refs = lines.iter().filter(|l| ref_value(l).is_some()).count();
    refs >= 3 && pw_lines * 5 >= lines.len() * 4
}

/// Playwright `browser_find` returns unfenced PW-shaped subtrees under a
/// column-0 `### Result` heading with match blocks separated by `----`.
fn is_browser_find(scan: &str) -> bool {
    // Compare raw LF lines without `.lines()` so CRLF `\r` is never silently
    // accepted here and later rejected by `find::parse`. Require the document to
    // open with a column-0 `### ` section heading (mirroring `is_playwright`)
    // so HTML pages or test logs that quote `### Result` stay in their domain.
    let mut checked_head = false;
    let mut in_section = false;
    let mut skipping_summary = false;
    let mut body: Vec<String> = Vec::new();
    let mut total_lines = 0;
    let mut folded = String::new();

    for line in scan.split('\n') {
        total_lines += 1;
        if total_lines > MAX_SCAN_LINES {
            break;
        }
        if !in_section {
            if !checked_head && !line.trim().is_empty() {
                if !line.starts_with("### ") {
                    return false;
                }
                checked_head = true;
            }
            if line == "### Result" {
                in_section = true;
                skipping_summary = true;
            }
            continue;
        }
        if skipping_summary {
            if line.trim().is_empty() {
                continue;
            }
            if line.starts_with("Found ") && line.contains(" match") && line.ends_with(':') {
                continue;
            }
            skipping_summary = false;
        }
        if fold_advance(&mut folded, line) {
            if let Some(prev) = body.last_mut() {
                prev.push(' ');
                prev.push_str(line.trim());
            }
            continue;
        }
        if line.starts_with("### ") {
            break;
        }
        if name_continues(line) {
            folded = line.to_owned();
        }
        if line.contains('\r') || line.contains("```yaml") || line.contains("```yml") {
            return false;
        }
        if line != "----" {
            let trimmed = line.trim();
            if !trimmed.is_empty() && body.len() < MAX_RATIO_LINES {
                body.push(trimmed.to_owned());
            }
        }
    }
    if !in_section {
        return false;
    }
    let Some(first_body) = body.first() else {
        return false;
    };
    if body.len() < 2 || !is_pw_line(first_body) {
        return false;
    }
    let pw_lines = body.iter().filter(|l| is_pw_find_line(l)).count();
    let unique_refs: BTreeSet<String> = body.iter().filter_map(|l| ref_value(l)).collect();
    unique_refs.len() >= 2 && pw_lines * 5 >= body.len() * 4
}

fn is_pw_find_line(line: &str) -> bool {
    line == "..." || is_pw_line(line)
}

fn is_pw_line(line: &str) -> bool {
    is_pw_element_line(line) || line.starts_with("- /url: ") || line.starts_with("- text: ")
}

/// Extracts a well-formed `eN` or `fNeM` element reference outside quoted names.
fn ref_value(line: &str) -> Option<String> {
    pw_element_ref(line)
}

/// Recognizes chrome-devtools-mcp uid listings for labeling and routing only.
/// The compressor never compresses them: upstream wraps names and attribute
/// values in raw quotes without escaping embedded quotes or newlines, so record
/// boundaries are byte-ambiguous and cannot be decomposed losslessly. Detected
/// CDT snapshots pass through unchanged. Shares the compressor's recognizer so
/// routing and the fail-open guard agree on the same shape.
fn is_chrome_devtools(scan: &str) -> bool {
    is_cdt_listing(scan)
}
