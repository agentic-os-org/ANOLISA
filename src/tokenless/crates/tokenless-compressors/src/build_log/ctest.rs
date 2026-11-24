//! Lossless structural compaction for CTest output.
//!
//! Compact contiguous non-failing CTest test-progress blocks (both serial
//! `Start`/`Result` pairs and parallel interleaved `-j` streams) while
//! preserving every test number, name, execution order, status suffix, elapsed
//! time, failure diagnostic, assertion detail, summary footer, non-SGR escape,
//! and unknown line verbatim without writing to Stash.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;

use serde_json::json;

use super::scope::{self, MAX_BYTES, MAX_EDITS, MAX_ROWS, complete, plain, valid_line};
use super::{LayoutEdit, saves_both};

// MAX_BYTES caps compacted headers because `saves_both` keeps each replacement shorter than its slice.
pub(super) const HEADER: &str =
    "CTest templates (kind<TAB>fields; SP/SK/SD=start+result, P/K/D=result, s/S=start, B=blank): ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Passed,
    Skipped,
    Disabled,
}

impl Status {
    fn code(self) -> &'static str {
        match self {
            Self::Passed => "P",
            Self::Skipped => "K",
            Self::Disabled => "D",
        }
    }

    fn start_pair_code(self) -> &'static str {
        match self {
            Self::Passed => "SP",
            Self::Skipped => "SK",
            Self::Disabled => "SD",
        }
    }

    fn expected_sec_width(self) -> usize {
        match self {
            Self::Passed => 8,
            Self::Skipped | Self::Disabled => 7,
        }
    }
}

struct StartRecord<'a> {
    lead_spaces: usize,
    id_width: usize,
    id: &'a str,
    name: &'a str,
}

struct ResultRecord<'a> {
    idx_width: usize,
    idx: &'a str,
    total: &'a str,
    id_width: usize,
    id: &'a str,
    name: &'a str,
    dots_len: usize,
    prefix_col: usize,
    status: Status,
    sec_pad: usize,
    sec: &'a str,
}

