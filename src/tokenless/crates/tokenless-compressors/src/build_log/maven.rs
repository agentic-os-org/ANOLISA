//! Share literal Maven formatting while retaining each record and its position.
//!
//! Unknown output closes a scope. Only a complete Maven phase, module or test banner
//! can reopen it; transfer-looking text cannot escape a diagnostic body.

use std::sync::LazyLock;

use regex::Regex;

use super::{LayoutEdit, saves_both};

const MAX_ROWS: usize = 256;
const MAX_BYTES: usize = 64 * 1024;
const MAX_EDITS: usize = 256;
const HEADER: &str = "Maven templates (id<TAB>fields; interleave with literals): ";

// Captures are the variable fields; all intervening bytes become shared
// literals. Keep these grammars anchored: arbitrary INFO text is not evidence.
static PATTERNS: LazyLock<Result<Vec<Regex>, regex::Error>> = LazyLock::new(|| {
    [
        r"^\[INFO\] --- ([A-Za-z0-9_.-]+(?::[A-Za-z0-9_.-]+){1,3}) \(([^()\t\r\n]+)\) @ ([A-Za-z0-9_.-]+) ---$",
        r"^\[INFO\] Running ([A-Za-z_$][A-Za-z0-9_.$]*)$",
        r"^\[INFO\] Tests run: ([0-9]+), Failures: ([0-9]+), Errors: ([0-9]+), Skipped: ([0-9]+), Time elapsed: ([0-9]+(?:\.[0-9]+)?) s (?:-- in|- in) ([A-Za-z_$][A-Za-z0-9_.$]*)$",
        r"^\[INFO\] Tests run: ([0-9]+), Failures: ([0-9]+), Errors: ([0-9]+), Skipped: ([0-9]+)$",
        r"^\[INFO\] ([A-Za-z_$][A-Za-z0-9_.$]*\.[A-Za-z_$][A-Za-z0-9_$]*(?:\([^\t\r\n]*\))?) (?:-- | )Time elapsed: ([0-9]+(?:\.[0-9]+)?) s$",
        r"^\[INFO\] Building ([^\t\r\n]+?)(?: +\[([0-9]+/[0-9]+)\])?$",
        r"^\[INFO\]   from ([^\t\r\n]+)$",
        r"^\[INFO\] -+< ([A-Za-z0-9_.-]+:[A-Za-z0-9_.-]+) >-+$",
        r"^\[INFO\] -+\[ ([A-Za-z0-9_.-]+) \]-+$",
        r"^\[INFO\] ([^\t\r\n]+?) +\.{3,} (SUCCESS|FAILURE|SKIPPED)(?: +\[ *([0-9]+(?:\.[0-9]+)? (?:s|min|h)) *\])?$",
        r"^\[INFO\] ([^\t\r\n]+?) +\[([A-Za-z0-9_.-]+)\]$",
        r"^\[INFO\] Reactor Summary for ([^\t\r\n]+):$",
        r"^\[INFO\] (?:Total time: +|Finished at: )([^\t\r\n]+)$",
        r"^\[INFO\] Rule ([0-9]+): ([A-Za-z0-9_.$]+) passed$",
        r"^\[INFO\] (?:Checking|Skipping) ([A-Za-z0-9_.-]+)(?: for updates)?$",
        r"^\[INFO\] (?:Copying|Copied) ([0-9]+) resources? from ([^\t\r\n]+) to ([^\t\r\n]+)$",
        r"^\[INFO\] (?:Compiling|Recompiling) ([0-9]+) source files?(?: with javac)?(?: \[[^\t\r\n]+\])? to ([^\t\r\n]+)$",
        r"^\[INFO\] (?:Building jar: |Building zip: |skip non existing resourceDirectory )([^\t\r\n]+)$",
        r"^\[INFO\] Using (?:auto detected|configured) provider ([A-Za-z0-9_.$]+)$",
    ]
    .into_iter()
    // Invalid developer-authored grammars fail closed; the grammar test reports them.
    .map(Regex::new)
    .collect()
});

static TRANSFER_SIZE: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"^[0-9]+(?:\.[0-9]+)?(?:/[0-9]+(?:\.[0-9]+)?)? (?:B|kB|MB|GB)$"));
static RECEIPT: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| {
    Regex::new(r"^ \([0-9]+(?:\.[0-9]+)? (?:B|kB|MB|GB) at [0-9]+(?:\.[0-9]+)? (?:B|kB|MB|GB)/s\)$")
});

