//! Single-pass state machine, system isolation, and block-level target pairing for `build_steps`.

use super::super::scope::{self, MAX_BYTES, MAX_EDITS, MAX_ROWS, complete, plain};
use super::super::{LayoutEdit, saves_both};
use super::grammar::{self, CompileRow, CompileTargetIndex, Counter, LineKind, ProgressAction};
use super::render::{self, CompileBlockItem};

const MAX_REOPEN_BLANKS: usize = 1;
const MAX_WINDOW_ROWS: usize = MAX_ROWS * 2;
const MAX_WINDOW_BYTES: usize = MAX_BYTES * 2;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SpanRole {
    Blank,
    Include,
    Break,
}

struct Span {
    start: usize,
    end: usize,
}

/// Plans atomic lossless template edits across contiguous CMake/Ninja and C/C++ compile blocks.
pub(super) fn plan(lines: &[&str]) -> Vec<LayoutEdit> {
    if super::is_compacted(lines) {
        return Vec::new();
    }

    let mut edits = Vec::new();
    let mut active = true;
    let mut open_fence = None;
    let mut index = scope::leading_preamble_len(lines, grammar::is_static_preamble);

    while index < lines.len() {
        let Some(line) = enter_line(lines, index, &mut active, &mut open_fence) else {
            index += 1;
            continue;
        };

        let Some(kind) = grammar::parse_line(line) else {
            if !grammar::is_in_scope_lifecycle(line) {
                active = false;
            }
            index += 1;
            continue;
        };

        match kind {
            LineKind::Blank => {
                index += 1;
            }
            LineKind::Compile(compile) if compile.has_ninja_step => {
                index = plan_ninja_compile(lines, index, &mut edits);
            }
            LineKind::Progress(_) | LineKind::Compile(_) | LineKind::MakeTag(_) => {
                index = plan_build_run(lines, index, &mut edits);
            }
        }

        if edits.len() >= MAX_EDITS {
            return edits;
        }
    }

    edits
}

/// Validates and enters a scoped line, closing on fences/controls and allowing up to one
/// intra-block blank line when reopening with a verified two-row system anchor.
fn enter_line<'a>(
    lines: &[&'a str],
    index: usize,
    active: &mut bool,
    open_fence: &mut Option<(u8, usize)>,
) -> Option<&'a str> {
    let raw = lines[index];
    let raw_body = raw.strip_suffix('\n').unwrap_or(raw);
    if scope::step_fence(open_fence, raw_body) {
        *active = false;
        return None;
    }
    let Some(line) = complete(raw) else {
        *active = false;
        return None;
    };
    if !plain(line) {
        *active = false;
        return None;
    }
    if !*active {
        if can_reopen(lines, index, line) {
            *active = true;
        } else {
            return None;
        }
    }
    Some(line)
}

fn can_reopen(lines: &[&str], index: usize, first_line: &str) -> bool {
    let Some(first) = grammar::parse_line(first_line) else {
        return false;
    };
    if matches!(first, LineKind::Blank) {
        return false;
    }

    let mut next = index + 1;
    let mut blanks = 0usize;
    while next < lines.len() && blanks < MAX_REOPEN_BLANKS && lines[next] == "\n" {
        next += 1;
        blanks += 1;
    }
    let Some(second_line) = lines.get(next).and_then(|raw| complete(raw)) else {
        return false;
    };
    let Some(second) = grammar::parse_line(second_line) else {
        return false;
    };
    can_reopen_pair(&first, &second)
}

fn is_non_ninja_make_item(kind: &LineKind<'_>) -> bool {
    match kind {
        LineKind::Compile(compile) => !compile.has_ninja_step,
        LineKind::MakeTag(_) => true,
        LineKind::Progress(progress) => {
            matches!(progress.counter, Counter::Percent(_))
                && progress.action == ProgressAction::BuildObject
        }
        LineKind::Blank => false,
    }
}

fn is_verbose_non_ninja_compile(kind: &LineKind<'_>) -> bool {
    matches!(kind, LineKind::Compile(compile) if !compile.has_ninja_step && compile.is_verbose)
}

