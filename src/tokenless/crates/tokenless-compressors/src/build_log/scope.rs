//! Shared terminal-stream scope guards, fence tracking, and preamble filters.
//!
//! Each BuildLog splitter provides its own line grammar while delegating control-character limits,
//! Markdown fence isolation, `\0` sampled detection, and shell/JVM preamble handling here.

// Lines exceeding MAX_LINE_BYTES close the active scope locally;
// any single line exceeding MAX_BYTES disables edits for the whole result.
pub(super) const MAX_LINE_BYTES: usize = 4096;
pub(super) const MAX_ROWS: usize = 256;
pub(super) const MAX_BYTES: usize = 64 * 1024;
pub(super) const MAX_EDITS: usize = 256;

pub(super) fn plain(line: &str) -> bool {
    !line.chars().any(char::is_control)
}

pub(super) fn valid_line(line: &str) -> bool {
    line.len() <= MAX_LINE_BYTES && plain(line)
}

pub(super) fn complete(line: &str) -> Option<&str> {
    complete_within(line, MAX_LINE_BYTES)
}

pub(super) fn complete_within(line: &str, max_bytes: usize) -> Option<&str> {
    (line.len() <= max_bytes)
        .then(|| line.strip_suffix('\n'))
        .flatten()
}

pub(super) fn is_compacted_header(line: &str, header: &str) -> bool {
    line.len() <= MAX_BYTES && line.starts_with(header) && plain(line)
}

/// Keeps already-compacted outputs terminal across repeated passes outside Markdown fences.
pub(super) fn is_compacted(lines: &[&str], header: &str) -> bool {
    let mut open_fence = None;
    lines.iter().any(|raw| {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        !step_fence(&mut open_fence, line) && is_compacted_header(line, header)
    })
}

/// Checks whether `lines` contains any unfenced compacted header from `headers` in one scan.
pub(super) fn is_any_compacted(lines: &[&str], headers: &[&str]) -> bool {
    let mut open_fence = None;
    lines.iter().any(|raw| {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        !step_fence(&mut open_fence, line)
            && headers
                .iter()
                .any(|header| is_compacted_header(line, header))
    })
}

/// Validates and enters a single scoped line during planning, closing on fences or unsafe lines.
pub(super) fn enter_line<'a>(
    lines: &[&'a str],
    index: usize,
    active: &mut bool,
    open_fence: &mut Option<(u8, usize)>,
    reopen: impl Fn(&str) -> bool,
) -> Option<&'a str> {
    let raw = lines[index];
    let raw_body = raw.strip_suffix('\n').unwrap_or(raw);
    if step_fence(open_fence, raw_body) {
        *active = false;
        return None;
    }
    let Some(line) = complete(raw) else {
        *active = false;
        return None;
    };
    if !plain(line) {
        *active = false;
        return None;
    }
    if !*active {
        if reopen_anchor(lines, index, reopen) {
            *active = true;
        } else {
            return None;
        }
    }
    Some(line)
}

/// Detects a format from a bounded head-and-tail sample while keeping Markdown fences isolated.
pub(super) fn detect_sampled(
    sample: &[String],
    skip_preambles: impl FnOnce(&[String]) -> usize,
    is_log_head: impl Fn(&str) -> bool,
    has_strong_evidence: impl Fn(&[String]) -> bool,
) -> bool {
    // Keep head inspection inside the head window so a sample gap never hides an enclosing
    // document or spans preambles.
    let (head, tail) = match sample.iter().position(|line| line == "\0") {
        Some(gap) => (&sample[..gap], Some(&sample[gap + 1..])),
        None => (sample, None),
    };
    let head_start = skip_preambles(head);
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

    // Across a sample gap, fence state from the unsampled middle is unknown;
    // accept tail evidence only when neither window carries an open or closing fence.
    tail.is_some_and(|tail| {
        !head_fences.unclosed && {
            let tail_fences = unfenced_runs(tail);
            !tail_fences.saw_fence && tail_fences.runs.iter().any(|run| has_strong_evidence(run))
        }
    })
}

pub(super) fn is_exit_anchor(line: &str) -> bool {
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

pub(super) fn is_xtrace(line: &str, binaries: &[&str]) -> bool {
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
    binaries.contains(&binary)
}

pub(super) fn is_java_env_option(line: &str) -> bool {
    line.starts_with("Picked up JAVA_TOOL_OPTIONS: ")
        || line.starts_with("Picked up _JAVA_OPTIONS: ")
        || line.starts_with("NOTE: Picked up JDK_JAVA_OPTIONS: ")
}

pub(super) fn is_jvm_warning(line: &str) -> bool {
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

pub(super) fn skip_leading_preambles(
    lines: &[String],
    mut is_preamble: impl FnMut(&str) -> bool,
) -> usize {
    let mut index = 0usize;
    while index < lines.len() {
        let line = lines[index].as_str();
        if !valid_line(line) || is_fence_line(line) {
            break;
        }
        if line.trim().is_empty() || is_preamble(line) {
            index += 1;
        } else {
            break;
        }
    }
    index
}

pub(super) fn leading_preamble_len(
    lines: &[&str],
    mut is_preamble: impl FnMut(&str) -> bool,
) -> usize {
    let mut last_preamble = None;
    for (index, &raw) in lines.iter().take(100).enumerate() {
        let Some(line) = complete(raw) else {
            break;
        };
        if !plain(line) || is_fence_line(line) {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        if is_preamble(line) {
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

pub(super) fn is_fence_line(line: &str) -> bool {
    fence_marker(line).is_some()
}

pub(super) fn step_fence(open_fence: &mut Option<(u8, usize)>, line: &str) -> bool {
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

fn reopen_anchor(lines: &[&str], index: usize, is_row: impl Fn(&str) -> bool) -> bool {
    complete(lines[index]).is_some_and(&is_row)
        && lines
            .get(index + 1)
            .and_then(|line| complete(line))
            .is_some_and(is_row)
}

fn is_env_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name.as_bytes()[0].is_ascii_alphabetic()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
