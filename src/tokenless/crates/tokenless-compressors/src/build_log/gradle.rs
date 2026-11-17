//! Share literal Gradle task-line formatting while retaining every record.
//!
//! Only complete, non-failure `> Task :<path>[ <status>]` records inside a
//! bounded run are compacted. Warnings, deprecations, test lines, stack
//! traces, control sequences, unknown output, unterminated lines and failure
//! records close the active run; `FAILED` tasks stay verbatim next to their
//! diagnostics.

use std::sync::LazyLock;

use regex::Regex;

use super::{LayoutEdit, saves_both};

const MAX_ROWS: usize = 256;
const MAX_BYTES: usize = 64 * 1024;
const MAX_EDITS: usize = 256;
const HEADER: &str = "Gradle templates (id<TAB>fields; interleave with literals): ";
/// A run reopens only after this many consecutive valid non-failure task lines.
pub(super) const REOPEN_ROWS: usize = 2;

static TASK: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| {
    Regex::new(
        r"^> Task :([A-Za-z_][A-Za-z0-9_.:-]*)( UP-TO-DATE| FROM-CACHE| SKIPPED| NO-SOURCE)?$",
    )
});

/// Require related Gradle task records in a log-shaped bounded sample.
pub(super) fn detect(sample: &[String]) -> bool {
    let Ok(task) = TASK.as_ref() else {
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
        .any(|run| has_strong_evidence(run, task))
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
                    .any(|run| has_strong_evidence(run, task))
        }
    })
}

fn is_preamble(line: &str) -> bool {
    if line.len() > MAX_BYTES || !plain(line) {
        return false;
    }
    line.starts_with("$ ")
        || is_gradle_xtrace(line)
        || line.starts_with("Picked up JAVA_TOOL_OPTIONS: ")
        || line.starts_with("Picked up _JAVA_OPTIONS: ")
        || line.starts_with("NOTE: Picked up JDK_JAVA_OPTIONS: ")
        || is_jvm_warning(line)
        || is_log_context(line)
        || is_wrapper_download(line)
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
    matches!(binary, "gradle" | "gradlew")
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

fn is_wrapper_download(line: &str) -> bool {
    line.starts_with("Downloading https://services.gradle.org/distributions/")
        || line.starts_with("Unzipping https://services.gradle.org/distributions/")
        // The wrapper announces retry backoff on slow mirrors.
        || (line.starts_with("Fetching distribution (retrying ") && line.ends_with(")."))
        || is_wrapper_progress(line)
}

static WRAPPER_PROGRESS: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| {
    // The wrapper prints its download progress after the URL line: one or
    // more bare-dot lines, or percentage-dot lines like
    // `..............10%..............20%…100%` that may end mid-mark.
    Regex::new(r"^(\.+([0-9]{1,3}%)?)+\.?$")
});

fn is_wrapper_progress(line: &str) -> bool {
    line.len() > 2
        && line.starts_with('.')
        && WRAPPER_PROGRESS
            .as_ref()
            .is_ok_and(|progress| progress.is_match(line))
}

fn is_log_context(line: &str) -> bool {
    line.starts_with("Welcome to Gradle ")
        || line.starts_with("Here are the highlights of this release:")
        // Short bullets of the Welcome banner, e.g. ` - Java 27 support`.
        // Prose-like bullets (sentence punctuation, long text) are not
        // banner context.
        || (line.starts_with(" - ")
            && line.len() <= 80
            && plain(line)
            && !line.trim_end_matches(['.', '!', '?']).ends_with(['.', '!', '?']))
        || line.starts_with("For more details see a recommended upgrade plan at ")
        || line.starts_with("For more details see https://docs.gradle.org/")
        || line.starts_with("To launch a new build instead of the daemon, run: ")
        || line.starts_with("Starting a Gradle Daemon")
        || line.starts_with("Stopping Daemon(s)")
        || line.starts_with("Daemon will be stopped at the end of the build")
        || line.starts_with("Daemon is stopping")
        || line.starts_with("Using gradle at ")
        || line.starts_with("Configuration cache entry ")
        || line.starts_with("Configuration cache is an incubating feature")
        || line.starts_with("Parallel Configuration Cache is an incubating feature.")
        || line.starts_with("Configuration on demand is an incubating feature.")
        || line.starts_with(
            "Calculating task graph as no cached configuration is available for tasks: ",
        )
        || line.starts_with("Reusing configuration cache.")
        || line.starts_with("> Configure project")
        || line.starts_with("> Task > ")
}

