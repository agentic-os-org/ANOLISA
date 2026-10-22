//! JSON-domain compression for PostTool results.
//!
//! One call owns record reduction, truncation, compact serialization, and
//! optional TOON representation selection. Fields and empty values are retained
//! unless an explicit bound omits their containing content. The caller owns
//! final acceptance and Stash commit or rollback.
//!
//! Every stage works on [`ExactValue`], which retains each number token as the
//! source literal it was scanned from. Numbers therefore survive
//! record reduction, truncation, compaction, and Stash payloads with their
//! spelling and JSON numeric type intact, and no per-number exactness audit
//! gates the rest of the document.

mod exact;

use std::collections::{BTreeMap, BTreeSet, HashSet};

use exact::{ExactMap, ExactValue};
use serde_json::Value;
use tokenless_ccr::{
    RecoveryMethod, StashStore, StashWrite, recovery_instruction, truncation_suffix_for,
};
use tokenless_protocol::estimate_tokens;

const LOSSLESS_MIN_SAVINGS_PERCENT: usize = 15;
const RECORD_MIN_ITEMS: usize = 33;
const RECORD_MAX_ITEMS: usize = 32;
const RECORD_HEAD_ITEMS: usize = 4;
const RECORD_TAIL_ITEMS: usize = 4;
const NUMERIC_MIN_SAMPLES: usize = 5;
const NUMERIC_OUTLIER_SIGMA: f64 = 2.0;

/// Configuration for one JSON-domain compressor.
#[derive(Debug, Clone)]
pub struct JsonCompressionConfig {
    /// Maximum string length in Unicode scalar values.
    pub truncate_strings_at: usize,
    /// Number of array items retained from the head.
    pub truncate_arrays_at: usize,
    /// Number of array items retained from the tail.
    pub array_tail_preserve: usize,
    /// Maximum JSON nesting depth before a subtree is replaced.
    pub max_depth: usize,
    /// Emits a bounded marker when truncating content.
    pub add_truncation_marker: bool,
}

impl Default for JsonCompressionConfig {
    fn default() -> Self {
        Self {
            truncate_strings_at: 4096,
            truncate_arrays_at: 32,
            array_tail_preserve: 8,
            max_depth: 8,
            add_truncation_marker: true,
        }
    }
}

/// Per-call facts that affect valid JSON representations.
pub struct JsonCompressionContext<'a> {
    /// Recovery instruction included before measuring any candidate.
    pub recovery: &'a RecoveryMethod,
    /// Store used to back truncation markers, when retrieval is reachable.
    pub stash: Option<&'a dyn StashStore>,
    /// Whether the host accepts a non-JSON text representation such as TOON.
    pub allow_toon: bool,
    /// Minimum candidate size before TOON is considered.
    pub min_toon_chars: usize,
    /// Whether an unrecoverable bounded candidate may displace a lossless one.
    pub allow_unrecoverable: bool,
}

/// Stable operations performed inside the JSON domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonOperation {
    /// JSON was serialized compactly, optionally unwrapping one JSON string
    /// envelope, without omitting payload values.
    Cleanup,
    /// A record collection was reduced using deterministic importance signals.
    RecordReduction,
    /// One or more values were bounded by string, array, or depth limits.
    Truncation,
    /// TOON was selected as the final representation.
    Toon,
}

impl JsonOperation {
    /// Stable internal operation identifier.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Cleanup => "json-cleanup",
            Self::RecordReduction => "json-record-reduction",
            Self::Truncation => "json-truncation",
            Self::Toon => "json-toon",
        }
    }
}

/// Recovery state of a JSON candidate relative to its input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recoverability {
    /// No bounded content was removed.
    Lossless,
    /// Every truncation has a reachable Stash marker.
    Retrievable,
    /// At least one truncation cannot be recovered.
    Unrecoverable,
}

/// Observability produced during one JSON compression attempt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JsonMetrics {
    /// Failed Stash writes while producing tentative candidates.
    pub stash_errors: usize,
    /// Record collections reduced in the selected candidate.
    pub record_reductions: usize,
    /// Records omitted from the selected candidate.
    pub records_omitted: usize,
    /// Truncations present in the selected candidate.
    pub truncations: usize,
    /// Selected truncations without a retrievable marker.
    pub unrecoverable_truncations: usize,
}

