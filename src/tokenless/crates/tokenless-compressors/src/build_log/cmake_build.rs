//! Lossless structural compaction for CMake and Ninja build progress logs.
//!
//! Compact contiguous Ninja (`[k/N] <action>`) and CMake Unix Makefiles (`[PPP%] <action>`)
//! progress lines by sharing repeated verbs, target extensions, directory buckets, and common
//! filename prefixes without writing to Stash, while leaving diagnostics, failures, and unknown
//! output verbatim in place.

use std::collections::HashMap;

use super::scope::{
    self, MAX_BYTES, MAX_EDITS, MAX_LINE_BYTES, MAX_ROWS, complete, plain, valid_line,
};
use super::{LayoutEdit, saves_both};

// MAX_BYTES caps compacted headers because `saves_both` keeps each replacement shorter than its slice.
const HEADER: &str = "CMake/Ninja templates (id<TAB>fields; interleave with literals): ";

const OBJECT_SUFFIXES: &[&str] = &[".o", ".obj", ".lo", ".pch", ".gch", ".pcm", ".ifc", ".res"];

// Match compound object extensions before bare `.o`/`.obj` so shared suffix templates stay maximal.
const EXT_SUFFIXES: &[&str] = &[
    ".tests.cpp.o",
    ".test.cpp.o",
    ".cpp.obj",
    ".cxx.obj",
    ".cc.obj",
    ".c.obj",
    ".cpp.o",
    ".cxx.o",
    ".cc.o",
    ".mm.o",
    ".m.o",
    ".cu.o",
    ".s.o",
    ".S.o",
    ".c.o",
    ".pch",
    ".gch",
    ".pcm",
    ".ifc",
    ".res",
    ".obj",
    ".lo",
    ".o",
    ".dylib",
    ".dll",
    ".exe",
    ".so",
    ".a",
];

#[derive(Clone, Copy)]
enum Counter<'a> {
    Ninja { step: &'a str, total: &'a str },
    Percent(&'a str),
}

#[derive(Clone, Copy)]
struct ParsedRow<'a> {
    counter: Counter<'a>,
    verb: &'a str,
    target: &'a str,
}

struct Record {
    literals: Vec<String>,
    fields: Vec<String>,
}

/// Detects a CMake or Ninja build progress stream from a bounded head-and-tail sample.
pub(super) fn detect(sample: &[String]) -> bool {
    scope::detect_sampled(
        sample,
        |head| scope::skip_leading_preambles(head, is_static_preamble),
        is_log_head,
        has_strong_evidence,
    )
}

/// Keeps compacted CMake/Ninja outputs terminal across repeated passes outside Markdown fences.
pub(super) fn is_compacted(lines: &[&str]) -> bool {
    scope::is_compacted(lines, HEADER)
}

