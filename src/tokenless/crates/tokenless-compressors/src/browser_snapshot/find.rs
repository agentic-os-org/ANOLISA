//! Playwright MCP `browser_find` parser: `### Result` header with one or
//! more unfenced aria subtrees separated by `----` lines.

use std::collections::BTreeSet;

use super::pw::{ParsedTreeBody, fold_advance, name_continues, parse_find_tree_body};
use super::{Parsed, is_diff_wrapper};

/// Parses a Playwright `browser_find` response, or returns `None` to fail open.
///
/// Anchors on a column-0 `### Result` heading, isolates parent `/url:` lookup
/// within each `----`-delimited match block, checks structural section assembly,
/// and requires unique element references before emitting records; encoding fidelity
/// is verified afterward by `encode::roundtrip_ok`.
pub(crate) fn parse(input: &str) -> Option<Parsed> {
    let mut from = None;
    let mut off = 0;
    for line in input.split_inclusive('\n') {
        if line == "### Result\n" {
            from = Some(off + line.len());
            break;
        }
        off += line.len();
    }
    let body_start = from?;
    let mut aria_start = body_start;
    let after_result = &input[body_start..];
    let summary_match = after_result
        .strip_prefix("Found ")
        .and_then(|_| after_result.find(":\n"))
        .filter(|&colon_pos| after_result[..colon_pos].contains(" match"));
    if let Some(colon_pos) = summary_match {
        let mut next = colon_pos + 2;
        while after_result[next..].starts_with('\n') {
            next += 1;
        }
        aria_start = body_start + next;
    }
    let prefix = input[..aria_start].to_owned();
    let first_non_empty = prefix
        .trim_start_matches('\u{feff}')
        .split('\n')
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    if is_diff_wrapper(&prefix) || !first_non_empty.starts_with("### ") {
        return None;
    }

    let section_end = find_section_end(input, aria_start);
    let raw_section = &input[aria_start..section_end];
    if raw_section.contains('\r')
        || raw_section.contains("```yaml")
        || raw_section.contains("```yml")
    {
        return None;
    }

    // Preserve trailing newlines (and any following ### Section) in suffix so
    // `table` renders cleanly without trailing line breaks, matching pw::parse.
    let body = raw_section.trim_end_matches('\n');
    let content_end = aria_start + body.len();
    let suffix = input[content_end..].to_owned();
    if body.is_empty() {
        return None;
    }

    let raw_blocks = split_blocks(body);
    let block_count = raw_blocks.len();
    let mut rebuilt_blocks = Vec::with_capacity(block_count);
    let mut records = Vec::new();

    for (block_idx, raw_block) in raw_blocks.iter().enumerate() {
        let ParsedTreeBody {
            items,
            records: mut block_records,
        } = parse_find_tree_body(raw_block)?;
        if block_records.is_empty() {
            return None;
        }
        for r in &mut block_records {
            r.block = block_idx;
        }
        records.extend(block_records);
        rebuilt_blocks.push(
            items
                .iter()
                .map(|(_, raw)| raw.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }

    let unique_refs: BTreeSet<&str> = records
        .iter()
        .map(|r| r.reference.as_str())
        .filter(|r| !r.is_empty())
        .collect();
    if unique_refs.len() < 2 {
        return None;
    }

    let rebuilt = format!("{prefix}{}{suffix}", rebuilt_blocks.join("\n----\n"));
    if rebuilt != input {
        return None;
    }

    Some(Parsed {
        prefix,
        suffix,
        records,
        use_block_column: block_count > 1,
    })
}

/// Stops at the next column-0 markdown `### ` heading outside an open quoted
/// element name so trailing sections (e.g. `### Events`) stay in `suffix`.
fn find_section_end(input: &str, body_start: usize) -> usize {
    let mut off = body_start;
    let mut folded = String::new();
    for line in input[body_start..].split_inclusive('\n') {
        let stripped = line.strip_suffix('\n').unwrap_or(line);
        if !fold_advance(&mut folded, stripped) {
            if stripped.starts_with("### ") {
                return off;
            }
            if name_continues(stripped) {
                folded = stripped.to_owned();
            }
        }
        off += line.len();
    }
    input.len()
}

/// Splits `body` on unquoted column-0 `----` block-separator lines while
/// keeping `----` inside folded multiline quoted element names intact.
fn split_blocks(body: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut block_start = 0;
    let mut off: usize = 0;
    let mut folded = String::new();

    for raw in body.split_inclusive('\n') {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        if !fold_advance(&mut folded, line) {
            if line == "----" {
                let end = off.saturating_sub(1).max(block_start);
                blocks.push(&body[block_start..end]);
                block_start = off + raw.len();
                off += raw.len();
                continue;
            }
            if name_continues(line) {
                folded = line.to_owned();
            }
        }
        off += raw.len();
    }
    blocks.push(&body[block_start..]);
    blocks
}
