//! Typed-region recognition for mixed-content tool outputs.
//!
//! A mixed output is prose, headers, or logs surrounding structured
//! regions: fenced code blocks and bracket-delimited JSON documents embedded
//! in surrounding text. Recognition is conservative and structural only —
//! a region exists when an explicit, verifiable boundary marks it, never
//! because its shape merely looks like data. Full-input parsing stays inside
//! the domain compressors, exactly as for standalone documents, and a region
//! the compressor rejects is preserved verbatim.

use tokenless_ccr::{RecoveryMethod, StashStore};
use tokenless_compressors::{
    BuildLogCompressor, JsonCompressionContext, JsonCompressor, Recoverability, TabularCompressor,
    TabularOperation,
};
use tokenless_protocol::AppliedOperation;

use super::content::{MAX_SCAN_LINES, scan_prefix};
use super::pipeline::{PostToolPipelineConfig, json_operations};

/// The compression domain a region routes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegionKind {
    Json,
    Tabular,
}

/// A line-level region inside a compound command's output whose class was
/// established by the command's own segments rather than by content alone.
///
/// The search segment's bytes are never claimable: a grep/rg run's matches
/// must survive complete, and a match can itself look like a log line, so
/// only runs of lines the command's non-search segments produced — bounded
/// by recognized search-hit lines on both ends or by the output edges — are
/// candidates. The run compresses only when the whole-run log compressor
/// reports a benefit; otherwise the run stays byte-for-byte.
pub(crate) struct SearchGuardedRun<'a> {
    /// The candidate lines, complete with their line endings.
    pub(crate) lines: Vec<&'a str>,
}

/// Per-region compression results for one mixed-content candidate.
pub(crate) struct MixedResult {
    /// Reconstructed output with compressed regions in place.
    pub(crate) output: String,
    /// Wire operations across all compressed regions, in region order.
    pub(crate) operations: Vec<AppliedOperation>,
    /// Weakest region recoverability.
    pub(crate) recoverability: Recoverability,
    /// Stash writes from every region.
    pub(crate) stash_writes: Vec<tokenless_ccr::StashWrite>,
    /// Failed stash writes across regions.
    pub(crate) stash_errors: usize,
    /// Unrecoverable truncations from a JSON region that truncated, summed
    /// across regions under the same attribution rule as the standalone
    /// JSON route.
    pub(crate) unrecoverable_truncations: Option<usize>,
}

