//! Consolidation quality filters — improve fact extraction accuracy.
//!
//! Three quality mechanisms:
//! 1. **Mutual exclusion**: skip auto-consolidation if Agent manually
//!    observed memories during the session (avoids duplicates).
//! 2. **Non-derivable filter**: reject facts that can be obtained in
//!    real-time from the codebase or Git history.
//! 3. **Absolute date normalization**: convert relative dates
//!    ("yesterday", "next week") to absolute ISO 8601 dates.

use chrono::{Datelike, Days, Duration, Local, NaiveDate};
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
///
/// Outside code spans, a keyword adjacent to a path separator stays
/// verbatim as well: it is presumed to be part of a file path quoted in
/// prose (error text, episode triggers) rather than a date reference.
/// Slash-joined prose like `today/tomorrow` is therefore also left
/// alone.
pub fn normalize_relative_dates(content: &str) -> String {
    let today = Local::now().date_naive();
    // One replacement date per rule, computed once per fact.
    let iso_dates: Vec<String> = REWRITE_RULES
        .iter()
        .map(|(_, offset)| offset_date(*offset, today).format("%Y-%m-%d").to_string())
        .collect();
    // Fast path: no backticks at all — normalize in one pass instead of
    // copying the content into `out` segment by segment.
    if !content.contains('`') {
        return normalize_unquoted(content, today, &iso_dates);
    }
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(open) = rest.find('`') {
        let open_run = rest[open..].bytes().take_while(|&b| b == b'`').count();
        let (prose, tail) = rest.split_at(open);
        let body = &tail[open_run..];
        out.push_str(&normalize_unquoted(prose, today, &iso_dates));
        match find_closing_run(body, open_run) {
            Some(at) => {
                // The span, opener through closer, is copied verbatim.
                let span_end = open_run + at + open_run;
                out.push_str(&tail[..span_end]);
                rest = &tail[span_end..];
            }
            None => {
                // Unclosed run: keep the remainder verbatim rather than
                // guess where the span was meant to end. Every value the
                // heuristic rules and the episodic extractor interpolate
                // into fact content goes through code_span / span_safe,
                // so this is only reachable when a caller passes text
                // with a stray backtick run of its own.
                out.push_str(tail);
                return out;
            }
        }
    }
    out.push_str(&normalize_unquoted(rest, today, &iso_dates));
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

/// How a rule's absolute date is derived from `today`.
#[derive(Clone, Copy)]
enum DateOffset {
    /// A fixed day distance (`Days(0)` is today).
    Days(i64),
    /// "last <weekday>" / 上周X: back to the most recent given weekday.
    LastWeekday(u32),
    /// "next <weekday>" / 下周X: forward to the next given weekday.
    NextWeekday(u32),
}

/// The single source of rewrite rules: the regex table, the pre-filter,
/// and the rewrite passes all iterate this list, so a keyword cannot be
/// added to one and missed by another.
static REWRITE_RULES: &[(&str, DateOffset)] = &[
    ("today", DateOffset::Days(0)),
    ("yesterday", DateOffset::Days(-1)),
    ("tomorrow", DateOffset::Days(1)),
    ("last monday", DateOffset::LastWeekday(1)),
    ("last tuesday", DateOffset::LastWeekday(2)),
    ("last wednesday", DateOffset::LastWeekday(3)),
    ("last thursday", DateOffset::LastWeekday(4)),
    ("last friday", DateOffset::LastWeekday(5)),
    ("last saturday", DateOffset::LastWeekday(6)),
    ("last sunday", DateOffset::LastWeekday(0)),
    ("next monday", DateOffset::NextWeekday(1)),
    ("next tuesday", DateOffset::NextWeekday(2)),
    ("next wednesday", DateOffset::NextWeekday(3)),
    ("next thursday", DateOffset::NextWeekday(4)),
    ("next friday", DateOffset::NextWeekday(5)),
    ("next saturday", DateOffset::NextWeekday(6)),
    ("next sunday", DateOffset::NextWeekday(0)),
    ("今天", DateOffset::Days(0)),
    ("昨天", DateOffset::Days(-1)),
    ("前天", DateOffset::Days(-2)),
    ("明天", DateOffset::Days(1)),
    ("后天", DateOffset::Days(2)),
    ("上周一", DateOffset::LastWeekday(1)),
    ("上周二", DateOffset::LastWeekday(2)),
    ("上周三", DateOffset::LastWeekday(3)),
    ("上周四", DateOffset::LastWeekday(4)),
    ("上周五", DateOffset::LastWeekday(5)),
    ("上周六", DateOffset::LastWeekday(6)),
    ("上周日", DateOffset::LastWeekday(0)),
    ("上周天", DateOffset::LastWeekday(0)),
    ("下周一", DateOffset::NextWeekday(1)),
    ("下周二", DateOffset::NextWeekday(2)),
    ("下周三", DateOffset::NextWeekday(3)),
    ("下周四", DateOffset::NextWeekday(4)),
    ("下周五", DateOffset::NextWeekday(5)),
    ("下周六", DateOffset::NextWeekday(6)),
    ("下周日", DateOffset::NextWeekday(0)),
    ("下周天", DateOffset::NextWeekday(0)),
];

/// Precompiled rule regexes, index-aligned with [`REWRITE_RULES`] —
/// ASCII literals get case-insensitive word boundaries, CJK literals a
/// plain escaped substring match.
static RULE_REGEXES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    REWRITE_RULES
        .iter()
        .map(|(literal, _)| {
            if literal.bytes().all(|b| b.is_ascii()) {
                Regex::new(&format!(r"(?i)\b{}\b", regex::escape(literal)))
                    .unwrap_or_else(|_| Regex::new(&regex::escape(literal)).unwrap())
            } else {
                Regex::new(&regex::escape(literal)).unwrap()
            }
        })
        .collect()
});

