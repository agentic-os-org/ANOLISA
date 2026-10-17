//! Consolidation quality filters — improve fact extraction accuracy.
//!
//! Three quality mechanisms:
//! 1. **Mutual exclusion**: skip auto-consolidation if Agent manually
//!    observed memories during the session (avoids duplicates).
//! 2. **Non-derivable filter**: reject facts that can be obtained in
//!    real-time from the codebase or Git history.
//! 3. **Absolute date normalization**: convert relative dates
//!    ("yesterday", "next week") to absolute ISO 8601 dates.

use chrono::{Datelike, Duration, Local, NaiveDate};
use regex::Regex;
use std::sync::LazyLock;

/// Check if the session already contains manual observations.
/// Returns true if consolidation should be skipped.
pub fn should_skip_consolidation(manual_observe_count: usize) -> bool {
    manual_observe_count > 0
}

/// Check if a fact's content is derivable from codebase or git history.
/// Returns true if the fact should be filtered out.
pub fn is_derivable(content: &str) -> bool {
    let lower = content.to_lowercase();

    // File listings and directory structures
    if lower.contains("file structure") || lower.contains("directory layout") {
        return true;
    }
    if lower.starts_with("files in ") || lower.starts_with("directory contains") {
        return true;
    }

    // Git history facts
    if lower.contains("committed by") || lower.contains("last commit") {
        return true;
    }
    if lower.contains("git log shows") || lower.contains("git blame") {
        return true;
    }

    // Code patterns visible from reading source
    if lower.contains("function ") && lower.contains(" is defined in ") {
        return true;
    }
    if lower.contains("module exports") || lower.contains("imports from") {
        return true;
    }

    // Package/library versions from manifests
    if lower.contains("version ")
        && (lower.contains("package.json") || lower.contains("cargo.toml"))
    {
        return true;
    }

    // API endpoint lists from reading router code
    if lower.contains("api endpoint") && lower.contains("defined in") {
        return true;
    }

    false
}

/// Normalize relative dates in content to absolute ISO 8601 dates.
/// Handles English and Chinese relative date expressions.
///
/// Backtick-quoted code spans are copied through verbatim: every
/// consolidation rule embeds the paths it talks about in backticks, and
/// rewriting a date keyword inside such a span would corrupt the path
/// (`notes/today/a.md` would become `notes/2026-10-16/a.md`, which does
/// not exist on disk) and leave the fact body disagreeing with the
/// `related_paths` frontmatter of the very same file.
///
/// A span is delimited by a run of N backticks closed by a run of exactly
/// N backticks (CommonMark); runs of a different length inside a span are
/// content, so ``double-quoted`` values and ```fenced``` blocks survive
/// intact instead of mis-pairing with a single backtick.
pub fn normalize_relative_dates(content: &str) -> String {
    // Fast path: no backticks at all — normalize in one pass instead of
    // copying the content into `out` segment by segment.
    if !content.contains('`') {
        return normalize_unquoted(content, Local::now().date_naive());
    }
    let today = Local::now().date_naive();
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(open) = rest.find('`') {
        let open_run = rest[open..].bytes().take_while(|&b| b == b'`').count();
        let (prose, tail) = rest.split_at(open);
        let body = &tail[open_run..];
        out.push_str(&normalize_unquoted(prose, today));
        match find_closing_run(body, open_run) {
            Some(at) => {
                // The span, opener through closer, is copied verbatim.
                let span_end = open_run + at + open_run;
                out.push_str(&tail[..span_end]);
                rest = &tail[span_end..];
            }
            None => {
                // Unclosed run: keep the remainder verbatim rather than
                // guess where the span was meant to end. The heuristics
                // sanitize the values they interpolate, so this is only
                // reachable when a caller passes text with a stray
                // backtick run of its own.
                out.push_str(tail);
                return out;
            }
        }
    }
    out.push_str(&normalize_unquoted(rest, today));
    out
}

