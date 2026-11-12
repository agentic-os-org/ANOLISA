//! Recognize complete Maven records without interpreting Java or plugin output.
//!
//! Like `pnpm`, this module handles one dialect end to end: detection on a
//! bounded sample, semantic roles, and lossless structural layout edits.
//! Download blocks share the repeated `[INFO] <action> from <repo>: <prefix>`
//! scaffolding while every artifact-relative path, receipt and duplicate is
//! retained in order. Diagnostics, warnings, and anything unparseable keep
//! their protection boundaries — an unparseable download-looking line closes
//! the eligible scope instead of reopening it.

use std::collections::HashSet;

use super::{LayoutEdit, LineRole, RoutineFamily, StringSpares, saves_both};

const MAX_LINE_BYTES: usize = 4096;
const MAX_DOWNLOAD_ROWS: usize = 256;
const MAX_DOWNLOAD_BYTES: usize = 64 * 1024;
const MAX_LAYOUT_EDITS: usize = 256;
const MIN_SHARED_PREFIX_BYTES: usize = 24;

const DOWNLOADING: &str = "Downloading from ";
const DOWNLOADED: &str = "Downloaded from ";
const INFO: &str = "[INFO] ";

pub(super) struct Plan {
    pub roles: Vec<LineRole>,
    pub run_starts: HashSet<usize>,
    pub layouts: Vec<LayoutEdit>,
    pub spares: StringSpares,
}

impl Plan {
    fn opaque(line_count: usize) -> Self {
        Self {
            roles: vec![LineRole::Unknown; line_count],
            run_starts: HashSet::new(),
            layouts: Vec::new(),
            spares: StringSpares::default(),
        }
    }
}

/// One parsed download record: the label (`Downloading`/`Downloaded`), the
/// repository id, the artifact URL path, and an optional verbatim receipt
/// suffix (`(12 kB at 210 kB/s)`) retained byte-for-byte.
struct Download<'a> {
    prefix: &'a str,
    action: &'a str,
    repository: &'a str,
    url: &'a str,
    receipt: Option<&'a str>,
}

pub(super) fn detect(sample: &[String]) -> bool {
    let mut completed = 0usize;
    let mut total = 0usize;
    for line in sample {
        if line.len() > MAX_LINE_BYTES {
            continue;
        }
        let record = line.strip_prefix(INFO).unwrap_or(line);
        if download(record).is_some() {
            total += 1;
            completed += usize::from(record.starts_with(DOWNLOADED));
        }
    }
    // A lone pair could be a log excerpt quoting a URL; three downloads with
    // at least one receipt is a dependency-resolution stream.
    total >= 3 && completed >= 1
}

pub(super) fn plan(lines: &[&str]) -> Plan {
    let mut plan = Plan::opaque(lines.len());
    if lines
        .iter()
        .any(|line| line.chars().any(|ch| ch.is_control() && ch != '\n'))
    {
        return plan;
    }
    let records = lines
        .iter()
        .map(|line| line.strip_suffix('\n').unwrap_or(line))
        .collect::<Vec<_>>();
    let mut index = 0;
    while index < lines.len() {
        let line = records[index];
        if !lines[index].ends_with('\n') || line.len() > MAX_LINE_BYTES {
            break;
        }
        if let Some(parsed) = download(line) {
            plan.roles[index] = LineRole::Routine(RoutineFamily::MavenDownload);
            // Extend over consecutive, complete, validated download records
            // that share the exact scaffolding (`prefix + action + repo`).
            let mut end = index + 1;
            let mut bytes = line.len() + 1;
            while end < lines.len()
                && lines[end].ends_with('\n')
                && records[end].len() <= MAX_LINE_BYTES
                && bytes + records[end].len() < MAX_DOWNLOAD_BYTES
            {
                let Some(next) = download(records[end]) else {
                    break;
                };
                if next.prefix != parsed.prefix
                    || next.repository != parsed.repository
                    || !same_ordinary_prefix(parsed.url, next.url)
                {
                    break;
                }
                bytes += records[end].len() + 1;
                plan.roles[end] = LineRole::Routine(RoutineFamily::MavenDownload);
                end += 1;
            }
            let rows = end - index;
            if (3..=MAX_DOWNLOAD_ROWS).contains(&rows)
                && let Some(replacement) = shared_prefix_block(&records[index..end])
            {
                let original = &lines[index..end].concat();
                if saves_both(original, &replacement) {
                    plan.roles[index..end].fill(LineRole::Routine(RoutineFamily::MavenDownload));
                    plan.layouts.push(LayoutEdit {
                        start: index,
                        end,
                        replacement: plan.spares.alloc(replacement),
                    });
                }
            }
            index = end;
        } else if module_banner(line) {
            plan.roles[index] = LineRole::Phase;
            index += 1;
        } else if line.is_empty() || build_outcome(line) || separator(line) {
            plan.roles[index] = LineRole::Summary;
            index += 1;
        } else {
            // Unknown content is opaque and also ends any layout edit
            // adjacency implicitly: nothing to do but advance.
            index += 1;
        }
        if plan.layouts.len() > MAX_LAYOUT_EDITS {
            return Plan::opaque(lines.len());
        }
    }
    plan
}