/// Compresses recognized regions of a mixed output and reconstructs the
/// whole content with unchanged bytes around them.
///
/// Each region runs its own domain compressor under the same configuration
/// the standalone route would use. A region the compressor rejects (invalid
/// JSON, malformed table, no per-region savings) stays byte-for-byte, so a
/// misrecognized boundary can never corrupt the surrounding document. A
/// fenced JSON region that selected TOON has its fence label updated to
/// `toon` together with its body, keeping representation metadata consistent
/// with the applied transformation.
///
/// Returns `None` when no region changed, no tentative Stash write was
/// produced, and no Stash error occurred. Otherwise an unchanged result or
/// any Stash error returns an operation-free candidate so the caller's
/// ledger rolls back all tentative writes before reporting the failures.
pub(crate) fn compress_regions(
    content: &str,
    config: &PostToolPipelineConfig,
    stash: Option<&dyn StashStore>,
    recovery: &RecoveryMethod,
    allow_toon: bool,
    embedded_html: bool,
) -> Option<MixedResult> {
    let json_compressor = JsonCompressor::new(config.json.clone());
    let mut output = String::with_capacity(content.len());
    let mut operations = Vec::new();
    let mut recoverability = Recoverability::Lossless;
    let mut stash_writes = Vec::new();
    let mut stash_errors = 0usize;
    let mut unrecoverable_truncations = 0usize;
    let mut truncated = false;
    let mut changed = false;
    let mut cursor = 0usize;

    for boundary in boundaries(content) {
        // Compress first: whether the opening fence needs relabeling depends
        // on the body's outcome. The prose before the fence and the fence
        // line are separate slices — the label rewrite touches only the
        // fence line, never the surrounding text.
        output.push_str(&content[cursor..boundary.open_start]);
        let fence_line = &content[boundary.open_start..boundary.open_end];
        cursor = boundary.open_end;
        let region = boundary.body;
        enum Applied {
            Json(tokenless_compressors::JsonOutcome),
            Tabular(tokenless_compressors::TabularOutcome),
        }
        let outcome = match boundary.kind {
            RegionKind::Json => {
                let context = JsonCompressionContext {
                    recovery,
                    stash,
                    allow_toon,
                    min_toon_chars: config.min_toon_chars,
                    allow_unrecoverable: !config.require_reversibility
                        || !config.compression_enabled,
                    embedded_html,
                };
                // A region whose selected candidate does not save space is
                // kept byte-for-byte, but its tentative Stash writes still
                // reach the ledger: the compressor may have written a
                // bounded candidate before choosing the original, and only
                // the ledger's rollback/orphan cleanup can remove those
                // rows. Discarding the whole outcome would leak an
                // unreferenced row and drop its storage-error accounting.
                // An unchanged region carries no operations, so the fence
                // keeps its label and the body is re-emitted as-is below.
                json_compressor
                    .compress(region, &context)
                    .ok()
                    .and_then(|outcome| {
                        if outcome.output == region {
                            recoverability = weakest(recoverability, outcome.recoverability);
                            stash_writes.extend(outcome.stash_writes);
                            stash_errors += outcome.metrics.stash_errors;
                            if outcome.operations.iter().any(|op| {
                                matches!(op, tokenless_compressors::JsonOperation::Truncation)
                            }) {
                                truncated = true;
                                unrecoverable_truncations +=
                                    outcome.metrics.unrecoverable_truncations;
                            }
                            None
                        } else {
                            Some(Applied::Json(outcome))
                        }
                    })
            }
            RegionKind::Tabular => {
                let outcome = TabularCompressor.compress(region);
                (!outcome.operations.is_empty() && outcome.output != region)
                    .then_some(Applied::Tabular(outcome))
            }
        };
        let toon = matches!(
            &outcome,
            Some(Applied::Json(outcome))
                if outcome.operations.iter().any(|op| matches!(op, tokenless_compressors::JsonOperation::Toon))
        );
        match (boundary.fence_label, toon) {
            // The fence label travels with the body: a JSON block that
            // selected TOON is announced as `toon`, not `json`. Only the
            // first info word (the language) is rewritten; any remaining
            // attributes keep their text and order.
            (Some(_), true) => output.push_str(&relabeled_fence_line(
                fence_line,
                boundary.fence_indent,
                boundary.fence_marker_run,
            )),
            _ => output.push_str(fence_line),
        }
        match outcome {
            Some(Applied::Json(outcome)) => {
                changed = true;
                output.push_str(&outcome.output);
                // Same operation attribution as the standalone JSON route:
                // Cleanup is only emitted when compaction itself shaped the
                // output, never alongside TOON.
                operations.extend(json_operations(&outcome.operations));
                recoverability = weakest(recoverability, outcome.recoverability);
                stash_writes.extend(outcome.stash_writes);
                stash_errors += outcome.metrics.stash_errors;
                if outcome
                    .operations
                    .iter()
                    .any(|op| matches!(op, tokenless_compressors::JsonOperation::Truncation))
                {
                    truncated = true;
                    unrecoverable_truncations += outcome.metrics.unrecoverable_truncations;
                }
            }
            Some(Applied::Tabular(outcome)) => {
                changed = true;
                output.push_str(&outcome.output);
                // Same operation attribution as the standalone tabular route:
                // RowReduction is retained for compatibility and never
                // emitted, but the mapping mirrors `pipeline.rs` so a revived
                // row-selection mode cannot silently report a lossless name.
                for operation in &outcome.operations {
                    match operation {
                        TabularOperation::Compaction => {
                            operations.push(AppliedOperation::TabularCompaction);
                        }
                        TabularOperation::RowReduction => {
                            operations.push(AppliedOperation::TabularRowReduction);
                        }
                    }
                }
                recoverability = weakest(recoverability, outcome.recoverability);
                // Documented as always empty, but mirror the standalone
                // route so a storage-error counter never silently stops
                // being accumulated.
                stash_writes.extend(outcome.stash_writes);
                stash_errors += outcome.metrics.stash_errors;
            }
            None => output.push_str(region),
        }
        cursor += region.len();
    }
    output.push_str(&content[cursor..]);
    if !changed && stash_writes.is_empty() && stash_errors == 0 {
        return None;
    }
    if !changed || stash_errors > 0 {
        // Any storage error fails the entire runtime request, so no region's
        // recovery marker will be delivered. Reject the whole candidate to
        // roll back successful writes too, including those from regions
        // whose selected output saved space.
        return Some(MixedResult {
            output: content.to_owned(),
            operations: Vec::new(),
            recoverability,
            stash_writes,
            stash_errors,
            unrecoverable_truncations: truncated.then_some(unrecoverable_truncations),
        });
    }
    Some(MixedResult {
        output,
        operations,
        recoverability,
        stash_writes,
        stash_errors,
        unrecoverable_truncations: truncated.then_some(unrecoverable_truncations),
    })
}

