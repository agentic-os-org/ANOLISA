//! Session history tools — query past sessions and their tool call logs.
//!
//! Tools:
//! - `memory_sessions`: list historical session summaries from facts/summary/
//! - `memory_timeline`: show tool call log from a specific session

use serde::Serialize;

use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::service::MemoryService;

/// Summary of a historical session.
#[derive(Debug, Serialize)]
pub struct SessionSummary {
    /// Session ID.
    pub session_id: String,
    /// When the session summary was created.
    pub created_at: String,
    /// Number of tool calls in the session.
    pub tool_calls: usize,
    /// Tools used (unique names).
    pub tools_used: Vec<String>,
    /// Files modified during the session.
    pub files_modified: Vec<String>,
    /// Brief description (from summary fact content).
    pub description: String,
}

/// A single tool call entry in a session timeline.
#[derive(Debug, Clone, Serialize)]
pub struct TimelineEntry {
    pub timestamp: String,
    pub tool: String,
    pub path: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// List historical session summaries from `facts/summary/` directory.
pub fn memory_sessions(svc: &MemoryService, limit: usize) -> Result<String> {
    let summary_dir = svc.mount.root.join("facts").join("summary");
    let mut sessions: Vec<SessionSummary> = Vec::new();

    if !summary_dir.exists() {
        return Ok("(no historical sessions found)".to_string());
    }

    for entry in std::fs::read_dir(&summary_dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let fm = parse_frontmatter_flat(&content);
        let body = extract_body(&content);

        let session_id = fm.get("session_id").cloned().unwrap_or_else(|| {
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        });
        let created_at = fm.get("created_at").cloned().unwrap_or_default();

        // Parse tool_calls count from content
        let tool_calls = parse_count_from_body(&body);
        let tools_used = parse_list_from_body(&body, "tools:");
        let files_modified = parse_list_from_body(&body, "files:");

        sessions.push(SessionSummary {
            session_id,
            created_at,
            tool_calls,
            tools_used,
            files_modified,
            description: body.chars().take(200).collect(),
        });
    }

    // Sort by created_at descending
    sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    sessions.truncate(limit);

    if sessions.is_empty() {
        return Ok("(no historical sessions found)".to_string());
    }

    svc.audit_log(
        AuditEntry::new("memory_sessions")
            .path(format!("{} sessions", sessions.len()))
            .bytes(0),
    );

    serde_json::to_string_pretty(&sessions)
        .map_err(|e| MemoryError::Other(format!("serialize: {e}")))
}

/// Show the tool call timeline for a specific session.
/// Reads from `.anolisa/session-logs/<session_id>.jsonl`.
pub fn memory_timeline(svc: &MemoryService, session_id: &str, limit: usize) -> Result<String> {
    // Validate session_id to prevent path traversal (e.g. "../../etc/passwd").
    if session_id.is_empty()
        || session_id.contains('/')
        || session_id.contains('\\')
        || session_id.contains("..")
        || session_id.contains('\0')
    {
        return Err(MemoryError::InvalidArgument(format!(
            "invalid session_id: {session_id:?}"
        )));
    }

    let log_path = svc
        .mount
        .meta_dir
        .join("session-logs")
        .join(format!("{session_id}.jsonl"));

    if !log_path.exists() {
        return Err(MemoryError::NotFound(format!(
            "session log for '{session_id}' not found"
        )));
    }

    let content = std::fs::read_to_string(&log_path)?;
    let mut entries: Vec<TimelineEntry> = Vec::new();

    for line in content.lines() {
        if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) {
            entries.push(TimelineEntry {
                timestamp: entry["ts"].as_str().unwrap_or("").to_string(),
                tool: entry["tool"].as_str().unwrap_or("").to_string(),
                path: entry["path"].as_str().unwrap_or("").to_string(),
                ok: entry["ok"].as_bool().unwrap_or(false),
                bytes: entry["bytes"].as_u64(),
                error: entry["error"].as_str().map(|s| s.to_string()),
            });
        }
    }

    // Take the most recent entries
    let total = entries.len();
    if entries.len() > limit {
        entries = entries[entries.len() - limit..].to_vec();
    }

    svc.audit_log(
        AuditEntry::new("memory_timeline")
            .path(session_id.to_string())
            .bytes(entries.len() as u64),
    );