fn has_non_ninja_compile(kind: &LineKind<'_>) -> bool {
    matches!(kind, LineKind::Compile(compile) if !compile.has_ninja_step)
}

fn can_reopen_pair(first: &LineKind<'_>, second: &LineKind<'_>) -> bool {
    match (first, second) {
        (LineKind::Progress(_), LineKind::Progress(_)) => true,
        (LineKind::Compile(a), LineKind::Compile(b)) if a.has_ninja_step && b.has_ninja_step => {
            true
        }
        _ if is_non_ninja_make_item(first) && is_non_ninja_make_item(second) => {
            if is_verbose_non_ninja_compile(first) || is_verbose_non_ninja_compile(second) {
                return true;
            }
            let has_make_tag =
                matches!(first, LineKind::MakeTag(_)) || matches!(second, LineKind::MakeTag(_));
            let has_compile = has_non_ninja_compile(first) || has_non_ninja_compile(second);
            has_make_tag
                && (has_compile
                    || matches!(
                        (first, second),
                        (LineKind::MakeTag(_), LineKind::MakeTag(_))
                    ))
                || (has_compile
                    && (matches!(first, LineKind::Progress(_))
                        || matches!(second, LineKind::Progress(_))))
        }
        _ => false,
    }
}

fn plan_ninja_compile(lines: &[&str], start: usize, edits: &mut Vec<LayoutEdit>) -> usize {
    let mut scan_end = start;
    let mut bytes = 0usize;
    let mut compile_rows = 0usize;
    let mut chunk: Vec<Option<CompileRow<'_>>> = Vec::new();

    while scan_end < lines.len() && scan_end - start < MAX_ROWS {
        let Some(next) = complete(lines[scan_end]) else {
            break;
        };
        if !plain(next) || scope::is_fence_line(next) {
            break;
        }
        let entry = match grammar::parse_line(next) {
            Some(LineKind::Blank) => None,
            Some(LineKind::Compile(row)) if row.has_ninja_step => Some(row),
            _ => break,
        };
        let line_len = lines[scan_end].len();
        if bytes + line_len > MAX_BYTES {
            break;
        }
        bytes += line_len;
        if entry.is_some() {
            compile_rows += 1;
        }
        chunk.push(entry);
        scan_end += 1;
    }

    let mut end = scan_end;
    while end > start && chunk.last().is_some_and(Option::is_none) {
        chunk.pop();
        end -= 1;
    }

    if compile_rows >= 2 && end > start {
        let items = chunk
            .iter()
            .map(|entry| match entry {
                Some(row) => CompileBlockItem::Compile(row),
                None => CompileBlockItem::Blank,
            })
            .collect::<Vec<_>>();
        let original = lines[start..end].concat();
        let replacement = render::render_compile(&items);
        if saves_both(&original, &replacement) {
            edits.push(LayoutEdit {
                start,
                end,
                replacement,
            });
        }
    }

    end.max(start + 1)
}

fn is_percent_build_progress(kind: &LineKind<'_>) -> bool {
    matches!(
        kind,
        LineKind::Progress(p)
            if matches!(p.counter, Counter::Percent(_))
                && p.action == ProgressAction::BuildObject
    )
}

fn is_compile_region_barrier(kind: &LineKind<'_>) -> bool {
    matches!(kind, LineKind::Progress(_)) && !is_percent_build_progress(kind)
}