fn weakest(left: Recoverability, right: Recoverability) -> Recoverability {
    use Recoverability::{Lossless, Retrievable, Unrecoverable};
    match (left, right) {
        (Unrecoverable, _) | (_, Unrecoverable) => Unrecoverable,
        (Retrievable, _) | (_, Retrievable) => Retrievable,
        _ => Lossless,
    }
}

/// The opening fence line with only its info word replaced by `toon`.
/// The indent and marker run come from the recognizer that opened the
/// fence (`Boundary` carries the line; the run it measured), so the
/// relabel cannot re-derive the fence grammar and drift from the opening
/// rule. Leading indentation, the marker run, and any attributes after the
/// language word are re-emitted unchanged.
fn relabeled_fence_line(fence_line: &str, indent_len: usize, marker_run: usize) -> String {
    let (indent, rest) = fence_line.split_at(indent_len);
    let info = rest[marker_run..].trim();
    let mut relabeled = String::with_capacity(fence_line.len() + 1);
    relabeled.push_str(indent);
    relabeled.push_str(&rest[..marker_run]);
    relabeled.push_str("toon");
    let after_word = info
        .split_ascii_whitespace()
        .next()
        .map_or("", |word| &info[word.len()..]);
    let attributes = after_word.trim();
    if !attributes.is_empty() {
        relabeled.push(' ');
        relabeled.push_str(attributes);
    }
    // Preserve the whitespace between marker and info word plus the trailing
    // whitespace/newline the original line carried. Slicing `rest` from the
    // info word's own offset keeps every offset in the same slice — byte-safe
    // for non-ASCII leading blanks and independent of the stripped indent.
    let info_offset =
        marker_run + (rest[marker_run..].len() - rest[marker_run..].trim_start().len());
    relabeled.push_str(&rest[info_offset + info.len()..]);
    relabeled
}

/// Cheap precheck for the pipeline gate: does the content carry at least
/// one region boundary a conservative recognizer would trust? A fenced
/// block with a supported info word, or a line whose first non-tab
/// character opens a bracket-balanced document. Bounded like the other
/// detectors: only the first [`MAX_SCAN_BYTES`] / [`MAX_SCAN_LINES`] are
/// inspected.
pub(crate) fn has_recognizable_regions(content: &str) -> bool {
    for (indent, trimmed) in scan_lines_prefixed(content) {
        if indent <= 3
            && !trimmed.starts_with('\t')
            && (opening_fence(trimmed).is_some()
                || trimmed.starts_with('{')
                || trimmed.starts_with('['))
        {
            return true;
        }
    }
    false
}

