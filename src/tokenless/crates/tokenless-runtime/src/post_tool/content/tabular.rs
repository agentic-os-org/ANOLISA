use tokenless_compressors::TabularCompressor;

use super::super::mixed::opening_fence;
use super::{MAX_SCAN_LINES, scan_prefix};

/// Matches bounded CSV/TSV datasets or standalone Markdown GFM pipe tables.
///
/// Markdown detection requires every inspected non-empty line in the table block
/// to belong to a single GFM pipe table without interior blank lines, CommonMark
/// block-interrupting lines (`>`, `#`, fences, lists, HTML blocks types 1–6,
/// thematic breaks), or 4-column tab-stop indentation, and conservatively returns
/// `false` when the bounded scan window is exhausted or subsequent content exists.
///
/// One-column tables may omit edge pipes on the header row and/or on a
/// colon-containing alignment delimiter (such as `:---:`, `:---`, `---:`, or
/// `:-`). Because CommonMark Setext underlines contain only `-` (or `=`),
/// requiring `:` on pipe-free delimiters keeps Setext headings like
/// `Summary\n---` and `| a |\n--` excluded. Subsequent rows without unescaped
/// pipes (plain text or escaped `\|` such as `left \| right`) remain valid
/// body rows, while malformed pipe rows such as a lone `|` are rejected.
/// Thematic breaks (`---`, `***`, `___`, `_ _ _`, …) end the table per
/// CommonMark §4.9.
///
/// Delimited CSV/TSV detection delegates to `TabularCompressor::detect`, which
/// checks at most 10 leading records. To keep tab- or comma-padded Markdown
/// tables followed by fenced JSON or prose on the mixed-content route without
/// misclassifying valid CSV/TSV datasets (including ones containing pipes or
/// Markdown-like separators inside cells), a `TabularCompressor::detect` hit is
/// rejected only when the 64 KiB scan prefix is NOT a complete rectangular
/// CSV/TSV table AND contains a GFM Markdown table outside RFC 4180 quoted
/// CSV fields.
pub(super) fn is_tabular(scan: &str) -> bool {
    if is_standalone_markdown_table(scan) {
        return true;
    }
    if !TabularCompressor::detect(scan) {
        return false;
    }
    if !is_delimited_prefix(scan) && has_markdown_table_outside_quotes(scan) {
        return false;
    }
    true
}

fn is_delimited_prefix(scan: &str) -> bool {
    let prefix = scan_prefix(scan);
    let complete = prefix.len() == scan.len();
    let mut found = None;
    for delimiter in *b",\t" {
        if scan_delimited_records(prefix.as_bytes(), delimiter, complete).is_some_and(|n| n >= 3) {
            if found.is_some() {
                return false;
            }
            found = Some(delimiter);
        }
    }
    found.is_some()
}

fn has_markdown_table_outside_quotes(scan: &str) -> bool {
    let bounded = scan_prefix(scan);
    let mut prev_header_cols: Option<usize> = None;
    let mut in_csv_quotes = false;
    for raw in bounded.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let was_in_quotes = in_csv_quotes;
        // Track RFC 4180 CSV quoted multiline fields across newlines (doubled
        // `""` escapes flip parity twice). A line that starts inside quotes or
        // opens an unclosed multiline quoted cell belongs to a CSV field, not a
        // standalone Markdown table row.
        for b in line.bytes() {
            if b == b'"' {
                in_csv_quotes = !in_csv_quotes;
            }
        }
        if was_in_quotes || in_csv_quotes {
            prev_header_cols = None;
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || leading_indent_cols(line) >= 4 || starts_block_interrupt(trimmed) {
            prev_header_cols = None;
            continue;
        }
        let split = split_gfm_row(trimmed);
        if prev_header_cols.is_some_and(|cols| is_matching_gfm_delimiter(trimmed, &split, cols)) {
            return true;
        }
        prev_header_cols = gfm_header_col_count(&split);
    }
    false
}

#[derive(Clone, Copy)]
enum DelimitedFieldState {
    Start,
    Unquoted,
    Quoted,
    Closed,
}

