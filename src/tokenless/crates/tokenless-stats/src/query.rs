//! Query and formatting utilities for tokenless stats.

use std::collections::{BTreeMap, HashMap};

use crate::record::{CompressionMode, StatsRecord};
use crate::recorder::{RetrieveTotals, StatsSummary, SummaryWindow};

/// Partition a record slice into the active view summaries report over and
/// the count of dry-run rows they exclude (roadmap §4.6: predicted savings
/// stay available for explicit comparison, never mixed into applied totals).
fn active_records(records: &[StatsRecord]) -> (Vec<&StatsRecord>, usize) {
    let active: Vec<&StatsRecord> = records
        .iter()
        .filter(|r| r.mode != CompressionMode::DryRun)
        .collect();
    let excluded = records.len() - active.len();
    (active, excluded)
}

/// Truncation attribution over the active view: (compressions with at least
/// one unmarked truncation, total truncation events). Legacy NULL rows
/// count zero.
fn truncation_totals(active: &[&StatsRecord]) -> (usize, i64) {
    let events: i64 = active
        .iter()
        .filter_map(|r| r.unrecoverable_truncations)
        .sum();
    let compressions = active
        .iter()
        .filter(|r| r.unrecoverable_truncations.is_some_and(|n| n > 0))
        .count();
    (compressions, events)
}

/// Format a number with thousands separators for readability.
fn format_num(n: usize) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + (s.len() - 1) / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(ch);
    }
    result
}

/// Format a summary report with overall stats and breakdown by operation type.
///
/// Totals cover active rows only; dry-run rows are excluded with a visible
/// count (use `stats summary --compare` for predicted-savings comparison).
/// When `session_total_tokens` is provided, adds an "Actual Savings Rate"
/// section showing the percentage of the entire session's token consumption
/// that tokenless saved — not just the tool-response portion. `retrieve`
/// must cover the same full history as `records`; pass `None` for a partial
/// window. Prefer [`format_summary_window`] for recorder-backed reports.
pub fn format_summary(
    records: &[StatsRecord],
    title: Option<&str>,
    session_total_tokens: Option<usize>,
    retrieve: Option<&RetrieveTotals>,
) -> String {
    let (active, excluded) = active_records(records);
    let total = StatsSummary::from_record_refs(active.iter().copied());

    let mut output = String::new();

    if let Some(t) = title {
        output.push_str(t);
        output.push('\n');
        output.push_str(&"=".repeat(60));
        output.push('\n');
    }

    output.push_str(&format!("Total Records: {}", total.total_records));
    if excluded > 0 {
        output.push_str(&format!(" ({excluded} dry-run records excluded)"));
    }
    output.push_str("\n\n");

    output.push_str("Character Savings:\n");
    output.push_str(&format!("  Before: {} chars\n", total.total_before_chars));
    output.push_str(&format!("  After:  {} chars\n", total.total_after_chars));
    output.push_str(&format!(
        "  Saved:  {} chars ({:.1}%)\n\n",
        total.chars_saved(),
        total.chars_percent()
    ));

    output.push_str("Token Savings:\n");
    output.push_str(&format!("  Before: {} tokens\n", total.total_before_tokens));
    output.push_str(&format!("  After:  {} tokens\n", total.total_after_tokens));
    output.push_str(&format!(
        "  Saved:  {} tokens ({:.1}%)\n\n",
        total.tokens_saved(),
        total.tokens_percent()
    ));

    // Breakdown by operation type
    let mut by_op: HashMap<&str, StatsSummary> = HashMap::new();
    for r in &active {
        let op = r.operation.as_str();
        let entry = by_op.entry(op).or_default();
        entry.total_records += 1;
        entry.total_before_chars += r.before_chars;
        entry.total_after_chars += r.after_chars;
        entry.total_before_tokens += r.before_tokens;
        entry.total_after_tokens += r.after_tokens;
    }

    output.push_str("Breakdown by Operation:\n");
    output.push_str(&"-".repeat(40));
    output.push('\n');

    let mut ops: Vec<_> = by_op.iter().collect();
    ops.sort_by_key(|b| std::cmp::Reverse(b.1.total_records));

    for (op, s) in ops {
        output.push_str(&format!("  {}: {} records\n", op, s.total_records));
        output.push_str(&format!(
            "    Chars: {} -> {} (-{:.1}%)\n",
            s.total_before_chars,
            s.total_after_chars,
            s.chars_percent()
        ));
        output.push_str(&format!(
            "    Tokens: {} -> {} (-{:.1}%)\n",
            s.total_before_tokens,
            s.total_after_tokens,
            s.tokens_percent()
        ));
    }

    // Attribution: net savings after retrieve read-back, plus truncation
    // exposure (roadmap §4.6 report).
    let (truncated_compressions, truncation_events) = truncation_totals(&active);
    output.push('\n');
    output.push_str("Attribution:\n");
    output.push_str(&format!(
        "  Gross Savings:  {} tokens\n",
        format_num(total.tokens_saved())
    ));
    if let Some(retrieve) = retrieve {
        let net = total.tokens_saved() as i64 - retrieve.retrieved_tokens as i64;
        output.push_str(&format!(
            "  Retrieved:      {} tokens\n",
            format_num(retrieve.retrieved_tokens as usize)
        ));
        output.push_str(&format!("  Net Savings:    {net} tokens\n"));
        output.push_str(&format!(
            "  Retrieves:      {} hits / {} misses / {} errors\n",
            retrieve.hits, retrieve.misses, retrieve.errors
        ));
    }
    output.push_str(&format!(
        "  Unrecoverable:  {truncated_compressions} compressions with unmarked truncations, {truncation_events} events\n"
    ));

    // Actual savings rate vs total session consumption
    if let Some(session_total) = session_total_tokens {
        let tool_share = if session_total > 0 {
            (total.total_before_tokens as f64 / session_total as f64) * 100.0
        } else {
            0.0
        };

        output.push('\n');
        output.push_str("Overall Savings vs Total Consumption:\n");
        output.push_str(&format!(
            "  Session Total:      {} tokens\n",
            format_num(session_total)
        ));
        output.push_str(&format!(
            "  Tool Response:      {} tokens ({:.1}% of session)\n",
            format_num(total.total_before_tokens),
            tool_share,
        ));
        output.push_str(&format!(
            "  Tokenless Saved:    {} tokens\n",
            format_num(total.tokens_saved())
        ));
        output.push('\n');
        output.push_str(&format!(
            "  Compression Rate:   {:.1}%  (within tool responses only)\n",
            total.tokens_percent()
        ));
        output.push_str(&format!(
            "  Actual Savings:     {:.1}%  ({:.1}% x {:.1}%) of total session\n",
            total.actual_savings_percent(session_total),
            total.tokens_percent(),
            tool_share,
        ));
    }

    output
}

