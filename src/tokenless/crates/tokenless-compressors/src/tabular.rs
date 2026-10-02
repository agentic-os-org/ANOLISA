//! CSV/TSV model views that preserve cells or recoverably select complete rows.
//! Original text, including quoting and line endings, backs every row omission.

use std::collections::BTreeSet;

use csv::StringRecord;
use tokenless_ccr::{RecoveryMethod, StashStore, StashWrite, compute_key, recovery_instruction};
use tokenless_protocol::estimate_tokens;

const DETECTION_BYTES: usize = 64 * 1024;
const DETECTION_RECORDS: usize = 10;
const FULL_SAVINGS_PERCENT: usize = 15;
const TARGET_ROWS: usize = 32;
const EDGE_ROWS: usize = 4;
const MAX_ROW_RANGES_BYTES: usize = 1024;

/// Transformations that can reach the model through the tabular domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabularOperation {
    /// Quoting and record terminators changed without changing any cells.
    Compaction,
    /// Complete rows were omitted behind a reference to the original text.
    /// Includes the same cell-preserving normalization as compaction.
    RowReduction,
}

/// Counts for the selected table view; the header is not a data row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TabularMetrics {
    /// Data rows in the parsed source.
    pub original_rows: usize,
    /// Data rows in the selected view.
    pub retained_rows: usize,
    /// Failed original-text writes: zero or one per compression attempt.
    pub stash_errors: usize,
}

/// Candidate and tentative storage writes for Runtime arbitration.
#[derive(Debug)]
pub struct TabularOutcome {
    /// CSV/TSV text, with an external recovery note when rows were omitted.
    pub output: String,
    /// Operations in the selected candidate.
    pub operations: Vec<TabularOperation>,
    /// Full views preserve cells, while reduced views require retrieval.
    pub recoverability: crate::Recoverability,
    /// Runtime commits or rolls back these writes after final arbitration.
    pub stash_writes: Vec<StashWrite>,
    /// Table and storage measurements.
    pub metrics: TabularMetrics,
}

/// Stateless compressor for rectangular, header-first CSV and TSV text.
#[derive(Debug, Clone, Copy, Default)]
pub struct TabularCompressor;

impl TabularCompressor {
    /// Detects an unambiguous delimiter in at most 64 KiB and ten records.
    /// Requires a header and two data records; a cut-off trailing record is ignored.
    #[must_use]
    pub fn detect(input: &str) -> bool {
        detect_delimiter(input).is_some()
    }

    /// Builds a full cell-equivalent view or a recoverable row selection.
    /// Malformed, ambiguous and unsupported tables stay unchanged. Row selection
    /// requires a reachable recovery method and a successful Stash write.
    #[must_use]
    pub fn compress_with_recovery(
        &self,
        input: &str,
        stash: Option<&dyn StashStore>,
        recovery: &RecoveryMethod,
    ) -> TabularOutcome {
        let mut outcome = TabularOutcome {
            output: input.to_owned(),
            operations: Vec::new(),
            recoverability: crate::Recoverability::Lossless,
            stash_writes: Vec::new(),
            metrics: TabularMetrics::default(),
        };
        let Some(table) = Table::parse(input) else {
            return outcome;
        };
        let count = table.records.len() - 1;
        outcome.metrics.original_rows = count;
        outcome.metrics.retained_rows = count;
        let full = table.render(0..count);
        if saves_both(&full, input) {
            outcome.output = full;
            outcome.operations.push(TabularOperation::Compaction);
        }
        let before = estimate_tokens(input);
        let after = estimate_tokens(&outcome.output);
        if (saves_both(&outcome.output, input)
            && (before - after) * 100 >= before * FULL_SAVINGS_PERCENT)
            || count <= TARGET_ROWS
        {
            return outcome;
        }
        // Rectangular text alone is not evidence of a header. Restrict omissions
        // to column labels; source calls and comma-separated prose keep all rows.
        let header = &table.records[0];
        if !header.iter().any(|cell| !cell.is_empty())
            || !header.iter().all(|cell| {
                cell.char_indices().all(|(index, c)| {
                    c.is_alphabetic()
                        || c == '_'
                        || (index > 0 && (c.is_numeric() || matches!(c, '-' | '.')))
                })
            })
        {
            return outcome;
        }
        let Some(store) = stash.filter(|_| recovery.is_available()) else {
            return outcome;
        };
        let selected = select_rows(&table.records[1..]);
        if selected.len() == count {
            return outcome;
        }
        let Some(ranges) = row_ranges(&selected) else {
            return outcome;
        };
        let mut reduced = table.render(selected.iter().copied());
        let hash = compute_key(input.as_bytes());
        let instruction = recovery_instruction(&hash, recovery);
        reduced.push_str(&format!(
            "\n\n[Table: kept {} of {count} data rows; selected data rows \
             (1-based, excluding header): {ranges}. Incomplete table; retrieve original \
             for complete enumeration or calculations. {instruction}]",
            selected.len()
        ));
        if !saves_both(&reduced, &outcome.output) || !saves_both(&reduced, input) {
            return outcome;
        }
        match store.stash(input) {
            Ok(write) => outcome.stash_writes.push(write),
            Err(_) => {
                outcome.metrics.stash_errors = 1;
                return outcome;
            }
        }
        outcome.output = reduced;
        outcome.operations = vec![TabularOperation::RowReduction];
        outcome.recoverability = crate::Recoverability::Retrievable;
        outcome.metrics.retained_rows = selected.len();
        outcome
    }
}

struct Table {
    delimiter: u8,
    records: Vec<StringRecord>,
}