/// A line the search segment may have produced: a numbered `path:line:text`
/// hit the search-results validators accept, inside the bounded scan window.
///
/// Recognition is deliberately the *compressor's* own shape grammar — the
/// same validators `SearchResultsCompressor` enforces before it would ever
/// rewrite a record — so a line claimed as a search byte is a line the
/// lossless search path itself would have recognized. Anything else (a
/// timestamp like `12:30:00`, a bare `build.log:41` count, prose with a
/// colon) is not a search byte here.
fn is_search_hit_line(line: &str) -> bool {
    // Mirrors `search_results::line_is_search_hit`: a path-like prefix
    // (`/` or `.` present, so `12:30:00` timestamps do not match), then a
    // line number, then a second colon.
    let Some(colon) = line.find(':') else {
        return false;
    };
    let path = &line[..colon];
    if path.is_empty() || !(path.contains('/') || path.contains('.')) {
        return false;
    }
    let rest = &line[colon + 1..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    digits >= 1 && rest.as_bytes().get(digits) == Some(&b':')
}

/// One piece of a search-guarded compound output, in source order: either a
/// recognized search-hit line (protected bytes) or a maximal run of lines the
/// non-search segments produced.
pub(crate) enum GuardedSegment<'a> {
    /// A search-hit line, emitted byte-for-byte and never compressed.
    SearchHit(&'a str),
    /// A run offered to the log domain as a whole.
    Run(SearchGuardedRun<'a>),
}

/// Splits a search-guarded compound output into ordered segments: every
/// recognized search-hit line becomes its own [`GuardedSegment::SearchHit`]
/// and the remaining lines group into maximal [`GuardedSegment::Run`]s.
/// Bounded by the shared scan window.
///
/// The split is conservative in both directions:
///
/// - A line the grammar recognizes as a search hit is a boundary and is
///   never inside a run, even if the log segment could have produced an
///   identical line (a log line that looks like a match is preserved).
/// - Everything else joins the current run, including blank lines and
///   prose: the run's own compressor decides whether the run compresses,
///   and a rejected run survives byte-for-byte.
pub(crate) fn search_guarded_runs(content: &str) -> Vec<GuardedSegment<'_>> {
    let mut segments = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in scan_prefix(content).split_inclusive('\n') {
        if is_search_hit_line(line.trim_end_matches(['\r', '\n'])) {
            if !current.is_empty() {
                segments.push(GuardedSegment::Run(SearchGuardedRun {
                    lines: std::mem::take(&mut current),
                }));
            }
            segments.push(GuardedSegment::SearchHit(line));
            continue;
        }
        current.push(line);
        if current.len() >= MAX_SCAN_LINES {
            segments.push(GuardedSegment::Run(SearchGuardedRun {
                lines: std::mem::take(&mut current),
            }));
        }
    }
    if !current.is_empty() {
        segments.push(GuardedSegment::Run(SearchGuardedRun { lines: current }));
    }
    segments
}

/// Whether a search-guarded run offers the log domain anything: the run's
/// complete text must be recognized as a build/test log and carry a routine
/// run worth reducing. The run is tried as a whole; a run the compressor
/// rejects compresses nothing.
pub(crate) fn search_guarded_run_is_compressible(run: &SearchGuardedRun<'_>) -> bool {
    let text: String = run.lines.concat();
    BuildLogCompressor::detect(&text)
}

/// Compresses one search-guarded run through the build-log domain with the
/// same stash lifecycle the standalone route uses (the log compressor takes
/// no domain configuration; its detection and omission policy are fixed).
///
/// The run's lines carry their own endings, so the compressor's output
/// splices back between the preserved search bytes with no separator
/// reconstruction. A run the compressor leaves unchanged (or that saves
/// nothing) reports `output: None`: the caller emits the run byte-for-byte,
/// exactly like a rejected region in the fence/bare-JSON walk, while the
/// run's tentative Stash writes and errors still reach the ledger.
pub(crate) fn compress_search_guarded_run(
    run: &SearchGuardedRun<'_>,
    stash: Option<&dyn StashStore>,
    recovery: &RecoveryMethod,
) -> Option<SearchGuardedRunOutcome> {
    let text: String = run.lines.concat();
    let outcome = BuildLogCompressor.compress_with_recovery(&text, stash, recovery);
    if outcome.output == text || outcome.operations.is_empty() {
        // Same contract as an unchanged fenced region: the run survives
        // byte-for-byte, but its tentative Stash writes and errors still
        // reach the caller's ledger.
        return Some(SearchGuardedRunOutcome {
            output: None,
            operations: Vec::new(),
            stash_writes: outcome.stash_writes,
            stash_errors: outcome.metrics.stash_errors,
        });
    }
    Some(SearchGuardedRunOutcome {
        output: Some(outcome.output),
        operations: outcome
            .operations
            .iter()
            .map(|operation| match operation {
                tokenless_compressors::BuildLogOperation::TerminalCleanup => {
                    AppliedOperation::TerminalCleanup
                }
                tokenless_compressors::BuildLogOperation::ProgressReduction => {
                    AppliedOperation::BuildLogReduction
                }
            })
            .collect(),
        stash_writes: outcome.stash_writes,
        stash_errors: outcome.metrics.stash_errors,
    })
}

/// The result of offering one search-guarded run to the log domain.
pub(crate) struct SearchGuardedRunOutcome {
    /// The compressed run, or `None` when the run survives byte-for-byte.
    pub(crate) output: Option<String>,
    /// Wire operations when the run compressed.
    pub(crate) operations: Vec<AppliedOperation>,
    /// Tentative Stash writes performed while producing candidates.
    pub(crate) stash_writes: Vec<tokenless_ccr::StashWrite>,
    /// Failed Stash writes.
    pub(crate) stash_errors: usize,
}

/// Iterates the bounded scan window as `(indent, trimmed_line)` pairs,
/// mirroring the budget [`super::content`] applies to every other detector:
/// at most [`MAX_SCAN_BYTES`] and [`MAX_SCAN_LINES`] of input.
///
/// The trim strips spaces only: a tab-indented line keeps its tab, so the
/// callers' `!trimmed.starts_with('\t')` guard sees it. A tab expands to a
/// four-column indent ([CommonMark tabs]), which is past the maximum
/// three-space indentation these recognizers accept.
///
/// [CommonMark tabs]: https://spec.commonmark.org/0.31.2/#tabs
fn scan_lines_prefixed(content: &str) -> impl Iterator<Item = (usize, &str)> + '_ {
    scan_prefix(content)
        .lines()
        .take(MAX_SCAN_LINES)
        .map(|line| {
            let trimmed = line.trim_start_matches(' ');
            (line.len() - trimmed.len(), trimmed)
        })
}

