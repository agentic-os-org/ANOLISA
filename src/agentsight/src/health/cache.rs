//! Prompt-cache hit-rate diagnosis over recorded LLM calls.
//!
//! Hit rates are token-weighted, never averages of per-call ratios. A missing
//! cache counter is reported as missing, not as a miss: the conservative rate
//! counts it as zero, the complete rate leaves it out. Low-hit causes come
//! from comparing each call with the previous call of the same session, so
//! they explain where uncached input came from rather than proving a defect.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::storage::sqlite::GenAISqliteStore;
use crate::storage::sqlite::genai::{CacheCallRow, CacheCallWindow, percentiles};

mod report;
#[cfg(test)]
mod tests;

pub use report::*;

/// Usage records a group needs before a low rate is worth reviewing.
pub const MIN_USAGE_CALLS: usize = 20;
/// Input tokens a group needs before a low rate is worth reviewing.
pub const MIN_INPUT_TOKENS: i64 = 200_000;
/// Drop against the baseline, in percentage points, that marks a group for review.
pub const REVIEW_DROP_PP: f64 = 15.0;
/// Share of a group's input missed by suspected causes that marks it for
/// review even above the hit-rate threshold.
pub const SUSPECTED_MISS_SHARE: f64 = 0.2;
/// Smaller hit-rate changes against the baseline count as unchanged.
const UNCHANGED_PP: f64 = 5.0;
/// Share of the previous input a call must read to count as having reused it.
const REUSED_SHARE: f64 = 0.9;
/// Missed reusable input below this many tokens is too small to explain.
const MIN_MISSED_TOKENS: i64 = 1024;
const MAX_EXAMPLES: usize = 3;

#[derive(Default)]
struct Classified {
    reason: Option<CacheReason>,
    missed: i64,
    previous_input: Option<i64>,
}

type GroupKey = (Option<String>, Option<String>, Option<String>);

fn lower(value: &Option<String>) -> Option<String> {
    value.as_deref().map(str::to_lowercase)
}

fn group_key(row: &CacheCallRow) -> GroupKey {
    (
        lower(&row.agent_name),
        lower(&row.provider),
        lower(&row.model),
    )
}

fn hit_ratio(row: &CacheCallRow) -> Option<f64> {
    let input = row.input_tokens.filter(|input| *input > 0)?;
    Some(row.cache_read_tokens? as f64 / input as f64)
}

/// Drops repeated captures of one call, keeping the first.
fn dedup(rows: Vec<CacheCallRow>) -> (Vec<CacheCallRow>, HashMap<GroupKey, usize>) {
    let mut seen_calls = HashSet::new();
    let mut seen_traces = HashSet::new();
    let mut dropped: HashMap<GroupKey, usize> = HashMap::new();
    let mut kept = Vec::with_capacity(rows.len());
    for row in rows {
        let call = row.call_id.clone().filter(|id| !id.is_empty());
        let trace = row
            .trace_id
            .clone()
            .filter(|id| !id.is_empty())
            .map(|id| (id, row.start_ns));
        let repeated = call.is_some_and(|id| !seen_calls.insert(id))
            || trace.is_some_and(|key| !seen_traces.insert(key));
        if repeated {
            *dropped.entry(group_key(&row)).or_default() += 1;
        } else {
            kept.push(row);
        }
    }
    (kept, dropped)
}