fn scan_delimited_records(input: &[u8], delimiter: u8, complete: bool) -> Option<usize> {
    let input = input.strip_prefix(b"\xef\xbb\xbf").unwrap_or(input);
    let mut state = DelimitedFieldState::Start;
    let mut fields = 1usize;
    let mut width = None;
    let mut count = 0usize;
    let mut started = false;
    for byte in input.iter().copied().chain(complete.then_some(b'\n')) {
        if matches!(state, DelimitedFieldState::Quoted) {
            if byte == b'"' {
                state = DelimitedFieldState::Closed;
            }
            continue;
        }
        if matches!(state, DelimitedFieldState::Closed) && byte == b'"' {
            state = DelimitedFieldState::Quoted;
            continue;
        }
        if byte == delimiter {
            fields += 1;
            started = true;
            state = DelimitedFieldState::Start;
        } else if matches!(byte, b'\r' | b'\n') {
            if started {
                if fields < 2 || width.is_some_and(|expected| expected != fields) {
                    return None;
                }
                width = Some(fields);
                count += 1;
            }
            fields = 1;
            started = false;
            state = DelimitedFieldState::Start;
        } else {
            state = match (state, byte) {
                (DelimitedFieldState::Start, b'"') => DelimitedFieldState::Quoted,
                (DelimitedFieldState::Closed, _) | (DelimitedFieldState::Unquoted, b'"') => {
                    return None;
                }
                _ => DelimitedFieldState::Unquoted,
            };
            started = true;
        }
    }
    if complete && matches!(state, DelimitedFieldState::Quoted) {
        None
    } else {
        Some(count)
    }
}

fn is_standalone_markdown_table(scan: &str) -> bool {
    let prefix = scan_prefix(scan);
    if prefix.len() < scan.len() {
        return false;
    }
    let mut row_count = 0usize;
    let mut header_cols = 0usize;
    let mut table_ended = false;

    for raw in prefix.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if row_count > 0 {
                table_ended = true;
            }
            continue;
        }
        if table_ended {
            // Content exists after the table block ended (interior blank line,
            // subsequent paragraph, second table, or trailing code block).
            return false;
        }
        // Exhausting the line window leaves the remainder unseen, so we cannot
        // prove the document is a standalone table; fail open per `content.rs`.
        if row_count == MAX_SCAN_LINES {
            return false;
        }
        // Four-column indentation (evaluated at 4-space tab stops per GFM §2.2)
        // starts an indented code block, not a GFM pipe table.
        if leading_indent_cols(line) >= 4 {
            return false;
        }
        if starts_block_interrupt(trimmed) {
            return false;
        }
        let split = split_gfm_row(trimmed);
        match row_count {
            0 => {
                let Some(cols) = gfm_header_col_count(&split) else {
                    return false;
                };
                header_cols = cols;
            }
            1 => {
                if !is_matching_gfm_delimiter(trimmed, &split, header_cols) {
                    return false;
                }
            }
            // GFM §4.12 / Example 200 permits body rows without unescaped pipes
            // (including `left \| right`), while rejecting bare lone `|` lines.
            _ => {
                if matches!(split, GfmRowSplit::EmptyPipeLine) {
                    return false;
                }
            }
        }
        row_count += 1;
    }

    row_count >= 3
}

/// Column count for a candidate GFM table header row.
///
/// Accepts piped rows and tentative pipe-free one-column headers (such as
/// `Summary` before `|---|` or `:---:`), while rejecting bare lone `|` lines.
fn gfm_header_col_count(split: &GfmRowSplit<'_>) -> Option<usize> {
    match split {
        GfmRowSplit::Piped(cols) => Some(cols.len()),
        GfmRowSplit::NoUnescapedPipes => Some(1),
        GfmRowSplit::EmptyPipeLine => None,
    }
}