/// A line that opens a fence under CommonMark: at least three of one marker,
/// indented at most three spaces (checked by the caller), with an info string
/// free of backticks when the marker is a backtick. Returns the marker byte,
/// the marker run length, and the info string.
fn opening_fence(trimmed: &str) -> Option<(u8, usize, &str)> {
    let marker = trimmed.as_bytes().first().copied()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let run = trimmed.bytes().take_while(|byte| *byte == marker).count();
    if run < 3 {
        return None;
    }
    let info = trimmed[run..].trim();
    (marker == b'~' || !info.contains('`')).then_some((marker, run, info))
}

/// Where one region starts and ends in the source, including the fence lines
/// around a fenced body.
struct Boundary<'a> {
    /// Offset of the opening fence line, or of the JSON document when bare.
    open_start: usize,
    /// Offset just past the opening fence line (or at the JSON document).
    open_end: usize,
    /// The region body: fence-interior lines, or the JSON document text.
    body: &'a str,
    kind: RegionKind,
    /// The fence info string of the opening line, when fenced.
    fence_label: Option<&'a str>,
    /// The opening fence line's leading-indent width and marker-run length,
    /// as measured by the recognizer. `relabeled_fence_line` reuses them
    /// instead of re-deriving the fence grammar.
    fence_indent: usize,
    fence_marker_run: usize,
}

/// Walks the content once, yielding one [`Boundary`] per recognized region.
///
/// The walk is a single left-to-right pass over the bounded scan window with
/// a persistent bracket state: a bare-JSON opener keeps its depth state across
/// lines instead of rescanning from every candidate, so any input costs
/// O(bytes + lines) — the same budget the other detectors observe.
fn boundaries(content: &str) -> Vec<Boundary<'_>> {
    let prefix = scan_prefix(content);
    // Count physical lines, including those inside skipped fences and JSON.
    // A region opened in this window can still close within the byte window.
    let opening_limit: usize = prefix
        .split_inclusive('\n')
        .take(MAX_SCAN_LINES)
        .map(str::len)
        .sum();
    let bytes = prefix.as_bytes();
    let mut boundaries = Vec::new();
    let mut cursor = 0;
    // A bare document opened on a candidate line: its byte state lives until
    // the document balances or the walk ends. Each byte feeds it exactly once.
    let mut doc: Option<OpenDoc> = None;
    while let Some(line_end) = next_line_end(bytes, cursor) {
        if let Some(mut state) = doc.take() {
            // The document spans this line: feed its bytes to the balance
            // state until it closes or the line ends.
            if let Some((body, after)) = state.feed(prefix, line_end) {
                boundaries.push(Boundary {
                    open_start: state.start,
                    open_end: state.start,
                    body,
                    kind: RegionKind::Json,
                    fence_label: None,
                    fence_indent: 0,
                    fence_marker_run: 0,
                });
                cursor = after;
            } else {
                doc = Some(state);
                cursor = line_end;
            }
            continue;
        }
        if cursor >= opening_limit {
            break;
        }
        let line = &prefix[cursor..line_end];
        // Strip spaces only: a leading tab must survive the trim so the
        // `starts_with('\t')` guards below reject the line. A tab expands
        // to a four-column indent (CommonMark), past the three-space bound.
        let trimmed = line.trim_start_matches(' ');
        let indent = line.len() - trimmed.len();
        if indent <= 3
            && !trimmed.starts_with('\t')
            && let Some((marker, run, info)) = opening_fence(trimmed)
        {
            if let Some(kind) = region_kind(info) {
                // A supported fence that never closes protects its
                // remainder: recognition stops here rather than
                // treating the unterminated interior as regions.
                let Some((body, after)) = close_fenced(prefix, line_end, marker, run) else {
                    break;
                };
                boundaries.push(Boundary {
                    open_start: cursor,
                    open_end: line_end,
                    body,
                    kind,
                    fence_label: Some(info),
                    fence_indent: indent,
                    fence_marker_run: run,
                });
                cursor = after;
                continue;
            }
            // A protected or unlabeled block: skip past its
            // closing fence, or to the end when unclosed.
            let (_, after) = close_any_fenced(prefix, line_end, marker, run);
            cursor = after;
            continue;
        }
        if indent <= 3
            && !trimmed.starts_with('\t')
            && (trimmed.starts_with('{') || trimmed.starts_with('['))
        {
            let mut state = OpenDoc::new(cursor + indent);
            if let Some((body, after)) = state.feed(prefix, line_end) {
                boundaries.push(Boundary {
                    open_start: state.start,
                    open_end: state.start,
                    body,
                    kind: RegionKind::Json,
                    fence_label: None,
                    fence_indent: 0,
                    fence_marker_run: 0,
                });
                cursor = after;
            } else {
                doc = Some(state);
                cursor = line_end;
            }
            continue;
        }
        cursor = line_end;
    }
    boundaries
}