/// Attaches a cause to every call by comparing it with the previous call of its session.
fn classify(rows: &[CacheCallRow], options: &CacheDiagnosisOptions) -> Vec<Classified> {
    let mut result: Vec<Classified> = rows.iter().map(|_| Classified::default()).collect();
    let mut sessions: BTreeMap<(Option<String>, String), Vec<usize>> = BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        let session = row
            .session_id
            .clone()
            .or_else(|| row.conversation_id.clone());
        match session.filter(|id| !id.is_empty()) {
            Some(id) => sessions
                .entry((lower(&row.agent_name), id))
                .or_default()
                .push(index),
            None => {
                result[index].reason = first_call_reason(row, options).map(|reason| match reason {
                    CacheReason::ColdStart => CacheReason::NoSessionContext,
                    other => other,
                })
            }
        }
    }
    let ttl_ns = i64::try_from(options.cache_ttl_secs)
        .unwrap_or(i64::MAX)
        .saturating_mul(1_000_000_000);
    for calls in sessions.values() {
        let mut previous: Option<&CacheCallRow> = None;
        for &index in calls {
            let row = &rows[index];
            if row.input_tokens.is_none() {
                continue;
            }
            result[index] = match previous {
                None => Classified {
                    reason: first_call_reason(row, options),
                    ..Classified::default()
                },
                Some(prev) => follow_up(prev, row, ttl_ns, options),
            };
            previous = Some(row);
        }
        mark_plateaus(rows, calls, &mut result);
    }
    result
}

fn first_call_reason(row: &CacheCallRow, options: &CacheDiagnosisOptions) -> Option<CacheReason> {
    row.input_tokens?;
    if row.cache_read_tokens.is_none() {
        return Some(CacheReason::CacheFieldsMissing);
    }
    if row.failed {
        return Some(CacheReason::FailedCalls);
    }
    (hit_ratio(row)? < options.low_hit_rate).then_some(CacheReason::ColdStart)
}

fn follow_up(
    prev: &CacheCallRow,
    row: &CacheCallRow,
    ttl_ns: i64,
    options: &CacheDiagnosisOptions,
) -> Classified {
    let input = row.input_tokens.unwrap_or(0);
    let previous_input = prev.input_tokens;
    let Some(read) = row.cache_read_tokens else {
        return Classified {
            reason: Some(CacheReason::CacheFieldsMissing),
            previous_input,
            ..Classified::default()
        };
    };
    if row.failed {
        return Classified {
            reason: Some(CacheReason::FailedCalls),
            previous_input,
            ..Classified::default()
        };
    }
    let reusable = previous_input.unwrap_or(0).min(input);
    let missed = reusable.saturating_sub(read).max(0);
    let low = (read as f64) < options.low_hit_rate * input as f64;
    if (read as f64) >= REUSED_SHARE * reusable as f64 || missed < MIN_MISSED_TOKENS {
        return Classified {
            reason: low.then_some(CacheReason::NewInputDominant),
            previous_input,
            ..Classified::default()
        };
    }
    let gap = row
        .start_ns
        .saturating_sub(prev.end_ns.unwrap_or(prev.start_ns));
    let changed = |a: Option<u64>, b: Option<u64>| a.is_some() && b.is_some() && a != b;
    let reason = if lower(&prev.model) != lower(&row.model) {
        CacheReason::ModelSwitched
    } else if gap > ttl_ns {
        CacheReason::CacheExpired
    } else if changed(prev.system_fingerprint, row.system_fingerprint) {
        CacheReason::SystemPromptChanged
    } else if changed(prev.tools_fingerprint, row.tools_fingerprint) {
        CacheReason::ToolsChanged
    } else if row.message_count < prev.message_count && row.message_count.is_some() {
        CacheReason::HistoryRewritten
    } else {
        CacheReason::PrefixNotReused
    };
    Classified {
        reason: Some(reason),
        missed,
        previous_input,
    }
}

/// Narrows unexplained misses inside runs of three or more identical non-zero
/// reads: the cache serves one fixed prefix while the history grows past it.
fn mark_plateaus(rows: &[CacheCallRow], calls: &[usize], result: &mut [Classified]) {
    let reads: Vec<(usize, i64)> = calls
        .iter()
        .filter_map(|&index| Some((index, rows[index].cache_read_tokens.filter(|r| *r > 0)?)))
        .collect();
    for run in reads
        .chunk_by(|a, b| a.1 == b.1)
        .filter(|run| run.len() >= 3)
    {
        for &(index, _) in run {
            if result[index].reason == Some(CacheReason::PrefixNotReused) {
                result[index].reason = Some(CacheReason::FixedPrefixPlateau);
            }
        }
    }
}

