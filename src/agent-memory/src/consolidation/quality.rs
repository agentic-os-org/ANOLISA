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
pub fn normalize_relative_dates(content: &str) -> String {
    let today = Local::now().date_naive();
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
}