/// The region bodies a scan recognizes, in order. A test-level view of
/// [`boundaries`]; the reconstruction path works from boundaries directly.
#[cfg(test)]
pub(crate) fn regions(content: &str) -> Vec<(&str, RegionKind)> {
    boundaries(content)
        .into_iter()
        .map(|boundary| (boundary.body, boundary.kind))
        .collect()
}

fn next_line_end(bytes: &[u8], from: usize) -> Option<usize> {
    bytes[from..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or((from < bytes.len()).then_some(bytes.len()), |index| {
            Some(from + index + 1)
        })
}

/// Returns the fence-interior body (excluding its final newline, which
/// reconstruction keeps as the separator before the closing fence) and the
/// offset just past the closing line. `None` when unclosed: the rest of the
/// output stays inside the block. The scan is bounded by the byte window,
/// not the candidate-line budget: once inside a recognized region the
/// remaining work is one linear pass over at most [`MAX_SCAN_BYTES`] bytes.
fn close_fenced(
    content: &str,
    body_start: usize,
    marker: u8,
    length: usize,
) -> Option<(&str, usize)> {
    let bytes = content.as_bytes();
    let mut cursor = body_start;
    while let Some(line_end) = next_line_end(bytes, cursor) {
        let line = &content[cursor..line_end];
        if is_closing_fence(line, marker, length) {
            let body = &content[body_start..cursor];
            let body = body
                .strip_suffix("\r\n")
                .or_else(|| body.strip_suffix('\n'))
                .unwrap_or(body);
            return Some((body, line_end));
        }
        cursor = line_end;
    }
    None
}

/// Same scan, but an unclosed fence ends at the byte window (used for
/// protected blocks, which must be skipped without aborting recognition
/// of later content).
fn close_any_fenced(content: &str, body_start: usize, marker: u8, length: usize) -> (&str, usize) {
    let bytes = content.as_bytes();
    let mut cursor = body_start;
    while let Some(line_end) = next_line_end(bytes, cursor) {
        let line = &content[cursor..line_end];
        if is_closing_fence(line, marker, length) {
            return (&content[body_start..cursor], line_end);
        }
        cursor = line_end;
    }
    (&content[body_start..cursor], cursor)
}

/// A closing fence: at least `length` of the same marker, then only
/// whitespace. Indented up to three spaces, matching the opening grammar.
fn is_closing_fence(line: &str, marker: u8, length: usize) -> bool {
    let trimmed = line.trim_start_matches(' ');
    let indent = line.len() - trimmed.len();
    let run = trimmed.bytes().take_while(|byte| *byte == marker).count();
    indent <= 3 && run >= length && trimmed[run..].trim().is_empty()
}

/// The balance state of a bare JSON document that opened on a candidate
/// line and may span further lines. Each byte of the content is fed to the
/// state machine exactly once, so an opener that never balances costs one
/// pass over its own lines, never a rescan per candidate.
struct OpenDoc {
    /// Offset of the opening bracket in the content.
    start: usize,
    /// Offset of the next byte the balance state has not consumed.
    scan: usize,
    depth: usize,
    in_string: bool,
    escaped: bool,
}

impl OpenDoc {
    /// Opens a document whose first bracket sits at `start`.
    fn new(start: usize) -> Self {
        Self {
            start,
            scan: start,
            depth: 0,
            in_string: false,
            escaped: false,
        }
    }

    /// Feeds `content[scan..until]` to the state. Returns the document text
    /// and the offset just past the line holding the closing bracket when
    /// the document balances; `None` while it stays open, and `None` after
    /// advancing past the offending byte when a closing bracket underflows
    /// — the walk then keeps feeding the same state, so the document is not
    /// abandoned and a later opener/closer pair can still close a region
    /// beginning at the original opener.
    fn feed<'a>(&mut self, content: &'a str, until: usize) -> Option<(&'a str, usize)> {
        let bytes = content.as_bytes();
        for index in self.scan..until {
            let byte = bytes[index];
            if self.in_string {
                if self.escaped {
                    self.escaped = false;
                } else if byte == b'\\' {
                    self.escaped = true;
                } else if byte == b'"' {
                    self.in_string = false;
                }
                continue;
            }
            match byte {
                b'"' => self.in_string = true,
                b'{' | b'[' => self.depth += 1,
                b'}' | b']' => {
                    // Underflow is unreachable (the first byte fed is the
                    // opener, so depth is at least 1 at any closing
                    // bracket), but bail forward past the byte all the same:
                    // leaving `scan` where it is would re-feed the same
                    // bytes on every remaining line — the rescan-per-
                    // candidate shape the walk exists to avoid.
                    self.depth = match self.depth.checked_sub(1) {
                        Some(depth) => depth,
                        None => {
                            self.scan = index + 1;
                            return None;
                        }
                    };
                    if self.depth == 0 {
                        self.scan = index + 1;
                        let after = content[index..]
                            .find('\n')
                            .map_or(content.len(), |newline| index + newline + 1);
                        return Some((&content[self.start..=index], after));
                    }
                }
                _ => {}
            }
        }
        self.scan = until;
        None
    }
}