fn median(mut values: Vec<i64>) -> Option<i64> {
    values.sort_unstable();
    values.get(values.len() / 2).copied()
}

fn latency_bucket(rows: &[&CacheCallRow]) -> CacheLatencyBucket {
    CacheLatencyBucket {
        calls: rows.len(),
        median_input_tokens: median(rows.iter().filter_map(|row| row.input_tokens).collect()),
        ttft_ms: percentiles(rows.iter().filter_map(|row| row.ttft_ms).collect()),
        e2e_latency_ms: percentiles(rows.iter().filter_map(|row| row.e2e_latency_ms).collect()),
    }
}

fn cost(rows: &[(&CacheCallRow, &Classified)], prices: CachePrices) -> CacheCost {
    let mut cost = CacheCost::default();
    let mut input_tokens = 0_i64;
    for (row, classified) in rows {
        let Some(input) = row.input_tokens else {
            continue;
        };
        let read = row.cache_read_tokens.unwrap_or(0);
        let write = row.cache_creation_tokens.unwrap_or(0);
        let uncached = input - read - write;
        if uncached < 0 {
            cost.incompatible_calls += 1;
            continue;
        }
        input_tokens += input;
        cost.input_cost += (uncached as f64 * prices.input
            + read as f64 * prices.cache_read
            + write as f64 * prices.cache_write)
            / 1e6;
        cost.no_cache_input_cost += input as f64 * prices.input / 1e6;
        cost.output_cost += row.output_tokens.unwrap_or(0) as f64 * prices.output / 1e6;
        if classified
            .reason
            .is_some_and(|r| r.kind() == FindingKind::Suspected)
        {
            cost.missed_savings +=
                classified.missed as f64 * (prices.input - prices.cache_read) / 1e6;
        }
    }
    cost.cache_savings = cost.no_cache_input_cost - cost.input_cost;
    cost.input_cost_per_mtok =
        (input_tokens > 0).then(|| cost.input_cost * 1e6 / input_tokens as f64);
    cost
}

fn findings(rows: &[(&CacheCallRow, &Classified)]) -> Vec<CacheFinding> {
    let mut by_reason: BTreeMap<CacheReason, Vec<(&CacheCallRow, &Classified)>> = BTreeMap::new();
    for &(row, classified) in rows {
        if let Some(reason) = classified.reason {
            by_reason.entry(reason).or_default().push((row, classified));
        }
    }
    let mut findings: Vec<CacheFinding> = by_reason
        .into_iter()
        .map(|(reason, mut calls)| {
            let unread = |row: &CacheCallRow| {
                row.input_tokens.unwrap_or(0) - row.cache_read_tokens.unwrap_or(0)
            };
            calls.sort_by_key(|(row, classified)| {
                std::cmp::Reverse((classified.missed, unread(row)))
            });
            CacheFinding {
                reason,
                kind: reason.kind(),
                calls: calls.len(),
                unread_tokens: calls.iter().map(|(row, _)| unread(row).max(0)).sum(),
                missed_tokens: calls.iter().map(|(_, classified)| classified.missed).sum(),
                ttft_ms_p50: percentiles(calls.iter().filter_map(|(row, _)| row.ttft_ms).collect())
                    .map(|summary| summary.p50),
                e2e_latency_ms_p50: percentiles(
                    calls
                        .iter()
                        .filter_map(|(row, _)| row.e2e_latency_ms)
                        .collect(),
                )
                .map(|summary| summary.p50),
                recommendation: reason.recommendation(),
                examples: calls
                    .iter()
                    .take(MAX_EXAMPLES)
                    .map(|(row, classified)| CacheExample {
                        trace_id: row.trace_id.clone(),
                        call_id: row.call_id.clone(),
                        session_id: row.session_id.clone(),
                        start_ns: row.start_ns,
                        input_tokens: row.input_tokens.unwrap_or(0),
                        cache_read_tokens: row.cache_read_tokens,
                        previous_input_tokens: classified.previous_input,
                    })
                    .collect(),
            }
        })
        .collect();
    findings.sort_by_key(|finding| {
        (
            finding.kind,
            std::cmp::Reverse((finding.missed_tokens, finding.unread_tokens)),
        )
    });
    findings
}