/// Complete result of one JSON-domain compression attempt.
#[derive(Debug)]
pub struct JsonOutcome {
    /// Candidate selected inside the JSON domain.
    pub output: String,
    /// Operations that shaped `output`, in execution order.
    pub operations: Vec<JsonOperation>,
    /// Recovery state of `output`.
    pub recoverability: Recoverability,
    /// Every tentative write performed while producing candidates. The
    /// Runtime ledger decides which writes reach the final output.
    pub stash_writes: Vec<StashWrite>,
    /// Metrics associated with the attempt and selected candidate.
    pub metrics: JsonMetrics,
}

/// JSON-domain compression failures.
#[derive(Debug, thiserror::Error)]
pub enum JsonError {
    /// Input was not valid JSON.
    #[error("invalid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
}

/// Compresses JSON tool results as one content domain.
#[derive(Debug, Clone, Default)]
pub struct JsonCompressor {
    config: JsonCompressionConfig,
}

impl JsonCompressor {
    /// Builds a compressor with explicit limits.
    #[must_use]
    pub fn new(config: JsonCompressionConfig) -> Self {
        Self { config }
    }

    /// Produces the best valid JSON-domain candidate.
    ///
    /// # Errors
    ///
    /// Returns [`JsonError::InvalidJson`] when the direct input or a detected
    /// JSON string envelope cannot be parsed.
    ///
    /// Numbers are never a reason to reject a document: each one is retained as
    /// its source literal, so an exact value that `serde_json::Value` could not
    /// hold no longer costs the rest of the response its compression.
    pub fn compress(
        &self,
        input: &str,
        context: &JsonCompressionContext<'_>,
    ) -> Result<JsonOutcome, JsonError> {
        // The detector skips one leading byte-order mark, so a marked document
        // is routed here; the parser must agree or that routing turns a
        // passthrough into an error. `serde_json` rejects the mark, so skip it
        // the same way. The stash still holds the original bytes.
        let source = input.strip_prefix('\u{feff}').unwrap_or(input);
        let document = match exact::parse(source) {
            Ok(document) => document,
            Err(exact::ScanRejected) => return rejected(source, input),
        };
        // Unwrap one string envelope carrying an object or array. Measure every
        // candidate against the received wire form so unwrapping counts toward
        // both operation attribution and the lossless savings gate.
        let unwrapped;
        let tree = match &document {
            ExactValue::String(inner) => match exact::parse(inner) {
                Ok(inner_tree) if inner_tree.is_container() => {
                    unwrapped = inner_tree;
                    &unwrapped
                }
                _ => &document,
            },
            _ => &document,
        };

        let mut full_session = Session::new(&self.config, None, false, context.recovery);
        let full = build_candidate(
            source,
            full_session.compress_value(tree, 0),
            &full_session,
            context,
        );
        if saves_at_least_percent(&full.output, source, LOSSLESS_MIN_SAVINGS_PERCENT) {
            return Ok(full.into_outcome(Vec::new(), 0));
        }

        let mut bounded_session = Session::new(&self.config, context.stash, true, context.recovery);
        let bounded = build_candidate(
            source,
            bounded_session.compress_value(tree, 0),
            &bounded_session,
            context,
        );
        let bounded_is_valid =
            context.allow_unrecoverable || bounded.recoverability != Recoverability::Unrecoverable;
        let bounded_is_smaller = strictly_smaller(&bounded.output, &full.output);
        let selected = if bounded_is_smaller && (bounded_is_valid || full.operations.is_empty()) {
            // When no lossless operation exists, return the bounded attempt so
            // the outer arbiter can preserve its recoverability-unavailable
            // disposition while still preventing it from reaching the model.
            bounded
        } else {
            full
        };

        Ok(selected.into_outcome(bounded_session.stash_writes, bounded_session.stash_errors))
    }
}

struct Candidate {
    output: String,
    operations: Vec<JsonOperation>,
    recoverability: Recoverability,
    metrics: JsonMetrics,
}

impl Candidate {
    fn into_outcome(self, stash_writes: Vec<StashWrite>, stash_errors: usize) -> JsonOutcome {
        JsonOutcome {
            output: self.output,
            operations: self.operations,
            recoverability: self.recoverability,
            stash_writes,
            metrics: JsonMetrics {
                stash_errors,
                ..self.metrics
            },
        }
    }
}