impl Table {
    fn parse(input: &str) -> Option<Self> {
        let delimiter = detect_delimiter(input)?;
        scan_records(input.as_bytes(), delimiter, usize::MAX, true)?;
        let records = csv::ReaderBuilder::new()
            .delimiter(delimiter)
            .has_headers(false)
            .from_reader(input.as_bytes())
            .records()
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        Some(Self { delimiter, records })
    }

    fn render(&self, indices: impl IntoIterator<Item = usize>) -> String {
        let mut output = String::new();
        write_record(&mut output, &self.records[0], self.delimiter);
        for index in indices {
            output.push('\n');
            write_record(&mut output, &self.records[index + 1], self.delimiter);
        }
        output
    }
}

fn detect_delimiter(input: &str) -> Option<u8> {
    let bytes = input.as_bytes();
    let prefix = &bytes[..bytes.len().min(DETECTION_BYTES)];
    let complete = bytes.len() <= DETECTION_BYTES;
    let mut found = None;
    for delimiter in *b",\t" {
        if scan_records(prefix, delimiter, DETECTION_RECORDS, complete).is_some_and(|n| n >= 3) {
            if found.is_some() {
                return None;
            }
            found = Some(delimiter);
        }
    }
    found
}

#[derive(Clone, Copy)]
enum FieldState {
    Start,
    Unquoted,
    Quoted,
    Closed,
}

// The csv reader deliberately accepts malformed quotes. Validate this input
// boundary before canonicalizing, so a malformed field cannot acquire new meaning.
fn scan_records(input: &[u8], delimiter: u8, limit: usize, complete: bool) -> Option<usize> {
    let input = input.strip_prefix(b"\xef\xbb\xbf").unwrap_or(input);
    let mut state = FieldState::Start;
    let mut fields = 1;
    let mut width = None;
    let mut count = 0;
    let mut started = false;
    for byte in input.iter().copied().chain(complete.then_some(b'\n')) {
        if matches!(state, FieldState::Quoted) {
            if byte == b'"' {
                state = FieldState::Closed;
            }
            continue;
        }
        if matches!(state, FieldState::Closed) && byte == b'"' {
            state = FieldState::Quoted;
            continue;
        }
        if byte == delimiter {
            fields += 1;
            started = true;
            state = FieldState::Start;
        } else if matches!(byte, b'\r' | b'\n') {
            if started {
                if fields < 2 || width.is_some_and(|expected| expected != fields) {
                    return None;
                }
                width = Some(fields);
                count += 1;
                if count == limit {
                    return Some(count);
                }
            }
            fields = 1;
            started = false;
            state = FieldState::Start;
        } else {
            state = match (state, byte) {
                (FieldState::Start, b'"') => FieldState::Quoted,
                (FieldState::Closed, _) | (FieldState::Unquoted, b'"') => return None,
                _ => FieldState::Unquoted,
            };
            started = true;
        }
    }
    if complete && matches!(state, FieldState::Quoted) {
        None
    } else {
        Some(count)
    }
}

fn write_record(output: &mut String, record: &StringRecord, delimiter: u8) {
    for (index, cell) in record.iter().enumerate() {
        if index > 0 {
            output.push(char::from(delimiter));
        }
        // A leading U+FEFF is cell data here, not a document BOM.
        if (output.is_empty() && cell.starts_with('\u{feff}'))
            || cell
                .bytes()
                .any(|b| b == delimiter || matches!(b, b'"' | b'\r' | b'\n'))
        {
            output.push('"');
            output.push_str(&cell.replace('"', "\"\""));
            output.push('"');
        } else {
            output.push_str(cell);
        }
    }
}

fn saves_both(candidate: &str, original: &str) -> bool {
    candidate.chars().count() < original.chars().count()
        && estimate_tokens(candidate) < estimate_tokens(original)
}

fn select_rows(rows: &[StringRecord]) -> BTreeSet<usize> {
    let mut selected = BTreeSet::new();
    selected.extend(0..EDGE_ROWS);
    selected.extend(rows.len() - EDGE_ROWS..rows.len());
    for (index, row) in rows.iter().enumerate() {
        if row.iter().any(has_diagnostic) {
            selected.insert(index);
        }
    }
    let ordinary: Vec<_> = (0..rows.len()).filter(|i| !selected.contains(i)).collect();
    let slots = TARGET_ROWS
        .saturating_sub(selected.len())
        .min(ordinary.len());
    for slot in 0..slots {
        selected.insert(ordinary[(2 * slot + 1) * ordinary.len() / (2 * slots)]);
    }
    selected
}

fn has_diagnostic(cell: &str) -> bool {
    const ENGLISH: &[&str] = &[
        "error",
        "failed",
        "failure",
        "fatal",
        "panic",
        "exception",
        "warn",
        "warning",
    ];
    ["错误", "失败", "警告"]
        .iter()
        .any(|signal| cell.contains(signal))
        || cell
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .any(|word| {
                ENGLISH
                    .iter()
                    .any(|signal| word.eq_ignore_ascii_case(signal))
            })
}

fn row_ranges(indices: &BTreeSet<usize>) -> Option<String> {
    let mut ranges = String::new();
    let mut iter = indices.iter().copied().peekable();
    while let Some(start) = iter.next() {
        let mut end = start;
        while iter.peek() == Some(&(end + 1)) {
            end += 1;
            iter.next();
        }
        let range = if start == end {
            (start + 1).to_string()
        } else {
            format!("{}-{}", start + 1, end + 1)
        };
        if ranges.len() + usize::from(!ranges.is_empty()) + range.len() > MAX_ROW_RANGES_BYTES {
            return None;
        }
        if !ranges.is_empty() {
            ranges.push(',');
        }
        ranges.push_str(&range);
    }
    Some(ranges)
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("tests/tabular_tests.rs");
}