    let mut out = format!(
        "Timeline for session {session_id} ({total} total entries, showing {}):\n\n",
        entries.len()
    );
    for entry in &entries {
        let status = if entry.ok { "✓" } else { "✗" };
        out.push_str(&format!(
            "[{}] {} {} {}\n",
            entry.timestamp, status, entry.tool, entry.path
        ));
        if let Some(ref err) = entry.error {
            out.push_str(&format!("       error: {err}\n"));
        }
    }

    Ok(out)
}

// ── Helpers ─────────────────────────────────────────────────────

fn parse_frontmatter_flat(content: &str) -> std::collections::HashMap<String, String> {
    let mut fm = std::collections::HashMap::new();
    if let Some(rest) = content.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---") {
            for line in rest[..end].lines() {
                if let Some((key, value)) = line.split_once(": ") {
                    let key = key.trim();
                    let value = value.trim().trim_matches('"');
                    if !key.starts_with(' ') && !key.starts_with('-') {
                        fm.insert(key.to_string(), value.to_string());
                    }
                }
            }
        }
    }
    fm
}

fn extract_body(content: &str) -> String {
    // Same fence rule as the canonical key reader, so a file that ends at its
    // closing fence yields an empty body instead of the raw fence block
    // (GH-3580).
    match super::frontmatter::split(content) {
        Some((_, body)) => body.trim().to_string(),
        None => content.trim().to_string(),
    }
}

/// Ordered most-specific first; the first needle present in a line wins.
const COUNT_NEEDLES: &[&str] = &["次工具调用", "tool calls", "工具调用", "tool call", "调用"];

/// Clause delimiters for the count scans: the number quantifying the
/// keyword has to sit in the keyword's own clause, so a duration in the
/// previous one ("耗时 15 分钟，调用 mem_read", "耗时 15 分钟: 工具调用 42
/// 次") cannot be picked up. The ASCII colon joins the set because it is
/// the common separator before a count in mixed-language bodies.
const CLAUSE_DELIMS: [char; 13] = [
    '，', '。', '；', '、', '：', ',', ';', '.', '!', '?', ':', '（', '）',
];

/// Text after the last clause delimiter in `head`.
///
/// Both slice boundaries are char-aligned: `rfind(char)` returns a
/// boundary-aligned offset, and `len_utf8` skips the whole delimiter.
fn clause_before(head: &str) -> &str {
    match head.rfind(CLAUSE_DELIMS) {
        Some(i) => {
            let end = i + head[i..].chars().next().map_or(1, char::len_utf8);
            &head[end..]
        }
        None => head,
    }
}

/// Text up to the next clause delimiter in `tail`.
fn clause_after(tail: &str) -> &str {
    match tail.find(CLAUSE_DELIMS) {
        Some(i) => &tail[..i],
        None => tail,
    }
}

