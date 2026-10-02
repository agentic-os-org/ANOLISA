//! Per-call status classification: did this one tool call succeed, fail, get
//! blocked, or stay unknown.
//!
//! Rules are applied in the priority order of `attribution_decision_spec.md`
//! §1 and short-circuit on the first match. The ordering is not cosmetic — it
//! encodes two measured facts about the corpus:
//!
//! * 56% of genuine failures (375 of 670) contain no error keyword at all, so
//!   text scanning cannot be the primary judge;
//! * 265 results read like errors while the call actually succeeded, so text
//!   scanning cannot be trusted without an exit-code short-circuit ahead of it.
//!
//! Consequently a free-text error word never outranks a structured signal, and
//! `Exit code 0` ends the judgment outright.

use agentsight_atif::{ObservationResult, ToolCall};

/// Outcome of a single tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallStatus {
    /// Succeeded, or at least produced no evidence of failure.
    Ok,
    /// Expected non-zero result from an existence/availability probe. The error
    /// *is* the answer, so it must not be counted as a defect.
    OkProbe,
    /// Failed on the provider's own report or on an anchored failure signature.
    Failed,
    /// Stopped by the user or by policy. Not an agent defect.
    Blocked,
    /// Could not be judged: no result correlated, or the payload is a
    /// placeholder. Explicitly not `Ok` — callers must not read it as success.
    Unknown,
}

/// How much weight a verdict carries. Only deterministic rules run here, so
/// `Medium` marks the single text-derived rule that needs human review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    High,
    Medium,
}

/// A classified call, carrying the rule that decided it and the excerpt that
/// proves it. Both are required: a verdict nobody can re-check is not evidence.
#[derive(Debug, Clone)]
pub struct CallVerdict {
    pub status: CallStatus,
    pub confidence: Confidence,
    /// Spec rule identifier, e.g. `"R1"`, `"R3(exit0)"`, `"B3"`.
    pub matched_rule: &'static str,
    /// Up to 200 chars of the payload starting at the match.
    pub evidence_quote: Option<String>,
    /// A downstream system's error quoted inside an otherwise successful
    /// payload. Recorded as an annotation because it does not change this
    /// call's status (spec B4).
    pub nested_error: Option<String>,
}

/// Longest excerpt kept as proof, matching the spec's quoting limit.
const QUOTE_LIMIT: usize = 200;

/// Window in which a weak text signal is still considered to describe *this*
/// call rather than something quoted further down the payload (spec B2).
const WEAK_SIGNAL_WINDOW: usize = 200;

/// Markers of a user or policy stop. Matching any means the call never ran to
/// completion by the agent's choice (spec R2 / B1).
const INTERCEPTION_MARKERS: &[&str] = &[
    "The user doesn't want to proceed",
    "User denied permission",
    "Permission denied: User denied",
    "[Request interrupted by user",
    // Observed in a live capture: a pending permission prompt is reported as a
    // tool error, so without this marker the user's own decision would be
    // charged to the agent.
    "haven't granted",
    "has not granted",
];

/// Anchored failure signatures (spec R4). Compared case-insensitively because
/// the same condition is reported with different capitalisation by different
/// tools (`No such file` from `ls`, `no such file` from a reader).
const FAILURE_SIGNATURES: &[&str] = &[
    "tool parameter validation failed",
    // Wording a live capture actually produced when a tool rejected its
    // arguments; the phrasing above never matched it.
    "validation failed for tool",
    // Observed in a live capture with neither an error flag nor an exit code, so
    // nothing else would have caught it.
    "command not found",
    "file has not been read yet",
    "only edit on /",
    "no such file or directory",
    "upstream server not found",
    "tool execution failed:",
    "error during web fetch for",
];

/// Commands whose whole purpose is to ask whether something exists. A non-zero
/// exit from these is the answer, not a fault (spec B3).
const PROBE_COMMANDS: &[&str] = &["ls", "test", "which", "stat", "pgrep", "ping"];

/// Payload markers meaning the call never actually produced output (spec B8).
const PLACEHOLDER_MARKERS: &[&str] = &["pending-post-tool-use"];