enum ParsedRow<'a> {
    Blank,
    Start(StartRecord<'a>),
    Result(ResultRecord<'a>),
}

/// Detects a CTest output stream from a bounded head-and-tail sample.
pub(super) fn detect(sample: &[String]) -> bool {
    scope::detect_sampled(
        sample,
        |head| scope::skip_leading_preambles(head, is_static_preamble),
        is_log_head,
        has_strong_evidence,
    )
}

/// Plans atomic lossless template edits across contiguous non-failing CTest blocks.
pub(super) fn plan(lines: &[&str]) -> Vec<LayoutEdit> {
    if is_compacted(lines) || lines.iter().any(|line| line.len() > MAX_BYTES) {
        return Vec::new();
    }

    let mut edits = Vec::new();
    let mut active = true;
    let mut open_fence = None;
    let mut index = scope::leading_preamble_len(lines, is_static_preamble);

    while index < lines.len() {
        let Some(line) = scope::enter_line(
            lines,
            index,
            &mut active,
            &mut open_fence,
            is_compactable_row,
        ) else {
            index += 1;
            continue;
        };

        if !is_compactable_row(line) {
            if !is_in_scope_lifecycle(line) {
                active = false;
            }
            index += 1;
            continue;
        }

        let region_start = index;
        let mut region_end = index;
        while region_end < lines.len() {
            let Some(next) = complete(lines[region_end]) else {
                break;
            };
            if !plain(next) {
                break;
            }
            if !is_compactable_row(next) && !next.is_empty() {
                break;
            }
            region_end += 1;
        }

        for (span_start, span_end) in paired_spans(lines, region_start, region_end) {
            let mut chunk_start = span_start;
            while chunk_start < span_end {
                while chunk_start < span_end && lines[chunk_start] == "\n" {
                    chunk_start += 1;
                }
                if chunk_start >= span_end {
                    break;
                }

                let mut scan_end = chunk_start;
                let mut bytes = 0usize;
                while scan_end < span_end && scan_end - chunk_start < MAX_ROWS {
                    let line_len = lines[scan_end].len();
                    if bytes + line_len > MAX_BYTES {
                        break;
                    }
                    bytes += line_len;
                    scan_end += 1;
                }

                let mut chunk_end = scan_end;
                while chunk_end > chunk_start && lines[chunk_end - 1] == "\n" {
                    chunk_end -= 1;
                }

                // When splitting inside a paired span, carry cut-boundary `Start` rows into
                // the next chunk so they can still collapse serially with their results.
                let mut trimmed_tail_start = false;
                if scan_end < span_end {
                    loop {
                        while chunk_end > chunk_start && lines[chunk_end - 1] == "\n" {
                            chunk_end -= 1;
                        }
                        if chunk_end > chunk_start
                            && complete(lines[chunk_end - 1])
                                .is_some_and(|tail| parse_start(tail).is_some())
                        {
                            chunk_end -= 1;
                            trimmed_tail_start = true;
                        } else {
                            break;
                        }
                    }
                }

                if chunk_end > chunk_start {
                    let result_count = lines[chunk_start..chunk_end]
                        .iter()
                        .filter(|raw| {
                            raw.strip_suffix('\n')
                                .and_then(parse_compactable_result)
                                .is_some()
                        })
                        .count();
                    if result_count >= 2 {
                        let original = lines[chunk_start..chunk_end].concat();
                        let replacement = render(&lines[chunk_start..chunk_end]);
                        if saves_both(&original, &replacement) {
                            edits.push(LayoutEdit {
                                start: chunk_start,
                                end: chunk_end,
                                replacement,
                            });
                            if edits.len() == MAX_EDITS {
                                return edits;
                            }
                        }
                    }
                }

                chunk_start = if trimmed_tail_start && chunk_end > chunk_start {
                    chunk_end
                } else {
                    scan_end
                }
                .max(chunk_start + 1);
            }
        }
        index = region_end.max(region_start + 1);
    }

    edits
}

// Pair `Start` and compactable `Result` lines across the full contiguous region so
// `MAX_ROWS` / `MAX_BYTES` splits never misclassify passing in-flight parallel starts.
fn paired_spans(lines: &[&str], region_start: usize, region_end: usize) -> Vec<(usize, usize)> {
    let mut pending: HashMap<(&str, &str), VecDeque<usize>> = HashMap::new();
    for (idx, raw) in lines.iter().enumerate().take(region_end).skip(region_start) {
        let Some(body) = raw.strip_suffix('\n') else {
            continue;
        };
        if let Some(start_rec) = parse_start(body) {
            pending
                .entry((start_rec.id, start_rec.name))
                .or_default()
                .push_back(idx);
        } else if let Some(res_rec) = parse_compactable_result(body)
            && let Some(queue) = pending.get_mut(&(res_rec.id, res_rec.name))
        {
            queue.pop_front();
        }
    }

    let mut unpaired = pending
        .into_values()
        .flat_map(VecDeque::into_iter)
        .collect::<Vec<_>>();
    unpaired.sort_unstable();

    let mut spans = Vec::new();
    let mut cursor = region_start;
    for split_idx in unpaired {
        if split_idx > cursor {
            push_trimmed_span(lines, cursor, split_idx, &mut spans);
        }
        cursor = split_idx + 1;
    }
    if cursor < region_end {
        push_trimmed_span(lines, cursor, region_end, &mut spans);
    }
    spans
}

fn push_trimmed_span(
    lines: &[&str],
    mut start: usize,
    mut end: usize,
    out: &mut Vec<(usize, usize)>,
) {
    while start < end && lines[start] == "\n" {
        start += 1;
    }
    while end > start && lines[end - 1] == "\n" {
        end -= 1;
    }
    if start < end {
        out.push((start, end));
    }
}

/// Prevents CTest `Start` and result lines from entering standalone Generic progress rules.
pub(super) fn is_ctest_line(line: &str) -> bool {
    parse_start(line).is_some() || is_any_result_prefix(line)
}

/// Keeps compacted CTest outputs terminal across repeated passes.
pub(super) fn is_compacted(lines: &[&str]) -> bool {
    scope::is_compacted(lines, HEADER)
}

fn is_ctest_digits(value: &str) -> bool {
    (1..=9).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit())
}