/// Plans atomic lossless template edits across contiguous CMake and Ninja build progress blocks.
pub(super) fn plan(lines: &[&str]) -> Vec<LayoutEdit> {
    if is_compacted(lines) || lines.iter().any(|line| line.len() > MAX_BYTES) {
        return Vec::new();
    }

    let mut edits = Vec::new();
    let mut active = true;
    let mut open_fence = None;
    let mut index = scope::leading_preamble_len(lines, is_static_preamble);

    while index < lines.len() {
        let Some(line) = scope::enter_line(lines, index, &mut active, &mut open_fence, |row| {
            parse_row(row).is_some()
        }) else {
            index += 1;
            continue;
        };

        if parse_row(line).is_none() {
            if !is_in_scope_lifecycle(line) {
                active = false;
            }
            index += 1;
            continue;
        }

        let start = index;
        let mut scan_end = index;
        let mut bytes = 0usize;
        let mut row_count = 0usize;
        let mut parsed_chunk = Vec::new();
        while scan_end < lines.len() && scan_end - start < MAX_ROWS {
            let Some(next) = complete(lines[scan_end]) else {
                break;
            };
            if !plain(next) {
                break;
            }
            let entry = if let Some(parsed) = parse_row(next) {
                Some(parsed)
            } else if next.is_empty() {
                None
            } else {
                break;
            };
            let line_len = lines[scan_end].len();
            if bytes + line_len > MAX_BYTES {
                break;
            }
            bytes += line_len;
            if entry.is_some() {
                row_count += 1;
            }
            parsed_chunk.push(entry);
            scan_end += 1;
        }

        let mut end = scan_end;
        while end > start && lines[end - 1] == "\n" {
            parsed_chunk.pop();
            end -= 1;
        }
        debug_assert_eq!(parsed_chunk.len(), end - start);
        index = scan_end.max(start + 1);

        if row_count >= 2 && end > start {
            let original = lines[start..end].concat();
            let replacement = render(&parsed_chunk);
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

fn parse_counter(line: &str) -> Option<(Counter<'_>, &str)> {
    let inner = line.strip_prefix('[')?;
    let (inside, body) = inner.split_once("] ")?;
    if let Some((raw_step, total)) = inside.split_once('/') {
        if raw_step.len() > 9 {
            return None;
        }
        let step_digits = raw_step.trim_start_matches(' ');
        if !(1..=9).contains(&step_digits.len())
            || !step_digits.bytes().all(|b| b.is_ascii_digit())
            || (step_digits != "0" && step_digits.starts_with('0'))
        {
            return None;
        }
        if !(1..=9).contains(&total.len())
            || !total.bytes().all(|b| b.is_ascii_digit())
            || total.starts_with('0')
        {
            return None;
        }
        let step_val: u64 = step_digits.parse().ok()?;
        let total_val: u64 = total.parse().ok()?;
        return (step_val <= total_val).then_some((
            Counter::Ninja {
                step: raw_step,
                total,
            },
            body,
        ));
    }
    let raw_pct = inside.strip_suffix('%')?;
    if !(1..=3).contains(&raw_pct.len()) {
        return None;
    }
    let pct_digits = raw_pct.trim_start_matches(' ');
    if pct_digits.is_empty()
        || !pct_digits.bytes().all(|b| b.is_ascii_digit())
        || (pct_digits != "0" && pct_digits.starts_with('0'))
    {
        return None;
    }
    let pct_val: u16 = pct_digits.parse().ok()?;
    (pct_val <= 100).then_some((Counter::Percent(raw_pct), body))
}

fn is_valid_lang(lang: &str) -> bool {
    (1..=24).contains(&lang.len())
        && lang.as_bytes()[0].is_ascii_alphabetic()
        && lang
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'-'))
}

fn is_single_path(path: &str) -> bool {
    !path.is_empty() && !path.bytes().any(|b| b.is_ascii_whitespace())
}

fn is_object_path(path: &str) -> bool {
    is_single_path(path)
        && OBJECT_SUFFIXES
            .iter()
            .any(|suffix| path.ends_with(suffix) && path.len() > suffix.len())
}

fn is_target_ident(target: &str) -> bool {
    !target.is_empty()
        && target
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'+' | b'-' | b'/'))
}

fn parse_action(body: &str) -> Option<(&str, &str)> {
    if let Some(target) = body.strip_prefix("Building NVCC (Device) object ") {
        return is_object_path(target).then(|| (&body[..body.len() - target.len()], target));
    }
    if let Some(rest) = body
        .strip_prefix("Building ")
        .or_else(|| body.strip_prefix("Compiling "))
        && let Some((lang, target)) = rest.split_once(" object ")
        && is_valid_lang(lang)
        && is_object_path(target)
    {
        return Some((&body[..body.len() - target.len()], target));
    }
    if let Some(target) = body
        .strip_prefix("Linking static target ")
        .or_else(|| body.strip_prefix("Linking target "))
    {
        return is_single_path(target).then(|| (&body[..body.len() - target.len()], target));
    }
    if let Some(rest) = body.strip_prefix("Linking ")
        && let Some((lang, after_lang)) = rest.split_once(' ')
        && is_valid_lang(lang)
    {
        for kind in [
            "executable ",
            "static library ",
            "shared library ",
            "shared module ",
            "module library ",
        ] {
            if let Some(target) = after_lang.strip_prefix(kind)
                && is_single_path(target)
            {
                return Some((&body[..body.len() - target.len()], target));
            }
        }
    }
    if let Some(rest) = body
        .strip_prefix("Creating library symlink ")
        .or_else(|| body.strip_prefix("Creating executable symlink "))
        && let Some((src, dst)) = rest.split_once(' ')
        && is_single_path(src)
        && is_single_path(dst)
    {
        return Some((&body[..body.len() - rest.len()], rest));
    }
    if let Some(target) = body.strip_prefix("Built target ") {
        return is_target_ident(target).then(|| (&body[..body.len() - target.len()], target));
    }
    if let Some(outputs) = body.strip_prefix("Generating ") {
        let valid = !outputs.is_empty()
            && outputs
                .split(", ")
                .all(|path| is_single_path(path) && (path.contains('/') || path.contains('.')));
        return valid.then(|| (&body[..body.len() - outputs.len()], outputs));
    }
    for prefix in [
        "Automatic MOC and UIC for target ",
        "Automatic MOC for target ",
        "Automatic UIC for target ",
        "Automatic RCC for target ",
    ] {
        if let Some(target) = body.strip_prefix(prefix) {
            return is_target_ident(target).then(|| (&body[..body.len() - target.len()], target));
        }
    }
    None
}

