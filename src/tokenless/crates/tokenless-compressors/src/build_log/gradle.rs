//! Lossless structural compaction for Gradle build logs.
//!
//! Compact contiguous Gradle task header blocks using literal templates and shared
//! prefixes while preserving every task path, outcome status, order, warning,
//! error, and unknown line verbatim.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use super::scope::{self, MAX_BYTES, MAX_EDITS, MAX_ROWS, complete, plain, valid_line};
use super::{LayoutEdit, saves_both};

pub(super) const HEADER: &str = "Gradle templates (id<TAB>fields; interleave with literals): ";

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
    scope::detect_sampled(
        sample,
        skip_leading_preambles,
        is_log_head,
        has_strong_evidence,
    )
}

/// Plans atomic lossless template edits across contiguous Gradle task blocks.
pub(super) fn plan(lines: &[&str]) -> Vec<LayoutEdit> {
    if is_compacted(lines) {
        return Vec::new();
    }

    let mut edits = Vec::new();
    let mut active = true;
    let mut open_fence = None;
    let mut index = leading_preamble_len(lines);

    while index < lines.len() {
        let Some(line) = scope::enter_line(lines, index, &mut active, &mut open_fence, |row| {
            parse_task(row).is_some()
        }) else {
            index += 1;
            continue;
        };

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

/// Keeps compacted Gradle outputs terminal across repeated passes.
pub(super) fn is_compacted(lines: &[&str]) -> bool {
    scope::is_compacted(lines, HEADER)
}

fn is_build_status(line: &str) -> bool {
    valid_line(line) && BUILD_STATUS.as_ref().is_ok_and(|re| re.is_match(line))
}

fn is_actionable_tasks(line: &str) -> bool {
    valid_line(line) && ACTIONABLE_TASKS.as_ref().is_ok_and(|re| re.is_match(line))
}

fn is_log_head(line: &str) -> bool {
    scope::is_compacted_header(line, HEADER)
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

fn is_static_preamble(line: &str) -> bool {
    if !valid_line(line) {
        return false;
    }
    line.starts_with("$ ")
        || scope::is_exit_anchor(line)
        || scope::is_xtrace(line, &["gradle", "gradlew", "gradlew.bat"])
        || scope::is_java_env_option(line)
        || scope::is_jvm_warning(line)
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
    let mut in_welcome = false;
    scope::skip_leading_preambles(lines, |line| {
        consume_welcome_line(line, &mut in_welcome) || is_static_preamble(line)
    })
}

fn leading_preamble_len(lines: &[&str]) -> usize {
    let mut in_welcome = false;
    scope::leading_preamble_len(lines, |line| {
        consume_welcome_line(line, &mut in_welcome) || is_static_preamble(line)
    })
}

fn has_strong_evidence(run: &[String]) -> bool {
    let compacted = run
        .iter()
        .any(|line| scope::is_compacted_header(line, HEADER));
    let task_count = run.iter().filter(|line| parse_task(line).is_some()).count();
    let task_plus_footer = task_count >= 1
        && run
            .windows(2)
            .any(|pair| is_build_status(&pair[0]) && is_actionable_tasks(&pair[1]));
    compacted || task_count >= 2 || task_plus_footer
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
mod tests;