struct Record {
    literals: Vec<String>,
    fields: Vec<String>,
    // Only URL directories and qualified method names share a field prefix.
    prefix_field: Option<(usize, char)>,
}

impl Record {
    fn info(body: &str) -> Self {
        Self {
            literals: vec!["[INFO]".into(), String::new()],
            fields: vec![body.into()],
            prefix_field: None,
        }
    }
}

/// Require related Maven records in a log-shaped bounded sample.
pub(super) fn detect(sample: &[String]) -> bool {
    let Ok(patterns) = PATTERNS.as_ref() else {
        return false;
    };
    // A quoted example is not a tool log. Keep head inspection inside the head
    // window so a sample gap never hides an enclosing document or spans preambles.
    let (head, tail) = match sample.iter().position(|line| line == "\0") {
        Some(gap) => (&sample[..gap], Some(&sample[gap + 1..])),
        None => (sample, None),
    };
    let first = head
        .iter()
        .find(|line| !line.trim().is_empty() && !is_preamble(line));
    if first.is_none_or(|line| !is_log_head(line)) {
        return false;
    }
    let head_fences = unfenced_runs(head);
    if head_fences
        .runs
        .iter()
        .any(|run| has_strong_evidence(run, patterns))
    {
        return true;
    }
    // Across a sample gap, fence state from the unsampled middle is unknown;
    // accept tail evidence only when neither window carries an open/closing fence.
    tail.is_some_and(|tail| {
        !head_fences.unclosed && {
            let tail_fences = unfenced_runs(tail);
            !tail_fences.saw_fence
                && tail_fences
                    .runs
                    .iter()
                    .any(|run| has_strong_evidence(run, patterns))
        }
    })
}

fn is_preamble(line: &str) -> bool {
    if line.len() > MAX_BYTES || !plain(line) {
        return false;
    }
    line.starts_with("$ ")
        || is_maven_xtrace(line)
        || line.starts_with("Picked up JAVA_TOOL_OPTIONS: ")
        || line.starts_with("Picked up _JAVA_OPTIONS: ")
        || line.starts_with("NOTE: Picked up JDK_JAVA_OPTIONS: ")
        || is_jvm_warning(line)
        || is_launcher_banner(line)
}

fn is_maven_xtrace(line: &str) -> bool {
    let pluses = line.bytes().take_while(|&b| b == b'+').count();
    if !matches!(pluses, 1 | 2) {
        return false;
    }
    let Some(rest) = line[pluses..].strip_prefix(' ') else {
        return false;
    };
    let Some(command) = rest
        .split_ascii_whitespace()
        .find(|token| !is_env_assignment(token))
    else {
        return false;
    };
    let binary = command.rsplit('/').next().unwrap_or(command);
    matches!(binary, "mvn" | "mvnw" | "mvnDebug" | "mvnd")
}

fn is_env_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name.as_bytes()[0].is_ascii_alphabetic()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn is_jvm_warning(line: &str) -> bool {
    if (line.starts_with("OpenJDK ") || line.starts_with("Java HotSpot(TM) "))
        && line.contains(" VM warning: ")
    {
        return true;
    }
    let Some(rest) = line.strip_prefix("WARNING: ") else {
        return false;
    };
    [
        "An illegal reflective access operation has occurred",
        "Illegal reflective access by ",
        "Please consider reporting this to the maintainers of ",
        "Use --illegal-access=",
        "All illegal access operations will be denied in a future release",
        "A terminally deprecated method in ",
        "A restricted method in ",
        "Use --enable-native-access=",
        "Restricted methods will be blocked in a future release",
        "sun.misc.Unsafe::",
        "java.lang.System::",
        "java.lang.Runtime::",
        "Unknown module: ",
    ]
    .iter()
    .any(|prefix| rest.starts_with(prefix))
        || (rest.starts_with("package ") && rest.contains(" not in "))
}

fn is_launcher_banner(line: &str) -> bool {
    line.starts_with("Apache Maven ")
        || line.starts_with("Maven home: ")
        || (line.starts_with("Java version: ") && line.contains(", vendor: "))
        || line.starts_with("Java home: ")
        || (line.starts_with("Default locale: ") && line.contains(", platform encoding: "))
        || (line.starts_with("OS name: \"") && line.contains("\", version: \""))
        || line.starts_with("[mvnd] ")
}