/// Format summary as machine-readable JSON.
///
/// Output structure:
/// ```json
/// {
///   "total": { "records": N, "before_tokens": N, "after_tokens": N,
///              "chars_saved_percent": 83.0, "tokens_saved_percent": 83.0, ... },
///   "by_operation": { "compress-response": { "records": N, ... }, ... }
/// }
/// ```
/// When `session_total_tokens` is provided, extra fields
/// (`session_total_tokens`, `actual_savings_tokens`, `actual_savings_percent`)
/// are added to the "total" object.
///
/// Totals cover active rows only; the excluded dry-run count and the §4.6
/// attribution report land as top-level keys (never inside `total`, whose
/// key set intentionally matches every `by_operation` entry). Retrieve
/// aggregates must cover the same history as `records`. Prefer
/// [`format_summary_window_json`] for recorder-backed reports.
pub fn format_summary_json(
    records: &[StatsRecord],
    session_total_tokens: Option<usize>,
    retrieve: Option<&RetrieveTotals>,
) -> String {
    serde_json::to_string_pretty(&summary_json_value(records, session_total_tokens, retrieve))
        .unwrap_or_default()
}

fn summary_json_value(
    records: &[StatsRecord],
    session_total_tokens: Option<usize>,
    retrieve: Option<&RetrieveTotals>,
) -> serde_json::Value {
    let (active, excluded) = active_records(records);
    let total = StatsSummary::from_record_refs(active.iter().copied());

    let mut by_op: BTreeMap<&str, StatsSummary> = BTreeMap::new();
    for r in &active {
        let entry = by_op.entry(r.operation.as_str()).or_default();
        entry.total_records += 1;
        entry.total_before_chars += r.before_chars;
        entry.total_after_chars += r.after_chars;
        entry.total_before_tokens += r.before_tokens;
        entry.total_after_tokens += r.after_tokens;
    }

    let by_op_json: serde_json::Map<String, serde_json::Value> = by_op
        .iter()
        .map(|(op, s)| {
            (
                op.to_string(),
                serde_json::json!({
                    "records": s.total_records,
                    "before_chars": s.total_before_chars,
                    "after_chars": s.total_after_chars,
                    "chars_saved": s.chars_saved(),
                    "before_tokens": s.total_before_tokens,
                    "after_tokens": s.total_after_tokens,
                    "tokens_saved": s.tokens_saved(),
                    "chars_saved_percent": s.chars_percent(),
                    "tokens_saved_percent": s.tokens_percent(),
                }),
            )
        })
        .collect();

    let mut total_json = serde_json::to_value(&total).unwrap_or_default();
    if let Some(obj) = total_json.as_object_mut() {
        obj.insert(
            "chars_saved".to_string(),
            serde_json::json!(total.chars_saved()),
        );
        obj.insert(
            "tokens_saved".to_string(),
            serde_json::json!(total.tokens_saved()),
        );
        obj.insert(
            "chars_saved_percent".to_string(),
            serde_json::json!(total.chars_percent()),
        );
        obj.insert(
            "tokens_saved_percent".to_string(),
            serde_json::json!(total.tokens_percent()),
        );

        if let Some(session_total) = session_total_tokens {
            obj.insert(
                "session_total_tokens".to_string(),
                serde_json::json!(session_total),
            );
            obj.insert(
                "actual_savings_tokens".to_string(),
                serde_json::json!(total.tokens_saved()),
            );
            obj.insert(
                "actual_savings_percent".to_string(),
                serde_json::json!(total.actual_savings_percent(session_total)),
            );
        }
    }

    let (truncated_compressions, truncation_events) = truncation_totals(&active);
    let mut attribution = serde_json::json!({
        "gross_savings_tokens": total.tokens_saved(),
        "compressions_with_unrecoverable_truncations": truncated_compressions,
        "unrecoverable_truncation_events": truncation_events,
    });
    if let Some(retrieve) = retrieve
        && let Some(obj) = attribution.as_object_mut()
    {
        obj.insert(
            "retrieved_tokens".to_string(),
            serde_json::json!(retrieve.retrieved_tokens),
        );
        obj.insert(
            "net_savings_tokens".to_string(),
            serde_json::json!(total.tokens_saved() as i64 - retrieve.retrieved_tokens as i64),
        );
        obj.insert(
            "retrieve_hits".to_string(),
            serde_json::json!(retrieve.hits),
        );
        obj.insert(
            "retrieve_misses".to_string(),
            serde_json::json!(retrieve.misses),
        );
        obj.insert(
            "retrieve_errors".to_string(),
            serde_json::json!(retrieve.errors),
        );
    }

    serde_json::json!({
        "schema_version": "1.1",
        "dry_run_records_excluded": excluded,
        "attribution": attribution,
        "total": total_json,
        "by_operation": by_op_json,
    })
}