fn plan_build_run(lines: &[&str], run_start: usize, edits: &mut Vec<LayoutEdit>) -> usize {
    let remaining = MAX_EDITS.saturating_sub(edits.len());
    if remaining == 0 {
        return run_start + 1;
    }

    let mut run_end = run_start;
    let mut window_bytes = 0usize;
    let mut window_cut_early = false;
    let mut parsed_run = Vec::new();
    while run_end < lines.len() {
        let Some(next) = complete(lines[run_end]) else {
            break;
        };
        if !plain(next) || scope::is_fence_line(next) {
            break;
        }
        let Some(kind) = grammar::parse_line(next) else {
            break;
        };
        match &kind {
            LineKind::Blank | LineKind::MakeTag(_) | LineKind::Progress(_) => {}
            LineKind::Compile(compile) if !compile.has_ninja_step => {}
            _ => break,
        }
        let line_len = lines[run_end].len();
        if !parsed_run.is_empty()
            && (parsed_run.len() >= MAX_WINDOW_ROWS || window_bytes + line_len > MAX_WINDOW_BYTES)
        {
            window_cut_early = true;
            break;
        }
        window_bytes += line_len;
        parsed_run.push(kind);
        run_end += 1;
    }

    // Phase 1: plan compile edits within regions bounded by non-build progress barriers
    // (`ProgressAction::Other` or any Ninja `[k/N]` progress line).
    let mut compile_roles = vec![SpanRole::Break; parsed_run.len()];
    let mut reg_start = 0usize;
    while reg_start < parsed_run.len() {
        if is_compile_region_barrier(&parsed_run[reg_start]) {
            reg_start += 1;
            continue;
        }
        let mut reg_end = reg_start + 1;
        while reg_end < parsed_run.len() && !is_compile_region_barrier(&parsed_run[reg_end]) {
            reg_end += 1;
        }

        let mut local_targets = CompileTargetIndex::default();
        let mut has_any_compile = false;
        for kind in &parsed_run[reg_start..reg_end] {
            if let LineKind::Compile(compile) = kind {
                local_targets.insert(compile);
                has_any_compile = true;
            }
        }

        if has_any_compile {
            for idx in reg_start..reg_end {
                compile_roles[idx] = match &parsed_run[idx] {
                    LineKind::Blank => SpanRole::Blank,
                    LineKind::Compile(_) => SpanRole::Include,
                    LineKind::Progress(p) => {
                        if matches!(p.counter, Counter::Percent(_))
                            && p.action == ProgressAction::BuildObject
                            && local_targets.matches(p.target)
                        {
                            SpanRole::Include
                        } else {
                            SpanRole::Break
                        }
                    }
                    LineKind::MakeTag(tag) => {
                        if local_targets.matches(tag.target) {
                            SpanRole::Include
                        } else {
                            SpanRole::Break
                        }
                    }
                };
            }
        }

        reg_start = reg_end;
    }

    let compile_spans = partition_included_spans(&compile_roles);
    let mut claimed = vec![false; parsed_run.len()];
    let mut compile_edits = Vec::new();
    for span in &compile_spans {
        if compile_edits.len() >= remaining {
            break;
        }
        emit_non_ninja_compile_span(
            lines,
            run_start,
            &parsed_run,
            span,
            remaining,
            &mut compile_edits,
            &mut claimed,
        );
    }

    // Phase 2: any `[k/N]` or `[PPP%]` progress lines not claimed by an accepted compile edit
    // stay available for CMake/Ninja progress compaction (merging across gaps left empty).
    let progress_roles = parsed_run
        .iter()
        .enumerate()
        .map(|(idx, kind)| {
            if claimed[idx] {
                SpanRole::Break
            } else {
                match kind {
                    LineKind::Blank => SpanRole::Blank,
                    LineKind::Progress(_) => SpanRole::Include,
                    LineKind::Compile(_) | LineKind::MakeTag(_) => SpanRole::Break,
                }
            }
        })
        .collect::<Vec<_>>();
    let progress_spans = partition_included_spans(&progress_roles);
    let compile_cutoff =
        (compile_edits.len() == remaining).then(|| compile_edits[remaining - 1].start);
    let mut progress_edits = Vec::new();
    for span in &progress_spans {
        if progress_edits.len() >= remaining
            || compile_cutoff.is_some_and(|cutoff| run_start + span.start >= cutoff)
        {
            break;
        }
        emit_progress_span(
            lines,
            run_start,
            &parsed_run,
            span,
            remaining,
            &mut progress_edits,
        );
    }

    let mut combined = compile_edits;
    combined.extend(progress_edits);
    combined.sort_by_key(|edit| edit.start);
    combined.truncate(remaining);

    if !window_cut_early {
        edits.extend(combined);
        return run_end.max(run_start + 1);
    }

    let mut half_len = 0usize;
    let mut half_bytes = 0usize;
    while half_len < parsed_run.len() && half_len < MAX_ROWS {
        let line_len = lines[run_start + half_len].len();
        if half_len > 0 && half_bytes + line_len > MAX_BYTES {
            break;
        }
        half_bytes += line_len;
        half_len += 1;
    }
    let commit_limit = run_start + half_len.max(1);

    let mut last_committed_end = None;
    let mut next_deferred_start = None;
    for edit in combined {
        if edit.end <= commit_limit {
            last_committed_end = Some(edit.end);
            edits.push(edit);
            if edits.len() >= MAX_EDITS {
                return last_committed_end.unwrap_or(commit_limit);
            }
        } else {
            next_deferred_start = Some(edit.start);
            break;
        }
    }
    if let Some(next_start) = next_deferred_start
        && next_start > run_start
        && next_start < commit_limit
    {
        return next_start;
    }
    if let Some(last_end) = last_committed_end {
        return commit_limit.max(last_end);
    }
    commit_limit
}

