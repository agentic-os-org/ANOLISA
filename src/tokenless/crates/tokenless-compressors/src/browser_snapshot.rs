//! Lossless re-encoding of Playwright MCP accessibility snapshots and `browser_find` matches.
//!
//! Playwright's fenced YAML aria tree (`- role "name" [ref=eN]`) and
//! `browser_find` responses (`### Result` + `----`-separated aria subtrees)
//! escape embedded quotes in YAML scalars, so record boundaries are explicit
//! and flatten into a TSV table losslessly: redundant cursor annotations on
//! interactive roles are dropped and repeated URL prefixes share a legend. For
//! multi-block `browser_find` results, the `----` separators move into a `block`
//! column rather than being echoed verbatim. Every element reference,
//! role/name binding, URL, and text byte is retained; the Runtime stashes the
//! original response for retrieval. Any parse or fidelity failure returns
//! `None` so the caller passes the input through unchanged.
//!
//! chrome-devtools-mcp uid snapshots are recognized by the detector but never
//! compressed: upstream wraps names and attribute values in raw quotes without
//! escaping embedded quotes or newlines, so a name can be byte-identical to a
//! different record split and no parser can establish those boundaries. CDT
//! snapshots therefore pass through unchanged.

mod encode;
mod find;
mod pw;

pub use pw::{fold_advance, is_pw_element_line, name_continues, pw_element_ref};

use tokenless_protocol::estimate_tokens;

/// Flattened accessibility-tree node. `reference` holds the Playwright `ref`
/// and `text` carries Playwright node content.
#[derive(Clone, PartialEq)]
pub(crate) struct Record {
    pub block: usize,
    pub depth: usize,
    pub role: String,
    pub name: String,
    pub reference: String,
    pub attrs: String,
    pub url: String,
    pub text: String,
}

/// Verbatim wrapper pieces plus the flattened tree.
pub(crate) struct Parsed {
    pub prefix: String,
    pub suffix: String,
    pub records: Vec<Record>,
    pub use_block_column: bool,
}

/// Scans a double-quoted scalar from the start of `s`, backslash-aware.
///
/// Returns the raw inner bytes (escapes and embedded newlines preserved) and
/// the byte index just past the closing quote.
pub(crate) fn scan_double_quoted(s: &str) -> Option<(String, usize)> {
    let mut chars = s.char_indices();
    if chars.next()?.1 != '"' {
        return None;
    }
    let mut out = String::new();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                let (_, next) = chars.next()?;
                out.push('\\');
                out.push(next);
            }
            '"' => return Some((out, i + 1)),
            _ => out.push(c),
        }
    }
    None
}

/// A diff of a snapshot file carries snapshot anchors in its context rows;
/// the diff domain owns those, so both parsers reject diff-shaped wrappers.
pub(crate) fn is_diff_wrapper(prefix: &str) -> bool {
    prefix.contains("diff --git ")
        || prefix.starts_with("--- ")
        || prefix.starts_with("+++ ")
        || prefix.starts_with("@@ ")
}

/// True for chrome-devtools-mcp uid listings, recognized by head anchor or
/// uid-dominant shape over the first 50 non-empty lines.
///
/// Shared by the Runtime detector (which labels these `browser_snapshot`) and
/// the compressor (which fails open on them). The compressor must reject the
/// whole listing by shape rather than let `pw::parse` search it for a YAML
/// fence: a CDT name may embed PW-shaped rows, and compressing that fragment
/// would both alter the CDT bytes and present literal name text as element
/// references.
pub fn is_cdt_listing(input: &str) -> bool {
    let lines: Vec<&str> = input
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(50)
        .collect();
    if let Some(pos) = lines
        .iter()
        .take(10)
        .position(|l| *l == "## Latest page snapshot")
        && lines.get(pos + 1).is_some_and(|l| is_uid_line(l))
    {
        return true;
    }
    if lines.len() < 3 || !lines.first().is_some_and(|l| is_uid_line(l)) {
        return false;
    }
    let uid_lines = lines.iter().filter(|l| is_uid_line(l)).count();
    uid_lines >= 3 && uid_lines * 5 >= lines.len() * 4
}

fn is_uid_line(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("uid=") else {
        return false;
    };
    let token_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let Some((generation, sequence)) = rest[..token_end].split_once('_') else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    digits(generation)
        && digits(sequence)
        && matches!(rest[token_end..].chars().next(), None | Some(' '))
}

/// Stateless compressor for browser accessibility snapshots.
pub struct BrowserSnapshotCompressor;

impl BrowserSnapshotCompressor {
    /// Returns a lossless record-table view, or `None` to keep the input.
    ///
    /// Fails open: chrome-devtools uid listings, unrecognized shapes, parse
    /// errors, reconstruction mismatches, and candidates without a net
    /// byte-and-token benefit all return `None`.
    #[must_use]
    pub fn compress(&self, input: &str) -> Option<String> {
        if is_cdt_listing(input) {
            return None;
        }
        let parsed = pw::parse(input).or_else(|| find::parse(input))?;
        let original = parsed.records;
        let mut records = original.clone();
        encode::strip_redundant_cursor(&mut records);
        let cols: &[encode::Col] = if parsed.use_block_column {
            encode::FIND_COLS
        } else {
            encode::PW_COLS
        };
        let mut table = encode::table(&records, cols);
        let mut legend_text = String::new();
        let mut legend: Vec<(String, String)> = Vec::new();
        if let Some(shared) = encode::share_urls(&records) {
            let shared_table = encode::table(&shared.records, cols);
            let candidate = format!("{}\n{shared_table}", shared.legend_text);
            if candidate.len() < table.len()
                && estimate_tokens(&candidate) < estimate_tokens(&table)
            {
                table = shared_table;
                legend_text = shared.legend_text;
                legend = shared.legend;
                records = shared.records;
            }
        }
        if !encode::roundtrip_ok(&original, &records, &table, cols, &legend) {
            return None;
        }
        let output = if legend_text.is_empty() {
            format!("{}{table}{}", parsed.prefix, parsed.suffix)
        } else {
            format!("{}{legend_text}\n{table}{}", parsed.prefix, parsed.suffix)
        };
        (output.len() < input.len() && estimate_tokens(&output) < estimate_tokens(input))
            .then_some(output)
    }
}

#[cfg(test)]
mod tests;