fn summarize(
    rows: &[(&CacheCallRow, &Classified)],
    duplicates: usize,
    options: &CacheDiagnosisOptions,
) -> CacheGroupDiagnosis {
    let (first, _) = rows[0];
    let mut coverage = CacheCoverage {
        calls: rows.len(),
        duplicate_calls: duplicates,
        ..CacheCoverage::default()
    };
    let mut tokens = CacheTokens::default();
    let mut complete = (0_i64, 0_i64);
    for (row, _) in rows {
        coverage.failed_calls += usize::from(row.failed);
        coverage.cache_creation_field_calls += usize::from(row.cache_creation_tokens.is_some());
        let Some(input) = row.input_tokens else {
            continue;
        };
        coverage.usage_calls += 1;
        tokens.input += input;
        tokens.output += row.output_tokens.unwrap_or(0);
        tokens.cache_creation += row.cache_creation_tokens.unwrap_or(0);
        if let Some(read) = row.cache_read_tokens {
            coverage.cache_read_field_calls += 1;
            coverage.explicit_zero_read_calls += usize::from(read == 0);
            tokens.cache_read += read;
            complete = (complete.0 + read, complete.1 + input);
        }
    }
    tokens.uncached_input = (tokens.input - tokens.cache_read - tokens.cache_creation).max(0);
    let rate = |read: i64, input: i64| (input > 0).then(|| read as f64 / input as f64);
    let conservative_hit_rate = if coverage.cache_read_field_calls > 0 {
        rate(tokens.cache_read, tokens.input)
    } else {
        None
    };
    let findings = findings(rows);
    let suspected_missed: i64 = findings
        .iter()
        .filter(|finding| finding.kind == FindingKind::Suspected)
        .map(|finding| finding.missed_tokens)
        .sum();
    let status = if coverage.usage_calls == 0 {
        CacheStatus::NoUsage
    } else if coverage.cache_read_field_calls == 0 {
        CacheStatus::Undeterminable
    } else if coverage.usage_calls < MIN_USAGE_CALLS || tokens.input < MIN_INPUT_TOKENS {
        CacheStatus::InsufficientSamples
    } else if conservative_hit_rate.is_some_and(|rate| rate < options.low_hit_rate)
        || suspected_missed as f64 >= SUSPECTED_MISS_SHARE * tokens.input as f64
    {
        CacheStatus::NeedsReview
    } else {
        CacheStatus::Healthy
    };
    let ratios: Vec<(&CacheCallRow, f64)> = rows
        .iter()
        .filter_map(|(row, _)| Some((*row, hit_ratio(row)?)))
        .collect();
    let bucket = |warm: bool| {
        let calls: Vec<&CacheCallRow> = ratios
            .iter()
            .filter(|(_, ratio)| (*ratio >= options.low_hit_rate) == warm)
            .map(|(row, _)| *row)
            .collect();
        latency_bucket(&calls)
    };
    CacheGroupDiagnosis {
        agent_name: first.agent_name.clone(),
        provider: first.provider.clone(),
        model: first.model.clone(),
        status,
        coverage,
        tokens,
        conservative_hit_rate,
        complete_hit_rate: rate(complete.0, complete.1),
        latency: CacheLatency {
            warm: bucket(true),
            cold: bucket(false),
        },
        cost: options.prices.map(|prices| cost(rows, prices)),
        findings,
        baseline: None,
    }
}

