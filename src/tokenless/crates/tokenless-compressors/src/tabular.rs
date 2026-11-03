//! Cell-preserving format compaction for complete CSV/TSV tables.

use csv::StringRecord;
use tokenless_ccr::{RecoveryMethod, StashStore, StashWrite};
use tokenless_protocol::estimate_tokens;

const DETECTION_BYTES: usize = 64 * 1024;
const DETECTION_RECORDS: usize = 10;

/// Current and historical transformations of CSV/TSV text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabularOperation {
    /// Quoting and record terminators changed without changing any cells.
    Compaction,
    /// Legacy row-selection operation; retained for compatibility and never emitted.
    RowReduction,
}

/// Counts for the selected table view; the header is not a data row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TabularMetrics {
    /// Data rows in the parsed source.
    pub original_rows: usize,
    /// Data rows in the selected view.
    pub retained_rows: usize,
    /// Legacy storage error count; always zero because compaction needs no storage.
    pub stash_errors: usize,
}

/// Complete-table candidate for Runtime arbitration.
#[derive(Debug)]
pub struct TabularOutcome {
    /// Complete CSV/TSV text with every original cell in source order.
    pub output: String,
    /// Operations in the selected candidate.
    pub operations: Vec<TabularOperation>,
    /// Always lossless at the cell level; source formatting may change.
    pub recoverability: crate::Recoverability,
    /// Legacy storage writes; always empty for cell-preserving compaction.
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

    /// Compacts formatting only when characters and estimated tokens both decrease.
    /// Preserves all cells and their order. Malformed, ambiguous and unsupported
    /// tables stay unchanged; no external storage or retrieval is required.
    #[must_use]
    pub fn compress(&self, input: &str) -> TabularOutcome {
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
        let full = table.render();
        if saves_both(&full, input) {
            outcome.output = full;
            outcome.operations.push(TabularOperation::Compaction);
        }
        outcome
    }

    /// Compatibility entry point for callers that supplied recovery capabilities.
    /// Storage and recovery arguments are ignored; delegates to full-table compaction.
    #[must_use]
    pub fn compress_with_recovery(
        &self,
        input: &str,
        _stash: Option<&dyn StashStore>,
        _recovery: &RecoveryMethod,
    ) -> TabularOutcome {
        self.compress(input)
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

    fn render(&self) -> String {
        let mut output = String::new();
        for (index, record) in self.records.iter().enumerate() {
            if index > 0 {
                output.push('\n');
            }
            write_record(&mut output, record, self.delimiter);
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

#[cfg(test)]
mod tests {
    use super::*;
    include!("tests/tabular_tests.rs");
}