/// The absolute date one rule resolves to.
fn offset_date(offset: DateOffset, today: NaiveDate) -> NaiveDate {
    match offset {
        DateOffset::Days(d) => today + Duration::days(d),
        DateOffset::LastWeekday(d) => today - Duration::days(days_since_weekday(d, &today)),
        DateOffset::NextWeekday(d) => today + Duration::days(days_until_weekday(d, &today)),
    }
}

/// Whether the match at `start..end` sits in a path context: the byte
/// before or after it is a path separator, so the keyword is part of a
/// file path (or slash-joined prose like `today/tomorrow`) and must
/// stay verbatim. This is the guard for every interpolated value that
/// is *not* inside a code span — error text, episode triggers, and any
/// future interpolation point that forgets `code_span`.
fn in_path_context(text: &str, start: usize, end: usize) -> bool {
    let bytes = text.as_bytes();
    (start > 0 && bytes[start - 1] == b'/') || (end < bytes.len() && bytes[end] == b'/')
}

/// Replace a relative date expression with an absolute date, except in
/// path context.
fn apply_rule(text: &str, re: &Regex, iso: &str) -> String {
    re.replace_all(text, |caps: &regex::Captures| {
        let matched = caps.get(0).expect("group 0 always present");
        if in_path_context(text, matched.start(), matched.end()) {
            caps[0].to_string()
        } else {
            iso.to_string()
        }
    })
    .to_string()
}

/// Cheap pre-filter: does the segment mention any keyword the rewrite
/// passes look for? A miss skips all of them. A hit is a superset of the
/// real matches — word boundaries, case, and path context are re-checked
/// by the passes themselves, so this only needs to avoid false
/// negatives.
fn may_contain_relative_date(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    REWRITE_RULES.iter().any(|(literal, _)| {
        if literal.bytes().all(|b| b.is_ascii()) {
            lower.contains(literal)
        } else {
            content.contains(literal)
        }
    }) || lower.contains("ago")
        || content.contains("天前")
}

/// The date rewrite itself, applied to prose that contains no code spans.
/// Each segment is checked against the keyword list before the rewrite
/// passes run — the prose between two code spans is usually a comma and
/// nothing else. `iso_dates` is index-aligned with [`REWRITE_RULES`] and
/// computed once per fact by the caller.
fn normalize_unquoted(content: &str, today: NaiveDate, iso_dates: &[String]) -> String {
    if !may_contain_relative_date(content) {
        return content.to_string();
    }
    let mut result = content.to_string();

    for (index, _) in REWRITE_RULES.iter().enumerate() {
        result = apply_rule(&result, &RULE_REGEXES[index], &iso_dates[index]);
    }

    // "N days ago" / "N 天前"
    result = DAYS_AGO_EN
        .replace_all(&result, |caps: &regex::Captures| {
            days_before(today, &caps[1], &caps[0])
        })
        .to_string();
    result = DAYS_AGO_CN
        .replace_all(&result, |caps: &regex::Captures| {
            days_before(today, &caps[1], &caps[0])
        })
        .to_string();

    result
}