/// True when `split` (for `trimmed`) is a valid GFM delimiter row matching a
/// header with `header_cols` columns.
///
/// Pipe-free one-column delimiters require `:` (`:---:`, `:---`, `---:`, or
/// `:-`) so CommonMark Setext underlines (`---`, `--`, `-`) remain excluded.
fn is_matching_gfm_delimiter(trimmed: &str, split: &GfmRowSplit<'_>, header_cols: usize) -> bool {
    match split {
        GfmRowSplit::Piped(cols) => {
            cols.len() == header_cols && cols.iter().all(|c| is_gfm_separator_cell(c))
        }
        GfmRowSplit::NoUnescapedPipes => {
            header_cols == 1 && trimmed.contains(':') && is_gfm_separator_cell(trimmed)
        }
        GfmRowSplit::EmptyPipeLine => false,
    }
}

/// Evaluates leading whitespace column width using GFM 4-column tab stops.
fn leading_indent_cols(line: &str) -> usize {
    let mut col = 0;
    for b in line.bytes() {
        match b {
            b' ' => col += 1,
            b'\t' => col = (col / 4 + 1) * 4,
            _ => break,
        }
    }
    col
}

/// True when `trimmed` opens a CommonMark block container or leaf construct that
/// interrupts GFM table blocks rather than parsing as a pipe row (GFM §4.12).
fn starts_block_interrupt(trimmed: &str) -> bool {
    let bytes = trimmed.as_bytes();
    if is_thematic_break(bytes) {
        return true;
    }
    if trimmed.starts_with('>') || opening_fence(trimmed).is_some() {
        return true;
    }
    if starts_gfm_html_block(trimmed) {
        return true;
    }
    let hashes = bytes.iter().take_while(|&&b| b == b'#').count();
    if (1..=6).contains(&hashes) && matches!(bytes.get(hashes), None | Some(b' ' | b'\t')) {
        return true;
    }
    if matches!(bytes, [b'-' | b'*' | b'+', b' ' | b'\t', ..]) {
        return true;
    }
    let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    (1..=9).contains(&digits)
        && matches!(bytes.get(digits), Some(b'.' | b')'))
        && matches!(bytes.get(digits + 1), Some(b' ' | b'\t'))
}

/// Matches CommonMark §4.9 thematic breaks: 3+ matching `-`, `_`, or `*`
/// characters, with only optional spaces or tabs, and no other characters.
fn is_thematic_break(bytes: &[u8]) -> bool {
    let marker = match bytes.first() {
        Some(&b) if matches!(b, b'-' | b'_' | b'*') => b,
        _ => return false,
    };
    let mut count = 0;
    for &b in bytes {
        if b == marker {
            count += 1;
        } else if b != b' ' && b != b'\t' {
            return false;
        }
    }
    count >= 3
}

/// True when `trimmed` matches GFM §4.6 HTML block start conditions 1–6, which
/// interrupt paragraphs and GFM tables (type 7 does not interrupt tables).
fn starts_gfm_html_block(trimmed: &str) -> bool {
    const TYPE_1_TAGS: &[&str] = &["pre", "script", "style", "textarea"];
    const TYPE_6_TAGS: &[&str] = &[
        "address",
        "article",
        "aside",
        "base",
        "basefont",
        "blockquote",
        "body",
        "caption",
        "center",
        "col",
        "colgroup",
        "dd",
        "details",
        "dialog",
        "dir",
        "div",
        "dl",
        "dt",
        "fieldset",
        "figcaption",
        "figure",
        "footer",
        "form",
        "frame",
        "frameset",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "head",
        "header",
        "hr",
        "html",
        "iframe",
        "legend",
        "li",
        "link",
        "main",
        "menu",
        "menuitem",
        "nav",
        "noframes",
        "ol",
        "optgroup",
        "option",
        "p",
        "param",
        "search",
        "section",
        "summary",
        "table",
        "tbody",
        "td",
        "tfoot",
        "th",
        "thead",
        "title",
        "tr",
        "track",
        "ul",
    ];

    let Some(after_lt) = trimmed.strip_prefix('<') else {
        return false;
    };
    if after_lt.starts_with("!--") || after_lt.starts_with('?') || after_lt.starts_with("![CDATA[")
    {
        return true;
    }
    if after_lt.starts_with('!')
        && after_lt
            .as_bytes()
            .get(1)
            .is_some_and(u8::is_ascii_alphabetic)
    {
        return true;
    }

    let (is_closing, tag_start) = match after_lt.strip_prefix('/') {
        Some(rest) => (true, rest),
        None => (false, after_lt),
    };
    let tag_len = tag_start
        .bytes()
        .take_while(u8::is_ascii_alphanumeric)
        .count();
    if tag_len == 0 {
        return false;
    }
    let tag = &tag_start[..tag_len];
    let rest = &tag_start[tag_len..];

    if !is_closing
        && TYPE_1_TAGS.iter().any(|t| tag.eq_ignore_ascii_case(t))
        && (rest.is_empty() || rest.starts_with([' ', '\t', '>']))
    {
        return true;
    }
    TYPE_6_TAGS.iter().any(|t| tag.eq_ignore_ascii_case(t))
        && (rest.is_empty() || rest.starts_with([' ', '\t', '>']) || rest.starts_with("/>"))
}

