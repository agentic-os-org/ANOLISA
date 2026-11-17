//! Lossless structural compaction for Gradle build logs.
//!
//! Compact contiguous Gradle task header blocks using literal templates and shared
//! prefixes while preserving every task path, outcome status, order, warning,
//! error, and unknown line verbatim.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use super::{LayoutEdit, saves_both};

// Lines exceeding MAX_LINE_BYTES close the active task scope locally;
// any single line exceeding MAX_BYTES disables Gradle edits for the whole result.
const MAX_LINE_BYTES: usize = 4096;
const MAX_ROWS: usize = 256;
const MAX_BYTES: usize = 64 * 1024;
const MAX_EDITS: usize = 256;
const HEADER: &str = "Gradle templates (id<TAB>fields; interleave with literals): ";

static BUILD_STATUS: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"^BUILD (?:SUCCESSFUL|FAILED) in [0-9]+"));
static ACTIONABLE_TASKS: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"^[0-9]+ actionable tasks?:"));

struct Record {
    literals: Vec<String>,
    fields: Vec<String>,
}

/// Detects a Gradle build log from a bounded head-and-tail sample.
pub(super) fn detect(sample: &[String]) -> bool {
    let (head, tail) = match sample.iter().position(|line| line == "\0") {
        Some(gap) => (&sample[..gap], Some(&sample[gap + 1..])),
        None => (sample, None),
    };
    let head_start = skip_leading_preambles(head);
    let first = head[head_start..]
        .iter()
        .find(|line| !line.trim().is_empty());
    if first.is_none_or(|line| !is_log_head(line)) {
        return false;
    }

    let head_fences = unfenced_runs(head);
    if head_fences.runs.iter().any(|run| has_strong_evidence(run)) {
        return true;
    }

    // Across a sample gap, fence state from the unsampled middle is unknown.
    tail.is_some_and(|tail| {
        !head_fences.unclosed && {
            let tail_fences = unfenced_runs(tail);
            !tail_fences.saw_fence && tail_fences.runs.iter().any(|run| has_strong_evidence(run))
        }
    })
}

/// Plans atomic lossless template edits across contiguous Gradle task blocks.
pub(super) fn plan(lines: &[&str]) -> Vec<LayoutEdit> {
    if is_compacted(lines) || lines.iter().any(|line| line.len() > MAX_BYTES) {
        return Vec::new();
    }

    let mut edits = Vec::new();
    let mut active = true;
    let mut open_fence = None;
    let mut index = leading_preamble_len(lines);

    while index < lines.len() {
        let raw_body = lines[index].strip_suffix('\n').unwrap_or(lines[index]);
        if step_fence(&mut open_fence, raw_body) {
            active = false;
            index += 1;
            continue;
        }
        let Some(line) = complete(lines[index]) else {
            active = false;
            index += 1;
            continue;
        };
        if !plain(line) {
            active = false;
            index += 1;
            continue;
        }
        if !active {
            if reopen_anchor(lines, index) {
                active = true;
            } else {
                index += 1;
                continue;
            }
        }

        if parse_task(line).is_none() {
            if !is_in_scope_lifecycle(line) {
                active = false;
            }
            index += 1;
            continue;
        }

        let start = index;
        let mut scan_end = index;
        let mut bytes = 0usize;
        let mut task_count = 0usize;
        while scan_end < lines.len() && scan_end - start < MAX_ROWS {
            let Some(next) = complete(lines[scan_end]) else {
                break;
            };
            if !plain(next) {
                break;
            }
            let is_task = parse_task(next).is_some();
            if !is_task && !next.is_empty() {
                break;
            }
            let line_len = lines[scan_end].len();
            if bytes + line_len > MAX_BYTES {
                break;
            }
            bytes += line_len;
            if is_task {
                task_count += 1;
            }
            scan_end += 1;
        }

        let mut end = scan_end;
        while end > start && lines[end - 1] == "\n" {
            end -= 1;
        }
        index = scan_end.max(start + 1);

        if task_count >= 2 && end > start {
            let original = lines[start..end].concat();
            let replacement = render(&lines[start..end]);
            if saves_both(&original, &replacement) {
                edits.push(LayoutEdit {
                    start,
                    end,
                    replacement,
                });
                if edits.len() == MAX_EDITS {
                    return edits;
                }
            }
        }
    }

    edits
}