/// Format a snapshot summary, disclosing rows omitted from its reporting window.
/// Retrieval query errors omit optional cost fields; strict callers must check
/// `window.retrieve_totals` before formatting.
pub fn format_summary_window(window: &SummaryWindow, title: Option<&str>) -> String {
    let retrieve = window
        .retrieve_totals
        .as_ref()
        .ok()
        .and_then(Option::as_ref);
    let mut output = format_summary(&window.records, title, None, retrieve);
    if !window.is_complete() {
        output.push_str(&format!(
            "\nSummary window: {} of {} stored records (before dry-run exclusion).\n\
             Retrieval and net savings omitted: this window does not cover the full history.\n",
            window.records.len(),
            window.total_records,
        ));
    }
    output
}

/// Format a snapshot summary with an additive `summary_scope` object.
/// Retrieval query errors omit optional cost fields; strict callers must check
/// `window.retrieve_totals` before formatting.
pub fn format_summary_window_json(window: &SummaryWindow) -> String {
    let retrieve = window
        .retrieve_totals
        .as_ref()
        .ok()
        .and_then(Option::as_ref);
    let mut output = summary_json_value(&window.records, None, retrieve);
    output["summary_scope"] = serde_json::json!({
        "records_selected": window.records.len(),
        "records_total": window.total_records,
        "complete": window.is_complete(),
    });
    serde_json::to_string_pretty(&output).unwrap_or_default()
}

/// Format a list of records for display
pub fn format_list(records: &[StatsRecord], limit: usize) -> String {
    if records.is_empty() {
        return "No records found.".to_string();
    }

    let display = if records.len() > limit {
        &records[..limit]
    } else {
        records
    };

    let mut output = String::new();
    output.push_str(&format!("Showing {} record(s):\n", display.len()));
    output.push_str(&"=".repeat(80));
    output.push('\n');

    for record in display {
        output.push_str(&record.format_summary_line());
        output.push('\n');
    }

    if records.len() > limit {
        output.push_str(&format!(
            "\n... and {} more (use --limit to show all)",
            records.len() - limit
        ));
    }

    output
}