fn partition_included_spans(roles: &[SpanRole]) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut idx = 0usize;
    while idx < roles.len() {
        if roles[idx] != SpanRole::Include {
            idx += 1;
            continue;
        }
        let start = idx;
        let mut last_included = idx + 1;
        idx += 1;
        while idx < roles.len() {
            match roles[idx] {
                SpanRole::Include => {
                    idx += 1;
                    last_included = idx;
                }
                SpanRole::Blank => {
                    let mut peek = idx + 1;
                    while peek < roles.len() && roles[peek] == SpanRole::Blank {
                        peek += 1;
                    }
                    if peek < roles.len() && roles[peek] == SpanRole::Include {
                        idx = peek;
                    } else {
                        break;
                    }
                }
                SpanRole::Break => break,
            }
        }
        spans.push(Span {
            start,
            end: last_included,
        });
    }
    spans
}

fn emit_progress_span(
    lines: &[&str],
    run_start: usize,
    parsed_run: &[LineKind<'_>],
    span: &Span,
    max_edits: usize,
    edits: &mut Vec<LayoutEdit>,
) {
    let mut cur = span.start;
    while cur < span.end && edits.len() < max_edits {
        while cur < span.end && matches!(parsed_run[cur], LineKind::Blank) {
            cur += 1;
        }
        if cur >= span.end {
            break;
        }

        let mut scan = cur;
        let mut bytes = 0usize;
        while scan < span.end && scan - cur < MAX_ROWS {
            let line_len = lines[run_start + scan].len();
            if bytes + line_len > MAX_BYTES {
                break;
            }
            bytes += line_len;
            scan += 1;
        }

        let mut end = scan;
        while end > cur && matches!(parsed_run[end - 1], LineKind::Blank) {
            end -= 1;
        }

        let rows = parsed_run[cur..end]
            .iter()
            .filter_map(|kind| match kind {
                LineKind::Progress(row) => Some(Some(*row)),
                LineKind::Blank => Some(None),
                _ => None,
            })
            .collect::<Vec<_>>();
        let row_count = rows.iter().filter(|entry| entry.is_some()).count();

        if row_count >= 2 && end > cur {
            let abs_start = run_start + cur;
            let abs_end = run_start + end;
            let original = lines[abs_start..abs_end].concat();
            let replacement = render::render_progress(&rows);
            if saves_both(&original, &replacement) {
                edits.push(LayoutEdit {
                    start: abs_start,
                    end: abs_end,
                    replacement,
                });
            }
        }

        cur = scan.max(cur + 1);
    }
}

fn emit_non_ninja_compile_span(
    lines: &[&str],
    run_start: usize,
    parsed_run: &[LineKind<'_>],
    span: &Span,
    max_edits: usize,
    edits: &mut Vec<LayoutEdit>,
    claimed: &mut [bool],
) {
    let mut cur = span.start;
    while cur < span.end && edits.len() < max_edits {
        while cur < span.end && matches!(parsed_run[cur], LineKind::Blank) {
            cur += 1;
        }
        if cur >= span.end {
            break;
        }

        let mut scan = cur;
        let mut bytes = 0usize;
        while scan < span.end && scan - cur < MAX_ROWS {
            let line_len = lines[run_start + scan].len();
            if bytes + line_len > MAX_BYTES {
                break;
            }
            bytes += line_len;
            scan += 1;
        }

        let mut end = scan;
        let mut chunk_targets;
        let mut cut_at = None;
        let emit_start = loop {
            chunk_targets = CompileTargetIndex::default();
            for kind in &parsed_run[cur..end] {
                if let LineKind::Compile(compile) = kind {
                    chunk_targets.insert(compile);
                }
            }
            while end > cur {
                match &parsed_run[end - 1] {
                    LineKind::Blank => end -= 1,
                    LineKind::MakeTag(tag) if !chunk_targets.matches(tag.target) => end -= 1,
                    LineKind::Progress(progress) if !chunk_targets.matches(progress.target) => {
                        end -= 1;
                    }
                    _ => break,
                }
            }
            let mut start_trim = cur;
            while start_trim < end {
                match &parsed_run[start_trim] {
                    LineKind::Blank => start_trim += 1,
                    LineKind::MakeTag(tag) if !chunk_targets.matches(tag.target) => start_trim += 1,
                    LineKind::Progress(progress) if !chunk_targets.matches(progress.target) => {
                        start_trim += 1;
                    }
                    _ => break,
                }
            }
            let first_unmatched = (start_trim..end).find(|&idx| match &parsed_run[idx] {
                LineKind::Progress(p) => !chunk_targets.matches(p.target),
                LineKind::MakeTag(t) => !chunk_targets.matches(t.target),
                _ => false,
            });
            match first_unmatched {
                Some(unmatched_idx) => {
                    cut_at = Some(unmatched_idx);
                    end = unmatched_idx;
                }
                None => break start_trim,
            }
        };

        let next_cur = if end > cur && end < scan {
            end
        } else if let Some(unmatched_idx) = cut_at {
            unmatched_idx.max(cur + 1)
        } else {
            scan.max(cur + 1)
        };

        let mut compile_rows = 0usize;
        let mut non_blank_rows = 0usize;
        let mut has_verbose_compile = false;
        let mut has_paired_label = false;
        let mut items = Vec::with_capacity(end.saturating_sub(emit_start));

        for kind in &parsed_run[emit_start..end] {
            match kind {
                LineKind::Blank => items.push(CompileBlockItem::Blank),
                LineKind::Compile(compile) => {
                    compile_rows += 1;
                    non_blank_rows += 1;
                    if compile.is_verbose {
                        has_verbose_compile = true;
                    }
                    items.push(CompileBlockItem::Compile(compile));
                }
                LineKind::Progress(progress) => {
                    let matched = chunk_targets.matches(progress.target);
                    debug_assert!(matched);
                    non_blank_rows += 1;
                    if matched {
                        has_paired_label = true;
                    }
                    items.push(CompileBlockItem::Progress(progress));
                }
                LineKind::MakeTag(tag) => {
                    let matched = chunk_targets.matches(tag.target);
                    debug_assert!(matched);
                    non_blank_rows += 1;
                    if matched {
                        has_paired_label = true;
                    }
                    items.push(CompileBlockItem::MakeTag(tag));
                }
            }
        }

        if compile_rows >= 1
            && non_blank_rows >= 2
            && (has_verbose_compile || has_paired_label)
            && end > emit_start
        {
            let abs_start = run_start + emit_start;
            let abs_end = run_start + end;
            let original = lines[abs_start..abs_end].concat();
            let replacement = render::render_compile(&items);
            if saves_both(&original, &replacement) {
                edits.push(LayoutEdit {
                    start: abs_start,
                    end: abs_end,
                    replacement,
                });
                claimed[emit_start..end].fill(true);
            }
        }

        cur = next_cur;
    }
}