/// Offset of a backtick run of exactly `len` bytes in `body`, if any.
/// Runs of any other length are span content, not delimiters.
fn find_closing_run(body: &str, len: usize) -> Option<usize> {
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let run = body[i..].bytes().take_while(|&b| b == b'`').count();
            if run == len {
                return Some(i);
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

/// Cheap pre-filter: does the segment mention any keyword the rewrite
/// passes look for? A miss skips all of them. A hit is a superset of the
/// real matches — word boundaries and case are re-checked by the passes
/// themselves, so this only needs to avoid false negatives.
fn may_contain_relative_date(content: &str) -> bool {
    const EN: &[&str] = &[
        "today",
        "yesterday",
        "tomorrow",
        "last monday",
        "last tuesday",
        "last wednesday",
        "last thursday",
        "last friday",
        "last saturday",
        "last sunday",
        "next monday",
        "next tuesday",
        "next wednesday",
        "next thursday",
        "next friday",
        "next saturday",
        "next sunday",
    ];
    // 上周X / 下周X all begin with 上周 / 下周; 天前 covers "N 天前".
    const CN: &[&str] = &[
        "今天", "昨天", "前天", "明天", "后天", "上周", "下周", "天前",
    ];
    let lower = content.to_lowercase();
    EN.iter().any(|k| lower.contains(k))
        || lower.contains("ago")
        || CN.iter().any(|k| content.contains(k))
}

/// The date rewrite itself, applied to prose that contains no code spans.
/// `today` is computed once per fact by the caller, and each segment is
/// checked against the keyword list before any of the ~35 rewrite passes
/// run — the prose between two code spans is usually a comma and nothing
/// else.
fn normalize_unquoted(content: &str, today: NaiveDate) -> String {
    if !may_contain_relative_date(content) {
        return content.to_string();
    }
    let mut result = content.to_string();

    // English patterns
    result = replace_date(&result, "today", today);
    result = replace_date(&result, "yesterday", today - Duration::days(1));
    result = replace_date(&result, "tomorrow", today + Duration::days(1));

    // "last Monday/Tuesday/..."
    for (day_name, days) in &[
        ("last monday", days_since_weekday(1, &today)),
        ("last tuesday", days_since_weekday(2, &today)),
        ("last wednesday", days_since_weekday(3, &today)),
        ("last thursday", days_since_weekday(4, &today)),
        ("last friday", days_since_weekday(5, &today)),
        ("last saturday", days_since_weekday(6, &today)),
        ("last sunday", days_since_weekday(0, &today)),
    ] {
        result = replace_date(&result, day_name, today - Duration::days(*days));
    }

    // "next Monday/Tuesday/..."
    for (day_name, days) in &[
        ("next monday", days_until_weekday(1, &today)),
        ("next tuesday", days_until_weekday(2, &today)),
        ("next wednesday", days_until_weekday(3, &today)),
        ("next thursday", days_until_weekday(4, &today)),
        ("next friday", days_until_weekday(5, &today)),
        ("next saturday", days_until_weekday(6, &today)),
        ("next sunday", days_until_weekday(0, &today)),
    ] {
        result = replace_date(&result, day_name, today + Duration::days(*days));
    }

    // Chinese patterns
    result = replace_date(&result, "今天", today);
    result = replace_date(&result, "昨天", today - Duration::days(1));
    result = replace_date(&result, "前天", today - Duration::days(2));
    result = replace_date(&result, "明天", today + Duration::days(1));
    result = replace_date(&result, "后天", today + Duration::days(2));

    // "上周X" (last week day X)
    for (cn_day, days) in &[
        ("上周一", days_since_weekday(1, &today)),
        ("上周二", days_since_weekday(2, &today)),
        ("上周三", days_since_weekday(3, &today)),
        ("上周四", days_since_weekday(4, &today)),
        ("上周五", days_since_weekday(5, &today)),
        ("上周六", days_since_weekday(6, &today)),
        ("上周日", days_since_weekday(0, &today)),
        ("上周天", days_since_weekday(0, &today)),
    ] {
        result = replace_date(&result, cn_day, today - Duration::days(*days));
    }

    // "下周X" (next week day X)
    for (cn_day, days) in &[
        ("下周一", days_until_weekday(1, &today)),
        ("下周二", days_until_weekday(2, &today)),
        ("下周三", days_until_weekday(3, &today)),
        ("下周四", days_until_weekday(4, &today)),
        ("下周五", days_until_weekday(5, &today)),
        ("下周六", days_until_weekday(6, &today)),
        ("下周日", days_until_weekday(0, &today)),
        ("下周天", days_until_weekday(0, &today)),
    ] {
        result = replace_date(&result, cn_day, today + Duration::days(*days));
    }

    // "N days ago" / "N 天前"
    result = DAYS_AGO_EN
        .replace_all(&result, |caps: &regex::Captures| {
            let n: i64 = caps[1].parse().unwrap_or(0);
            let date = today - Duration::days(n);
            date.format("%Y-%m-%d").to_string()
        })
        .to_string();
    result = DAYS_AGO_CN
        .replace_all(&result, |caps: &regex::Captures| {
            let n: i64 = caps[1].parse().unwrap_or(0);
            let date = today - Duration::days(n);
            date.format("%Y-%m-%d").to_string()
        })
        .to_string();

    result
}

/// Replace a relative date expression with an absolute date.
// Precompiled keyword regexes — compiled once, reused across all calls.
// Keywords are static; only the replacement dates vary per call.
static KEYWORD_REGEXES: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    [
        "today",
        "yesterday",
        "tomorrow",
        "last monday",
        "last tuesday",
        "last wednesday",
        "last thursday",
        "last friday",
        "last saturday",
        "last sunday",
        "next monday",
        "next tuesday",
        "next wednesday",
        "next thursday",
        "next friday",
        "next saturday",
        "next sunday",
    ]
    .iter()
    .map(|kw| {
        let re = Regex::new(&format!(r"(?i)\b{}\b", regex::escape(kw)))
            .unwrap_or_else(|_| Regex::new(&regex::escape(kw)).unwrap());
        (*kw, re)
    })
    .collect()
});