fn valid_line(line: &str) -> bool {
    line.len() <= MAX_LINE_BYTES && plain(line)
}

fn complete(line: &str) -> Option<&str> {
    (line.len() <= MAX_LINE_BYTES)
        .then(|| line.strip_suffix('\n'))
        .flatten()
}

fn plain(line: &str) -> bool {
    !line.chars().any(char::is_control)
}

fn is_compacted_header(line: &str) -> bool {
    line.len() <= MAX_BYTES && plain(line) && line.starts_with(HEADER)
}

/// Keeps compacted Gradle outputs terminal across repeated passes.
pub(super) fn is_compacted(lines: &[&str]) -> bool {
    let mut open_fence = None;
    lines.iter().any(|raw| {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        !step_fence(&mut open_fence, line) && is_compacted_header(line)
    })
}

fn is_build_status(line: &str) -> bool {
    valid_line(line) && BUILD_STATUS.as_ref().is_ok_and(|re| re.is_match(line))
}

fn is_actionable_tasks(line: &str) -> bool {
    valid_line(line) && ACTIONABLE_TASKS.as_ref().is_ok_and(|re| re.is_match(line))
}

fn is_log_head(line: &str) -> bool {
    is_compacted_header(line)
        || (valid_line(line)
            && (is_configure_project(line) || parse_task(line).is_some() || is_build_status(line)))
}

fn valid_task_path(path: &str) -> bool {
    !path.is_empty()
        && path.split(':').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
        })
}

fn is_configure_project(line: &str) -> bool {
    valid_line(line)
        && (line == "> Configure project :"
            || line
                .strip_prefix("> Configure project :")
                .is_some_and(valid_task_path))
}

fn parse_task_header(line: &str) -> Option<(&str, &'static str, bool)> {
    if !valid_line(line) {
        return None;
    }
    let rest = line.strip_prefix("> Task :")?;
    if rest.is_empty() {
        return None;
    }
    let (path, suffix, is_failed) = match rest.rsplit_once(' ') {
        Some((path, status)) => match status {
            "UP-TO-DATE" => (path, " UP-TO-DATE", false),
            "FROM-CACHE" => (path, " FROM-CACHE", false),
            "SKIPPED" => (path, " SKIPPED", false),
            "NO-SOURCE" => (path, " NO-SOURCE", false),
            "FAILED" => (path, " FAILED", true),
            _ => return None,
        },
        None => (rest, "", false),
    };
    valid_task_path(path).then_some((path, suffix, is_failed))
}

/// Prevents Gradle task headers (including `FAILED`) from entering Generic progress rules.
pub(super) fn is_task_line(line: &str) -> bool {
    parse_task_header(line).is_some()
}

fn parse_task(line: &str) -> Option<(&str, &'static str)> {
    let (path, suffix, is_failed) = parse_task_header(line)?;
    (!is_failed).then_some((path, suffix))
}

fn is_in_scope_lifecycle(line: &str) -> bool {
    line.is_empty() || is_configure_project(line) || is_config_or_daemon_notice(line)
}

fn is_config_or_daemon_notice(line: &str) -> bool {
    line.starts_with("Starting a Gradle Daemon")
        || line.starts_with("To honour the JVM settings for this build")
        || line.starts_with("Calculating task graph")
        || line.starts_with("Reusing configuration cache")
        || matches!(
            line,
            "Parallel Configuration Cache is an incubating feature."
                | "Configuration on demand is an incubating feature."
                | "Configuration cache is an incubating feature."
                | "Type-safe project accessors is an incubating feature."
        )
}

fn is_wrapper_dots(line: &str) -> bool {
    let mut dots = 0usize;
    for byte in line.bytes() {
        match byte {
            b'.' => dots += 1,
            b'0'..=b'9' | b'%' => {}
            _ => return false,
        }
    }
    dots >= 4
}

