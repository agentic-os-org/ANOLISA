//! Detailed before/after reports for recorded token-saving operations.
//!
//! Reports keep per-stage metrics while linking only content-identical active
//! stages, so a multi-stage pipeline is measured from its first input to its
//! final output without counting intermediate inputs more than once.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;

use chrono::{DateTime, Local};
use serde::Serialize;
use similar::{ChangeTag, TextDiff};

use crate::record::{CompressionMode, OperationType, StatsRecord};

const SCHEMA_VERSION: &str = "1.0";
const MAX_DIFF_INPUT_BYTES: usize = 1024 * 1024;
const MAX_DIFF_LINES: usize = 500;

/// Ordering applied to chains in a session overview.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DiffSort {
    /// Largest estimated token saving first.
    #[default]
    Saved,
    /// Most recently started chain first.
    Time,
}

/// Records and verified adjacency metadata used to build diff chains.
///
/// Recorder queries can provide database-side link decisions without loading
/// stored payloads. Manually constructed sets fall back to comparing content
/// in memory.
#[derive(Debug)]
pub struct DiffRecords {
    records: Vec<StatsRecord>,
    linked_to_previous: Option<HashSet<i64>>,
}

impl DiffRecords {
    /// Wraps records whose links should be inferred from their loaded content.
    pub fn from_records(records: Vec<StatsRecord>) -> Self {
        Self {
            records,
            linked_to_previous: None,
        }
    }

    /// Returns the records in this diff input.
    pub fn as_slice(&self) -> &[StatsRecord] {
        &self.records
    }

    /// Returns whether the diff input contains no records.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub(crate) fn from_prelinked(
        records: Vec<StatsRecord>,
        linked_to_previous: HashSet<i64>,
    ) -> Self {
        Self {
            records,
            linked_to_previous: Some(linked_to_previous),
        }
    }

    fn links(&self, previous: &StatsRecord, next: &StatsRecord) -> bool {
        self.linked_to_previous.as_ref().map_or_else(
            || records_link(previous, next),
            |ids| ids.contains(&next.id),
        )
    }
}

/// Serializable report for a record, session, or tool-use diff.
#[derive(Debug, Serialize)]
pub struct DiffReport {
    schema_version: String,
    scope: DiffScope,
    saving_records_only: bool,
    split_chains: bool,
    chains: Vec<DiffChain>,
}

#[derive(Debug, Serialize)]
struct DiffScope {
    kind: DiffScopeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    record_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_use_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum DiffScopeKind {
    Record,
    Session,
    ToolUse,
}

#[derive(Debug, Serialize)]
struct DiffChain {
    status: ChainStatus,
    mode: String,
    agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_use_id: Option<String>,
    started_at: String,
    #[serde(skip)]
    sort_timestamp: DateTime<Local>,
    before_bytes: usize,
    after_bytes: usize,
    before_tokens: usize,
    after_tokens: usize,
    emitted_tokens: usize,
    saved_tokens: i64,
    saved_percent: f64,
    stages: Vec<DiffStage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<ContentDiff>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ChainStatus {
    Standalone,
    Linked,
}

impl ChainStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Standalone => "standalone",
            Self::Linked => "linked",
        }
    }
}

#[derive(Debug, Serialize)]
struct DiffStage {
    record_id: i64,
    timestamp: String,
    operation: String,
    agent_id: String,
    mode: String,
    before_bytes: usize,
    after_bytes: usize,
    before_tokens: usize,
    after_tokens: usize,
    emitted_tokens: usize,
    saved_tokens: i64,
    saved_percent: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    stash: Option<StashMetrics>,
}

#[derive(Debug, Serialize)]
struct StashMetrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    writes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    errors: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<i64>,
}