fn is_log_head(line: &str) -> bool {
    if line.len() > MAX_BYTES {
        return false;
    }
    for tag in ["[INFO]", "[WARNING]", "[ERROR]"] {
        if let Some(rest) = line.strip_prefix(tag)
            && (rest.is_empty() || rest.starts_with(' '))
        {
            return plain(line);
        }
    }
    (plain(line) && (line.starts_with(HEADER) || is_download_line(line))) || raw_transfer(line)
}

struct Unfenced<'a> {
    runs: Vec<&'a [String]>,
    saw_fence: bool,
    unclosed: bool,
}

fn unfenced_runs(segment: &[String]) -> Unfenced<'_> {
    let mut runs = Vec::new();
    let mut saw_fence = false;
    let mut open_fence: Option<(u8, usize)> = None;
    let mut run_start = 0;
    for (index, line) in segment.iter().enumerate() {
        let trimmed = line.trim_start();
        let marker = (*b"`~").into_iter().find_map(|ch| {
            let count = trimmed.bytes().take_while(|&b| b == ch).count();
            (count >= 3).then_some((ch, count, &trimmed[count..]))
        });
        if let Some((open_ch, open_len)) = open_fence {
            if let Some((ch, count, rest)) = marker
                && ch == open_ch
                && count >= open_len
                && rest.trim().is_empty()
            {
                open_fence = None;
                run_start = index + 1;
            }
        } else if let Some((ch, count, _)) = marker {
            saw_fence = true;
            if run_start < index {
                runs.push(&segment[run_start..index]);
            }
            open_fence = Some((ch, count));
        }
    }
    if open_fence.is_none() && run_start < segment.len() {
        runs.push(&segment[run_start..]);
    }
    Unfenced {
        runs,
        saw_fence,
        unclosed: open_fence.is_some(),
    }
}

fn has_strong_evidence(run: &[String], patterns: &[Regex]) -> bool {
    let valid = |line: &str| line.len() <= MAX_BYTES && plain(line);
    run.iter()
        .any(|line| valid(line) && line.starts_with(HEADER))
        || run.windows(2).any(|rows| {
            rows.iter().all(|line| valid(line))
                && patterns[0].is_match(&rows[0])
                && (known(&rows[1]).is_some()
                    || download(&rows[1]).is_some()
                    || patterns[2].is_match(&rows[1]))
        })
        || run.windows(4).any(|rows| {
            rows.iter().all(|line| valid(line))
                && ([7, 5, 6, 8]
                    .iter()
                    .zip(rows)
                    .all(|(pattern, line)| patterns[*pattern].is_match(line))
                    || (matches!(
                        rows[0].as_str(),
                        "[INFO] BUILD SUCCESS" | "[INFO] BUILD FAILURE"
                    ) && separator(&rows[1])
                        && rows[2].starts_with("[INFO] Total time:")
                        && patterns[12].is_match(&rows[2])
                        && rows[3].starts_with("[INFO] Finished at: ")
                        && patterns[12].is_match(&rows[3])))
        })
        || run.windows(3).any(|rows| {
            rows.iter().all(|line| valid(line))
                && separator(&rows[0])
                && rows[1] == "[INFO]  T E S T S"
                && separator(&rows[2])
        })
}

/// Transfer labels alone are not Cargo or Generic build evidence either.
pub(super) fn is_download_line(line: &str) -> bool {
    let line = line.trim().strip_prefix("[INFO] ").unwrap_or(line.trim());
    line.starts_with("Downloading from ") || line.starts_with("Downloaded from ")
}

fn separator(line: &str) -> bool {
    line.strip_prefix("[INFO] ")
        .is_some_and(|bar| bar.len() >= 3 && bar.bytes().all(|byte| byte == b'-'))
}

/// Keep encoded results terminal, including when no further edits are possible.
pub(super) fn is_compacted(lines: &[&str]) -> bool {
    lines.iter().any(|line| line.starts_with(HEADER))
}