/// Classify one tool call against the observation result correlated to it.
///
/// `result` is `None` when no `ObservationResult.source_call_id` matched this
/// call's `tool_call_id`, which yields `Unknown` rather than a guess.
pub fn classify_call(call: &ToolCall, result: Option<&ObservationResult>) -> CallVerdict {
    let Some(result) = result else {
        return verdict(CallStatus::Unknown, Confidence::High, "R0", None);
    };

    let text = result_text(result);
    let nested = nested_error(&text);

    // R2 first — a user or policy stop must never be attributed to the agent.
    // This deliberately outranks the provider flag: a live capture shows a
    // pending permission prompt arriving with `is_error: true`, so checking the
    // flag first would turn the user's own refusal into an agent failure.
    if let Some(idx) = find_any(&text, INTERCEPTION_MARKERS) {
        return with_nested(
            verdict(
                CallStatus::Blocked,
                Confidence::High,
                "R2",
                quote_from(&text, idx),
            ),
            nested,
        );
    }

    // R1 — the provider's own flag. Note that an explicit `false` is *not*
    // treated as proof of success: the flag is only ever `true` or absent in
    // the measured corpus, so a `false` would be an untested code path. Letting
    // it fall through can only add evidence, never excuse a real failure.
    if result.is_error() == Some(true) {
        return with_nested(
            verdict(
                CallStatus::Failed,
                Confidence::High,
                "R1",
                quote_from(&text, 0),
            ),
            nested,
        );
    }

    // R3 — a structured exit code beats any text reading, in both directions.
    if let Some((code, idx)) = last_exit_code(&text) {
        if code == 0 {
            // Deliberate short-circuit: error words in the body of a
            // successful command (a grep hit, a log line) are not this call's
            // failure. This is the single biggest false-positive guard (B2).
            return with_nested(
                verdict(
                    CallStatus::Ok,
                    Confidence::High,
                    "R3(exit0)",
                    quote_from(&text, idx),
                ),
                nested,
            );
        }
        let status = if is_probe(call) {
            CallStatus::OkProbe
        } else {
            CallStatus::Failed
        };
        let rule = if status == CallStatus::OkProbe {
            "B3"
        } else {
            "R3"
        };
        // Quote the error output, not the exit-code marker: the code decides the
        // verdict but says nothing about why, and "exited with code 1)" is
        // useless to a reader trying to check the finding.
        let quote_at = weak_error_prefix(&text).unwrap_or(0);
        return with_nested(
            verdict(status, Confidence::High, rule, quote_from(&text, quote_at)),
            nested,
        );
    }

    // R4 — anchored signatures, still demotable to a probe result.
    if let Some(idx) = find_signature(&text) {
        let status = if is_probe(call) {
            CallStatus::OkProbe
        } else {
            CallStatus::Failed
        };
        let rule = if status == CallStatus::OkProbe {
            "B3"
        } else {
            "R4"
        };
        return with_nested(
            verdict(status, Confidence::High, rule, quote_from(&text, idx)),
            nested,
        );
    }

    // B8 — empty or placeholder payloads are unknown, never OK, so that an
    // agent claiming success on top of one can be caught later.
    if is_placeholder_or_empty(&text) {
        return with_nested(
            verdict(
                CallStatus::Unknown,
                Confidence::High,
                "B8",
                quote_from(&text, 0),
            ),
            nested,
        );
    }

    // R5 — last resort. Only a line that *starts* with an error token inside
    // the opening window counts, and it is flagged for review rather than
    // trusted, because this is where the 265 measured false positives live.
    if let Some(idx) = weak_error_prefix(&text) {
        return with_nested(
            verdict(
                CallStatus::Failed,
                Confidence::Medium,
                "R5",
                quote_from(&text, idx),
            ),
            nested,
        );
    }

    with_nested(
        verdict(CallStatus::Ok, Confidence::High, "default", None),
        nested,
    )
}

fn verdict(
    status: CallStatus,
    confidence: Confidence,
    matched_rule: &'static str,
    evidence_quote: Option<String>,
) -> CallVerdict {
    CallVerdict {
        status,
        confidence,
        matched_rule,
        evidence_quote,
        nested_error: None,
    }
}

fn with_nested(mut v: CallVerdict, nested: Option<String>) -> CallVerdict {
    v.nested_error = nested;
    v
}