/// Format a single record showing before/after text content.
/// If before and after are identical, shows original text with "(no compression)" note.
/// An empty or absent after text stays visible behind an explicit placeholder:
/// it is a real compression result (e.g. content stripped to nothing) or a row
/// stored without content, and the record's metrics may still show savings.
pub fn format_show(record: &StatsRecord) -> String {
    let before = record.before_text.as_deref().unwrap_or("");
    let after = record.after_text.as_deref().unwrap_or("");

    if before.is_empty() && after.is_empty() {
        return "  (no text content stored)\n".to_string();
    }

    let mut output = String::new();

    if before == after {
        // No compression happened: identical text in and out.
        output.push_str("=== Original (no compression) ===\n");
        output.push_str(before);
        if !before.is_empty() && !before.ends_with('\n') {
            output.push('\n');
        }
    } else {
        output.push_str("=== Before ===\n");
        output.push_str(before);
        if !before.is_empty() && !before.ends_with('\n') {
            output.push('\n');
        }
        output.push_str("\n=== After ===\n");
        if after.is_empty() {
            // Say which of the two non-compressed states this is instead of
            // hiding it behind the "(no compression)" label.
            if record.after_text.is_some() {
                output.push_str("  (after content is empty)\n");
            } else {
                output.push_str("  (after content not stored)\n");
            }
        } else {
            output.push_str(after);
            if !after.ends_with('\n') {
                output.push('\n');
            }
        }
    }

    output
}

/// Sum tokens by operation type. `use_before` selects `before_tokens` (the
/// raw/baseline context) when true, else `after_tokens` (the compressed
/// context actually seen under tokenless).
fn tokens_by_op(records: &[StatsRecord], use_before: bool) -> BTreeMap<&'static str, usize> {
    let mut map: BTreeMap<&'static str, usize> = BTreeMap::new();
    for r in records {
        let t = if use_before {
            r.before_tokens
        } else {
            r.after_tokens
        };
        *map.entry(r.operation.as_str()).or_default() += t;
    }
    map
}

/// Format a side-by-side comparison between a baseline (compression-off /
/// dry-run) run and a tokenless (compression-on / active) run.
///
/// Baseline context uses each record's `before_tokens` (the raw text that
/// reached the LLM); the tokenless side uses `after_tokens` (the compressed
/// text). Savings = baseline − tokenless.
pub fn format_compare(baseline: &[StatsRecord], tokenless: &[StatsRecord]) -> String {
    let base_by_op = tokens_by_op(baseline, true);
    let tls_by_op = tokens_by_op(tokenless, false);

    let base_total: usize = base_by_op.values().sum();
    let tls_total: usize = tls_by_op.values().sum();

    let mut ops: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    ops.extend(base_by_op.keys().copied());
    ops.extend(tls_by_op.keys().copied());

    let saved_total = base_total.saturating_sub(tls_total);
    let saved_pct = if base_total > 0 {
        (saved_total as f64 / base_total as f64) * 100.0
    } else {
        0.0
    };

    let mut output = String::new();
    output.push_str("Tokenless Comparison Report\n");
    output.push_str(&"=".repeat(60));
    output.push('\n');
    output.push_str(&format!(
        "{:<22}{:>12}{:>14}{:>10}{:>12}\n",
        "operation", "baseline", "tokenless", "saved", "saved%"
    ));
    output.push_str(&"-".repeat(68));
    output.push('\n');

    for op in ops {
        let b = *base_by_op.get(op).unwrap_or(&0);
        let t = *tls_by_op.get(op).unwrap_or(&0);
        let saved = b.saturating_sub(t);
        let pct = if b > 0 {
            (saved as f64 / b as f64) * 100.0
        } else {
            0.0
        };
        output.push_str(&format!("{op:<22}{b:>12}{t:>14}{saved:>10}{pct:>11.1}%\n"));
    }

    output.push_str(&"-".repeat(68));
    output.push('\n');
    output.push_str(&format!(
        "{:<22}{:>12}{:>14}{:>10}{:>11.1}%\n",
        "TOTAL", base_total, tls_total, saved_total, saved_pct
    ));

    output
}

/// Format the comparison as machine-readable JSON.
pub fn format_compare_json(baseline: &[StatsRecord], tokenless: &[StatsRecord]) -> String {
    let base_by_op = tokens_by_op(baseline, true);
    let tls_by_op = tokens_by_op(tokenless, false);

    let base_total: usize = base_by_op.values().sum();
    let tls_total: usize = tls_by_op.values().sum();
    let saved_total = base_total.saturating_sub(tls_total);
    let saved_pct = if base_total > 0 {
        (saved_total as f64 / base_total as f64) * 100.0
    } else {
        0.0
    };

    let by_op_json = |map: &BTreeMap<&str, usize>| -> serde_json::Map<String, serde_json::Value> {
        map.iter()
            .map(|(op, &v)| (op.to_string(), serde_json::json!(v)))
            .collect()
    };

    let output = serde_json::json!({
        "schema_version": "1.0",
        "baseline_tokens": base_total,
        "tokenless_tokens": tls_total,
        "saved_tokens": saved_total,
        "saved_percent": saved_pct,
        "baseline_by_operation": by_op_json(&base_by_op),
        "tokenless_by_operation": by_op_json(&tls_by_op),
    });

    serde_json::to_string_pretty(&output).unwrap_or_default()
}

#[cfg(test)]
mod tests;