/// Mark verified transfer and redraw records that remain inside an active scope.
pub(super) fn scoped_transfers(lines: &[&str]) -> Vec<bool> {
    let mut eligible = vec![false; lines.len()];
    if is_compacted(lines) || lines.iter().any(|line| line.len() > MAX_BYTES) {
        return eligible;
    }
    let mut active = true;
    let mut index = leading_preamble_len(lines);
    while index < lines.len() {
        let Some(line) = complete(lines[index]) else {
            active = false;
            index += 1;
            continue;
        };
        if anchor(lines, index) {
            active = true;
        }
        if !active {
            index += 1;
            continue;
        }
        if line.contains('\r') {
            active = raw_transfer(line);
            eligible[index] = active;
            index += 1;
            continue;
        }
        if download(line).is_some() {
            eligible[index] = true;
        } else if known(line).is_none() {
            active = false;
        }
        index += 1;
    }
    eligible
}

/// Plan atomic literal-sharing edits without omitting or stashing records.
pub(super) fn plan(lines: &[&str]) -> Vec<LayoutEdit> {
    plan_with_downloads(lines, true)
}

fn leading_preamble_len(lines: &[&str]) -> usize {
    let mut last_preamble = None;
    for (index, &raw) in lines.iter().take(100).enumerate() {
        let Some(line) = complete(raw) else {
            break;
        };
        if is_preamble(line) {
            last_preamble = Some(index);
        } else if !line.trim().is_empty() {
            break;
        }
    }
    last_preamble.map_or(0, |index| index + 1)
}

fn plan_with_downloads(lines: &[&str], include_downloads: bool) -> Vec<LayoutEdit> {
    // Already encoded results are terminal representations. This also avoids
    // promoting previously opaque text after the surrounding layout shrinks.
    if is_compacted(lines) || lines.iter().any(|line| line.len() > MAX_BYTES) {
        return Vec::new();
    }
    let mut edits = Vec::new();
    let mut active = true;
    let mut index = leading_preamble_len(lines);
    while index < lines.len() {
        let Some(line) = complete(lines[index]) else {
            active = false;
            index += 1;
            continue;
        };
        if anchor(lines, index) {
            active = true;
        }
        if !active {
            index += 1;
            continue;
        }
        if line.contains('\r') {
            // Recognition keeps a transfer's scope alive, but never rewrites
            // a redraw or allows an edit to cross it.
            active = raw_transfer(line);
            index += 1;
            continue;
        }
        let downloads = download(line).is_some();
        let start = index;
        let mut records = Vec::new();
        let mut bytes = 0;
        let mut oversized = false;
        while index < lines.len() {
            let Some(line) = complete(lines[index]) else {
                break;
            };
            if line.contains('\r') {
                break;
            }
            let record = if downloads {
                download(line)
            } else if download(line).is_none() {
                known(line)
            } else {
                None
            };
            let Some(record) = record else { break };
            bytes += lines[index].len();
            oversized |= index - start >= MAX_ROWS || bytes > MAX_BYTES;
            if !oversized {
                records.push(record);
            }
            index += 1;
        }
        if index == start {
            active = false;
            index += 1;
        } else if !oversized && records.len() > 1 && (!downloads || include_downloads) {
            let replacement = render(&records);
            if saves_both(&lines[start..index].concat(), &replacement) {
                edits.push(LayoutEdit {
                    start,
                    end: index,
                    replacement,
                });
                if edits.len() == MAX_EDITS {
                    // Each Maven edit decodes independently, so the prefix is complete.
                    return edits;
                }
            }
        }
    }
    edits
}

fn complete(line: &str) -> Option<&str> {
    (line.len() <= MAX_BYTES)
        .then(|| line.strip_suffix('\n'))
        .flatten()
}

fn plain(line: &str) -> bool {
    !line.chars().any(char::is_control)
}

fn anchor(lines: &[&str], index: usize) -> bool {
    let Ok(patterns) = PATTERNS.as_ref() else {
        return false;
    };
    let Some(line) = complete(lines[index]) else {
        return false;
    };
    if !plain(line) {
        return false;
    }
    if patterns[0].is_match(line) {
        return true;
    }
    if line == "[INFO] -------------------------------------------------------"
        && lines.get(index + 1) == Some(&"[INFO]  T E S T S\n")
        && lines.get(index + 2)
            == Some(&"[INFO] -------------------------------------------------------\n")
    {
        return true;
    }
    // A module identity alone could be diagnostic text. Require its complete
    // Building / pom / packaging banner before reopening the scope.
    patterns[7].is_match(line)
        && lines
            .get(index + 1)
            .and_then(|line| complete(line))
            .is_some_and(|line| patterns[5].is_match(line))
        && lines
            .get(index + 2)
            .and_then(|line| complete(line))
            .is_some_and(|line| patterns[6].is_match(line))
        && lines
            .get(index + 3)
            .and_then(|line| complete(line))
            .is_some_and(|line| patterns[8].is_match(line))
}