fn build_candidate(
    baseline: &str,
    transformed: ExactValue<'_>,
    session: &Session<'_>,
    context: &JsonCompressionContext<'_>,
) -> Candidate {
    let compact = exact::write_to_string(&transformed);
    let mut output = baseline.to_owned();
    let mut transformed_selected = false;
    let mut toon_selected = false;
    if strictly_smaller(&compact, baseline) {
        output.clone_from(&compact);
        transformed_selected = true;
    }
    if let Some(toon) = context
        .allow_toon
        .then(|| toon_candidate(&transformed, &compact, context.min_toon_chars))
        .flatten()
        .filter(|toon| {
            strictly_smaller(toon, baseline)
                && (!transformed_selected || strictly_smaller(toon, &output))
        })
    {
        output = toon;
        transformed_selected = true;
        toon_selected = true;
    }

    let mut operations = Vec::new();
    if transformed_selected
        && !toon_selected
        && session.record_reductions == 0
        && session.truncations == 0
        && compact != baseline
    {
        operations.push(JsonOperation::Cleanup);
    }
    if transformed_selected && session.record_reductions > 0 {
        operations.push(JsonOperation::RecordReduction);
    }
    if transformed_selected && session.truncations > 0 {
        operations.push(JsonOperation::Truncation);
    }
    if toon_selected {
        operations.push(JsonOperation::Toon);
    }

    let transformation_selected = transformed_selected;
    let metrics = if transformation_selected {
        JsonMetrics {
            stash_errors: 0,
            record_reductions: session.record_reductions,
            records_omitted: session.records_omitted,
            truncations: session.truncations,
            unrecoverable_truncations: session.unrecoverable_truncations,
        }
    } else {
        JsonMetrics::default()
    };
    let recoverability = if transformation_selected {
        session.recoverability()
    } else {
        Recoverability::Lossless
    };

    Candidate {
        output,
        operations,
        recoverability,
        metrics,
    }
}

/// Outcome for a document the exact scanner does not accept.
///
/// The scanner accepts exactly the documents `serde_json` accepts, so this is
/// normally `serde_json`'s own diagnostic, which the pipeline routes to
/// passthrough. Should the two ever disagree, the #7069 fail-safe applies:
/// return the source response unchanged instead of compressing a document this
/// crate cannot represent exactly.
fn rejected(source: &str, input: &str) -> Result<JsonOutcome, JsonError> {
    match serde_json::from_str::<Value>(source) {
        Err(error) => Err(JsonError::InvalidJson(error)),
        Ok(_) => Ok(JsonOutcome {
            output: input.to_owned(),
            operations: Vec::new(),
            recoverability: Recoverability::Lossless,
            stash_writes: Vec::new(),
            metrics: JsonMetrics::default(),
        }),
    }
}

/// Builds the TOON candidate for an already-compacted exact document.
///
/// TOON's numeric domain is narrower than JSON's — `toon-format` stores
/// `i64`/`u64`/`f64` — so a shorter encoding may only replace the exact JSON
/// tree when decoding restores every retained value and type. The check runs
/// against the exact document, not against an approximated `serde_json` tree:
/// comparing two already-rounded trees would accept silent precision loss. A
/// candidate that fails keeps the compacted JSON, which already carries the
/// exact literals.
fn toon_candidate(value: &ExactValue<'_>, compact: &str, min_chars: usize) -> Option<String> {
    if compact.chars().count() < min_chars {
        return None;
    }
    let approximate = exact::to_serde_value(value)?;
    let encoded = toon_format::encode_default(&approximate).ok()?;
    let candidate = encoded.trim_end().to_owned();
    let decoded: Value = toon_format::decode_default(&candidate).ok()?;
    (!candidate.is_empty() && exact::matches_exact(&decoded, value)).then_some(candidate)
}

fn strictly_smaller(candidate: &str, baseline: &str) -> bool {
    candidate.chars().count() < baseline.chars().count()
        && estimate_tokens(candidate) < estimate_tokens(baseline)
}

fn saves_at_least_percent(candidate: &str, baseline: &str, percent: usize) -> bool {
    if !strictly_smaller(candidate, baseline) {
        return false;
    }
    let baseline_tokens = estimate_tokens(baseline);
    let saved_tokens = baseline_tokens - estimate_tokens(candidate);
    saved_tokens.saturating_mul(100) >= baseline_tokens.saturating_mul(percent)
}

