//! Record-table encoding, cursor pruning, URL prefix sharing, and fidelity checks.

use std::collections::BTreeMap;

use super::Record;

/// Table columns for the Playwright record view.
#[derive(Clone, Copy)]
pub(crate) enum Col {
    Block,
    Depth,
    Role,
    Name,
    Ref,
    Attrs,
    Url,
    Text,
}

pub(crate) const PW_COLS: &[Col] = &[
    Col::Depth,
    Col::Role,
    Col::Name,
    Col::Ref,
    Col::Attrs,
    Col::Url,
    Col::Text,
];

pub(crate) const FIND_COLS: &[Col] = &[
    Col::Block,
    Col::Depth,
    Col::Role,
    Col::Name,
    Col::Ref,
    Col::Attrs,
    Col::Url,
    Col::Text,
];

impl Col {
    fn header(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Depth => "depth",
            Self::Role => "role",
            Self::Name => "name",
            Self::Ref => "ref",
            Self::Attrs => "attrs",
            Self::Url => "url",
            Self::Text => "text",
        }
    }

    fn value(self, record: &Record) -> String {
        match self {
            Self::Block => record.block.to_string(),
            Self::Depth => record.depth.to_string(),
            Self::Role => record.role.clone(),
            Self::Name => record.name.clone(),
            Self::Ref => record.reference.clone(),
            Self::Attrs => record.attrs.clone(),
            Self::Url => record.url.clone(),
            Self::Text => record.text.clone(),
        }
    }
}