fn is_gradle_xtrace(line: &str) -> bool {
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
    matches!(binary, "gradle" | "gradlew" | "gradlew.bat")
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

fn is_exit_anchor(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("exit code:")
        .or_else(|| lower.strip_prefix("exit status:"));
    rest.is_some_and(|tail| {
        let trimmed = tail.trim();
        let digits = trimmed.strip_prefix('-').unwrap_or(trimmed);
        !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
    })
}

fn is_static_preamble(line: &str) -> bool {
    if !valid_line(line) {
        return false;
    }
    line.starts_with("$ ")
        || is_exit_anchor(line)
        || is_gradle_xtrace(line)
        || line.starts_with("Picked up JAVA_TOOL_OPTIONS: ")
        || line.starts_with("Picked up _JAVA_OPTIONS: ")
        || line.starts_with("NOTE: Picked up JDK_JAVA_OPTIONS: ")
        || is_jvm_warning(line)
        || line.starts_with("Fetching distribution (retrying ")
        || line.starts_with("Downloading https://services.gradle.org/distributions/gradle-")
        || line.starts_with("Unzipping ")
        || is_wrapper_dots(line)
        || line.starts_with("Please read https://docs.gradle.org/")
        || is_config_or_daemon_notice(line)
}

fn consume_welcome_line(line: &str, in_welcome: &mut bool) -> bool {
    if line.starts_with("Welcome to Gradle ") {
        *in_welcome = true;
        return true;
    }
    if *in_welcome {
        if line == "Here are the highlights of this release:" || line.starts_with(" - ") {
            return true;
        }
        *in_welcome = false;
        if line.starts_with("For more details see https://docs.gradle.org/") {
            return true;
        }
    }
    false
}

fn skip_leading_preambles(lines: &[String]) -> usize {
    let mut index = 0usize;
    let mut in_welcome = false;
    while index < lines.len() {
        let line = lines[index].as_str();
        if !valid_line(line) {
            break;
        }
        if line.trim().is_empty() {
            index += 1;
            continue;
        }
        if consume_welcome_line(line, &mut in_welcome) || is_static_preamble(line) {
            index += 1;
        } else {
            break;
        }
    }
    index
}

fn leading_preamble_len(lines: &[&str]) -> usize {
    let mut last_preamble = None;
    let mut in_welcome = false;
    for (index, &raw) in lines.iter().take(100).enumerate() {
        let Some(line) = complete(raw) else {
            break;
        };
        if !plain(line) {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        if consume_welcome_line(line, &mut in_welcome) || is_static_preamble(line) {
            last_preamble = Some(index);
        } else {
            break;
        }
    }
    last_preamble.map_or(0, |index| index + 1)
}

struct Unfenced<'a> {
    runs: Vec<&'a [String]>,
    saw_fence: bool,
    unclosed: bool,
}

fn fence_marker(line: &str) -> Option<(u8, usize, &str)> {
    let trimmed = line.trim_start();
    (*b"`~").into_iter().find_map(|ch| {
        let count = trimmed.bytes().take_while(|&b| b == ch).count();
        (count >= 3).then_some((ch, count, &trimmed[count..]))
    })
}

fn step_fence(open_fence: &mut Option<(u8, usize)>, line: &str) -> bool {
    let marker = fence_marker(line);
    if let Some((open_ch, open_len)) = *open_fence {
        if let Some((ch, count, rest)) = marker
            && ch == open_ch
            && count >= open_len
            && rest.trim().is_empty()
        {
            *open_fence = None;
        }
        return true;
    }
    if let Some((ch, count, _)) = marker {
        *open_fence = Some((ch, count));
        return true;
    }
    false
}

fn unfenced_runs(segment: &[String]) -> Unfenced<'_> {
    let mut runs = Vec::new();
    let mut saw_fence = false;
    let mut open_fence = None;
    let mut run_start = 0;
    for (index, line) in segment.iter().enumerate() {
        let was_open = open_fence.is_some();
        let is_fenced = step_fence(&mut open_fence, line);
        if !was_open && is_fenced {
            saw_fence = true;
            if run_start < index {
                runs.push(&segment[run_start..index]);
            }
        } else if was_open && open_fence.is_none() {
            run_start = index + 1;
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

fn has_strong_evidence(run: &[String]) -> bool {
    let compacted = run.iter().any(|line| is_compacted_header(line));
    let task_count = run.iter().filter(|line| parse_task(line).is_some()).count();
    let task_plus_footer = task_count >= 1
        && run
            .windows(2)
            .any(|pair| is_build_status(&pair[0]) && is_actionable_tasks(&pair[1]));
    compacted || task_count >= 2 || task_plus_footer
}

fn reopen_anchor(lines: &[&str], index: usize) -> bool {
    complete(lines[index]).is_some_and(|line| parse_task(line).is_some())
        && lines
            .get(index + 1)
            .and_then(|line| complete(line))
            .is_some_and(|line| parse_task(line).is_some())
}

fn render(chunk: &[&str]) -> String {
    let mut name_counts: HashMap<(&str, &str), usize> = HashMap::new();
    for raw in chunk {
        if let Some((path, suffix)) = raw.strip_suffix('\n').and_then(parse_task)
            && let Some((_, task_name)) = path.rsplit_once(':')
        {
            *name_counts.entry((task_name, suffix)).or_insert(0) += 1;
        }
    }

    let mut records = Vec::with_capacity(chunk.len());
    for raw in chunk {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        if line.is_empty() {
            records.push(Record {
                literals: vec![String::new()],
                fields: Vec::new(),
            });
            continue;
        }
        if let Some((path, suffix)) = parse_task(line) {
            if let Some((module_path, task_name)) = path.rsplit_once(':')
                && name_counts.get(&(task_name, suffix)).copied().unwrap_or(0) >= 3
            {
                records.push(Record {
                    literals: vec!["> Task :".into(), format!(":{task_name}{suffix}")],
                    fields: vec![module_path.into()],
                });
            } else {
                records.push(Record {
                    literals: vec!["> Task :".into(), suffix.into()],
                    fields: vec![path.into()],
                });
            }
        }
    }

    let mut templates: Vec<Vec<String>> = Vec::new();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut ids = Vec::with_capacity(records.len());
    for (index, record) in records.iter().enumerate() {
        let id = if let Some(existing) = templates
            .iter()
            .position(|template| *template == record.literals)
        {
            existing
        } else {
            templates.push(record.literals.clone());
            groups.push(Vec::new());
            templates.len() - 1
        };
        groups[id].push(index);
        ids.push(id);
    }

    let mut prefix_lens = vec![0usize; templates.len()];
    for (id, group) in groups.iter().enumerate() {
        // Blank rows carry no fields and must never participate in module-prefix sharing.
        if records[group[0]].fields.is_empty() || group.len() < 2 {
            continue;
        }
        let first = &records[group[0]].fields[0];
        let common_bytes = group[1..]
            .iter()
            .map(|&index| {
                first
                    .chars()
                    .zip(records[index].fields[0].chars())
                    .take_while(|(a, b)| a == b)
                    .map(|(ch, _)| ch.len_utf8())
                    .sum::<usize>()
            })
            .min()
            .unwrap_or(0);
        if let Some(pos) = first[..common_bytes].rfind([':', '-', '_']) {
            let prefix = &first[..=pos];
            templates[id][0].push_str(prefix);
            prefix_lens[id] = prefix.len();
        }
    }

    let header = serde_json::json!({
        "rows": records.len(),
        "literals": templates,
    });
    let mut output = format!("{HEADER}{header}\n");
    for (record, id) in records.iter().zip(ids) {
        if record.fields.is_empty() {
            output.push_str(&format!("{id}\n"));
        } else {
            output.push_str(&format!("{id}\t{}\n", &record.fields[0][prefix_lens[id]..]));
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::super::{BuildLogCompressor, BuildLogOperation, sample_lines};
    use super::*;
    use crate::Recoverability;
    use tokenless_ccr::{InMemoryStore, RecoveryMethod, StashStore};

    #[allow(dead_code)]
    mod oracle {
        include!("../../tests/common/gradle_oracle.rs");
    }

    use oracle::{decode, strip_sgr};
    include!("../tests/gradle_tests.rs");
}