fn parse_row(line: &str) -> Option<ParsedRow<'_>> {
    if !valid_line(line) || line.contains('\t') {
        return None;
    }
    let (counter, body) = parse_counter(line)?;
    let (verb, target) = parse_action(body)?;
    Some(ParsedRow {
        counter,
        verb,
        target,
    })
}

fn is_ninja_diagnostic(line: &str) -> bool {
    valid_line(line) && (line.starts_with("ninja: error: ") || line.starts_with("ninja: fatal: "))
}

fn is_ninja_warning(line: &str) -> bool {
    valid_line(line) && line.starts_with("ninja: warning: ")
}

fn is_ninja_no_work(line: &str) -> bool {
    valid_line(line) && line == "ninja: no work to do."
}

fn is_ninja_dir_notice(line: &str) -> bool {
    valid_line(line) && line.starts_with("ninja: Entering directory ")
}

fn is_make_dir_notice(line: &str) -> bool {
    if !valid_line(line) {
        return false;
    }
    let Some(rest) = line
        .strip_prefix("mingw32-make")
        .or_else(|| line.strip_prefix("gmake"))
        .or_else(|| line.strip_prefix("make"))
    else {
        return false;
    };
    let tail = if let Some(after_open) = rest.strip_prefix('[') {
        let Some((digits, tail)) = after_open.split_once(']') else {
            return false;
        };
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        tail
    } else {
        rest
    };
    tail.starts_with(": Entering directory ") || tail.starts_with(": Leaving directory ")
}

fn is_non_control_line(line: &str) -> bool {
    line.len() <= MAX_LINE_BYTES && !line.chars().any(|ch| ch != '\t' && ch.is_control())
}

fn is_counter_step(line: &str) -> bool {
    line.starts_with('[')
        && is_non_control_line(line)
        && parse_counter(line).is_some_and(|(_, body)| {
            let trimmed = body.trim_start();
            !trimmed.is_empty() && !trimmed.starts_with("FAILED:") && !trimmed.starts_with("ninja:")
        })
}

/// Counter steps that `parse_row` cannot represent in a template block; standard
/// progress rows outside layouts stay eligible for fallback routine omission.
pub(super) fn is_custom_counter_step(line: &str) -> bool {
    is_counter_step(line) && parse_row(line).is_none()
}

fn is_cmake_regen_step(line: &str) -> bool {
    valid_line(line)
        && (line == "Re-running CMake..."
            || parse_counter(line).is_some_and(|(_, body)| {
                matches!(
                    body,
                    "Re-running CMake..." | "Running CMake to regenerate build system..."
                )
            }))
}

fn is_static_preamble(line: &str) -> bool {
    if !valid_line(line) {
        return false;
    }
    line.starts_with("$ ")
        || scope::is_exit_anchor(line)
        || scope::is_xtrace(
            line,
            &[
                "cmake",
                "cmake.exe",
                "ninja",
                "ninja-build",
                "ninja.exe",
                "make",
                "gmake",
                "mingw32-make",
            ],
        )
        || is_ninja_dir_notice(line)
        || is_ninja_warning(line)
        || is_make_dir_notice(line)
        || line.starts_with("Change Dir: ")
        || line.starts_with("Run Build Command(s): ")
}

fn is_in_scope_lifecycle(line: &str) -> bool {
    line.is_empty()
        || is_ninja_dir_notice(line)
        || is_make_dir_notice(line)
        || is_ninja_no_work(line)
        || is_cmake_regen_step(line)
        || is_counter_step(line)
        || (valid_line(line)
            && (line.starts_with("Change Dir: ") || line.starts_with("Run Build Command(s): ")))
}