/// Flatten a result payload to text. Every producer writes a JSON string, but
/// the schema permits any value, so non-strings are rendered rather than lost.
pub fn result_text(result: &ObservationResult) -> String {
    match result.content.as_ref() {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// Excerpt at most [`QUOTE_LIMIT`] chars starting at a byte offset, clamped to a
/// char boundary so multi-byte payloads cannot panic.
fn quote_from(text: &str, byte_idx: usize) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    let start = (0..=byte_idx)
        .rev()
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(0);
    Some(text[start..].chars().take(QUOTE_LIMIT).collect())
}

fn find_any(text: &str, markers: &[&str]) -> Option<usize> {
    markers.iter().filter_map(|m| text.find(m)).min()
}

fn find_signature(text: &str) -> Option<usize> {
    let lower = text.to_lowercase();
    let mut hit = FAILURE_SIGNATURES
        .iter()
        .filter_map(|s| lower.find(s))
        .min();

    // Two-part contract signature: both halves must be present, otherwise
    // "Invalid arguments" alone would catch ordinary validation chatter.
    if hit.is_none()
        && lower.contains("invalid arguments for")
        && lower.contains("missing required")
    {
        hit = lower.find("invalid arguments for");
    }

    // A traceback only indicts this call when it opens the payload; quoted
    // deeper down it belongs to something the tool merely reported.
    if hit.is_none()
        && lower
            .trim_start()
            .starts_with("traceback (most recent call last)")
    {
        hit = Some(0);
    }

    // An HTTP error status counts only at the start of a line, so that a URL or
    // a sentence mentioning "HTTP 404" does not trip it.
    if hit.is_none() {
        hit = http_error_line(text);
    }

    // `lower` may differ in length from `text` for non-ASCII input, so an index
    // taken from it is not necessarily valid in `text`. Fall back to offset 0,
    // which still yields a usable quote.
    hit.map(|i| if text.is_char_boundary(i) { i } else { 0 })
}

/// Byte offset of a line beginning with an HTTP 4xx/5xx status.
fn http_error_line(text: &str) -> Option<usize> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("HTTP ") {
            let mut digits = rest.chars();
            let first = digits.next();
            if matches!(first, Some('4') | Some('5'))
                && digits.clone().take(2).all(|c| c.is_ascii_digit())
                && digits.count() >= 2
            {
                return Some(offset);
            }
        }
        offset += line.len();
    }
    None
}

/// Spellings of a reported process exit code, compared case-insensitively.
///
/// More than one is needed because the wording is the tool's choice, not a
/// protocol field: a live capture emits `(Command exited with code 1)`, which
/// the single `Exit code ` marker missed, dropping a structured exit code down
/// to the weak text heuristic and grading a certain failure as a guess.
const EXIT_CODE_MARKERS: &[&str] = &["exit code ", "exited with code ", "exit status "];

/// Last reported exit code in the payload plus its offset.
///
/// The last occurrence wins because chained commands emit one per sub-command
/// and only the final one describes the call as a whole.
fn last_exit_code(text: &str) -> Option<(i64, usize)> {
    let lower = text.to_lowercase();
    let mut found: Option<(i64, usize)> = None;
    for marker in EXIT_CODE_MARKERS {
        let mut cursor = 0;
        while let Some(rel) = lower[cursor..].find(marker) {
            let marker_at = cursor + rel;
            let digits: String = lower[marker_at + marker.len()..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(code) = digits.parse::<i64>() {
                // Keep the latest position across all spellings, not just
                // within one, so a mixed payload still reports its final code.
                if found.is_none_or(|(_, at)| marker_at >= at) {
                    found = Some((code, marker_at));
                }
            }
            cursor = marker_at + marker.len();
        }
    }
    found
}

/// Whether the call is an existence/availability probe.
///
/// Requires *every* chained segment to be a probe: one real command mixed in
/// means a non-zero exit could have come from the real work.
fn is_probe(call: &ToolCall) -> bool {
    let Some(command) = command_of(call) else {
        return false;
    };
    // `&&`, `||`, `;` and `|` are all built from these three characters, so
    // splitting per character and dropping empty pieces covers every separator
    // without needing a multi-string pattern.
    let segments: Vec<&str> = command
        .split([';', '|', '&'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    !segments.is_empty()
        && segments.iter().all(|segment| {
            segment
                .split_whitespace()
                .next()
                .map(|head| {
                    let bare = head.rsplit('/').next().unwrap_or(head);
                    PROBE_COMMANDS.contains(&bare)
                })
                .unwrap_or(false)
        })
}

/// Shell command carried by a call, when it has one.
fn command_of(call: &ToolCall) -> Option<&str> {
    call.arguments
        .get("command")
        .or_else(|| call.arguments.get("cmd"))
        .and_then(|v| v.as_str())
}

fn is_placeholder_or_empty(text: &str) -> bool {
    text.trim().is_empty() || PLACEHOLDER_MARKERS.iter().any(|m| text.contains(m))
}

/// Byte offset of a line inside the opening window that begins with an error
/// token. Returns `None` when the only error words sit further in, which is the
/// case the spec's B2 guard is built around.
fn weak_error_prefix(text: &str) -> Option<usize> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if offset >= WEAK_SIGNAL_WINDOW {
            return None;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("Error:") || trimmed.starts_with("Exception:") {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

/// A downstream system's exception quoted inside the payload (spec B4).
///
/// Only reported when it is not at the very start, since a leading exception is
/// this call's own failure and is handled by [`find_signature`].
fn nested_error(text: &str) -> Option<String> {
    const NESTED_MARKERS: &[&str] = &["java.lang.", "NullPointerException", "SQLException"];
    let idx = NESTED_MARKERS
        .iter()
        .filter_map(|m| text.find(m))
        .min()
        .filter(|i| *i > 0)?;
    quote_from(text, idx)
}

#[cfg(test)]
#[path = "outcome_tests.rs"]
mod tests;
