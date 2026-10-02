//! JSON-domain compression for PostTool results.
//!
//! One call owns the complete JSON decision: tree cleanup, record reduction,
//! truncation, structured-slot restoration, compact serialization, and
//! optional TOON representation selection. The caller owns final acceptance
//! and Stash commit or rollback.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde_json::{Map, Value};
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
    /// Removes null-valued object fields and array entries.
    pub drop_nulls: bool,
    /// Removes empty strings, arrays, and objects.
    pub drop_empty_fields: bool,
    /// Emits a bounded marker when truncating content.
    pub add_truncation_marker: bool,
}

impl Default for JsonCompressionConfig {
    fn default() -> Self {
        Self {
            truncate_strings_at: 4096,
            truncate_arrays_at: 32,
            array_tail_preserve: 8,
            drop_nulls: true,
            drop_empty_fields: true,
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
    /// Whether empty top-level fields must survive replacement.
    pub preserve_top_level_shape: bool,
    /// Minimum candidate size before TOON is considered.
    pub min_toon_chars: usize,
    /// Whether an unrecoverable bounded candidate may displace a lossless one.
    pub allow_unrecoverable: bool,
}

/// Stable operations performed inside the JSON domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonOperation {
    /// Structural cleanup or compact JSON serialization changed the value.
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
    pub fn compress(
        &self,
        input: &str,
        context: &JsonCompressionContext<'_>,
    ) -> Result<JsonOutcome, JsonError> {
        let (normalized, original) = parse_input(input)?;
        let mut full_session = Session::new(&self.config, None, false, context.recovery);
        let full = build_candidate(
            &normalized,
            &original,
            full_session.compress_value(&original, 0),
            &full_session,
            context,
        )?;
        if saves_at_least_percent(&full.output, &normalized, LOSSLESS_MIN_SAVINGS_PERCENT) {
            return Ok(full.into_outcome(Vec::new(), 0));
        }

        let mut bounded_session = Session::new(&self.config, context.stash, true, context.recovery);
        let bounded = build_candidate(
            &normalized,
            &original,
            bounded_session.compress_value(&original, 0),
            &bounded_session,
            context,
        )?;
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
    normalized: &str,
    original: &Value,
    transformed: Value,
    session: &Session<'_>,
    context: &JsonCompressionContext<'_>,
) -> Result<Candidate, JsonError> {
    let transformed = if context.preserve_top_level_shape {
        restore_top_level_shape(original, transformed)
    } else {
        transformed
    };
    let compact = serde_json::to_string(&transformed)?;
    let mut output = normalized.to_owned();
    let mut transformed_selected = false;
    let mut toon_selected = false;
    if strictly_smaller(&compact, normalized) {
        output.clone_from(&compact);
        transformed_selected = true;
    }
    if let Some(toon) = context
        .allow_toon
        .then(|| toon_candidate(&transformed, context.min_toon_chars))
        .flatten()
        .filter(|toon| {
            strictly_smaller(toon, normalized)
                && (!transformed_selected || strictly_smaller(toon, &output))
        })
    {
        output = toon;
        transformed_selected = true;
        toon_selected = true;
    }

    let mut operations = Vec::new();
    if transformed_selected
        && (session.cleanup_changes > 0
            || (!toon_selected
                && session.record_reductions == 0
                && session.truncations == 0
                && compact != normalized))
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

    Ok(Candidate {
        output,
        operations,
        recoverability,
        metrics,
    })
}

fn parse_input(input: &str) -> Result<(String, Value), JsonError> {
    let outer: Value = serde_json::from_str(input)?;
    if let Value::String(inner) = &outer
        && let Ok(value @ (Value::Object(_) | Value::Array(_))) = serde_json::from_str(inner)
    {
        return Ok((serde_json::to_string(&value)?, value));
    }
    Ok((input.to_owned(), outer))
}

fn restore_top_level_shape(original: &Value, transformed: Value) -> Value {
    let Value::Object(original) = original else {
        return transformed;
    };
    let mut transformed = match transformed {
        Value::Object(transformed) => transformed,
        other => return other,
    };
    for (key, value) in original {
        if !transformed.contains_key(key) && is_empty_or_null(value) {
            transformed.insert(key.clone(), value.clone());
        }
    }
    Value::Object(transformed)
}

fn toon_candidate(value: &Value, min_chars: usize) -> Option<String> {
    let compact = serde_json::to_string(value).ok()?;
    if compact.chars().count() < min_chars {
        return None;
    }
    let encoded = toon_format::encode_default(value).ok()?;
    let candidate = encoded.trim_end().to_owned();
    (!candidate.is_empty()).then_some(candidate)
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

fn is_empty(value: &Value) -> bool {
    value.as_str() == Some("")
        || value.as_array().is_some_and(Vec::is_empty)
        || value.as_object().is_some_and(Map::is_empty)
}

fn is_empty_or_null(value: &Value) -> bool {
    value.is_null() || is_empty(value)
}

fn is_record_array(values: &[Value]) -> bool {
    values.len() >= RECORD_MIN_ITEMS && values.iter().all(Value::is_object)
}

fn select_record_indices(values: &[Value]) -> BTreeSet<usize> {
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

    let mut seen = selected
        .iter()
        .map(|index| values[*index].to_string())
        .collect::<HashSet<_>>();
    let ordinary = values
        .iter()
        .enumerate()
        .filter_map(|(index, value)| {
            (!selected.contains(&index) && seen.insert(value.to_string())).then_some(index)
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

fn modal_record_shape(values: &[Value]) -> Vec<String> {
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

fn record_shape(record: &Map<String, Value>) -> Vec<String> {
    let mut shape = record.keys().cloned().collect::<Vec<_>>();
    shape.sort_unstable();
    shape
}

fn has_error_signal(record: &Map<String, Value>) -> bool {
    const ERROR_FIELDS: [&str; 4] = ["error", "errors", "exception", "failure"];
    const STATUS_FIELDS: [&str; 4] = ["status", "state", "level", "severity"];
    const STATUS_SIGNALS: [&str; 7] = [
        "error", "failed", "fatal", "critical", "panic", "timeout", "warn",
    ];

    record.iter().any(|(key, value)| {
        let key = key.to_ascii_lowercase();
        if ERROR_FIELDS.contains(&key.as_str()) {
            return !is_empty_or_null(value);
        }
        STATUS_FIELDS.contains(&key.as_str())
            && value.as_str().is_some_and(|status| {
                let status = status.to_ascii_lowercase();
                STATUS_SIGNALS.iter().any(|signal| status.contains(signal))
            })
    })
}

fn select_numeric_outliers(values: &[Value], selected: &mut BTreeSet<usize>) {
    let mut samples = BTreeMap::<String, Vec<(usize, f64)>>::new();
    for (index, value) in values.iter().enumerate() {
        let Some(record) = value.as_object() else {
            continue;
        };
        for (field, value) in record {
            if let Some(number) = value.as_f64().filter(|number| number.is_finite()) {
                samples
                    .entry(field.clone())
                    .or_default()
                    .push((index, number));
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
    drop_fields: HashSet<&'static str>,
    stash_writes: Vec<StashWrite>,
    stash_errors: usize,
    cleanup_changes: usize,
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
            drop_fields: HashSet::from([
                "debug",
                "trace",
                "traces",
                "stack",
                "stacktrace",
                "logs",
                "logging",
            ]),
            stash_writes: Vec::new(),
            stash_errors: 0,
            cleanup_changes: 0,
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

    fn compress_value(&mut self, value: &Value, depth: usize) -> Value {
        if self.bounds_enabled && depth > self.config.max_depth {
            self.truncations += 1;
            let type_name = match value {
                Value::Null => "null",
                Value::Bool(_) => "bool",
                Value::Number(_) => "number",
                Value::String(_) => "string",
                Value::Array(_) => "array",
                Value::Object(_) => "object",
            };
            if let Ok(serialized) = serde_json::to_string(value)
                && let Some(key) = self.stash_payload(&serialized)
            {
                return Value::String(format!(
                    "{type_name} truncated at depth {depth}. {}",
                    recovery_instruction(&key, self.recovery)
                ));
            }
            self.mark_unrecoverable();
            return Value::String(format!("<{type_name} truncated at depth {depth}>"));
        }

        match value {
            Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
            Value::String(value) => self.compress_string(value),
            Value::Array(value) => self.compress_array(value, depth),
            Value::Object(value) => self.compress_object(value, depth),
        }
    }

    fn compress_string(&mut self, value: &str) -> Value {
        if !self.bounds_enabled || value.chars().count() <= self.config.truncate_strings_at {
            return Value::String(value.to_owned());
        }
        self.truncations += 1;

        let suffix_len = truncation_suffix_for("000000000000000000000000", self.recovery)
            .chars()
            .count();
        let reversible_fits =
            self.config.add_truncation_marker && self.config.truncate_strings_at > suffix_len;
        if reversible_fits && let Some(key) = self.stash_payload(value) {
            let keep = self.config.truncate_strings_at - suffix_len;
            return Value::String(format!(
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
        Value::String(output)
    }

    fn compress_array(&mut self, values: &[Value], depth: usize) -> Value {
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
            self.push_if_kept(&mut output, value, depth);
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
            output.push(Value::String(marker));
        } else if truncate {
            self.mark_unrecoverable();
        }

        for value in values.iter().skip(values.len() - tail) {
            self.push_if_kept(&mut output, value, depth);
        }
        Value::Array(output)
    }

    fn compress_all_array_items(&mut self, values: &[Value], depth: usize) -> Value {
        let mut output = Vec::with_capacity(values.len());
        for value in values {
            self.push_if_kept(&mut output, value, depth);
        }
        Value::Array(output)
    }

    fn reduce_records(&mut self, values: &[Value], depth: usize) -> Option<Value> {
        let selected = select_record_indices(values);
        let omitted = values.len() - selected.len();
        if omitted == 0 {
            return None;
        }
        let payload = serde_json::to_string(values).ok()?;
        let key = self.stash_payload(&payload)?;

        let mut output = Vec::with_capacity(selected.len() + 1);
        for index in selected {
            self.push_if_kept(&mut output, &values[index], depth);
        }
        // Cleanup can drop selected records that collapsed to null or empty
        // values. Recount from the records actually emitted so the marker and
        // `records_omitted` stay consistent with the output; the complete
        // array is stashed either way.
        let omitted = values.len() - output.len();
        output.push(Value::String(format!(
            "{omitted} of {} records omitted. {}",
            values.len(),
            recovery_instruction(&key, self.recovery)
        )));
        self.record_reductions += 1;
        self.records_omitted += omitted;
        Some(Value::Array(output))
    }

    fn push_if_kept(&mut self, output: &mut Vec<Value>, value: &Value, depth: usize) {
        let compressed = self.compress_value(value, depth + 1);
        if (self.config.drop_nulls && compressed.is_null())
            || (self.config.drop_empty_fields && is_empty(&compressed))
        {
            self.cleanup_changes += 1;
            return;
        }
        output.push(compressed);
    }

    fn compress_object(&mut self, values: &Map<String, Value>, depth: usize) -> Value {
        let mut output = Map::new();
        for (key, value) in values {
            if self.drop_fields.contains(key.as_str()) {
                self.cleanup_changes += 1;
                continue;
            }
            let compressed = self.compress_value(value, depth + 1);
            if (self.config.drop_nulls && compressed.is_null())
                || (self.config.drop_empty_fields && is_empty(&compressed))
            {
                self.cleanup_changes += 1;
                continue;
            }
            output.insert(key.clone(), compressed);
        }
        Value::Object(output)
    }

    fn stash_dropped(&mut self, dropped: &[Value]) -> Option<String> {
        if dropped.is_empty() {
            return None;
        }
        let payload = match serde_json::to_string(dropped) {
            Ok(payload) => payload,
            Err(_) => return None,
        };
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