fn is_log_head(line: &str) -> bool {
    if line.len() > MAX_BYTES {
        return false;
    }
    task_line(line).is_some() || plain(line) && (line.starts_with(HEADER) || is_preamble(line))
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

fn has_strong_evidence(run: &[String], task: &Regex) -> bool {
    // Two consecutive complete task lines are evidence; a single `> Task :`
    // line alone could be quoted prose, so it is never enough.
    run.windows(2).any(|rows| {
        rows.iter()
            .all(|line| line.len() <= MAX_BYTES && plain(line) && task.is_match(line))
    })
}

fn plain(line: &str) -> bool {
    !line.chars().any(char::is_control)
}

/// Keep encoded results terminal, including when no further edits are possible.
pub(super) fn is_compacted(lines: &[&str]) -> bool {
    lines.iter().any(|line| line.starts_with(HEADER))
}

/// Plan atomic literal-sharing edits without omitting or stashing records.
pub(super) fn plan(lines: &[&str]) -> Vec<LayoutEdit> {
    // Already encoded results are terminal representations. This also avoids
    // promoting previously opaque text after the surrounding layout shrinks.
    if is_compacted(lines) || lines.iter().any(|line| line.len() > MAX_BYTES) {
        return Vec::new();
    }
    let Ok(task) = TASK.as_ref() else {
        return Vec::new();
    };
    let mut edits = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let Some((run_start, run_end)) = next_run(lines, index, task) else {
            break;
        };
        index = run_end;
        let rows = run_end - run_start;
        let bytes: usize = lines[run_start..run_end]
            .iter()
            .map(|line| line.len())
            .sum();
        // Oversized blocks are rejected atomically: no partial edit may
        // hide a limit breach inside an otherwise compacted run.
        if rows > MAX_ROWS || bytes > MAX_BYTES {
            continue;
        }
        let records = lines[run_start..run_end]
            .iter()
            .filter_map(|line| task_line(line))
            .collect::<Vec<_>>();
        if records.len() < REOPEN_ROWS {
            continue;
        }
        let replacement = render(&records);
        if saves_both(&lines[run_start..run_end].concat(), &replacement) {
            edits.push(LayoutEdit {
                start: run_start,
                end: run_end,
                replacement,
            });
            if edits.len() == MAX_EDITS {
                // Each Gradle edit decodes independently, so the prefix is complete.
                return edits;
            }
        }
    }
    edits
}

/// One complete non-failure task record: `"> Task :"` + path + status.
struct Record {
    path: String,
    status: Option<&'static str>,
}

fn task_line(line: &str) -> Option<Record> {
    let stripped = line.strip_suffix('\n').unwrap_or(line);
    if !plain(stripped) || stripped.len() > MAX_BYTES {
        return None;
    }
    let captures = TASK.as_ref().ok()?.captures(stripped)?;
    let path = captures.get(1)?.as_str();
    // `> Task :` plus a bounded identifier path is already distinctive; the
    // detection gate additionally requires two consecutive records, and
    // compaction requires a run of at least three.
    // The status capture includes the leading space; restore it from the
    // closed set so the record borrows no data from the input line.
    let status = captures.get(2).and_then(|m| match m.as_str().trim() {
        "UP-TO-DATE" => Some(" UP-TO-DATE"),
        "FROM-CACHE" => Some(" FROM-CACHE"),
        "SKIPPED" => Some(" SKIPPED"),
        "NO-SOURCE" => Some(" NO-SOURCE"),
        _ => None,
    });
    Some(Record {
        path: path.to_owned(),
        status,
    })
}

/// Find the next bounded run of consecutive complete non-failure task lines.
/// Iterative: untrusted input must never drive unbounded recursion.
fn next_run(lines: &[&str], start: usize, task: &Regex) -> Option<(usize, usize)> {
    let mut index = start;
    let mut consecutive = 0;
    while index < lines.len() {
        let line = lines[index];
        if line.len() > MAX_BYTES {
            return None;
        }
        let is_task = line
            .strip_suffix('\n')
            .is_some_and(|stripped| plain(stripped) && task.is_match(stripped));
        if is_task {
            consecutive += 1;
            index += 1;
            continue;
        }
        // Non-task output always closes the current run. A run reopens only
        // after two consecutive valid non-failure task lines.
        if consecutive >= REOPEN_ROWS {
            return Some((index - consecutive, index));
        }
        consecutive = 0;
        // A failure record never joins a run, but it does not stop the scan:
        // failed records and their diagnostics stay verbatim while later
        // compactable runs still compact.
        index += 1;
    }
    (consecutive >= REOPEN_ROWS).then_some((index - consecutive, index))
}