fn is_valid_sec(value: &str) -> bool {
    let Some((whole, frac)) = value.split_once('.') else {
        return false;
    };
    !whole.is_empty()
        && !frac.is_empty()
        && whole.bytes().all(|b| b.is_ascii_digit())
        && frac.bytes().all(|b| b.is_ascii_digit())
}

fn parse_start(line: &str) -> Option<StartRecord<'_>> {
    if !valid_line(line) {
        return None;
    }
    let lead_spaces = line.bytes().take_while(|&b| b == b' ').count();
    let rest = line[lead_spaces..].strip_prefix("Start ")?;
    let id_pad = rest.bytes().take_while(|&b| b == b' ').count();
    let (id, name) = rest[id_pad..].split_once(": ")?;
    (is_ctest_digits(id) && !name.is_empty()).then_some(StartRecord {
        lead_spaces,
        id_width: id_pad + id.len(),
        id,
        name,
    })
}

fn parse_compactable_result(line: &str) -> Option<ResultRecord<'_>> {
    if !valid_line(line) {
        return None;
    }
    let idx_pad = line.bytes().take_while(|&b| b == b' ').count();
    let (progress, tail) = line[idx_pad..].split_once(" Test ")?;
    let (idx, total) = progress.split_once('/')?;
    if !is_ctest_digits(idx) || !is_ctest_digits(total) {
        return None;
    }
    let id_pad = tail.bytes().take_while(|&b| b == b' ').count();
    let (id, body) = tail[id_pad..].strip_prefix('#')?.split_once(": ")?;
    if !is_ctest_digits(id) {
        return None;
    }

    let before_sec_suffix = body.strip_suffix(" sec")?;
    let (before_sec_pad, sec) = before_sec_suffix.rsplit_once(' ')?;
    if !is_valid_sec(sec) {
        return None;
    }
    let extra_spaces = before_sec_pad
        .bytes()
        .rev()
        .take_while(|&b| b == b' ')
        .count();
    let sec_pad = 1 + extra_spaces;
    let before_status = &before_sec_pad[..before_sec_pad.len() - extra_spaces];

    let (status, before_dots_and_status) =
        if let Some(prefix) = before_status.strip_suffix("   Passed") {
            (Status::Passed, prefix)
        } else if let Some(prefix) = before_status.strip_suffix("***Skipped") {
            (Status::Skipped, prefix)
        } else {
            let prefix = before_status.strip_suffix("***Not Run (Disabled)")?;
            (Status::Disabled, prefix)
        };

    let dots_len = before_dots_and_status
        .bytes()
        .rev()
        .take_while(|&b| b == b'.')
        .count();
    if dots_len < 3 {
        return None;
    }
    let name =
        before_dots_and_status[..before_dots_and_status.len() - dots_len].strip_suffix(' ')?;
    if name.is_empty() {
        return None;
    }

    let idx_width = idx_pad + idx.len();
    let id_width = id_pad + id.len();
    // Account for UTF-8 scalar widths so prefix column alignment stays exact across names.
    let prefix_col =
        idx_width + 1 + total.len() + 6 + id_width + 3 + name.chars().count() + 1 + dots_len;

    Some(ResultRecord {
        idx_width,
        idx,
        total,
        id_width,
        id,
        name,
        dots_len,
        prefix_col,
        status,
        sec_pad,
        sec,
    })
}

fn is_compactable_row(line: &str) -> bool {
    parse_start(line).is_some() || parse_compactable_result(line).is_some()
}

fn is_any_result_prefix(line: &str) -> bool {
    if !valid_line(line) {
        return false;
    }
    let trimmed = line.trim_start_matches(' ');
    let rest = if let Some((progress, tail)) = trimmed.split_once(" Test ") {
        let Some((idx, total)) = progress.split_once('/') else {
            return false;
        };
        if !is_ctest_digits(idx) || !is_ctest_digits(total) {
            return false;
        }
        // Indexed `<idx>/<total> Test #<id>: <body>` prefixes are specific enough without suffix checks.
        return extract_test_id_body(tail).is_some();
    } else if let Some(tail) = trimmed.strip_prefix("Test ") {
        tail
    } else {
        return false;
    };
    let Some(body) = extract_test_id_body(rest) else {
        return false;
    };
    body.contains("...") || body.contains("***") || body.ends_with(" sec")
}