/// First digit run in the keyword's own following clause that is immediately
/// quantified ("42 次" / "42次").
///
/// For the verb-like needles ("调用", "tool call(s)") the count normally
/// *follows* the verb in the verb-object-counter word order ("调用工具 42
/// 次", "调用 mem_read 15 次"), and any digit before the verb is usually a
/// distractor (a version number, a retry count, a duration). Byte offsets
/// into an ASCII-digit run are char-aligned, so the slices are safe.
fn quantified_forward(tail: &str) -> Option<usize> {
    let clause = clause_after(tail);
    let bytes = clause.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let parsed = if clause[i..].trim_start().starts_with('次') {
                clause[start..i].parse::<usize>().ok()
            } else {
                None
            };
            if let Some(count) = parsed {
                return Some(count);
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Digits directly after the keyword, allowing whitespace, one quantifier
/// noun and one colon: accepts "… 42 次", "…次数: 42", "…总数 42", "…: 42";
/// rejects "…: 无数据，耗时 15 分钟" and "…（成功 40，失败 2）共 42 次".
/// The noun list is longest-first because only one noun is stripped.
fn quantified_after(tail: &str) -> Option<usize> {
    let mut t = tail.trim_start();
    for noun in ["总次数", "总数", "次数", "合计", "数", "次"] {
        if let Some(rest) = t.strip_prefix(noun) {
            t = rest.trim_start();
            break;
        }
    }
    let t = t.trim_start_matches([':', '：']).trim_start();
    let n = t.chars().take_while(char::is_ascii_digit).count();
    (n > 0).then(|| t[..n].parse().ok()).flatten()
}

/// A "total N times" pattern ("共 42 次") anywhere in `text`.
///
/// Hand-authored summaries put the total in a later clause than the
/// keyword ("工具调用（成功 40，失败 2）共 42 次") or behind tool names
/// ("调用 gpt-4o 与 mem_write，共 42 次"), where `quantified_after`'s
/// adjacency cannot reach — or in an earlier one ("共 42 次，工具调用
/// 全部成功"), which the clause-bounded look-back deliberately cannot
/// reach either, so the scan runs on both sides of the keyword. The 共
/// prefix distinguishes a total from an ordinal ("第 3 次"), and requiring
/// the quantified 次 to close the clause ("…共 42 次" / "…共 42 次，…")
/// keeps "共 3 次重试" — a total of *retries* — from being read as a
/// call count, on either side.
fn total_scan(text: &str) -> Option<usize> {
    let mut rest = text;
    while let Some(rel) = rest.find('共') {
        let after = &rest[rel + '共'.len_utf8()..];
        let t = after.trim_start();
        let n = t.chars().take_while(char::is_ascii_digit).count();
        let closes_clause = n > 0
            && t[n..]
                .trim_start()
                .strip_prefix('次')
                .is_some_and(|behind| {
                    behind.trim_start().is_empty() || behind.trim_start().starts_with(CLAUSE_DELIMS)
                });
        let parsed = if closes_clause {
            t[..n].parse::<usize>().ok()
        } else {
            None
        };
        if let Some(count) = parsed {
            return Some(count);
        }
        rest = after;
    }
    None
}

/// True when the digits right after the keyword quantify *tools*, not calls
/// ("调用 3 个工具"). Such a run is a weaker candidate than a real total, so
/// the 共 N 次 scans get priority over it.
fn tool_measure_after(tail: &str) -> bool {
    let t = tail.trim_start();
    let n = t.chars().take_while(char::is_ascii_digit).count();
    n > 0 && t[n..].trim_start().starts_with('个')
}

fn parse_count_from_body(body: &str) -> usize {
    // The count is anchored to a call-count keyword, not "first number on
    // the line": the summary generator writes the duration before the
    // count ("Session 持续约 15 分钟，共 42 次工具调用（…）"), so a
    // first-number scan reports the minutes as the count.
    //
    // The needle loop is the OUTER one so a needle's specificity applies
    // across the whole body: the loose 调用 tail only gets a turn when the
    // tighter wordings are absent from every line, not just from the
    // first line that contains 调用. Within one hit the verb-like needles
    // first look for a count in their own following clause ("调用工具 42
    // 次" — the count follows the verb, so a digit before it is usually a
    // distractor); otherwise the number before the keyword wins over one
    // after it, and both word orders ("42 次工具调用" and "工具调用 42
    // 次") report the count.
    //
    // Both scans are bounded to the hit's own clause (#8308): look-back
    // stops at the last clause delimiter so a duration in the previous
    // clause cannot win, and fall-forward requires the digits to sit
    // directly after the keyword (modulo a quantifier noun and colon) so
    // digits inside tool names ("gpt-4o", "sqlite3") and parenthesised
    // breakdowns ("（成功 40，失败 2）") are skipped.
    for needle in COUNT_NEEDLES {
        let verb_like = matches!(*needle, "调用" | "tool call" | "tool calls");
        for line in body.lines() {
            // Every occurrence of the needle on the line, not just the first:
            // a hand-authored line can mention the verb before the count
            // carries it ("调用 mem_write 失败，随后调用 mem_read 15 次").
            // Bounded: an unbounded occurrence loop makes a long
            // unpunctuated line quadratic (measured 71 s for one 56 KB
            // line vs 2.2 ms on main).
            for (pos, hit) in line.match_indices(needle).take(32) {
                let tail = &line[pos + hit.len()..];
                let forward = if verb_like {
                    quantified_forward(tail)
                } else {
                    None
                };
                if let Some(count) = forward {
                    return count;
                }
                if let Some(count) = quantified_back(&line[..pos]) {
                    return count;
                }
                let weak = tool_measure_after(tail);
                if !weak {
                    if let Some(count) = quantified_after(tail) {
                        return count;
                    }
                }
                if let Some(count) = total_scan(tail) {
                    return count;
                }
                if let Some(count) = total_scan(&line[..pos]) {
                    return count;
                }
                if weak {
                    if let Some(count) = quantified_after(tail) {
                        return count;
                    }
                }
            }
        }
    }
    // Last resort: the count can sit on a line of its own, away from every
    // keyword ("…\n工具调用若干\n共 7 次"). All scans above are per-line.
    if let Some(count) = total_scan(body) {
        return count;
    }
    0
}

/// Last digit run in the keyword's own preceding clause that is separated
/// from the keyword only by whitespace, other digits and a quantifier noun
/// ("42 次工具调用", "共 15 次调用工具", "Session had 42 tool calls").
///
/// Anything else in the gap disqualifies the run, which is what keeps a
/// duration that shares the keyword's clause from being reported as the
/// count ("耗时 15 分钟 无工具调用" / "Session lasted 15 minutes
/// with no tool calls." read 0, not 15) — the gap of the count itself is
/// empty or a bare quantifier. Digits stay allowed in the gap so an
/// unparseable run (a hash- or ID-like token) is skipped instead of
/// discarding the already-parsed neighbour. Byte offsets into an
/// ASCII-digit run are char-aligned, so the slices are safe.
fn quantified_back(head: &str) -> Option<usize> {
    let clause = clause_before(head);
    let bytes = clause.as_bytes();
    let mut found = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let gap_ok = clause[i..].chars().all(|c| {
                c.is_whitespace() || c.is_ascii_digit() || matches!(c, '次' | '个' | '回')
            });
            let parsed = if gap_ok {
                clause[start..i].parse::<usize>().ok()
            } else {
                None
            };
            if let Some(count) = parsed {
                found = Some(count);
            }
        } else {
            i += 1;
        }
    }
    found
}