/// Diagnoses one window, grouping calls by agent, provider and model case-insensitively.
pub fn diagnose(
    window: CacheCallWindow,
    start_ns: i64,
    end_ns: i64,
    options: &CacheDiagnosisOptions,
) -> CacheDiagnosis {
    let (rows, duplicates) = dedup(window.rows);
    let classified = classify(&rows, options);
    let mut groups: BTreeMap<GroupKey, Vec<(&CacheCallRow, &Classified)>> = BTreeMap::new();
    for (row, classified) in rows.iter().zip(&classified) {
        groups
            .entry(group_key(row))
            .or_default()
            .push((row, classified));
    }
    CacheDiagnosis {
        start_ns,
        end_ns,
        truncated: window.truncated,
        groups: groups
            .into_iter()
            .map(|(key, rows)| {
                let duplicates = duplicates.get(&key).copied().unwrap_or(0);
                summarize(&rows, duplicates, options)
            })
            .collect(),
    }
}

/// Filters and windows of one diagnosis.
#[derive(Debug, Clone, Default)]
pub struct CacheDiagnosisRequest {
    pub start_ns: i64,
    pub end_ns: i64,
    pub agent_name: Option<String>,
    pub session_id: Option<String>,
    /// Earlier `[start_ns, end_ns]` window to compare with, such as the days before a fix.
    pub baseline: Option<(i64, i64)>,
}

/// Reads the requested windows from `store` and diagnoses them.
///
/// # Errors
///
/// Returns an error when a window cannot be read from the store.
pub fn diagnose_store(
    store: &GenAISqliteStore,
    request: &CacheDiagnosisRequest,
    options: &CacheDiagnosisOptions,
) -> Result<CacheDiagnosis, Box<dyn std::error::Error>> {
    let read = |start_ns: i64, end_ns: i64| -> Result<_, Box<dyn std::error::Error>> {
        let window = store.get_cache_call_window(
            start_ns,
            end_ns,
            request.agent_name.as_deref(),
            request.session_id.as_deref(),
        )?;
        Ok(diagnose(window, start_ns, end_ns, options))
    };
    let mut current = read(request.start_ns, request.end_ns)?;
    if let Some((start_ns, end_ns)) = request.baseline {
        compare(&mut current, &read(start_ns, end_ns)?);
    }
    Ok(current)
}

fn sufficient(status: CacheStatus) -> bool {
    matches!(status, CacheStatus::Healthy | CacheStatus::NeedsReview)
}

/// Attaches the matching baseline group to every current group.
///
/// A sufficiently sampled group whose rate dropped by [`REVIEW_DROP_PP`] or
/// more is marked for review even when it stays above the absolute threshold.
pub fn compare(current: &mut CacheDiagnosis, baseline: &CacheDiagnosis) {
    let key = |group: &CacheGroupDiagnosis| {
        (
            lower(&group.agent_name),
            lower(&group.provider),
            lower(&group.model),
        )
    };
    let baseline: HashMap<GroupKey, &CacheGroupDiagnosis> = baseline
        .groups
        .iter()
        .map(|group| (key(group), group))
        .collect();
    for group in &mut current.groups {
        let Some(before) = baseline.get(&key(group)) else {
            continue;
        };
        let change = group
            .conservative_hit_rate
            .zip(before.conservative_hit_rate)
            .map(|(now, then)| (now - then) * 100.0);
        let trend = match change {
            None => CacheTrend::Incomparable,
            Some(pp) if pp >= UNCHANGED_PP => CacheTrend::Improved,
            Some(pp) if pp <= -UNCHANGED_PP => CacheTrend::Regressed,
            Some(_) => CacheTrend::Unchanged,
        };
        let sufficient_samples = sufficient(group.status) && sufficient(before.status);
        if sufficient_samples && change.is_some_and(|pp| pp <= -REVIEW_DROP_PP) {
            group.status = CacheStatus::NeedsReview;
        }
        group.baseline = Some(CacheComparison {
            status: before.status,
            conservative_hit_rate: before.conservative_hit_rate,
            hit_rate_change_pp: change,
            input_cost_per_mtok: before
                .cost
                .as_ref()
                .and_then(|cost| cost.input_cost_per_mtok),
            trend,
            sufficient_samples,
        });
    }
}