fn extract_test_id_body(rest: &str) -> Option<&str> {
    let rest = rest.trim_start_matches(' ');
    let after_hash = rest.strip_prefix('#')?;
    let (id, body) = after_hash.split_once(": ")?;
    (is_ctest_digits(id) && !body.is_empty()).then_some(body)
}

fn is_complete_result_evidence(line: &str) -> bool {
    if parse_compactable_result(line).is_some() {
        return true;
    }
    if !is_any_result_prefix(line) {
        return false;
    }
    if line.contains("***") {
        return true;
    }
    line.strip_suffix(" sec")
        .and_then(|head| head.rsplit_once(' '))
        .is_some_and(|(left, sec)| is_valid_sec(sec) && left.contains(".."))
}

fn is_test_project(line: &str) -> bool {
    valid_line(line)
        && line
            .strip_prefix("Test project ")
            .is_some_and(|path| !path.trim().is_empty())
}

fn is_ctest_summary(line: &str) -> bool {
    if !valid_line(line) {
        return false;
    }
    let trimmed = line.trim();
    let Some((pct, rest)) = trimmed.split_once("% tests passed, ") else {
        return false;
    };
    let Some((failed, total)) = rest.split_once(" tests failed out of ") else {
        return false;
    };
    is_ctest_digits(pct) && is_ctest_digits(failed) && is_ctest_digits(total)
}

fn is_no_tests(line: &str) -> bool {
    valid_line(line) && line.trim() == "No tests were found!!!"
}

fn is_log_head(line: &str) -> bool {
    scope::is_compacted_header(line, HEADER)
        || is_test_project(line)
        || parse_start(line).is_some()
        || is_complete_result_evidence(line)
}

fn is_in_scope_lifecycle(line: &str) -> bool {
    line.is_empty()
        || is_test_project(line)
        || line.starts_with("Internal ctest changing into directory: ")
}

fn is_static_preamble(line: &str) -> bool {
    if !valid_line(line) {
        return false;
    }
    line.starts_with("$ ")
        || scope::is_exit_anchor(line)
        || scope::is_xtrace(line, &["ctest", "ctest.exe"])
        || line.starts_with("Internal ctest changing into directory: ")
        || (line.starts_with("Cannot find file: ") && line.ends_with("/DartConfiguration.tcl"))
        || line.starts_with("UpdateCTestConfiguration  from :")
        || line.starts_with("Parse Config file:")
}

fn has_strong_evidence(run: &[String]) -> bool {
    let compacted = run
        .iter()
        .any(|line| scope::is_compacted_header(line, HEADER));
    let result_count = run
        .iter()
        .filter(|line| is_complete_result_evidence(line))
        .count();
    let project_with_outcome = run.iter().any(|line| is_test_project(line))
        && run.iter().any(|line| {
            is_complete_result_evidence(line) || is_ctest_summary(line) || is_no_tests(line)
        });
    compacted || result_count >= 2 || project_with_outcome
}

fn mode<T: Copy + Default + Eq + Hash + Ord>(values: impl Iterator<Item = T>) -> T {
    let mut counts: HashMap<T, (usize, usize)> = HashMap::new();
    for (order, val) in values.enumerate() {
        let entry = counts.entry(val).or_insert((0, order));
        entry.0 += 1;
    }
    counts
        .into_iter()
        .max_by(|(val_a, (cnt_a, ord_a)), (val_b, (cnt_b, ord_b))| {
            cnt_a
                .cmp(cnt_b)
                .then_with(|| ord_b.cmp(ord_a))
                .then_with(|| val_a.cmp(val_b))
        })
        .map(|(val, _)| val)
        .unwrap_or_default()
}