/// Escapes backslash, tab, and newline so every record occupies one TSV row.
pub(crate) fn escape_cell(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

/// Single-pass inverse of [`escape_cell`]; sequential replaces would corrupt
/// literal backslash-n sequences.
pub(crate) fn unescape_cell(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn header_line(cols: &[Col]) -> String {
    cols.iter()
        .map(|c| c.header())
        .collect::<Vec<_>>()
        .join("\t")
}

/// Renders records as a header plus one escaped TSV row per record.
pub(crate) fn table(records: &[Record], cols: &[Col]) -> String {
    let mut out = header_line(cols);
    for record in records {
        out.push('\n');
        out.push_str(
            &cols
                .iter()
                .map(|c| escape_cell(&c.value(record)))
                .collect::<Vec<_>>()
                .join("\t"),
        );
    }
    out
}

/// Roles whose cursor=pointer annotation is implied by the role itself.
/// On generic containers the annotation is a clickability signal and stays.
const INTERACTIVE: &[&str] = &[
    "link",
    "button",
    "checkbox",
    "radio",
    "textbox",
    "searchbox",
    "combobox",
    "listbox",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "tab",
    "switch",
    "option",
    "slider",
    "spinbutton",
    "treeitem",
];

/// Removes cursor=pointer only where the role already implies interactivity.
pub(crate) fn strip_redundant_cursor(records: &mut [Record]) {
    for record in records.iter_mut() {
        if INTERACTIVE.contains(&record.role.as_str())
            && record
                .attrs
                .split_whitespace()
                .any(|a| a == "cursor=pointer")
        {
            record.attrs = without_cursor_pointer(&record.attrs);
        }
    }
}

/// Attrs with any cursor=pointer token removed, whitespace-normalized.
fn without_cursor_pointer(attrs: &str) -> String {
    attrs
        .split_whitespace()
        .filter(|a| *a != "cursor=pointer")
        .collect::<Vec<_>>()
        .join(" ")
}

/// URL-shared candidate: encoded records plus the legend that restores them.
pub(crate) struct Shared {
    pub records: Vec<Record>,
    pub legend_text: String,
    pub legend: Vec<(String, String)>,
}

/// Directory-style prefix: up to the last slash, never shorter than the
/// scheme://host/ root so a bare host is not split mid-token.
fn url_prefix(url: &str) -> &str {
    match url.rfind('/') {
        // Bound past the shortest scheme://host/ root so the host is never split.
        Some(i) if i > 8 => &url[..=i],
        _ => url,
    }
}

fn is_legend_id(s: &str) -> bool {
    s.strip_prefix('U')
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// True for URLs that could be confused with legend references (U1/...).
fn looks_like_legend_ref(url: &str) -> bool {
    let Some(rest) = url.strip_prefix('U') else {
        return false;
    };
    let digits = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
    digits > 0 && matches!(rest[digits..].chars().next(), None | Some('/'))
}

/// Shares prefixes used by at least two URLs behind a U1=... legend.
///
/// Returns `None` when nothing is shared or any original URL could collide
/// with a legend reference; the caller still applies a net-benefit check.
pub(crate) fn share_urls(records: &[Record]) -> Option<Shared> {
    if records.iter().any(|r| looks_like_legend_ref(&r.url)) {
        return None;
    }
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for record in records {
        if !record.url.is_empty() {
            *counts.entry(url_prefix(&record.url)).or_default() += 1;
        }
    }
    let mut shared: Vec<(&str, usize)> = counts.into_iter().filter(|(_, n)| *n >= 2).collect();
    shared.sort_by(|a, b| {
        (b.1 * b.0.len())
            .cmp(&(a.1 * a.0.len()))
            .then_with(|| a.0.cmp(b.0))
    });
    if shared.is_empty() {
        return None;
    }
    let legend: Vec<(String, String)> = shared
        .iter()
        .enumerate()
        .map(|(i, (prefix, _))| (format!("U{}", i + 1), (*prefix).to_owned()))
        .collect();
    let ids: BTreeMap<&str, &str> = legend
        .iter()
        .map(|(id, prefix)| (prefix.as_str(), id.as_str()))
        .collect();
    let encoded = records
        .iter()
        .map(|record| {
            let mut out = record.clone();
            if !record.url.is_empty() {
                let prefix = url_prefix(&record.url);
                if let Some(id) = ids.get(prefix) {
                    let suffix = &record.url[prefix.len()..];
                    out.url = if suffix.is_empty() {
                        (*id).to_owned()
                    } else {
                        format!("{id}/{suffix}")
                    };
                }
            }
            out
        })
        .collect();
    let legend_text = legend
        .iter()
        .map(|(id, prefix)| format!("{id}={prefix}"))
        .collect::<Vec<_>>()
        .join("\n");
    Some(Shared {
        records: encoded,
        legend_text,
        legend,
    })
}

/// Restores a legend-encoded URL cell to its original value.
pub(crate) fn decode_url(cell: &str, legend: &[(String, String)]) -> String {
    if let Some((id, suffix)) = cell.split_once('/')
        && is_legend_id(id)
        && let Some((_, prefix)) = legend.iter().find(|(lid, _)| lid == id)
    {
        return format!("{prefix}{suffix}");
    }
    if is_legend_id(cell)
        && let Some((_, prefix)) = legend.iter().find(|(lid, _)| lid == cell)
    {
        return prefix.clone();
    }
    cell.to_owned()
}

/// Verifies encoding fidelity before a candidate may be emitted.
///
/// Checks that every table cell unescapes to the encoded record field, that
/// legend decoding restores every original URL, and that the transforms left
/// all other fields untouched. Order and count must match. `attrs` is checked
/// against the original too: equal to the original with at most the
/// cursor=pointer token removed.
pub(crate) fn roundtrip_ok(
    original: &[Record],
    encoded: &[Record],
    table: &str,
    cols: &[Col],
    legend: &[(String, String)],
) -> bool {
    if original.len() != encoded.len() {
        return false;
    }
    let mut lines = table.split('\n');
    if lines.next() != Some(header_line(cols).as_str()) {
        return false;
    }
    for (idx, line) in lines.enumerate() {
        let (Some(orig), Some(enc)) = (original.get(idx), encoded.get(idx)) else {
            return false;
        };
        let cells: Vec<String> = line.split('\t').map(unescape_cell).collect();
        if cells.len() != cols.len() {
            return false;
        }
        for (col, cell) in cols.iter().zip(&cells) {
            if *cell != col.value(enc) {
                return false;
            }
            match col {
                Col::Url => {
                    if decode_url(cell, legend) != orig.url {
                        return false;
                    }
                }
                Col::Attrs => {
                    let orig_attrs = col.value(orig);
                    if *cell != orig_attrs && *cell != without_cursor_pointer(&orig_attrs) {
                        return false;
                    }
                }
                _ => {
                    if col.value(orig) != *cell {
                        return false;
                    }
                }
            }
        }
    }
    true
}