static DAYS_AGO_EN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+)\s*days?\s*ago").unwrap());

static DAYS_AGO_CN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+)\s*天前").unwrap());

fn replace_date(text: &str, relative: &str, absolute: NaiveDate) -> String {
    let date_str = absolute.format("%Y-%m-%d").to_string();
    // For CJK characters, use simple replace (word boundaries don't work)
    if relative.chars().any(|c| c > '\u{7f}') {
        text.replace(relative, &date_str)
    } else {
        // Use precompiled regex if available, else compile on the fly
        if let Some((_, re)) = KEYWORD_REGEXES.iter().find(|(kw, _)| *kw == relative) {
            re.replace_all(text, date_str.as_str()).to_string()
        } else {
            let re = Regex::new(&format!(r"(?i)\b{}\b", regex::escape(relative)))
                .unwrap_or_else(|_| Regex::new(&regex::escape(relative)).unwrap());
            re.replace_all(text, date_str.as_str()).to_string()
        }
    }
}

/// Days since a given weekday (0=Sun, 1=Mon, ..., 6=Sat).
fn days_since_weekday(target: u32, today: &NaiveDate) -> i64 {
    let today_wd = today.weekday().num_days_from_sunday();
    let diff = (today_wd + 7 - target) % 7;
    if diff == 0 { 7 } else { diff as i64 }
}