fn name_prefix_candidate(name: &str) -> Option<&str> {
    let slash = name.rfind('/').map(|pos| pos + 1).unwrap_or(0);
    let colon = name.rfind("::").map(|pos| pos + 2).unwrap_or(0);
    let end = slash.max(colon);
    (end >= 4).then(|| &name[..end])
}

fn encode_name(name: &str, prefixes: &[String]) -> String {
    if let Some((pid, prefix)) = prefixes
        .iter()
        .enumerate()
        .filter(|(_, prefix)| name.starts_with(prefix.as_str()))
        .max_by_key(|(_, prefix)| prefix.len())
    {
        return format!("@{pid}:{}", &name[prefix.len()..]);
    }
    if name.starts_with('@') {
        return format!("@{name}");
    }
    name.to_owned()
}

fn sec_token(sec: &str, sec_pad: usize, status: Status) -> String {
    let expected_pad = status.expected_sec_width().saturating_sub(sec.len()).max(1);
    if sec_pad == expected_pad {
        sec.to_owned()
    } else {
        format!("{}{sec}", " ".repeat(sec_pad))
    }
}

fn is_serial_pair(
    start_rec: &StartRecord<'_>,
    result_rec: &ResultRecord<'_>,
    start_indent: usize,
    idx_width: usize,
    id_width: usize,
    total: &str,
) -> bool {
    start_rec.lead_spaces == start_indent
        && start_rec.id_width == id_width
        && result_rec.idx_width == idx_width
        && result_rec.total == total
        && result_rec.id_width == id_width
        && start_rec.id == result_rec.id
        && start_rec.name == result_rec.name
}