/// Maps a fence info string to the domain that handles it. Only the info
/// word matters (CommonMark takes the first word as the language); formats
/// the compressors explicitly support participate, and every other label
/// protects its block. The info string is decoration around an explicit
/// boundary, never a host-provided semantic hint the content itself lacks.
fn region_kind(info: &str) -> Option<RegionKind> {
    let word = info
        .trim()
        .split_ascii_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match word.as_str() {
        "json" => Some(RegionKind::Json),
        "csv" | "tsv" => Some(RegionKind::Tabular),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::content::MAX_SCAN_BYTES;
    use super::*;

    fn kinds(content: &str) -> Vec<(String, RegionKind)> {
        boundaries(content)
            .into_iter()
            .map(|boundary| (boundary.body.to_owned(), boundary.kind))
            .collect()
    }

    #[test]
    fn fences_with_supported_and_unknown_labels_partition_the_scan() {
        let content = "a\n```json\n{\"x\": 1}\n```\nb\n```csv\n1,2\n3,4\n5,6\n```\nc\n";
        let found = kinds(content);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0], ("{\"x\": 1}".to_owned(), RegionKind::Json));
        assert_eq!(found[1], ("1,2\n3,4\n5,6".to_owned(), RegionKind::Tabular));
    }

    #[test]
    fn tilde_fences_and_longer_closing_fences_close() {
        let content = "~~~json starts-here\n{\"x\": 1}\n~~~~~\n";
        let found = kinds(content);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "{\"x\": 1}");
    }

    #[test]
    fn backtick_info_string_cannot_open_a_fence() {
        // The line opens no fence, so its body is scanned as ordinary
        // content: the object line is still a bare JSON region.
        let content = "```json `quote`\n{\"x\": 1}\n```\n";
        let found = kinds(content);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "{\"x\": 1}");
    }

    #[test]
    fn bracket_balance_survives_braces_inside_strings() {
        // One line, one balanced document: the `}` inside the string value
        // must not close it early, so the whole object is one region.
        let content = "text\n{\"s\": \"}\"}\nmore\n";
        let found = kinds(content);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "{\"s\": \"}\"}");
    }

    #[test]
    fn unbalanced_opening_bracket_protects_its_remainder() {
        // Like an unclosed fence, a bare document that never balances keeps
        // the rest of the scan window inside it — the state machine that
        // makes the walk single-pass cannot resume scanning mid-document
        // without reintroducing a rescan per candidate.
        let content = "text\n{\"open\": [\nstill prose\n";
        assert!(kinds(content).is_empty());
    }

    #[test]
    fn escaped_quotes_inside_json_strings() {
        let content = "x\n{\"s\": \"a\\\"}\"}\ny\n";
        let found = kinds(content);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "{\"s\": \"a\\\"}\"}");
    }

    #[test]
    fn protected_blocks_hide_supported_regions_inside_them() {
        let content = "```python\nx = 1\n```json fake label\n{\"n\": 1}\n```\n";
        // The python block swallows everything up to its closing fence.
        assert!(kinds(content).is_empty());
    }

    #[test]
    fn indented_fences_up_to_three_spaces_open() {
        let content = "   ```json\n{\"x\": 1}\n   ```\n";
        let found = kinds(content);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "{\"x\": 1}");
    }

    #[test]
    fn relabeling_keeps_indent_and_tail_byte_aligned() {
        // The tail slice must stay within the same slice the offsets were
        // measured on: a rest-relative offset used against the whole line
        // duplicates the info word's tail (```toonn) or, with multi-byte
        // attributes, cuts inside a UTF-8 sequence and panics.
        // The indent and marker run come from the recognizer; the tests
        // measure them the same way `boundaries` does.
        let measured = |line: &str| {
            let indent = line.len() - line.trim_start_matches(' ').len();
            let run = line.as_bytes()[indent..]
                .iter()
                .take_while(|byte| **byte == b'`')
                .count();
            (indent, run)
        };
        for (line, expected) in [
            ("```json", "```toon"),
            (" ```json", " ```toon"),
            ("   ```json", "   ```toon"),
            ("```json\t", "```toon\t"),
            ("```json title=x", "```toon title=x"),
        ] {
            let (indent, run) = measured(line);
            assert_eq!(
                relabeled_fence_line(line, indent, run),
                expected,
                "{line:?}"
            );
        }
        // Non-ASCII attributes: the tail offset must not split a char.
        let unicode = "  ```json title=中文";
        let (indent, run) = measured(unicode);
        assert_eq!(
            relabeled_fence_line(unicode, indent, run),
            "  ```toon title=中文"
        );
    }

    #[test]
    fn tab_indented_openers_are_not_regions() {
        // A tab expands to a four-column indent (CommonMark): an indented
        // code block the three-space bound must exclude, so neither the
        // fence nor the bare-document recognizer may claim its lines. An
        // unindented line ends the indented block (CommonMark), so a bare
        // document on such a line inside a tab-indented fence is a
        // legitimate region, not part of the block.
        assert!(kinds("\t```json\n\t{\"x\": 1}\n\t```\n").is_empty());
        assert!(kinds("text\n\t{\"x\": 1}\nmore\n").is_empty());
        assert!(!has_recognizable_regions(
            "\t```json\n\t{\"x\": 1}\n\t```\n"
        ));
        assert!(!has_recognizable_regions("\t{\"x\": 1}\n"));
        // A tab after spaces is still a tab indent under the four-column rule.
        assert!(!has_recognizable_regions("  \t{\"x\": 1}\n"));
    }

    #[test]
    fn region_beyond_the_line_budget_is_not_recognized() {
        // The detection window is shared with the other domains: a region
        // opening past MAX_SCAN_LINES is never scanned, so it and the bytes
        // around it must survive verbatim (fail-closed, not a rescan).
        let filler = "prose line\n".repeat(MAX_SCAN_LINES);
        let content = format!("{filler}```json\n{{\"x\": 1}}\n```\n");
        assert!(kinds(&content).is_empty());
        assert!(!has_recognizable_regions(&content));
    }

    #[test]
    fn region_line_budget_counts_protected_fences_and_multiline_json() {
        let table = "a,b\n1,2";
        for prefix_lines in [MAX_SCAN_LINES - 1, MAX_SCAN_LINES, MAX_SCAN_LINES + 7] {
            for (name, prefix) in [
                ("prose", "prose\n".repeat(prefix_lines)),
                (
                    "protected fence",
                    format!("```python\n{}```\n", "pass\n".repeat(prefix_lines - 2)),
                ),
                (
                    "supported fence",
                    format!("```json\n[\n{}0\n]\n```\n", "0,\n".repeat(prefix_lines - 5)),
                ),
                (
                    "bare JSON",
                    format!("[\n{}0\n]\n", "0,\n".repeat(prefix_lines - 3)),
                ),
            ] {
                assert_eq!(prefix.lines().count(), prefix_lines);
                let content = format!("{prefix}```csv\n{table}\n```\n");
                let found = kinds(&content);
                assert_eq!(
                    found.iter().any(|(body, _)| *body == table),
                    prefix_lines < MAX_SCAN_LINES,
                    "{name}: CSV opener on line {}",
                    prefix_lines + 1,
                );
            }
        }
    }

    #[test]
    fn region_beyond_the_byte_budget_is_not_recognized() {
        // Same contract at the byte bound: scan_prefix cuts at MAX_SCAN_BYTES,
        // and an opener only past the cut is invisible to the walk.
        let filler = "x".repeat(MAX_SCAN_BYTES + 1);
        let content = format!("{filler}\n```json\n{{\"x\": 1}}\n```\n");
        assert!(kinds(&content).is_empty());
        assert!(!has_recognizable_regions(&content));
    }

    #[test]
    fn region_inside_the_window_is_still_recognized_with_out_of_window_prose() {
        // Recognition-only: a region inside the window is still recognized
        // even when more content follows beyond the cut. The applied
        // end-to-end contract — the region compresses, the head prose, the
        // prose between the fence and the cut, and the out-of-window tail
        // survive byte-for-byte — is pinned by
        // `out_of_window_tail_survives_byte_for_byte_through_the_pipeline`
        // in the mixed pipeline tests; the in-window case is covered by
        // `mixed_content_compression_follows_its_opt_in_switch`.
        let body = "{\"x\": 1}";
        let head = "prose\n".repeat(10);
        let tail = "y".repeat(MAX_SCAN_BYTES + 1);
        let content = format!("{head}```json\n{body}\n```\n{tail}\n");
        let found = kinds(&content);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, body);
    }
}