fn known(line: &str) -> Option<Record> {
    if !plain(line) {
        return None;
    }
    for (index, pattern) in PATTERNS.as_ref().ok()?.iter().enumerate() {
        let Some(captures) = pattern.captures(line) else {
            continue;
        };
        if matches!(index, 2 | 3)
            && [2, 3].iter().any(|field| {
                captures
                    .get(*field)
                    .is_none_or(|value| value.as_str() != "0")
            })
        {
            return None;
        }
        let mut literals = Vec::new();
        let mut fields = Vec::new();
        let mut cursor = 0;
        for capture in captures.iter().skip(1).flatten() {
            literals.push(line[cursor..capture.start()].into());
            fields.push(capture.as_str().into());
            cursor = capture.end();
        }
        literals.push(line[cursor..].into());
        return Some(Record {
            literals,
            fields,
            prefix_field: (index == 4).then_some((0, '.')),
        });
    }
    let body = line.strip_prefix("[INFO]")?;
    if matches!(
        body,
        "" | " "
            | " Scanning for projects..."
            | " Reactor Build Order:"
            | " Results:"
            | "  T E S T S"
            | " BUILD SUCCESS"
            | " BUILD FAILURE"
            | " Nothing to compile - all classes are up to date."
            | " No sources to compile"
            | " No tests to run."
            | " Tests are skipped."
    ) || body
        .strip_prefix(' ')
        .is_some_and(|bar| bar.len() >= 3 && bar.bytes().all(|b| b == b'-'))
    {
        return Some(Record::info(body));
    }
    None
}

fn download(line: &str) -> Option<Record> {
    if !plain(line) {
        return None;
    }
    let (prefix, body) = line
        .strip_prefix("[INFO] ")
        .map_or(("", line), |body| ("[INFO] ", body));
    let (action, rest) = body.split_once(" from ")?;
    if !matches!(action, "Downloading" | "Downloaded") {
        return None;
    }
    let (repo, rest) = rest.split_once(": ")?;
    if repo.is_empty()
        || !repo
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return None;
    }
    let (url, receipt) = rest
        .find(' ')
        .map_or((rest, ""), |position| rest.split_at(position));
    if !url_valid(url)
        || match action {
            "Downloading" => !receipt.is_empty(),
            _ => !RECEIPT.as_ref().ok()?.is_match(receipt),
        }
    {
        return None;
    }
    Some(Record {
        literals: vec![
            prefix.into(),
            format!(" from {repo}: "),
            String::new(),
            String::new(),
        ],
        fields: vec![action.into(), url.into(), receipt.into()],
        prefix_field: Some((1, '/')),
    })
}

fn url_valid(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return false;
    };
    let Some((host, path)) = rest.split_once('/') else {
        return false;
    };
    let (name, port) = host
        .split_once(':')
        .map_or((host, None), |(name, port)| (name, Some(port)));
    !path.is_empty()
        && port.is_none_or(|port| {
            !port.is_empty()
                && port.bytes().all(|b| b.is_ascii_digit())
                && port.parse::<u16>().is_ok()
        })
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.as_bytes()[0].is_ascii_alphanumeric()
                && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._~+-".contains(&b))
}

fn raw_transfer(line: &str) -> bool {
    let mut saw_transfer = false;
    for part in line.split('\r') {
        let part = part.trim_end_matches(' ');
        if part.is_empty() {
            continue;
        }
        if !transfer(part) && download(part).is_none() {
            return false;
        }
        saw_transfer = true;
    }
    saw_transfer
}