fn render(chunk: &[&str]) -> String {
    let parsed = chunk
        .iter()
        .filter_map(|raw| {
            let line = raw.strip_suffix('\n').unwrap_or(raw);
            if line.is_empty() {
                Some(ParsedRow::Blank)
            } else if let Some(s) = parse_start(line) {
                Some(ParsedRow::Start(s))
            } else {
                parse_compactable_result(line).map(ParsedRow::Result)
            }
        })
        .collect::<Vec<_>>();

    let total = mode(parsed.iter().filter_map(|row| match row {
        ParsedRow::Result(r) => Some(r.total),
        _ => None,
    }));
    let idx_width = mode(parsed.iter().filter_map(|row| match row {
        ParsedRow::Result(r) => Some(r.idx_width),
        _ => None,
    }));
    let id_width = mode(parsed.iter().filter_map(|row| match row {
        ParsedRow::Result(r) => Some(r.id_width),
        _ => None,
    }));
    let dom_col = mode(parsed.iter().filter_map(|row| match row {
        ParsedRow::Result(r) => Some(r.prefix_col),
        _ => None,
    }));
    let has_starts = parsed.iter().any(|row| matches!(row, ParsedRow::Start(_)));
    let start_indent = if has_starts {
        mode(parsed.iter().filter_map(|row| match row {
            ParsedRow::Start(s) => Some(s.lead_spaces),
            _ => None,
        }))
    } else {
        idx_width * 2 + 2
    };

    let mut names_by_tid: HashMap<&str, HashSet<&str>> = HashMap::new();
    for row in &parsed {
        if let ParsedRow::Result(r) = row {
            names_by_tid.entry(r.id).or_default().insert(r.name);
        }
    }
    let unique_tid_name: HashMap<&str, &str> = names_by_tid
        .into_iter()
        .filter_map(|(tid, names)| {
            (names.len() == 1)
                .then(|| names.into_iter().next().map(|name| (tid, name)))
                .flatten()
        })
        .collect();

    let mut emitted_names = Vec::new();
    let mut scan = 0usize;
    while scan < parsed.len() {
        match (&parsed[scan], parsed.get(scan + 1)) {
            (ParsedRow::Start(s), Some(ParsedRow::Result(r)))
                if is_serial_pair(s, r, start_indent, idx_width, id_width, total) =>
            {
                emitted_names.push(r.name);
                scan += 2;
            }
            (ParsedRow::Start(s), _) => {
                if !(s.lead_spaces == start_indent
                    && s.id_width == id_width
                    && unique_tid_name.get(s.id).copied() == Some(s.name))
                {
                    emitted_names.push(s.name);
                }
                scan += 1;
            }
            (ParsedRow::Result(r), _) => {
                emitted_names.push(r.name);
                scan += 1;
            }
            (ParsedRow::Blank, _) => scan += 1,
        }
    }

    let mut prefix_counts: HashMap<&str, usize> = HashMap::new();
    for name in emitted_names {
        if let Some(prefix) = name_prefix_candidate(name) {
            *prefix_counts.entry(prefix).or_insert(0) += 1;
        }
    }
    let mut prefix_entries = prefix_counts
        .into_iter()
        .filter(|&(_, count)| count >= 2)
        .collect::<Vec<_>>();
    prefix_entries.sort_by(|(pref_a, cnt_a), (pref_b, cnt_b)| {
        cnt_b.cmp(cnt_a).then_with(|| pref_a.cmp(pref_b))
    });
    let prefixes = prefix_entries
        .into_iter()
        .map(|(pref, _)| pref.to_owned())
        .collect::<Vec<_>>();

    let mut rows = Vec::with_capacity(parsed.len());
    let mut cursor = 0usize;
    while cursor < parsed.len() {
        match (&parsed[cursor], parsed.get(cursor + 1)) {
            (ParsedRow::Start(s), Some(ParsedRow::Result(r)))
                if is_serial_pair(s, r, start_indent, idx_width, id_width, total) =>
            {
                rows.push(format_standard_result(
                    r.status.start_pair_code(),
                    r,
                    dom_col,
                    &prefixes,
                ));
                cursor += 2;
            }
            (ParsedRow::Start(s), _) => {
                if s.lead_spaces == start_indent
                    && s.id_width == id_width
                    && unique_tid_name.get(s.id).copied() == Some(s.name)
                {
                    rows.push(format!("s\t{}", s.id));
                } else {
                    rows.push(format!(
                        "S\t{}\t{}\t{}\t{}",
                        s.lead_spaces,
                        s.id_width,
                        s.id,
                        encode_name(s.name, &prefixes)
                    ));
                }
                cursor += 1;
            }
            (ParsedRow::Result(r), _) => {
                if r.idx_width == idx_width && r.total == total && r.id_width == id_width {
                    rows.push(format_standard_result(
                        r.status.code(),
                        r,
                        dom_col,
                        &prefixes,
                    ));
                } else {
                    rows.push(format!(
                        "R\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                        r.idx_width,
                        r.idx,
                        r.total,
                        r.id_width,
                        r.id,
                        encode_name(r.name, &prefixes),
                        r.dots_len,
                        r.status.code(),
                        r.sec_pad,
                        r.sec
                    ));
                }
                cursor += 1;
            }
            (ParsedRow::Blank, _) => {
                rows.push("B".to_owned());
                cursor += 1;
            }
        }
    }

    let header = json!({
        "rows": rows.len(),
        "total": total,
        "start_indent": start_indent,
        "idx_width": idx_width,
        "id_width": id_width,
        "col": dom_col,
        "prefixes": prefixes,
    });
    let mut output = format!("{HEADER}{header}\n");
    for row in rows {
        output.push_str(&row);
        output.push('\n');
    }
    output
}

fn format_standard_result(
    kind: &str,
    r: &ResultRecord<'_>,
    dom_col: usize,
    prefixes: &[String],
) -> String {
    let id_tok = if r.idx == r.id { "" } else { r.id };
    let base_len = r.prefix_col - r.dots_len;
    let expected_dots = dom_col.saturating_sub(base_len).max(3);
    let sec_tok = sec_token(r.sec, r.sec_pad, r.status);
    let name_tok = encode_name(r.name, prefixes);
    if r.dots_len == expected_dots {
        format!("{kind}\t{}\t{id_tok}\t{name_tok}\t{sec_tok}", r.idx)
    } else {
        format!(
            "{kind}\t{}\t{id_tok}\t{name_tok}\t{sec_tok}\t{}",
            r.idx, r.dots_len
        )
    }
}

#[cfg(test)]
mod tests;