/// Parse one complete download record in either shape:
/// `[INFO] Downloading from central: https://…/path` (batch mode) or
/// `Downloading from central: https://…/path` (default mode). The optional
/// prefix is captured so compaction preserves it explicitly. A `Downloaded`
/// record may carry a parenthesized receipt after the URL; it is validated
/// strictly (parenthesized, printable, spaces allowed, no nested parens) and
/// kept verbatim.
fn download(line: &str) -> Option<Download<'_>> {
    let (prefix, rest) = if let Some(rest) = line.strip_prefix(INFO) {
        (INFO, rest)
    } else {
        ("", line)
    };
    let (action, rest) = if let Some(rest) = rest.strip_prefix(DOWNLOADED) {
        (DOWNLOADED, rest)
    } else {
        (DOWNLOADING, rest.strip_prefix(DOWNLOADING)?)
    };
    let (repository, remainder) = rest.split_once(": ")?;
    if !valid_repository(repository) {
        return None;
    }
    // A `Downloaded` record may end with a receipt: `… <url> (<bytes> at <rate>)`.
    // Split on the LAST ` (` whose tail is a strictly validated receipt so a
    // URL that merely contains ` (` still fails validation and stays opaque.
    let (url, receipt) =
        if action == DOWNLOADED && remainder.ends_with(')') && remainder.contains(" (") {
            match receipt_split(remainder) {
                Some((url, receipt)) => (url, Some(receipt)),
                None => (remainder, None),
            }
        } else {
            (remainder, None)
        };
    if !valid_url(url) {
        return None;
    }
    Some(Download {
        prefix,
        action,
        repository,
        url,
        receipt,
    })
}

/// Split `…url (receipt)` at the last ` (` whose remainder is a valid receipt.
fn receipt_split(remainder: &str) -> Option<(&str, &str)> {
    let open = remainder.rfind(" (")?;
    let url = &remainder[..open];
    let receipt = &remainder[open + 2..remainder.len() - 1];
    let valid_receipt = !receipt.is_empty()
        && receipt.len() <= 128
        && receipt
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        && !receipt.contains('(')
        && !receipt.contains(')')
        && receipt.contains(" at ");
    valid_receipt.then_some((url, receipt))
}

fn valid_repository(repository: &str) -> bool {
    !repository.is_empty()
        && repository.len() <= 128
        && repository
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'@'))
}

/// Ordinary URL characters: no whitespace, no control bytes, and no line
/// continuation semantics. Paths with anything unusual stay unparsed.
fn valid_url(url: &str) -> bool {
    !url.is_empty()
        && url.len() <= 2048
        && url
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'\\')
}

/// The longest shared prefix of two artifact URLs, cut at a `/` boundary and
/// never empty. Sharing stops before the last path segment, so each row keeps
/// a distinguishing tail.
fn shared_url_prefix<'a>(a: &'a str, b: &str) -> &'a str {
    let shared = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    let mut cut = 0;
    for (index, byte) in a[..shared].bytes().enumerate() {
        if byte == b'/' {
            cut = index + 1;
        }
    }
    &a[..cut]
}

fn same_ordinary_prefix(a: &str, b: &str) -> bool {
    shared_url_prefix(a, b).len() >= MIN_SHARED_PREFIX_BYTES
}

/// Build the compact block: one header declaring the shared scaffolding, then
/// one row per record retaining its action verb and its distinguishing URL
/// tail, in order.
fn shared_prefix_block(records: &[&str]) -> Option<String> {
    let first = download(records[0])?;
    let mut prefix = shared_url_prefix(first.url, first.url);
    for record in records.iter().skip(1) {
        let parsed = download(record)?;
        prefix = shared_url_prefix(prefix, parsed.url);
    }
    if prefix.len() < MIN_SHARED_PREFIX_BYTES || prefix.is_empty() {
        return None;
    }
    // Every row must keep a non-empty distinguishing tail.
    if records
        .iter()
        .any(|record| download(record).is_none_or(|parsed| parsed.url.len() <= prefix.len()))
    {
        return None;
    }
    let header = format!(
        "{}{} records from {}: {}",
        first.prefix,
        if records.iter().any(|record| record.contains(DOWNLOADING))
            && records.iter().any(|record| record.contains(DOWNLOADED))
        {
            "download"
        } else if records[0].contains(DOWNLOADING) {
            "downloading"
        } else {
            "downloaded"
        },
        first.repository,
        prefix,
    );
    let mut output = String::with_capacity(records.iter().map(|record| record.len()).sum());
    output.push_str(&header);
    output.push('\n');
    for record in records {
        let parsed = download(record)?;
        let verb = if parsed.action == DOWNLOADING {
            "get "
        } else {
            "ok "
        };
        output.push_str(parsed.prefix);
        output.push_str(verb);
        output.push_str(&parsed.url[prefix.len()..]);
        if let Some(receipt) = parsed.receipt {
            output.push_str(" (");
            output.push_str(receipt);
            output.push(')');
        }
        output.push('\n');
    }
    Some(output)
}

/// `[INFO] Building <name> <version>` module banners.
fn module_banner(line: &str) -> bool {
    let Some(rest) = line.strip_prefix(INFO) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix("Building ") else {
        return false;
    };
    let mut parts = rest.split_ascii_whitespace();
    let (name, version) = (parts.next(), parts.next());
    let valid_part = |value: Option<&str>| {
        value.is_some_and(|value| {
            !value.is_empty()
                && value.len() <= 214
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
    };
    valid_part(name) && valid_part(version) && parts.next().is_none()
}

/// `BUILD SUCCESS` / `BUILD FAILURE` terminal records.
fn build_outcome(line: &str) -> bool {
    line.strip_prefix(INFO) == Some("BUILD SUCCESS")
        || line.strip_prefix(INFO) == Some("BUILD FAILURE")
}

/// The `---` rule lines framing the Maven summary.
fn separator(line: &str) -> bool {
    line.len() >= 4 && line.bytes().all(|byte| byte == b'-' || byte == b'=')
}