#[derive(Debug, Serialize)]
struct ContentDiff {
    available: bool,
    normalization: DiffNormalization,
    truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    omitted_reason: Option<DiffOmittedReason>,
    hunks: Vec<DiffHunk>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum DiffNormalization {
    None,
    Json,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum DiffOmittedReason {
    MissingContent,
    ContentTooLarge,
}

#[derive(Debug, Serialize)]
struct DiffHunk {
    old_start: usize,
    old_len: usize,
    new_start: usize,
    new_len: usize,
    lines: Vec<DiffLine>,
}

#[derive(Debug, Serialize)]
struct DiffLine {
    kind: DiffLineKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    old_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_line: Option<usize>,
    text: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum DiffLineKind {
    Context,
    Delete,
    Insert,
}

/// Builds a detailed report for one stored record.
pub fn record_report(record: &StatsRecord, context: usize) -> DiffReport {
    DiffReport {
        schema_version: SCHEMA_VERSION.to_string(),
        scope: DiffScope {
            kind: DiffScopeKind::Record,
            record_id: Some(record.id),
            session_id: record.session_id.clone(),
            tool_use_id: record.tool_use_id.clone(),
        },
        saving_records_only: true,
        split_chains: false,
        chains: vec![build_chain(&[record], true, context)],
    }
}

/// Builds a metrics-only session overview and applies the requested chain limit.
pub fn session_report(
    records: &DiffRecords,
    session_id: &str,
    limit: usize,
    sort: DiffSort,
) -> DiffReport {
    let (mut chains, split_chains) = build_chains(records, false, 0);
    sort_chains(&mut chains, sort);
    chains.truncate(limit);

    DiffReport {
        schema_version: SCHEMA_VERSION.to_string(),
        scope: DiffScope {
            kind: DiffScopeKind::Session,
            record_id: None,
            session_id: Some(session_id.to_string()),
            tool_use_id: None,
        },
        saving_records_only: true,
        split_chains,
        chains,
    }
}

/// Builds detailed reports for every independently linked chain of one tool use.
pub fn tool_use_report(
    records: &DiffRecords,
    session_id: &str,
    tool_use_id: &str,
    context: usize,
) -> DiffReport {
    let (chains, split_chains) = build_chains(records, true, context);
    DiffReport {
        schema_version: SCHEMA_VERSION.to_string(),
        scope: DiffScope {
            kind: DiffScopeKind::ToolUse,
            record_id: None,
            session_id: Some(session_id.to_string()),
            tool_use_id: Some(tool_use_id.to_string()),
        },
        saving_records_only: true,
        split_chains,
        chains,
    }
}

/// Formats a diff report for interactive terminal display.
pub fn format_diff_report(report: &DiffReport, color: bool) -> String {
    let mut output = String::new();
    output.push_str("Tokenless Diff\n");
    output.push_str(&"=".repeat(72));
    output.push('\n');
    match report.scope.kind {
        DiffScopeKind::Record => {
            let _ = writeln!(
                output,
                "Record:  {}",
                report.scope.record_id.unwrap_or_default()
            );
        }
        DiffScopeKind::Session => {
            let _ = writeln!(
                output,
                "Session: {}",
                escape_terminal_controls(report.scope.session_id.as_deref().unwrap_or("-"))
            );
        }
        DiffScopeKind::ToolUse => {
            let _ = writeln!(
                output,
                "Session: {}\nTool:    {}",
                escape_terminal_controls(report.scope.session_id.as_deref().unwrap_or("-")),
                escape_terminal_controls(report.scope.tool_use_id.as_deref().unwrap_or("-"))
            );
        }
    }
    output.push_str("Token counts are estimated; only operations with savings are recorded.\n");

    if matches!(report.scope.kind, DiffScopeKind::Session) {
        output.push('\n');
        output.push_str(&format!(
            "{:<28} {:<8} {:>6} {:>10} {:>10} {:>10} {:>10} {:>9}\n",
            "tool / record", "mode", "stages", "before", "after", "emitted", "saved", "saved%"
        ));
        output.push_str(&"-".repeat(97));
        output.push('\n');
        for chain in &report.chains {
            let label = chain
                .tool_use_id
                .as_deref()
                .map(escape_terminal_controls)
                .map(|tool| truncate_label(&tool, 28))
                .unwrap_or_else(|| format!("record:{}", chain.stages[0].record_id));
            let _ = writeln!(
                output,
                "{:<28} {:<8} {:>6} {:>10} {:>10} {:>10} {:>10} {:>8.1}%",
                label,
                chain.mode,
                chain.stages.len(),
                format_num(chain.before_tokens),
                format_num(chain.after_tokens),
                format_num(chain.emitted_tokens),
                format_signed(chain.saved_tokens),
                chain.saved_percent
            );
        }
        if report
            .chains
            .iter()
            .any(|chain| chain.mode == CompressionMode::DryRun.as_str())
        {
            output.push_str(
                "\nFor dry-run rows, after and saved are predictions; emitted remains before.\n",
            );
        }
        if report.split_chains {
            output.push_str(
                "\nSome tool uses contain disconnected records and are shown as separate chains.\n",
            );
        }
        return output;
    }

    for (index, chain) in report.chains.iter().enumerate() {
        if report.chains.len() > 1 {
            let _ = writeln!(output, "\nChain {} of {}", index + 1, report.chains.len());
        }
        format_chain(&mut output, chain, color);
    }
    output
}

fn format_chain(output: &mut String, chain: &DiffChain, color: bool) {
    let dry_run = chain.mode == CompressionMode::DryRun.as_str();
    let label = if dry_run {
        "Estimated predicted tokens"
    } else {
        "Estimated tokens"
    };
    let saved_label = if dry_run { "Predicted saved" } else { "Saved" };
    let _ = writeln!(
        output,
        "\nAgent: {}\nSession: {}\nTool: {}\nMode: {}\nStatus: {}\n{}: {} -> {}\nEmitted tokens: {}\n{}: {} ({:.1}%)\nBytes: {} -> {}",
        escape_terminal_controls(&chain.agent_id),
        escape_terminal_controls(chain.session_id.as_deref().unwrap_or("-")),
        escape_terminal_controls(chain.tool_use_id.as_deref().unwrap_or("-")),
        chain.mode,
        chain.status.as_str(),
        label,
        format_num(chain.before_tokens),
        format_num(chain.after_tokens),
        format_num(chain.emitted_tokens),
        saved_label,
        format_signed(chain.saved_tokens),
        chain.saved_percent,
        format_num(chain.before_bytes),
        format_num(chain.after_bytes)
    );

    output.push_str("\nStages:\n");
    output.push_str(&format!(
        "{:<8} {:<22} {:>10} {:>10} {:>10} {:<18}\n",
        "record", "operation", "before", "after", "saved", "stash (w/e/size)"
    ));
    for stage in &chain.stages {
        let _ = writeln!(
            output,
            "{:<8} {:<22} {:>10} {:>10} {:>10} {:<18}",
            stage.record_id,
            stage.operation,
            format_num(stage.before_tokens),
            format_num(stage.after_tokens),
            format_signed(stage.saved_tokens),
            format_stash(stage.stash.as_ref())
        );
    }

    let Some(diff) = &chain.diff else {
        return;
    };
    output.push('\n');
    if !diff.available {
        let reason = match diff.omitted_reason {
            Some(DiffOmittedReason::MissingContent) => "missing content",
            Some(DiffOmittedReason::ContentTooLarge) => "content exceeds 1 MiB",
            None => "unavailable",
        };
        let _ = writeln!(
            output,
            "Content diff omitted: {reason}. Use `tokenless stats show <id>` for full content."
        );
        return;
    }

    if matches!(diff.normalization, DiffNormalization::Json) {
        output.push_str("Content diff (JSON normalized for display):\n");
    } else {
        output.push_str("Content diff:\n");
    }
    output.push_str("--- before\n+++ after\n");
    for hunk in &diff.hunks {
        let _ = writeln!(
            output,
            "@@ -{},{} +{},{} @@",
            hunk.old_start, hunk.old_len, hunk.new_start, hunk.new_len
        );
        for line in &hunk.lines {
            let text = escape_terminal_controls(&line.text);
            let (prefix, ansi) = match line.kind {
                DiffLineKind::Context => (' ', None),
                DiffLineKind::Delete => ('-', Some("\x1b[31m")),
                DiffLineKind::Insert => ('+', Some("\x1b[32m")),
            };
            if color && let Some(ansi) = ansi {
                let _ = writeln!(output, "{ansi}{prefix}{text}\x1b[0m");
            } else {
                let _ = writeln!(output, "{prefix}{text}");
            }
        }
    }
    if diff.truncated {
        output.push_str("... diff truncated after 500 lines; use `stats show` for full content.\n");
    }
}

fn build_chains(
    records: &DiffRecords,
    include_content: bool,
    context: usize,
) -> (Vec<DiffChain>, bool) {
    let mut ordered: Vec<&StatsRecord> = records.records.iter().collect();
    ordered.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut groups: BTreeMap<String, Vec<&StatsRecord>> = BTreeMap::new();
    let mut standalone = Vec::new();
    for record in ordered {
        if let Some(tool_use_id) = &record.tool_use_id {
            groups.entry(tool_use_id.clone()).or_default().push(record);
        } else {
            standalone.push(build_chain(&[record], include_content, context));
        }
    }

    let mut chains = standalone;
    for grouped_records in groups.values() {
        let mut current: Vec<&StatsRecord> = Vec::new();
        for record in grouped_records {
            if current
                .last()
                .is_some_and(|previous| records.links(previous, record))
            {
                current.push(record);
            } else {
                if !current.is_empty() {
                    chains.push(build_chain(&current, include_content, context));
                }
                current = vec![record];
            }
        }
        if !current.is_empty() {
            chains.push(build_chain(&current, include_content, context));
        }
    }

    chains.sort_by(|left, right| {
        left.sort_timestamp
            .cmp(&right.sort_timestamp)
            .then_with(|| left.stages[0].record_id.cmp(&right.stages[0].record_id))
    });

    let mut tool_counts: HashMap<&str, usize> = HashMap::new();
    for chain in &chains {
        if let Some(tool) = chain.tool_use_id.as_deref() {
            *tool_counts.entry(tool).or_default() += 1;
        }
    }
    let split_chains = tool_counts.values().any(|count| *count > 1);
    (chains, split_chains)
}

fn records_link(previous: &StatsRecord, next: &StatsRecord) -> bool {
    if previous.mode != CompressionMode::Active || next.mode != CompressionMode::Active {
        return false;
    }
    match (content_pair(previous), content_pair(next)) {
        (Some((_, previous_after)), Some((next_before, _))) => previous_after == next_before,
        _ => false,
    }
}

fn build_chain(records: &[&StatsRecord], include_content: bool, context: usize) -> DiffChain {
    let first = records[0];
    let last = records[records.len() - 1];
    let before_tokens = first.before_tokens;
    let after_tokens = last.after_tokens;
    let mode = first.mode.clone();
    let diff = if include_content {
        Some(build_content_diff(
            content_pair(first).map(|pair| pair.0),
            content_pair(last).map(|pair| pair.1),
            context,
        ))
    } else {
        None
    };

    DiffChain {
        status: if records.len() > 1 {
            ChainStatus::Linked
        } else {
            ChainStatus::Standalone
        },
        mode: mode.as_str().to_string(),
        agent_id: first.agent_id.clone(),
        session_id: first.session_id.clone(),
        tool_use_id: first.tool_use_id.clone(),
        started_at: first.timestamp.to_rfc3339(),
        sort_timestamp: first.timestamp,
        before_bytes: first.before_chars,
        after_bytes: last.after_chars,
        before_tokens,
        after_tokens,
        emitted_tokens: match mode {
            CompressionMode::Active => after_tokens,
            CompressionMode::DryRun => before_tokens,
        },
        saved_tokens: token_delta(before_tokens, after_tokens),
        saved_percent: saved_percent(before_tokens, after_tokens),
        stages: records.iter().map(|record| build_stage(record)).collect(),
        diff,
    }
}

fn build_stage(record: &StatsRecord) -> DiffStage {
    let stash = if record.stash_writes.is_some()
        || record.stash_errors.is_some()
        || record.stash_size.is_some()
    {
        Some(StashMetrics {
            writes: record.stash_writes,
            errors: record.stash_errors,
            size: record.stash_size,
        })
    } else {
        None
    };
    DiffStage {
        record_id: record.id,
        timestamp: record.timestamp.to_rfc3339(),
        operation: record.operation.as_str().to_string(),
        agent_id: record.agent_id.clone(),
        mode: record.mode.as_str().to_string(),
        before_bytes: record.before_chars,
        after_bytes: record.after_chars,
        before_tokens: record.before_tokens,
        after_tokens: record.after_tokens,
        emitted_tokens: match record.mode {
            CompressionMode::Active => record.after_tokens,
            CompressionMode::DryRun => record.before_tokens,
        },
        saved_tokens: token_delta(record.before_tokens, record.after_tokens),
        saved_percent: saved_percent(record.before_tokens, record.after_tokens),
        stash,
    }
}

fn content_pair(record: &StatsRecord) -> Option<(&str, &str)> {
    if record.operation == OperationType::RewriteCommand
        && let (Some(before), Some(after)) = (&record.before_output, &record.after_output)
    {
        return Some((before, after));
    }
    match (&record.before_text, &record.after_text) {
        (Some(before), Some(after)) => Some((before, after)),
        _ => None,
    }
}

fn build_content_diff(before: Option<&str>, after: Option<&str>, context: usize) -> ContentDiff {
    let (Some(before), Some(after)) = (before, after) else {
        return omitted_diff(DiffOmittedReason::MissingContent);
    };
    if before.len() > MAX_DIFF_INPUT_BYTES || after.len() > MAX_DIFF_INPUT_BYTES {
        return omitted_diff(DiffOmittedReason::ContentTooLarge);
    }

    let (before, after, normalization) = match (
        serde_json::from_str::<serde_json::Value>(before),
        serde_json::from_str::<serde_json::Value>(after),
    ) {
        (Ok(before_json), Ok(after_json)) => (
            serde_json::to_string_pretty(&before_json).unwrap_or_else(|_| before.to_string()),
            serde_json::to_string_pretty(&after_json).unwrap_or_else(|_| after.to_string()),
            DiffNormalization::Json,
        ),
        _ => (
            before.to_string(),
            after.to_string(),
            DiffNormalization::None,
        ),
    };

    let text_diff = TextDiff::from_lines(&before, &after);
    let context = context.min(MAX_DIFF_LINES);
    let mut hunks = Vec::new();
    let mut emitted_lines = 0usize;
    let mut truncated = false;
    for group in text_diff.grouped_ops(context) {
        if emitted_lines >= MAX_DIFF_LINES {
            truncated = true;
            break;
        }
        let old_start = group.first().map_or(0, |op| op.old_range().start);
        let new_start = group.first().map_or(0, |op| op.new_range().start);
        let mut lines = Vec::new();
        'ops: for op in &group {
            for change in text_diff.iter_changes(op) {
                if emitted_lines >= MAX_DIFF_LINES {
                    truncated = true;
                    break 'ops;
                }
                let kind = match change.tag() {
                    ChangeTag::Equal => DiffLineKind::Context,
                    ChangeTag::Delete => DiffLineKind::Delete,
                    ChangeTag::Insert => DiffLineKind::Insert,
                };
                lines.push(DiffLine {
                    kind,
                    old_line: change.old_index().map(|index| index + 1),
                    new_line: change.new_index().map(|index| index + 1),
                    text: trim_diff_newline(change.value()).to_string(),
                });
                emitted_lines += 1;
            }
        }
        // A group can be cut mid-operation by the output cap. Derive the
        // ranges from the emitted prefix so each hunk remains self-consistent.
        let old_len = lines.iter().filter(|line| line.old_line.is_some()).count();
        let new_len = lines.iter().filter(|line| line.new_line.is_some()).count();
        hunks.push(DiffHunk {
            old_start: if old_len == 0 {
                old_start
            } else {
                old_start + 1
            },
            old_len,
            new_start: if new_len == 0 {
                new_start
            } else {
                new_start + 1
            },
            new_len,
            lines,
        });
    }

    ContentDiff {
        available: true,
        normalization,
        truncated,
        omitted_reason: None,
        hunks,
    }
}

fn omitted_diff(reason: DiffOmittedReason) -> ContentDiff {
    ContentDiff {
        available: false,
        normalization: DiffNormalization::None,
        truncated: false,
        omitted_reason: Some(reason),
        hunks: Vec::new(),
    }
}

fn trim_diff_newline(value: &str) -> &str {
    let value = value.strip_suffix('\n').unwrap_or(value);
    value.strip_suffix('\r').unwrap_or(value)
}

fn escape_terminal_controls(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control() {
            escaped.extend(character.escape_default());
        } else {
            escaped.push(character);
        }
    }
    escaped
}

fn sort_chains(chains: &mut [DiffChain], sort: DiffSort) {
    match sort {
        DiffSort::Saved => chains.sort_by(|left, right| {
            right
                .saved_tokens
                .cmp(&left.saved_tokens)
                .then_with(|| right.sort_timestamp.cmp(&left.sort_timestamp))
        }),
        DiffSort::Time => chains.sort_by(|left, right| {
            right
                .sort_timestamp
                .cmp(&left.sort_timestamp)
                .then_with(|| right.stages[0].record_id.cmp(&left.stages[0].record_id))
        }),
    }
}

fn token_delta(before: usize, after: usize) -> i64 {
    match before.cmp(&after) {
        Ordering::Greater => i64::try_from(before - after).unwrap_or(i64::MAX),
        Ordering::Less => -i64::try_from(after - before).unwrap_or(i64::MAX),
        Ordering::Equal => 0,
    }
}

fn saved_percent(before: usize, after: usize) -> f64 {
    if before == 0 {
        0.0
    } else {
        ((before as f64 - after as f64) / before as f64) * 100.0
    }
}

fn truncate_label(value: &str, width: usize) -> String {
    let count = value.chars().count();
    if count <= width {
        return value.to_string();
    }
    value
        .chars()
        .take(width.saturating_sub(1))
        .chain(std::iter::once('…'))
        .collect()
}

fn format_num(value: usize) -> String {
    let digits = value.to_string();
    let mut output = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            output.push(',');
        }
        output.push(character);
    }
    output
}

fn format_signed(value: i64) -> String {
    if value < 0 {
        format!("-{}", format_num(value.unsigned_abs() as usize))
    } else {
        format_num(value as usize)
    }
}

fn format_stash(stash: Option<&StashMetrics>) -> String {
    stash.map_or_else(
        || "-".to_string(),
        |stash| {
            format!(
                "{}/{}/{}",
                stash
                    .writes
                    .map_or_else(|| "-".to_string(), |value| value.to_string()),
                stash
                    .errors
                    .map_or_else(|| "-".to_string(), |value| value.to_string()),
                stash
                    .size
                    .map_or_else(|| "-".to_string(), |value| value.to_string())
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("tests/diff_tests.rs");
}