fn render(records: &[Record]) -> String {
    // Each template is `["<prefix:>", "<status>"]`; rows carry only the path
    // tail, so a decoder interleaves `literals[0] + path + literals[1]`.
    let mut templates: Vec<Vec<String>> = Vec::new();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut ids = Vec::with_capacity(records.len());
    for (index, record) in records.iter().enumerate() {
        let template = vec![
            "> Task :".to_owned(),
            record.status.unwrap_or("").to_owned(),
        ];
        let id = if let Some(id) = templates.iter().position(|stored| *stored == template) {
            id
        } else {
            templates.push(template);
            groups.push(Vec::new());
            templates.len() - 1
        };
        groups[id].push(index);
        ids.push(id);
    }
    // Factor the shared module-path prefix of a template's task path at
    // `:`/`-`/`_` boundaries once the template has enough complete rows.
    let mut shared = vec![0; templates.len()];
    for (id, group) in groups.iter().enumerate() {
        if group.len() < 3 {
            continue;
        }
        let first = &records[group[0]].path;
        let length = group
            .iter()
            .map(|index| {
                first
                    .chars()
                    .zip(records[*index].path.chars())
                    .take_while(|(a, b)| a == b)
                    .map(|(ch, _)| ch.len_utf8())
                    .sum::<usize>()
            })
            .min()
            .unwrap_or(0);
        if let Some(end) = first[..length].rfind([':', '-', '_']) {
            let prefix = &first[..=end];
            // The prefix becomes part of the shared literal so a decoder
            // reassembles the full path from the stored tail alone.
            templates[id][0].push_str(prefix);
            shared[id] = prefix.len();
        }
    }
    let header = serde_json::json!({"rows": records.len(), "literals": templates});
    let mut output = format!("{HEADER}{header}\n");
    for (index, id) in ids.iter().enumerate() {
        output.push_str(&id.to_string());
        output.push('\t');
        output.push_str(&records[index].path[shared[*id]..]);
        output.push('\n');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_grammar_compiles() {
        assert!(TASK.is_ok(), "{TASK:?}");
    }

    #[test]
    fn task_grammar_accepts_complete_records_and_rejects_prose() {
        for line in [
            "> Task :core:compileJava UP-TO-DATE",
            "> Task :build FROM-CACHE",
            "> Task :test SKIPPED",
            "> Task :lint NO-SOURCE",
            "> Task :compileJava",
            "> Task :a-b_c:d UP-TO-DATE",
        ] {
            assert!(task_line(line).is_some(), "{line}");
        }
        for line in [
            "> Task",
            "> Task :",
            "> Task :: UP-TO-DATE",
            "> Task :path FAILED",
            "> Task quoted in prose",
            "the > Task :x line inside a sentence",
            "> Task :1.2.3 UP-TO-DATE",
        ] {
            assert!(task_line(line).is_none(), "{line}");
        }
    }
}

#[cfg(test)]
mod plan_debug {
    use super::*;

    /// The `:library:core-module{index:02}` shape is the prefix-factoring
    /// case worth pinning: one template literal carries the shared module
    /// prefix at a `-` boundary, and each row stores only its tail.
    #[test]
    fn prefix_factoring_stores_the_shared_module_prefix_in_the_literal() {
        let block = (0..30)
            .map(|index| format!("> Task :library:core-module{index:02}:compileJava UP-TO-DATE\n"))
            .collect::<String>();
        let lines = block.split_inclusive('\n').collect::<Vec<_>>();
        let edits = plan(&lines);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].start, 0);
        assert_eq!(edits[0].end, 30);
        let replacement = &edits[0].replacement;
        let header = replacement
            .lines()
            .find(|line| line.starts_with(HEADER))
            .unwrap();
        let header: serde_json::Value =
            serde_json::from_str(header.strip_prefix(HEADER).unwrap()).unwrap();
        let literals = header["literals"].as_array().unwrap();
        assert_eq!(header["rows"].as_u64(), Some(30));
        // The factored prefix lives in the template literal (factored at the
        // `-` token boundary), and rows carry only the module tail.
        assert!(
            literals
                .iter()
                .any(|literal| literal[0].as_str().unwrap().contains(":library:core-")),
            "{literals:?}"
        );
        assert!(replacement.contains("\tmodule00:compileJava"));
        assert!(!replacement.contains(":library:core-module"));
    }

    #[test]
    fn wrapper_preambles_from_captured_gradle_distributions() {
        for line in [
            "Downloading https://services.gradle.org/distributions/gradle-8.10-bin.zip",
            // Bare-dot progress lines.
            &".".repeat(130),
            // Percentage-dot progress, including a trailing partial mark.
            "..............10%..............20%...............30%..............40%\
             ...............50%..............60%...............70%..............80%\
             ...............90%..............100%",
            "Fetching distribution (retrying 3 times, with an initial back off of 5000 ms).",
            "Parallel Configuration Cache is an incubating feature.",
            "Configuration on demand is an incubating feature.",
            "For more details see https://docs.gradle.org/9.8.1/release-notes.html",
            " - Java 27 support",
            " - Maven mirror settings reuse",
        ] {
            assert!(is_preamble(line), "{line:?}");
        }
        for line in [
            "Downloading a random tarball from https://services.gradle.org/distributions/x.zip",
            "Fetching distribution (no backoff",
            ".",
            "..",
            "....not.a.percentage....",
            " - a bullet in an arbitrary list that keeps going for quite a while and is far too long to be a release highlight entry in any banner",
        ] {
            assert!(!is_preamble(line), "{line:?}");
        }
    }
}