static DAYS_AGO_EN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+)\s*days?\s*ago").unwrap());

static DAYS_AGO_CN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+)\s*天前").unwrap());

/// Render `days` before `today` as an ISO date, or return `matched` unchanged
/// when the phrase cannot be rewritten.
///
/// The regexes accept an unbounded digit run, so `N` is whatever the memory
/// content says. `chrono::Duration::days` panics once the count overflows its
/// internal nanosecond range, and `NaiveDate - Duration` panics outside the
/// representable date range. A heuristic rewrite must not abort the
/// consolidation pass over user content, so both cases leave the phrase as it
/// was written.
fn days_before(today: NaiveDate, days: &str, matched: &str) -> String {
    let Ok(n) = days.parse::<u64>() else {
        return matched.to_string();
    };
    match today.checked_sub_days(Days::new(n)) {
        Some(date) => date.format("%Y-%m-%d").to_string(),
        None => matched.to_string(),
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

    #[test]
    fn day_count_past_duration_range_leaves_phrase_intact() {
        // 1e14 days overflows `TimeDelta::days`; this used to panic the
        // consolidation pass with "TimeDelta::days out of bounds".
        let phrase = "99999999999999 days ago";
        assert_eq!(normalize_relative_dates(phrase), phrase);
    }

    #[test]
    fn day_count_past_date_range_leaves_phrase_intact() {
        // 9.9e9 days fits `TimeDelta` but leaves `NaiveDate`'s ±262143-year
        // range, so the subtraction used to panic.
        let phrase = "9999999999天前";
        assert_eq!(normalize_relative_dates(phrase), phrase);
    }

    #[test]
    fn day_count_past_date_range_after_parse_leaves_phrase_intact() {
        // Fits u64 and i32, so `checked_sub_days` takes its `add_days` branch
        // and still returns None: 200_000_000 days is ~547k years.
        let phrase = "200000000 days ago";
        assert_eq!(normalize_relative_dates(phrase), phrase);
    }

    #[test]
    fn unparseable_day_count_leaves_phrase_intact() {
        // Does not fit i64. This one did not panic before the fix — the
        // `unwrap_or(0)` fallback rewrote it to today's date, silently
        // corrupting the stored fact.
        let phrase = "99999999999999999999999 days ago";
        assert_eq!(normalize_relative_dates(phrase), phrase);
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

    #[test]
    fn keyword_prefilter_covers_every_rewrite_literal() {
        // A keyword the cheap pre-filter misses would silently never fire
        // (the short-circuit skips its rewrite pass). The rules are one
        // table shared by the passes, the regexes, and this check, so a
        // new literal is covered by existing — but the pre-filter's own
        // logic (ASCII case-folding, plain CJK contains) is still worth
        // pinning per literal.
        for (literal, _) in REWRITE_RULES {
            assert!(
                may_contain_relative_date(literal),
                "prefilter misses {literal:?}"
            );
        }
        // Sentinels for the two "N days ago" regex passes.
        assert!(may_contain_relative_date("42 days ago"));
        assert!(may_contain_relative_date("42 天前"));
    }

    #[test]
    fn path_context_keeps_slash_adjacent_keywords_verbatim() {
        // A keyword adjacent to a path separator is part of a path
        // quoted in prose and must stay verbatim; the same keyword in
        // plain prose still normalizes.
        let path_in_prose = normalize_relative_dates("failed to open notes/today/a.md: missing");
        assert!(
            path_in_prose.contains("notes/today/a.md"),
            "{path_in_prose}"
        );
        let slash_joined = normalize_relative_dates("pick today/tomorrow");
        assert!(slash_joined.contains("today/tomorrow"), "{slash_joined}");
        let plain = normalize_relative_dates("we ship today");
        assert!(!plain.contains("today"), "{plain}");
    }
}