fn parse_list_from_body(body: &str, prefix: &str) -> Vec<String> {
    for line in body.lines() {
        let lower = line.to_lowercase();
        if lower.starts_with(prefix) || lower.contains(prefix.trim_end_matches(':')) {
            // Extract comma-separated or backtick-quoted items
            let rest = line.split_once(':').map(|(_, v)| v).unwrap_or(line);
            return rest
                .split(',')
                .map(|s| s.trim().trim_matches('`').trim_matches('\'').to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_frontmatter_basic() {
        let content = "---\nsession_id: ses_abc\ncreated_at: 2026-06-11T10:00:00Z\n---\nBody";
        let fm = parse_frontmatter_flat(content);
        assert_eq!(fm.get("session_id").unwrap(), "ses_abc");
    }

    #[test]
    fn extract_body_works() {
        let content = "---\nid: x\n---\n\nSession had 42 tool calls.";
        let body = extract_body(content);
        assert!(body.contains("42 tool calls"));
    }

    #[test]
    fn extract_body_closing_fence_without_trailing_newline() {
        // GH-3580: a summary that ends at its closing fence has no body, so
        // the counts parsed out of it stay zero instead of reading frontmatter
        // values as if they were body text.
        let content = "---\nid: x\n---";
        assert_eq!(extract_body(content), "");
        assert_eq!(
            extract_body("---\nid: x\n---\nSession had 42 tool calls."),
            "Session had 42 tool calls."
        );
    }

    #[test]
    fn parse_count_from_body_finds_number() {
        let body = "Session had 42 tool calls across 5 tools.";
        assert_eq!(parse_count_from_body(body), 42);
    }

    #[test]
    fn parse_count_chinese() {
        let body = "共 15 次工具调用";
        assert_eq!(parse_count_from_body(body), 15);
    }

    #[test]
    fn parse_count_returns_zero_when_not_found() {
        let body = "No tool calls mentioned here.";
        assert_eq!(parse_count_from_body(body), 0);
    }

    #[test]
    fn parse_count_generator_format_reports_calls_not_minutes() {
        // Regression: the summary generator writes the duration before the
        // call count, so the old first-number scan reported the minutes.
        let body = "Session 持续约 15 分钟，共 42 次工具调用（40 成功，2 失败），使用工具类型: `mem_write`，总数据量 1234 bytes。";
        assert_eq!(parse_count_from_body(body), 42);
    }

    #[test]
    fn parse_count_accepts_count_after_keyword_word_order() {
        // Free-form and agent-authored summaries also put the count after
        // the keyword; the exact-substring fallback of this PR's first
        // revision returned 0 for these (main's loose tail already
        // handled them). The needle table keeps the precision (a bare
        // duration line still reports 0) while restoring the recall.
        assert_eq!(parse_count_from_body("工具调用 42 次"), 42);
        assert_eq!(parse_count_from_body("本次会话共 15 次调用工具"), 15);
    }

    #[test]
    fn parse_count_skips_unparseable_runs_without_losing_the_count() {
        // A hash- or ID-like token between the count and the keyword must
        // not discard the count: a look-back that only kept the last
        // parseable run clobbered a good value with None in an
        // intermediate revision of this patch when a later, unparseable
        // run appeared.
        assert_eq!(
            parse_count_from_body("42 99999999999999999999999999 次工具调用"),
            42
        );
    }

    #[test]
    fn parse_count_duration_without_call_keyword_is_zero() {
        // The old "any line containing 次" fallback reported the minutes of
        // a plain duration line as the call count.
        assert_eq!(parse_count_from_body("第一次尝试，耗时 15 分钟"), 0);
    }

    #[test]
    fn parse_count_fact_title_and_other_english_forms() {
        // Fact titles and non-canonical English phrasings carried the same
        // duration-first trap as the generator line; the keyword anchoring
        // fixes them all (pinned so the behaviour cannot silently regress).
        assert_eq!(parse_count_from_body("活跃会话 15 分钟，42 次工具调用"), 42);
        assert_eq!(
            parse_count_from_body("Session 持续约 <1 分钟，共 7 次工具调用（…）"),
            7
        );
        assert_eq!(
            parse_count_from_body("Session lasted 15 minutes with 42 tool calls."),
            42
        );
    }

    #[test]
    fn parse_count_clause_bounded_shapes() {
        // The #8308 matrix: hand-authored summaries where the unbounded
        // in-line digit scans misread the shape. Look-back must stay inside
        // the keyword's own clause, and the fall-forward must not walk into
        // tool names or parenthesised breakdowns.
        // A quantifier noun between keyword and number must not break the
        // parse (the strict-adjacency variant of the fix returned 0 here).
        assert_eq!(parse_count_from_body("工具调用次数: 42"), 42);
        // Negated keyword: the duration in the following clause must not be
        // reported as the count.
        assert_eq!(
            parse_count_from_body("本次会话未产生工具调用，持续 15 分钟"),
            0
        );
        // Digits inside tool names must not be picked up on either side.
        assert_eq!(
            parse_count_from_body("调用 gpt-4o 与 mem_write，共 42 次"),
            42
        );
        assert_eq!(parse_count_from_body("调用 sqlite3 共 42 次"), 42);
        // A parenthesised success/failure breakdown sits between keyword and
        // the total; the total must win.
        assert_eq!(
            parse_count_from_body("工具调用（成功 40，失败 2）共 42 次"),
            42
        );
    }

    #[test]
    fn parse_count_verb_forward_shapes() {
        // Verb-object-counter word order: the count follows the verb, and a
        // digit before the verb (retry count, duration, version number) is a
        // distractor. The clause-bounded look-back alone regressed all of
        // these to 0 because the loose 调用 needle had no route at all to a
        // count that follows it (the recall rows tracked on #8308).
        assert_eq!(parse_count_from_body("调用工具 42 次"), 42);
        assert_eq!(parse_count_from_body("本次会话调用工具 42 次。"), 42);
        assert_eq!(parse_count_from_body("本次会话共调用工具 42 次"), 42);
        assert_eq!(parse_count_from_body("调用 mem_read 15 次"), 15);
        // Two per-tool counts in one line: the first clause's count is the
        // best a single-number answer can do without summing.
        assert_eq!(
            parse_count_from_body("调用 mem_write 7 次，mem_read 3 次"),
            7
        );
        // Synonym quantifier nouns after the keyword (合计, 总数).
        assert_eq!(parse_count_from_body("工具调用合计 42 次"), 42);
        assert_eq!(parse_count_from_body("工具调用总数 42"), 42);
        // A distractor digit before the verb must not win over the forward
        // count.
        assert_eq!(parse_count_from_body("gpt-5 调用 12 次"), 12);
        assert_eq!(parse_count_from_body("重试 3 次后调用工具 42 次"), 42);
        assert_eq!(parse_count_from_body("耗时 15 分钟后调用工具 42 次"), 42);
        assert_eq!(parse_count_from_body("第 3 次尝试中共调用工具 42 次"), 42);
        assert_eq!(
            parse_count_from_body("使用 claude-3.5-sonnet，调用 mem_read 15 次"),
            15
        );
    }

    #[test]
    fn parse_count_ascii_colon_bounds_the_clause() {
        // The ASCII colon is a clause delimiter like the fullwidth one, so a
        // duration (or error count, or version) in the colon-separated
        // prefix cannot be picked up as the count.
        assert_eq!(parse_count_from_body("使用 gpt-4.1: 调用工具 42 次"), 42);
        assert_eq!(parse_count_from_body("耗时 15 分钟: 工具调用 42 次"), 42);
        assert_eq!(parse_count_from_body("Duration 15 min: tool calls 42"), 42);
        assert_eq!(parse_count_from_body("Errors 3: tool calls 42"), 42);
        assert_eq!(parse_count_from_body("失败 2 次: 工具调用 42 次"), 42);
        assert_eq!(parse_count_from_body("Notes v1.2: tool calls 42"), 42);
    }

    #[test]
    fn parse_count_ignores_totals_of_other_things() {
        // "共 N 次" quantifying retries, not tool calls: the quantified 次
        // must close its clause for total_scan to accept it.
        // Remaining out-of-scope rows are tracked on #8308.
        assert_eq!(
            parse_count_from_body("耗时 15 分钟，调用若干\n共 42 次工具调用"),
            42
        );
        assert_eq!(parse_count_from_body("耗时 15 分钟，调用了 3 个工具"), 0);
        assert_eq!(parse_count_from_body("工具调用次数未知，耗时 15 分钟"), 0);
        assert_eq!(parse_count_from_body("次工具调用: 无数据，耗时 15 分钟"), 0);
        assert_eq!(parse_count_from_body("工具调用失败，共 3 次重试"), 0);
        assert_eq!(
            parse_count_from_body("未记录工具调用，共 15 次重试后放弃"),
            0
        );
    }

    #[test]
    fn parse_count_second_needle_occurrence_on_the_line() {
        // line.find() only ever tried the needle's FIRST occurrence, so a line
        // that mentions the verb before the count carries it read 0 (the
        // multi-occurrence recall rows tracked on #8308).
        assert_eq!(
            parse_count_from_body("调用 mem_write 失败，随后调用 mem_read 15 次"),
            15
        );
        assert_eq!(
            parse_count_from_body("先调用 mem_write，再调用 mem_read 15 次"),
            15
        );
        assert_eq!(
            parse_count_from_body("工具调用失败，重试后调用 mem_read 15 次"),
            15
        );
        assert_eq!(
            parse_count_from_body("调用 mem_write（未成功），调用 mem_read 15 次"),
            15
        );
        assert_eq!(
            parse_count_from_body("本次调用工具：mem_write、mem_read，共调用 42 次"),
            42
        );
        // The total may also sit in a clause BEFORE the keyword, where the
        // clause-bounded look-back deliberately cannot reach it.
        assert_eq!(parse_count_from_body("共 42 次，工具调用全部成功"), 42);
        // The 次-closes-its-clause guard still rejects a total of retries.
        assert_eq!(
            parse_count_from_body("共 3 次重试后放弃，未产生工具调用"),
            0
        );
    }

    #[test]
    fn parse_count_duration_in_the_keywords_own_clause_is_not_a_count() {
        // The look-back run has to be the keyword's own quantifier, so a duration
        // sharing the clause cannot be reported as the count (the duration rows
        // tracked on #8308).
        assert_eq!(parse_count_from_body("耗时 15 分钟 无工具调用"), 0);
        assert_eq!(parse_count_from_body("耗时 15 分钟 调用 mem_read"), 0);
        assert_eq!(parse_count_from_body("耗时 15 分钟 工具调用若干"), 0);
        assert_eq!(
            parse_count_from_body("Session lasted 15 minutes with no tool calls."),
            0
        );
        // A real count in the same clause still wins over the duration.
        assert_eq!(
            parse_count_from_body("Session 持续 15 分钟 共 42 次工具调用"),
            42
        );
        assert_eq!(parse_count_from_body("耗时 2 小时 共 5 次调用"), 5);
        // The committed overflow behaviour is unchanged: digits stay allowed in
        // the gap, so an unparseable run does not hide its parseable neighbour.
        assert_eq!(
            parse_count_from_body("42 99999999999999999999999999 次工具调用"),
            42
        );
    }

    /// The round-8 review's 95-row differential corpus, verbatim: every
    /// committed expectation, #8308's declared rows, the round-7 additions
    /// and 33 adversarial rows. Rows whose comment below says "recorded"
    /// are not scored - their value is a disclosed remainder (tracked on
    /// #8308), pinned here so drift is visible. This table is what makes
    /// the corpus number in the PR discussion reproducible without
    /// rebuilding it out of review prose.
    #[test]
    fn parse_count_differential_corpus() {
        let rows: &[(&str, usize)] = &[
            ("Session had 42 tool calls across 5 tools.", 42),
            ("共 15 次工具调用", 15),
            ("No tool calls mentioned here.", 0),
            (
                "Session 持续约 15 分钟，共 42 次工具调用（40 成功，2 失败），使用工具类型: `mem_write`，总数据量 1234 bytes。",
                42,
            ),
            ("工具调用 42 次", 42),
            ("本次会话共 15 次调用工具", 15),
            ("42 99999999999999999999999999 次工具调用", 42),
            ("第一次尝试，耗时 15 分钟", 0),
            ("活跃会话 15 分钟，42 次工具调用", 42),
            ("Session 持续约 <1 分钟，共 7 次工具调用（…）", 7),
            ("Session lasted 15 minutes with 42 tool calls.", 42),
            ("工具调用次数: 42", 42),
            ("本次会话未产生工具调用，持续 15 分钟", 0),
            ("调用 gpt-4o 与 mem_write，共 42 次", 42),
            ("调用 sqlite3 共 42 次", 42),
            ("工具调用（成功 40，失败 2）共 42 次", 42),
            ("调用工具 42 次", 42),
            ("本次会话调用工具 42 次。", 42),
            ("本次会话共调用工具 42 次", 42),
            ("调用 mem_read 15 次", 15),
            ("调用 mem_write 7 次，mem_read 3 次", 7),
            ("工具调用合计 42 次", 42),
            ("工具调用总数 42", 42),
            ("gpt-5 调用 12 次", 12),
            ("重试 3 次后调用工具 42 次", 42),
            ("耗时 15 分钟后调用工具 42 次", 42),
            ("第 3 次尝试中共调用工具 42 次", 42),
            ("使用 claude-3.5-sonnet，调用 mem_read 15 次", 15),
            ("使用 gpt-4.1: 调用工具 42 次", 42),
            ("耗时 15 分钟: 工具调用 42 次", 42),
            ("Duration 15 min: tool calls 42", 42),
            ("Errors 3: tool calls 42", 42),
            ("失败 2 次: 工具调用 42 次", 42),
            ("Notes v1.2: tool calls 42", 42),
            ("耗时 15 分钟，调用若干\n共 42 次工具调用", 42),
            ("耗时 15 分钟，调用了 3 个工具", 0),
            ("工具调用次数未知，耗时 15 分钟", 0),
            ("次工具调用: 无数据，耗时 15 分钟", 0),
            ("工具调用失败，共 3 次重试", 0),
            ("未记录工具调用，共 15 次重试后放弃", 0),
            // round-7 §C: multi-occurrence lines.
            ("调用 mem_write（未成功），调用 mem_read 15 次", 15),
            ("调用 mem_write 失败，随后调用 mem_read 15 次", 15),
            ("先调用 mem_write，再调用 mem_read 15 次", 15),
            ("工具调用失败，重试后调用 mem_read 15 次", 15),
            ("本次调用工具：mem_write、mem_read，共调用 42 次", 42),
            ("共 42 次，工具调用全部成功", 42),
            ("共 3 次重试后放弃，未产生工具调用", 0),
            // round-7 §D: duration in the keyword's own clause.
            ("耗时 15 分钟 无工具调用", 0),
            ("耗时 15 分钟 调用 mem_read", 0),
            ("耗时 15 分钟 工具调用若干", 0),
            ("持续 15 分钟 本次会话未产生工具调用", 0),
            ("本次会话 15 分钟 未产生任何调用", 0),
            ("Session lasted 15 minutes with no tool calls.", 0),
            ("Session 持续 15 分钟 共 42 次工具调用", 42),
            ("耗时 2 小时 共 5 次调用", 5),
            ("42 99999999999999999999999999 次工具调用", 42),
            // #8308 declared remainders: recorded, not scored.
            ("共 1,234 次工具调用", 234),
            ("Tool Calls: 42", 0),
            ("本次调用 3 个工具", 3),
            ("调用 mem_write 7 次，mem_read 3 次（合计 10 次）", 7),
            // Recorded: the left-side total reads a non-call total (#8308).
            ("共 5 次，重试失败，工具调用未记录", 5),
            ("共 3 次失败，随后工具调用 42 次", 42),
            ("共 42 次，无工具调用", 42),
            ("共 3 次重试，未产生工具调用", 0),
            ("共 12 次，工具调用耗时 15 分钟", 12),
            ("调用 mem_read 15 次，随后调用 mem_write 42 次", 15),
            (
                "工具调用失败，重试后调用 mem_read 15 次，再调用 mem_write 7 次",
                15,
            ),
            ("共 42 次工具调用，其中调用 mem_read 15 次", 42),
            // Recorded: a tool measure right after the verb (#8308).
            ("调用 3 个工具", 3),
            ("调用了 3 个工具", 0),
            ("本次会话调用 5 个工具，共 42 次", 42),
            ("第 2 次工具调用耗时 15 分钟", 2),
            ("耗时 15 分钟，工具调用 42 次", 42),
            ("工具调用耗时 15 分钟，共 42 次", 42),
            ("Session 持续 15 分钟，42 tool calls", 42),
            ("15 分钟 42 次工具调用", 42),
            ("1 tool call only", 1),
            ("tool calls: 42", 42),
            ("Total: 42 tool calls in 15 minutes", 42),
            ("42 tool calls, 15 minutes", 42),
            ("The session made 42 tool calls over 15 minutes.", 42),
            ("15 minutes, 42 tool calls", 42),
            ("", 0),
            ("\n\n", 0),
            ("工具调用", 0),
            ("调用", 0),
            ("共 次工具调用", 0),
            ("工具调用\u{7f}\u{1f600} 42 次", 42),
            ("调用次数：４２", 0),
            ("工具调用 0 次", 0),
            ("共 0 次，无工具调用", 0),
            ("工具调用 9999999999999999999999999 次", 0),
            ("第一行 耗时 15 分钟\n第二行 工具调用 42 次", 42),
            ("工具调用 42 次\n耗时 15 分钟", 42),
            ("耗时 15 分钟\n工具调用若干\n共 7 次", 7),
        ];
        for (body, expected) in rows {
            assert_eq!(parse_count_from_body(body), *expected, "body: {body:?}");
        }
    }

    /// Guards the writer/reader contract, not just the parser: if
    /// rule_session_summary's wording changes, this fails, whereas the
    /// fixture-based tests above would stay green and drift silently.
    #[test]
    fn summary_count_round_trips_through_the_real_generator() {
        use crate::config::ConsolidationConfig;
        use crate::consolidation::{FactCategory, OwnedAuditEntry, run_consolidation_owned};

        // 7 calls spread over 12 minutes, so the duration and the count
        // differ and a first-number scan cannot pass by coincidence.
        let entries: Vec<OwnedAuditEntry> = (0..7usize)
            .map(|i| OwnedAuditEntry {
                ts: format!("2026-10-19T10:{:02}:00+00:00", i * 2),
                tool: if i % 2 == 0 { "mem_write" } else { "mem_read" }.to_string(),
                path: "notes/a.md".to_string(),
                ok: i != 3,
                bytes: Some(100),
                error: None,
                trace_id: None,
            })
            .collect();

        let facts =
            run_consolidation_owned(&entries, "round-trip", &ConsolidationConfig::default());
        let summary = facts
            .iter()
            .find(|f| f.category == FactCategory::Summary)
            .expect("rule_session_summary always emits one summary fact");

        let md = summary.to_markdown(); // exactly what writer.rs puts on disk
        assert_eq!(parse_count_from_body(&extract_body(&md)), entries.len());
    }
}