/// Days until a given weekday (0=Sun, 1=Mon, ..., 6=Sat).
fn days_until_weekday(target: u32, today: &NaiveDate) -> i64 {
    let today_wd = today.weekday().num_days_from_sunday();
    let diff = (target + 7 - today_wd) % 7;
    if diff == 0 { 7 } else { diff as i64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_consolidation_when_manual_observe() {
        assert!(should_skip_consolidation(1));
        assert!(should_skip_consolidation(5));
        assert!(!should_skip_consolidation(0));
    }

    #[test]
    fn derivable_file_structure() {
        assert!(is_derivable("The file structure of the project is:"));
        assert!(is_derivable("directory layout shows src/ and tests/"));
        assert!(!is_derivable(
            "The team decided to use JWT for authentication"
        ));
    }

    #[test]
    fn derivable_git_history() {
        assert!(is_derivable("Last commit was by Alice"));
        assert!(is_derivable("git log shows 5 commits today"));
        assert!(!is_derivable("We decided to use feature flags for rollout"));
    }

    #[test]
    fn derivable_code_patterns() {
        assert!(is_derivable(
            "function calculateTotal is defined in src/cart.ts"
        ));
        assert!(is_derivable("Module exports the Config type"));
        assert!(!is_derivable("Error handling should use Result types"));
    }

    #[test]
    fn normalize_today() {
        let result = normalize_relative_dates("Meeting today about auth");
        assert!(!result.contains("today"));
        assert!(result.contains(&Local::now().date_naive().format("%Y-%m-%d").to_string()));
    }

    #[test]
    fn normalize_yesterday() {
        let result = normalize_relative_dates("Fixed the bug yesterday");
        assert!(!result.contains("yesterday"));
        let yesterday = (Local::now().date_naive() - Duration::days(1))
            .format("%Y-%m-%d")
            .to_string();
        assert!(result.contains(&yesterday));
    }

    #[test]
    fn normalize_chinese() {
        let result = normalize_relative_dates("明天上线");
        assert!(!result.contains("明天"));
        let tomorrow = (Local::now().date_naive() + Duration::days(1))
            .format("%Y-%m-%d")
            .to_string();
        assert!(result.contains(&tomorrow));
    }

    #[test]
    fn normalize_n_days_ago() {
        let result = normalize_relative_dates("Reported 3 days ago");
        assert!(!result.contains("days ago"));
        let expected = (Local::now().date_naive() - Duration::days(3))
            .format("%Y-%m-%d")
            .to_string();
        assert!(result.contains(&expected));
    }

    #[test]
    fn normalize_chinese_n_days() {
        let result = normalize_relative_dates("5天前发现的bug");
        assert!(!result.contains("天前"));
        let expected = (Local::now().date_naive() - Duration::days(5))
            .format("%Y-%m-%d")
            .to_string();
        assert!(result.contains(&expected));
    }

    // ── quoted code spans must not be rewritten ───────────────────

    #[test]
    fn quoted_paths_are_not_date_normalized() {
        // rule_working_context embeds the directory and every touched file
        // in backticks. A directory legitimately named `today` must survive
        // normalization: the rewritten path would not exist on disk and
        // would disagree with the related_paths frontmatter of the same
        // fact.
        let content = "Agent 在 `notes/today/` 目录下创建/修改了 2 个文件：`notes/today/a.md`, `notes/today/b.md`。";
        let out = normalize_relative_dates(content);
        assert!(
            out.contains("notes/today/a.md"),
            "the quoted path must stay verbatim, got: {out}"
        );
        let today = Local::now().date_naive().format("%Y-%m-%d").to_string();
        assert!(
            !out.contains(&today),
            "no date may be written into a quoted path, got: {out}"
        );
    }

    #[test]
    fn quoted_chinese_paths_are_not_date_normalized() {
        let content = "Agent 在 `计划/明天/` 目录下创建/修改了 1 个文件：`计划/明天/上线.md`。";
        let out = normalize_relative_dates(content);
        assert!(
            out.contains("计划/明天/上线.md"),
            "the quoted Chinese path must stay verbatim, got: {out}"
        );
    }

    #[test]
    fn prose_around_quoted_paths_still_normalizes() {
        let content = "Discussed this today; see `notes/today/standup.md` for details.";
        let out = normalize_relative_dates(content);
        let today = Local::now().date_naive().format("%Y-%m-%d").to_string();
        assert!(
            out.contains(&today),
            "the prose keyword must still be normalized, got: {out}"
        );
        assert!(
            out.contains("notes/today/standup.md"),
            "the quoted path must stay verbatim, got: {out}"
        );
    }

    #[test]
    fn double_backtick_runs_are_not_date_normalized() {
        // A double-backtick run is one delimiter, not two single-backtick
        // pairs: the recorded query value inside it must survive verbatim.
        let content = "Agent 通过 `memory_search` 搜索了 ``today``。";
        let out = normalize_relative_dates(content);
        assert!(
            out.contains("``today``"),
            "a double-quoted value must stay verbatim, got: {out}"
        );
    }

    #[test]
    fn mismatched_run_lengths_do_not_close_a_span() {
        // The inner single backtick is span content, not a closer for the
        // surrounding double-backtick run.
        let content = "See ``notes/today/a`` and yesterday for details.";
        let out = normalize_relative_dates(content);
        assert!(
            out.contains("notes/today/a"),
            "the span content must stay verbatim, got: {out}"
        );
        assert!(
            !out.contains("yesterday"),
            "prose after the span must still normalize, got: {out}"
        );
    }

    #[test]
    fn fenced_code_blocks_are_not_date_normalized() {
        let content = "Notes:\n```bash\nls notes/today/\n```\nDone yesterday.";
        let out = normalize_relative_dates(content);
        assert!(
            out.contains("ls notes/today/"),
            "the fenced body must stay verbatim, got: {out}"
        );
        assert!(
            !out.contains("yesterday"),
            "prose after the fence must still normalize, got: {out}"
        );
    }

    #[test]
    fn unclosed_backtick_keeps_remainder_verbatim() {
        // Degradation contract: an unterminated run disables normalization
        // for the remainder rather than guessing a span end.
        let content = "Fixed `config.rs yesterday";
        let out = normalize_relative_dates(content);
        assert_eq!(
            out, content,
            "the unclosed remainder must stay verbatim, got: {out}"
        );
    }
}