fn is_log_head(line: &str) -> bool {
    scope::is_compacted_header(line, HEADER)
        || parse_row(line).is_some()
        || is_counter_step(line)
        || is_ninja_no_work(line)
        || is_ninja_diagnostic(line)
        || is_cmake_regen_step(line)
}

fn has_strong_evidence(run: &[String]) -> bool {
    let compacted = run
        .iter()
        .any(|line| scope::is_compacted_header(line, HEADER));
    let row_count = run.iter().filter(|line| parse_row(line).is_some()).count();
    let ninja_outcome = run
        .iter()
        .any(|line| is_ninja_no_work(line) || is_ninja_diagnostic(line));
    let single_step_stopped = row_count >= 1
        && run.iter().any(|line| {
            valid_line(line)
                && (line.starts_with("ninja: build stopped:") || line.starts_with("FAILED: "))
        });
    compacted || row_count >= 2 || ninja_outcome || single_step_stopped
}

fn split_ext(target: &str) -> (&str, &str) {
    if target.contains(' ') {
        return (target, "");
    }
    let basename = target.rsplit('/').next().unwrap_or(target);
    for ext in EXT_SUFFIXES {
        if let Some(stem) = target.strip_suffix(ext)
            && basename.len() > ext.len()
        {
            return (stem, ext);
        }
    }
    (target, "")
}

fn render(rows: &[Option<ParsedRow<'_>>]) -> String {
    let mut parsed_rows = Vec::with_capacity(rows.len());
    let mut dir_counts: HashMap<(Option<&str>, &str, &str, &str), usize> = HashMap::new();
    for entry in rows {
        let Some(parsed) = *entry else {
            parsed_rows.push(None);
            continue;
        };
        let (total_opt, idx_field) = match parsed.counter {
            Counter::Ninja { step, total } => (Some(total), step),
            Counter::Percent(pct) => (None, pct),
        };
        let (stem, ext) = split_ext(parsed.target);
        if let Some(pos) = stem.rfind('/') {
            let dir = &stem[..=pos];
            *dir_counts
                .entry((total_opt, parsed.verb, ext, dir))
                .or_insert(0) += 1;
        }
        parsed_rows.push(Some((total_opt, parsed.verb, idx_field, stem, ext)));
    }

    let mut records = Vec::with_capacity(parsed_rows.len());
    for entry in parsed_rows {
        let Some((total_opt, verb, idx_field, mut stem, ext)) = entry else {
            records.push(Record {
                literals: vec![String::new()],
                fields: Vec::new(),
            });
            continue;
        };
        let mut lit1 = match total_opt {
            Some(total) => format!("/{total}] {verb}"),
            None => format!("%] {verb}"),
        };
        if let Some(pos) = stem.rfind('/') {
            let dir = &stem[..=pos];
            if dir_counts
                .get(&(total_opt, verb, ext, dir))
                .copied()
                .unwrap_or(0)
                >= 3
            {
                lit1.push_str(dir);
                stem = &stem[dir.len()..];
            }
        }
        records.push(Record {
            literals: vec!["[".into(), lit1, ext.into()],
            fields: vec![idx_field.into(), stem.into()],
        });
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
        // Blank rows carry no fields and must never participate in target-prefix sharing.
        if records[group[0]].fields.is_empty() || group.len() < 2 {
            continue;
        }
        let first = &records[group[0]].fields[1];
        let common_bytes = group[1..]
            .iter()
            .map(|&index| {
                first
                    .chars()
                    .zip(records[index].fields[1].chars())
                    .take_while(|(a, b)| a == b)
                    .map(|(ch, _)| ch.len_utf8())
                    .sum::<usize>()
            })
            .min()
            .unwrap_or(0);
        if let Some(pos) = first[..common_bytes].rfind(['/', '_', '-']) {
            let prefix = &first[..=pos];
            templates[id][1].push_str(prefix);
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
            output.push_str(&format!(
                "{id}\t{}\t{}\n",
                record.fields[0],
                &record.fields[1][prefix_lens[id]..],
            ));
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::super::{BuildLogCompressor, sample_lines};
    use super::*;
    use crate::Recoverability;
    use tokenless_ccr::{InMemoryStore, RecoveryMethod};

    #[allow(dead_code)]
    mod oracle {
        include!("../../tests/common/literal_oracle.rs");
    }

    include!("../tests/cmake_build_tests.rs");
}
