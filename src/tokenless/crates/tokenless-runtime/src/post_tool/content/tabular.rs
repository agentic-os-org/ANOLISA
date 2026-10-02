use super::{non_empty_lines, scan_prefix};
use tokenless_compressors::TabularCompressor;

/// Retains Markdown detection and delegates bounded CSV/TSV record validation.
pub(super) fn is_tabular(scan: &str) -> bool {
    let lines: Vec<&str> = non_empty_lines(scan_prefix(scan)).take(10).collect();
    let markdown = lines.len() >= 3
        && lines[0].starts_with('|')
        && lines[0].ends_with('|')
        && lines[1].chars().all(|c| matches!(c, '|' | '-' | ':' | ' '));
    if markdown {
        return true;
    }
    TabularCompressor::detect(scan)
}
