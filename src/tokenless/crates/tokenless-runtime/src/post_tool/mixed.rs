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
    JsonCompressionContext, JsonCompressor, Recoverability, TabularCompressor,
};
use tokenless_protocol::AppliedOperation;

use super::pipeline::PostToolPipelineConfig;

/// The compression domain a region routes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegionKind {
    Json,
    Tabular,
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
/// Returns `None` when no region changed: the arbiter would reject an
/// operation-free candidate, so the caller routes to passthrough directly.
pub(crate) fn compress_regions(
    content: &str,
    config: &PostToolPipelineConfig,
    stash: Option<&dyn StashStore>,
    recovery: &RecoveryMethod,
) -> Option<MixedResult> {
    let json_compressor = JsonCompressor::new(config.json.clone());
    let mut output = String::with_capacity(content.len());
    let mut operations = Vec::new();
    let mut recoverability = Recoverability::Lossless;
    let mut stash_writes = Vec::new();
    let mut stash_errors = 0usize;
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
                    allow_toon: config.allow_toon,
                    min_toon_chars: config.min_toon_chars,
                    allow_unrecoverable: !config.require_reversibility
                        || !config.compression_enabled,
                };
                json_compressor
                    .compress(region, &context)
                    .ok()
                    .filter(|outcome| outcome.output != region)
                    .map(Applied::Json)
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
            // selected TOON is announced as `toon`, not `json`. The whole
            // info string is replaced — the label identifies the region's
            // representation, and the body no longer is JSON.
            (Some(label), true) => output.push_str(&fence_line.replace(label, "toon")),
            _ => output.push_str(fence_line),
        }
        match outcome {
            Some(Applied::Json(outcome)) => {
                changed = true;
                output.push_str(&outcome.output);
                // Same operation attribution as the standalone JSON route:
                // Cleanup is only emitted when compaction itself shaped the
                // output, never alongside TOON.
                for operation in &outcome.operations {
                    match operation {
                        tokenless_compressors::JsonOperation::Toon => {
                            operations.push(AppliedOperation::Toon);
                        }
                        tokenless_compressors::JsonOperation::Truncation => {
                            operations.push(AppliedOperation::JsonTruncation);
                        }
                        tokenless_compressors::JsonOperation::RecordReduction => {
                            operations.push(AppliedOperation::JsonRecordReduction);
                        }
                        tokenless_compressors::JsonOperation::Cleanup => {
                            operations.push(AppliedOperation::JsonCleanup);
                        }
                    }
                }
                recoverability = weakest(recoverability, outcome.recoverability);
                stash_writes.extend(outcome.stash_writes);
                stash_errors += outcome.metrics.stash_errors;
            }
            Some(Applied::Tabular(outcome)) => {
                changed = true;
                output.push_str(&outcome.output);
                operations.push(AppliedOperation::TabularCompaction);
                recoverability = weakest(recoverability, outcome.recoverability);
            }
            None => output.push_str(region),
        }
        cursor += region.len();
    }
    output.push_str(&content[cursor..]);
    if !changed {
        return None;
    }
    Some(MixedResult {
        output,
        operations,
        recoverability,
        stash_writes,
        stash_errors,
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

/// Cheap precheck for the pipeline gate: does the content carry at least
/// one region boundary a conservative recognizer would trust? A fenced
/// block with a supported info word, or a line whose first non-space
/// character opens a bracket-balanced document.
pub(crate) fn has_recognizable_regions(content: &str) -> bool {
    let bytes = content.as_bytes();
    let mut cursor = 0;
    while let Some(line_end) = next_line_end(bytes, cursor) {
        let line = &content[cursor..line_end];
        let trimmed = line.trim_start_matches([' ', '\t']);
        let indent = line.len() - trimmed.len();
        if indent <= 3 && !trimmed.starts_with('\t') && opening_fence(trimmed).is_some() {
            return true;
        }
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            return true;
        }
        cursor = line_end;
    }
    false
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
}

/// Walks the content once, yielding one [`Boundary`] per recognized region.
fn boundaries(content: &str) -> Vec<Boundary<'_>> {
    let mut boundaries = Vec::new();
    let mut cursor = 0;
    while let Some((kind, open_start, open_end, body, label, after)) =
        next_boundary(content, cursor)
    {
        boundaries.push(Boundary {
            open_start,
            open_end,
            body,
            kind,
            fence_label: label,
        });
        cursor = after;
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

type Found<'a> = (RegionKind, usize, usize, &'a str, Option<&'a str>, usize);

/// Finds the next region at or after `from`. Returns the kind, the offsets
/// of and just past the opening line, the body slice, the fence label, and
/// the offset to resume scanning from — past the closing fence line for a
/// fenced region and past the closing line for a bare document, so the
/// closing marker is never itself mistaken for an opening fence.
fn next_boundary(content: &str, from: usize) -> Option<Found<'_>> {
    let bytes = content.as_bytes();
    let mut cursor = from;
    while let Some(line_end) = next_line_end(bytes, cursor) {
        let line = &content[cursor..line_end];
        let trimmed = line.trim_start_matches([' ', '\t']);
        let indent = line.len() - trimmed.len();
        if indent <= 3
            && !trimmed.starts_with('\t')
            && let Some((marker, run, info)) = opening_fence(trimmed)
        {
            if let Some(kind) = region_kind(info) {
                // A supported fence that never closes protects its
                // remainder: recognition stops here rather than
                // treating the unterminated interior as regions.
                let (body, after) = close_fenced(content, line_end, marker, run)?;
                return Some((kind, cursor, line_end, body, Some(info), after));
            }
            // A protected or unlabeled block: skip past its
            // closing fence, or to the end when unclosed.
            let (_, after) = close_any_fenced(content, line_end, marker, run);
            cursor = after;
            continue;
        }
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            let start = cursor + indent;
            if let Some((body, after)) = balanced_json(content, start) {
                return Some((RegionKind::Json, start, start, body, None, after));
            }
        }
        cursor = line_end;
    }
    None
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
/// output stays inside the block.
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

/// Same scan, but an unclosed fence simply ends the scan instead of
/// returning `None` (used for protected blocks, which must be skipped
/// without aborting recognition of later content).
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
    (&content[body_start..], content.len())
}

/// A closing fence: at least `length` of the same marker, then only
/// whitespace. Indented up to three spaces, matching the opening grammar.
fn is_closing_fence(line: &str, marker: u8, length: usize) -> bool {
    let trimmed = line.trim_start_matches(' ');
    let run = trimmed.bytes().take_while(|byte| *byte == marker).count();
    run >= length && trimmed[run..].trim().is_empty()
}

/// Scans a bracket-balanced JSON document from `start`. Returns the document
/// text and the offset just past the line holding its closing bracket.
/// `None` when the document never closes.
fn balanced_json(content: &str, start: usize) -> Option<(&str, usize)> {
    let bytes = content.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, byte) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match *byte {
            b'"' => in_string = true,
            b'{' | b'[' => depth += 1,
            b'}' | b']' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    let after = content[index..]
                        .find('\n')
                        .map_or(content.len(), |newline| index + newline + 1);
                    return Some((&content[start..=index], after));
                }
            }
            _ => {}
        }
    }
    None
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
    fn unbalanced_opening_bracket_is_skipped_not_truncated() {
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
}
