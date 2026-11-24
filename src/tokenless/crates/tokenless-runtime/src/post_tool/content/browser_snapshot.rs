//! Browser accessibility-snapshot shape detection.
//!
//! Runs first in the domain chain: snapshots can embed diff, test, or source
//! markers that later detectors would claim, while `[ref=` / `uid=` line
//! anchors are unique to snapshots. Shape only — the compressor parsers are
//! authoritative and fail open.

use tokenless_compressors::is_cdt_listing;

/// Non-empty lines inspected by the ratio fallbacks.
const MAX_RATIO_LINES: usize = 50;

/// True for Playwright MCP aria snapshots or chrome-devtools-mcp uid listings.
pub(super) fn is_browser_snapshot(scan: &str) -> bool {
    if looks_like_diff(scan) {
        return false;
    }
    is_playwright(scan) || is_chrome_devtools(scan)
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
    // Wrapped snapshot: the response opens with a column-0 Playwright markdown
    // section heading (`### Open tabs`, `### Ran Playwright code`, `### Page`,
    // `### Snapshot`, etc.) and contains a column-0 `### Snapshot` heading
    // followed by a fenced `yaml` block carrying at least one well-formed
    // `[ref=eN]`/`[ref=fNeM]` row. Anchoring both the opening section and the
    // fenced YAML body keeps HTML pages, test logs, and prose markdown that
    // quote a snapshot from being hijacked as BrowserSnapshot. Note that
    // plain-text action preambles and ANSI-styled first headings fail open by
    // design, whereas documents that themselves open with a column-0 `### `
    // heading and quote a verbatim snapshot remain an inherent boundary of
    // shape-based detection.
    let first_raw = scan.lines().find(|l| !l.trim().is_empty());
    if first_raw.is_some_and(|l| l.starts_with("### ")) && has_wrapped_pw_snapshot(scan) {
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

/// True when a column-0 `### Snapshot` heading is followed by a ```` ```yaml ````
/// fence containing at least one valid Playwright element reference before the
/// fence closes (or the scan window ends). Stops before any subsequent `### `
/// heading without consuming it so repeated `### Snapshot` headings still work.
fn has_wrapped_pw_snapshot(scan: &str) -> bool {
    let mut lines = scan.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim_end() != "### Snapshot" {
            continue;
        }
        while let Some(sub_line) = lines.peek() {
            let trimmed = sub_line.trim_end();
            if trimmed.starts_with("### ") {
                // Leave the next section heading to the outer loop.
                break;
            }
            lines.next();
            if trimmed == "```yaml" {
                for body_line in lines.by_ref() {
                    if body_line.trim_end() == "```" {
                        break;
                    }
                    if ref_value(body_line).is_some() {
                        return true;
                    }
                }
                break;
            }
        }
    }
    false
}

fn is_pw_line(line: &str) -> bool {
    line.starts_with("- ")
        && (line.contains("[ref=") || line.starts_with("- /url: ") || line.starts_with("- text: "))
}

/// Extracts a well-formed `eN` or `fNeM` element reference, if present.
fn ref_value(line: &str) -> Option<&str> {
    let start = line.find("[ref=")? + "[ref=".len();
    let end = start + line[start..].find(']')?;
    let value = &line[start..end];
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if let Some(rest) = value.strip_prefix('e') {
        return digits(rest).then_some(value);
    }
    let (frame, element) = value.strip_prefix('f')?.split_once('e')?;
    (digits(frame) && digits(element)).then_some(value)
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
