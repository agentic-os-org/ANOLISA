//! Playwright MCP aria-snapshot parser: fenced YAML tree under a markdown
//! page wrapper. The wrapper and any trailer are preserved verbatim; the tree
//! is flattened into records with `/url` children attached to their parent.

use super::{Parsed, Record, is_diff_wrapper, scan_double_quoted};

/// Parses a Playwright snapshot response, or `None` for any other shape.
pub(crate) fn parse(input: &str) -> Option<Parsed> {
    // Anchor on the `### Snapshot` section's fence so an earlier fenced YAML
    // block (e.g. under `### Ran Playwright code`) stays verbatim in the
    // prefix. Fall back to the first fence for wrappers without the heading.
    let mut from = 0;
    let mut off = 0;
    for line in input.split_inclusive('\n') {
        if line.trim_end() == "### Snapshot" {
            from = off + line.len();
            break;
        }
        off += line.len();
    }
    let open = from + input[from..].find("```yaml\n")?;
    let body_start = open + "```yaml\n".len();
    let close = body_start + input[body_start..].find("\n```")?;
    let prefix = input[..open].to_owned();
    let suffix = input[close + "\n```".len()..].to_owned();
    if is_diff_wrapper(&prefix) {
        return None;
    }
    let body = &input[body_start..close];
    let ParsedTreeBody { items, records } = parse_tree_body(body)?;

    let rebuilt = format!(
        "{prefix}```yaml\n{}\n```{suffix}",
        items
            .iter()
            .map(|(_, raw)| raw.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    if rebuilt != input {
        return None;
    }
    Some(Parsed {
        prefix,
        suffix,
        records,
        use_block_column: false,
    })
}

/// Folded raw line pairs paired with parsed aria records for one tree body.
pub(super) struct ParsedTreeBody {
    pub(super) items: Vec<(String, String)>,
    pub(super) records: Vec<Record>,
}

/// Parses one Playwright aria tree body into folded raw line pairs and records.
pub(super) fn parse_tree_body(body: &str) -> Option<ParsedTreeBody> {
    parse_tree_body_with_gaps(body, false)
}

/// Parses one Playwright `browser_find` tree body, accepting indented `...`
/// context-gap markers emitted when omitted sibling/off-path rows are elided.
pub(super) fn parse_find_tree_body(body: &str) -> Option<ParsedTreeBody> {
    parse_tree_body_with_gaps(body, true)
}

fn parse_tree_body_with_gaps(body: &str, allow_gaps: bool) -> Option<ParsedTreeBody> {
    // Folded multi-line quoted names become one parse item; raw spans keep
    // the exact source bytes so reconstruction stays byte-exact. Folding is
    // driven by the name scalar's own closure, not a whole-row quote count: a
    // literal quote in the inline value (after `:`) must not fold subsequent
    // elements into this row's text. CRLF falls open: a trailing \r lands in
    // the tail and breaks reconstruction.
    let mut items: Vec<(String, String)> = Vec::new();
    let mut folded = String::new();
    for line in body.split('\n') {
        if fold_advance(&mut folded, line) {
            if let Some(prev) = items.last_mut() {
                prev.0.push(' ');
                prev.0.push_str(line.trim());
                prev.1.push('\n');
                prev.1.push_str(line);
            }
            continue;
        }
        if name_continues(line) {
            folded = line.to_owned();
        }
        items.push((line.to_owned(), line.to_owned()));
    }

    let mut records: Vec<Record> = Vec::new();
    for (line, _) in &items {
        if line.trim().is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start_matches(' ').len();
        let trimmed = line.trim_start_matches(' ');
        if !indent.is_multiple_of(2) {
            return None;
        }
        let depth = indent / 2;
        if allow_gaps && trimmed == "..." {
            records.push(Record {
                block: 0,
                depth,
                role: "...".to_owned(),
                name: String::new(),
                reference: String::new(),
                attrs: String::new(),
                url: String::new(),
                text: String::new(),
            });
            continue;
        }
        let rest = trimmed.strip_prefix("- ")?;
        if let Some(url) = rest.strip_prefix("/url: ") {
            let parent_depth = depth.checked_sub(1)?;
            let parent = records.iter_mut().rev().find(|r| r.depth <= parent_depth)?;
            // Reject orphan /url rows whose enclosing record is not at the
            // immediate parent depth, is a structural `...` gap marker, or
            // already carries a URL (keeping the last would drop a URL).
            if parent.depth != parent_depth || parent.role == "..." || !parent.url.is_empty() {
                return None;
            }
            parent.url = url.to_owned();
            continue;
        }
        if let Some(text) = rest.strip_prefix("text: ") {
            records.push(Record {
                block: 0,
                depth,
                role: "text".to_owned(),
                name: String::new(),
                reference: String::new(),
                attrs: String::new(),
                url: String::new(),
                text: text.to_owned(),
            });
            continue;
        }
        let (role, name, attrs, reference, text) = parse_element(rest)?;
        records.push(Record {
            block: 0,
            depth,
            role,
            name,
            reference,
            attrs,
            url: String::new(),
            text,
        });
    }

    Some(ParsedTreeBody { items, records })
}

/// True when a parse item opens a double-quoted accessible name that has not
/// closed by end of line, so the next raw line continues the name.
///
/// Scans only the name scalar with the same backslash-aware scanner used to
/// parse it. A complete name followed by a literal quote in the inline YAML
/// value (e.g. `: Width 10"`) or in attributes does not continue: counting
/// quotes across the whole row would fold subsequent actionable elements into
/// this row's text cell.
pub fn name_continues(s: &str) -> bool {
    let Some(rest) = s.trim_start().strip_prefix("- ") else {
        return false;
    };
    let role_end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(rest.len());
    if role_end == 0 || !rest[role_end..].starts_with(" \"") {
        return false;
    }
    scan_double_quoted(&rest[role_end + 1..]).is_none()
}

/// Continues an open multi-line double-quoted element name across `line`,
/// clearing `folded` when the closing quote arrives.
pub fn fold_advance(folded: &mut String, line: &str) -> bool {
    if folded.is_empty() {
        return false;
    }
    folded.push(' ');
    folded.push_str(line.trim());
    if !name_continues(folded) {
        folded.clear();
    }
    true
}

/// Splits a single-quoted YAML scalar, honoring `''` escapes.
fn split_single_quoted(s: &str) -> Option<(String, &str)> {
    let mut chars = s.char_indices();
    if chars.next()?.1 != '\'' {
        return None;
    }
    let mut out = String::new();
    while let Some((i, c)) = chars.next() {
        if c == '\'' {
            if s[i + 1..].starts_with('\'') {
                out.push('\'');
                chars.next();
                continue;
            }
            return Some((out, &s[i + 1..]));
        }
        out.push(c);
    }
    None
}

type Element = (String, String, String, String, String);

/// Parses `ROLE "name" [attrs]...[: text]`, optionally wrapped as a whole
/// single-quoted YAML scalar when the name contains mapping specials.
fn parse_element(rest: &str) -> Option<Element> {
    if rest.starts_with('\'') {
        let (inner, after) = split_single_quoted(rest)?;
        let text = match after {
            "" => String::new(),
            ":" => String::new(),
            _ => after.strip_prefix(": ")?.to_owned(),
        };
        let (role, name, attrs, reference, tail) = scan_role_name_attrs(&inner)?;
        if !tail.is_empty() {
            return None;
        }
        return Some((role, name, attrs, reference, text));
    }
    let (role, name, attrs, reference, tail) = scan_role_name_attrs(rest)?;
    let text = match tail {
        "" | ":" => String::new(),
        _ => tail.strip_prefix(": ")?.to_owned(),
    };
    Some((role, name, attrs, reference, text))
}

/// Consumes role, optional quoted name, and the bracket-attribute sequence;
/// returns the unconsumed tail plus fields with `ref=` lifted out of attrs.
fn scan_role_name_attrs(s: &str) -> Option<(String, String, String, String, &str)> {
    let role_end = s
        .find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(s.len());
    if role_end == 0 {
        return None;
    }
    let role = s[..role_end].to_owned();
    let mut pos = role_end;
    let mut name = String::new();
    if s[pos..].starts_with(" \"") {
        let (inner, next) = scan_double_quoted(&s[pos + 1..])?;
        name = inner;
        pos += 1 + next;
    }
    let mut attrs: Vec<String> = Vec::new();
    while s[pos..].starts_with(" [") {
        let close = pos + 1 + s[pos + 1..].find(']')?;
        attrs.push(s[pos + 2..close].to_owned());
        pos = close + 1;
    }
    let reference = attrs
        .iter()
        .find(|a| a.starts_with("ref="))
        .map(|a| a[4..].to_owned())
        .unwrap_or_default();
    let attrs = attrs
        .iter()
        .filter(|a| !a.starts_with("ref="))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    Some((role, name, attrs, reference, &s[pos..]))
}

/// True when `line` is a syntactically valid Playwright element line (not
/// `/url:` or `text:`), with or without `[ref=...]`.
pub fn is_pw_element_line(line: &str) -> bool {
    let Some(rest) = line.trim_start_matches(' ').strip_prefix("- ") else {
        return false;
    };
    !rest.starts_with("/url: ") && !rest.starts_with("text: ") && parse_element(rest).is_some()
}

/// Extracts a well-formed `eN` or `fNeM` `[ref=...]` attribute outside the
/// quoted accessible name scalar.
pub fn pw_element_ref(line: &str) -> Option<String> {
    let rest = line.trim_start_matches(' ').strip_prefix("- ")?;
    if rest.starts_with("/url: ") || rest.starts_with("text: ") {
        return None;
    }
    let (_, _, _, reference, _) = parse_element(rest)?;
    is_valid_ref(&reference).then_some(reference)
}

fn is_valid_ref(value: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if let Some(rest) = value.strip_prefix('e') {
        return digits(rest);
    }
    let Some((frame, element)) = value.strip_prefix('f').and_then(|r| r.split_once('e')) else {
        return false;
    };
    digits(frame) && digits(element)
}