fn is_record_array(values: &[ExactValue<'_>]) -> bool {
    values.len() >= RECORD_MIN_ITEMS && values.iter().all(ExactValue::is_object)
}

fn select_record_indices(values: &[ExactValue<'_>]) -> BTreeSet<usize> {
    let mut selected = BTreeSet::new();
    selected.extend(0..values.len().min(RECORD_HEAD_ITEMS));
    selected.extend(values.len().saturating_sub(RECORD_TAIL_ITEMS)..values.len());

    let modal_shape = modal_record_shape(values);
    for (index, value) in values.iter().enumerate() {
        let Some(record) = value.as_object() else {
            continue;
        };
        if has_error_signal(record) || record_shape(record) != modal_shape {
            selected.insert(index);
        }
    }
    select_numeric_outliers(values, &mut selected);

    // Deduplication compares records by value, not by spelling: two records
    // whose numbers differ only in equivalent spellings (`1.2500`, `1.25`) are
    // the same record for sampling purposes, and the first one kept still
    // carries its own literal into the output.
    let mut seen = selected
        .iter()
        .map(|index| dedup_key(&values[*index]))
        .collect::<HashSet<_>>();
    let ordinary = values
        .iter()
        .enumerate()
        .filter_map(|(index, value)| {
            (!selected.contains(&index) && seen.insert(dedup_key(value))).then_some(index)
        })
        .collect::<Vec<_>>();
    let sample_count = RECORD_MAX_ITEMS
        .saturating_sub(selected.len())
        .min(ordinary.len());
    for slot in 0..sample_count {
        let position = (2 * slot + 1) * ordinary.len() / (2 * sample_count);
        selected.insert(ordinary[position]);
    }
    selected
}

fn dedup_key(value: &ExactValue<'_>) -> String {
    let mut key = String::new();
    exact::write_dedup_key(value, &mut key);
    key
}

fn modal_record_shape(values: &[ExactValue<'_>]) -> Vec<String> {
    let mut counts = BTreeMap::<Vec<String>, usize>::new();
    for value in values {
        if let Some(record) = value.as_object() {
            *counts.entry(record_shape(record)).or_default() += 1;
        }
    }
    let mut mode = Vec::new();
    let mut mode_count = 0;
    for (shape, count) in counts {
        if count > mode_count {
            mode = shape;
            mode_count = count;
        }
    }
    mode
}

fn record_shape(record: &ExactMap<'_>) -> Vec<String> {
    let mut shape = record.keys().cloned().collect::<Vec<_>>();
    shape.sort_unstable();
    shape
}

fn has_error_signal(record: &ExactMap<'_>) -> bool {
    const ERROR_FIELDS: [&str; 4] = ["error", "errors", "exception", "failure"];
    const STATUS_FIELDS: [&str; 4] = ["status", "state", "level", "severity"];
    const STATUS_SIGNALS: [&str; 7] = [
        "error", "failed", "fatal", "critical", "panic", "timeout", "warn",
    ];

    record.iter().any(|(key, value)| {
        let key = key.to_ascii_lowercase();
        if ERROR_FIELDS.contains(&key.as_str()) {
            return !value.is_empty_or_null();
        }
        STATUS_FIELDS.contains(&key.as_str())
            && value.as_str().is_some_and(|status| {
                let status = status.to_ascii_lowercase();
                STATUS_SIGNALS.iter().any(|signal| status.contains(signal))
            })
    })
}

fn select_numeric_outliers(values: &[ExactValue<'_>], selected: &mut BTreeSet<usize>) {
    let mut samples = BTreeMap::<String, Vec<(usize, f64)>>::new();
    for (index, value) in values.iter().enumerate() {
        let Some(record) = value.as_object() else {
            continue;
        };
        for (field, value) in record {
            let ExactValue::Number(literal) = value else {
                continue;
            };
            match exact::approximate(literal) {
                // The approximation only ever ranks candidates. Output keeps the
                // retained literal, so a rounded statistic cannot leak into it.
                Some(number) => samples
                    .entry(field.clone())
                    .or_default()
                    .push((index, number)),
                // A literal with no finite approximation cannot join the
                // statistics. Retain its record rather than let a sample budget
                // decide a value nothing measured.
                None => {
                    selected.insert(index);
                }
            }
        }
    }

    for values in samples
        .values()
        .filter(|values| values.len() >= NUMERIC_MIN_SAMPLES)
    {
        let mean = values.iter().map(|(_, value)| value).sum::<f64>() / values.len() as f64;
        let variance = values
            .iter()
            .map(|(_, value)| (value - mean).powi(2))
            .sum::<f64>()
            / values.len() as f64;
        let threshold = NUMERIC_OUTLIER_SIGMA * variance.sqrt();
        if threshold == 0.0 {
            continue;
        }
        selected.extend(
            values
                .iter()
                .filter(|(_, value)| (value - mean).abs() > threshold)
                .map(|(index, _)| *index),
        );
    }
}

struct Session<'a> {
    recovery: &'a RecoveryMethod,
    config: &'a JsonCompressionConfig,
    stash: Option<&'a dyn StashStore>,
    bounds_enabled: bool,
    stash_writes: Vec<StashWrite>,
    stash_errors: usize,
    record_reductions: usize,
    records_omitted: usize,
    truncations: usize,
    unrecoverable_truncations: usize,
}

impl<'a> Session<'a> {
    fn new(
        config: &'a JsonCompressionConfig,
        stash: Option<&'a dyn StashStore>,
        bounds_enabled: bool,
        recovery: &'a RecoveryMethod,
    ) -> Self {
        Self {
            recovery,
            config,
            stash: stash.filter(|_| recovery.is_available()),
            bounds_enabled,
            stash_writes: Vec::new(),
            stash_errors: 0,
            record_reductions: 0,
            records_omitted: 0,
            truncations: 0,
            unrecoverable_truncations: 0,
        }
    }

    fn recoverability(&self) -> Recoverability {
        if self.record_reductions == 0 && self.truncations == 0 {
            Recoverability::Lossless
        } else if self.stash.is_some() && self.unrecoverable_truncations == 0 {
            Recoverability::Retrievable
        } else {
            Recoverability::Unrecoverable
        }
    }

    fn compress_value<'doc>(&mut self, value: &ExactValue<'doc>, depth: usize) -> ExactValue<'doc> {
        if self.bounds_enabled && depth > self.config.max_depth {
            self.truncations += 1;
            let type_name = value.type_name();
            // The stashed subtree is written by the exact serializer, so a
            // truncated subtree is retrievable with its literals intact.
            let serialized = exact::write_to_string(value);
            if let Some(key) = self.stash_payload(&serialized) {
                return ExactValue::String(format!(
                    "{type_name} truncated at depth {depth}. {}",
                    recovery_instruction(&key, self.recovery)
                ));
            }
            self.mark_unrecoverable();
            return ExactValue::String(format!("<{type_name} truncated at depth {depth}>"));
        }

        match value {
            // A number is carried through untouched. Its retained literal is the
            // value, so no stage may reserialize it through `serde_json`.
            ExactValue::Null | ExactValue::Bool(_) | ExactValue::Number(_) => value.clone(),
            ExactValue::String(value) => self.compress_string(value),
            ExactValue::Array(value) => self.compress_array(value, depth),
            ExactValue::Object(value) => self.compress_object(value, depth),
        }
    }

    fn compress_string<'doc>(&mut self, value: &str) -> ExactValue<'doc> {
        if !self.bounds_enabled || value.chars().count() <= self.config.truncate_strings_at {
            return ExactValue::String(value.to_owned());
        }
        self.truncations += 1;

        let suffix_len = truncation_suffix_for("000000000000000000000000", self.recovery)
            .chars()
            .count();
        let reversible_fits =
            self.config.add_truncation_marker && self.config.truncate_strings_at > suffix_len;
        if reversible_fits && let Some(key) = self.stash_payload(value) {
            let keep = self.config.truncate_strings_at - suffix_len;
            return ExactValue::String(format!(
                "{}{}",
                prefix_chars(value, keep),
                truncation_suffix_for(&key, self.recovery)
            ));
        }
        self.mark_unrecoverable();

        const MARKER: &str = "… (truncated)";
        let marker_len = MARKER.chars().count();
        let attach_marker =
            self.config.add_truncation_marker && self.config.truncate_strings_at > marker_len;
        let keep = if attach_marker {
            self.config.truncate_strings_at - marker_len
        } else {
            self.config.truncate_strings_at
        };
        let mut output = prefix_chars(value, keep).to_owned();
        if attach_marker {
            output.push_str(MARKER);
        }
        ExactValue::String(output)
    }

    fn compress_array<'doc>(
        &mut self,
        values: &[ExactValue<'doc>],
        depth: usize,
    ) -> ExactValue<'doc> {
        if self.bounds_enabled && is_record_array(values) {
            return self
                .reduce_records(values, depth)
                .unwrap_or_else(|| self.compress_all_array_items(values, depth));
        }
        if !self.bounds_enabled {
            return self.compress_all_array_items(values, depth);
        }

        let head = self.config.truncate_arrays_at;
        let budget = head.saturating_add(self.config.array_tail_preserve);
        let truncate = values.len() > head && values.len() > budget;
        if truncate {
            self.truncations += 1;
        }
        let tail = if truncate {
            self.config.array_tail_preserve
        } else if values.len() > head {
            values.len() - head
        } else {
            0
        };
        let head_end = values.len().min(head);
        let mut output = Vec::new();
        for value in values.iter().take(head_end) {
            output.push(self.compress_value(value, depth + 1));
        }

        if truncate && self.config.add_truncation_marker {
            let tail_start = values.len() - tail;
            let dropped = &values[head_end..tail_start];
            let marker = if let Some(key) = self.stash_dropped(dropped) {
                format!(
                    "{} items omitted. {}",
                    dropped.len(),
                    recovery_instruction(&key, self.recovery)
                )
            } else {
                self.mark_unrecoverable();
                format!("<... {} more items truncated, not stashed>", dropped.len())
            };
            output.push(ExactValue::String(marker));
        } else if truncate {
            self.mark_unrecoverable();
        }

        for value in values.iter().skip(values.len() - tail) {
            output.push(self.compress_value(value, depth + 1));
        }
        ExactValue::Array(output)
    }

    fn compress_all_array_items<'doc>(
        &mut self,
        values: &[ExactValue<'doc>],
        depth: usize,
    ) -> ExactValue<'doc> {
        let mut output = Vec::with_capacity(values.len());
        for value in values {
            output.push(self.compress_value(value, depth + 1));
        }
        ExactValue::Array(output)
    }

    fn reduce_records<'doc>(
        &mut self,
        values: &[ExactValue<'doc>],
        depth: usize,
    ) -> Option<ExactValue<'doc>> {
        let selected = select_record_indices(values);
        let omitted = values.len() - selected.len();
        if omitted == 0 {
            return None;
        }
        // The complete collection is stashed exactly: retrieval must return the
        // omitted records with the numbers they were written with.
        let payload = exact::write_array_to_string(values);
        let key = self.stash_payload(&payload)?;

        let mut output = Vec::with_capacity(selected.len() + 1);
        for index in selected {
            output.push(self.compress_value(&values[index], depth + 1));
        }
        output.push(ExactValue::String(format!(
            "{omitted} of {} records omitted. {}",
            values.len(),
            recovery_instruction(&key, self.recovery)
        )));
        self.record_reductions += 1;
        self.records_omitted += omitted;
        Some(ExactValue::Array(output))
    }

    fn compress_object<'doc>(&mut self, values: &ExactMap<'doc>, depth: usize) -> ExactValue<'doc> {
        let mut output = ExactMap::new();
        for (key, value) in values {
            let compressed = self.compress_value(value, depth + 1);
            output.insert(key.clone(), compressed);
        }
        ExactValue::Object(output)
    }

    fn stash_dropped(&mut self, dropped: &[ExactValue<'_>]) -> Option<String> {
        if dropped.is_empty() {
            return None;
        }
        let payload = exact::write_array_to_string(dropped);
        self.stash_payload(&payload)
    }

    fn stash_payload(&mut self, payload: &str) -> Option<String> {
        let stash = self.stash?;
        match stash.stash(payload) {
            Ok(write) => {
                let key = write.key.clone();
                self.stash_writes.push(write);
                Some(key)
            }
            Err(_) => {
                self.stash_errors += 1;
                None
            }
        }
    }

    fn mark_unrecoverable(&mut self) {
        self.unrecoverable_truncations += 1;
    }
}

fn prefix_chars(value: &str, count: usize) -> &str {
    let end = value
        .char_indices()
        .nth(count)
        .map_or(value.len(), |(index, _)| index);
    &value[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("tests/json_tests.rs");
}