/// Result of scanning a candidate GFM table row for unescaped `|` delimiters.
enum GfmRowSplit<'a> {
    /// At least one unescaped `|` was present; contains the trimmed cell slices.
    Piped(Vec<&'a str>),
    /// No unescaped `|` appeared on the line (valid pipe-free row candidate, e.g.
    /// a 1-column header, colon-aligned delimiter, or GFM Example 200 body row).
    NoUnescapedPipes,
    /// Line carried only a lone edge pipe (`|`) with no cells; callers must fail closed.
    EmptyPipeLine,
}

/// Splits a GFM table row into cells, honoring `\|` escapes and optional edge pipes.
///
/// Returns [`GfmRowSplit::Piped`] for piped rows, [`GfmRowSplit::NoUnescapedPipes`]
/// when no unescaped `|` exists, and [`GfmRowSplit::EmptyPipeLine`] for a bare lone
/// `|` that must fail closed.
fn split_gfm_row(line: &str) -> GfmRowSplit<'_> {
    let bytes = line.as_bytes();
    let mut pipes = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        if bytes[i] == b'|' {
            pipes.push(i);
        }
        i += 1;
    }
    if pipes.is_empty() {
        return GfmRowSplit::NoUnescapedPipes;
    }
    let has_leading_pipe = pipes[0] == 0;
    let has_trailing_pipe = pipes[pipes.len() - 1] == bytes.len() - 1;

    let start_idx = if has_leading_pipe { 1 } else { 0 };
    let end_idx = if has_trailing_pipe {
        bytes.len() - 1
    } else {
        bytes.len()
    };
    if start_idx > end_idx {
        return GfmRowSplit::EmptyPipeLine;
    }
    let inner = &line[start_idx..end_idx];

    let mut cells = Vec::new();
    let mut cell_start = 0;
    let inner_bytes = inner.as_bytes();
    let mut j = 0;
    while j < inner_bytes.len() {
        if inner_bytes[j] == b'\\' && j + 1 < inner_bytes.len() {
            j += 2;
            continue;
        }
        if inner_bytes[j] == b'|' {
            // Invariant: `cell_start` and `j` only land at `0` or on/just past an
            // ASCII `|` (`0x7C`). UTF-8 continuation bytes (`>= 0x80`) never
            // match `|` or `\`, so advancing past `|` always slices at valid
            // char boundaries.
            cells.push(inner[cell_start..j].trim());
            cell_start = j + 1;
        }
        j += 1;
    }
    cells.push(inner[cell_start..].trim());
    GfmRowSplit::Piped(cells)
}

fn is_gfm_separator_cell(cell: &str) -> bool {
    let stripped = cell.strip_prefix(':').unwrap_or(cell);
    let stripped = stripped.strip_suffix(':').unwrap_or(stripped);
    !stripped.is_empty() && stripped.bytes().all(|b| b == b'-')
}