fn transfer(line: &str) -> bool {
    let Some((count, sizes)) = line
        .strip_prefix("Progress (")
        .and_then(|rest| rest.split_once("): "))
    else {
        return false;
    };
    if count.is_empty() || !count.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let Ok(count) = count.parse::<usize>() else {
        return false;
    };
    let Ok(pattern) = TRANSFER_SIZE.as_ref() else {
        return false;
    };
    count > 0
        && count <= MAX_ROWS
        && sizes.split(" | ").count() == count
        && sizes.split(" | ").all(|size| pattern.is_match(size))
}

fn render(records: &[Record]) -> String {
    let mut templates: Vec<Vec<String>> = Vec::new();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut ids = Vec::with_capacity(records.len());
    for (index, record) in records.iter().enumerate() {
        let id = if let Some(id) = templates
            .iter()
            .position(|template| *template == record.literals)
        {
            id
        } else {
            templates.push(record.literals.clone());
            groups.push(Vec::new());
            templates.len() - 1
        };
        groups[id].push(index);
        ids.push(id);
    }
    let mut shared = vec![None; templates.len()];
    for (id, group) in groups.iter().enumerate() {
        let Some((field, delimiter)) = records[group[0]].prefix_field else {
            continue;
        };
        let first = &records[group[0]].fields[field];
        let length = group
            .iter()
            .map(|index| {
                first
                    .chars()
                    .zip(records[*index].fields[field].chars())
                    .take_while(|(a, b)| a == b)
                    .map(|(ch, _)| ch.len_utf8())
                    .sum::<usize>()
            })
            .min()
            .unwrap_or(0);
        // The shared byte length is computed from whole Unicode characters.
        if let Some(end) = first[..length].rfind(delimiter) {
            let prefix = &first[..=end];
            templates[id][field].push_str(prefix);
            shared[id] = Some((field, prefix.len()));
        }
    }
    let header = serde_json::json!({"rows": records.len(), "literals": templates});
    let mut output = format!("{HEADER}{header}\n");
    for (record, id) in records.iter().zip(ids) {
        output.push_str(&id.to_string());
        for (field, value) in record.fields.iter().enumerate() {
            output.push('\t');
            let offset = shared[id]
                .filter(|(shared_field, _)| *shared_field == field)
                .map_or(0, |(_, length)| length);
            output.push_str(&value[offset..]);
        }
        output.push('\n');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    mod oracle {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/common/maven_oracle.rs"
        ));
    }

    #[test]
    fn static_grammars_compile() {
        assert!(PATTERNS.is_ok(), "{PATTERNS:?}");
        assert!(RECEIPT.is_ok(), "{RECEIPT:?}");
        assert!(TRANSFER_SIZE.is_ok(), "{TRANSFER_SIZE:?}");
    }

    #[test]
    fn captured_edits_round_trip_individually_and_download_ablation_keeps_other_edits() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/build_logs/maven");
        let mut paths = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            if path.extension().is_none_or(|extension| extension != "txt")
                || path.to_string_lossy().ends_with(".expected.txt")
            {
                continue;
            }
            let input = std::fs::read_to_string(&path).unwrap();
            let input = oracle::strip_sgr(&input);
            let lines = input.split_inclusive('\n').collect::<Vec<_>>();
            let edits = plan(&lines);
            for edit in &edits {
                let original = lines[edit.start..edit.end].concat();
                assert!(
                    saves_both(&original, &edit.replacement),
                    "{}",
                    path.display()
                );
                assert_eq!(
                    oracle::decode(&edit.replacement),
                    original,
                    "{}",
                    path.display()
                );
            }
            let without_downloads = plan_with_downloads(&lines, false);
            for edit in &without_downloads {
                assert!(edits.iter().any(|other| other.start == edit.start
                    && other.end == edit.end
                    && other.replacement == edit.replacement));
            }
            let render = |edits: &[LayoutEdit]| {
                super::super::render_reduced(
                    &lines,
                    &[],
                    &[],
                    &tokenless_ccr::RecoveryMethod::None,
                    edits,
                )
            };
            let full = render(&edits);
            let ablated = render(&without_downloads);
            assert_eq!(oracle::decode(&full), input);
            println!(
                "{}: before={}, candidate={}, without_downloads={}",
                path.file_name().unwrap().to_string_lossy(),
                tokenless_protocol::estimate_tokens(&input),
                tokenless_protocol::estimate_tokens(&full),
                tokenless_protocol::estimate_tokens(&ablated)
            );
            assert_eq!(oracle::decode(&ablated), input);
        }
    }
}
