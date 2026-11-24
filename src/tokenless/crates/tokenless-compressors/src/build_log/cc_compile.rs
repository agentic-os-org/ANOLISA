//! Lossless structural compaction for verbose C and C++ compiler invocation logs.
//!
//! Compact CMake `VERBOSE=1`, Ninja `-v`, and native GNU Make `V=1` compile
//! streams by factoring shared compiler binaries, flags, dependency options,
//! working-directory prefixes, directory buckets, and path extensions without
//! writing to Stash, while leaving diagnostics, failures, and handwritten
//! `Make` logs outside this pass.

use std::ops::Range;

use super::cmake_build::{
    self, Counter, is_cmake_regen_step, is_make_dir_notice, is_ninja_dir_notice, is_ninja_no_work,
    is_ninja_warning, is_object_path, parse_action, parse_counter,
};
use super::scope::{
    self, ClaimedCursor, MAX_BYTES, MAX_EDITS, MAX_ROWS, complete, plain, valid_line,
};
use super::{LayoutEdit, saves_both};

// MAX_BYTES caps compacted headers because `saves_both` keeps each replacement shorter than its slice.
pub(super) const HEADER: &str =
    "C/C++ compile templates (id<TAB>fields; interleave with literals): ";

const EXT_SUFFIXES: &[&str] = &[
    ".tests.cpp.o.d",
    ".test.cpp.o.d",
    ".cpp.o.d",
    ".cxx.o.d",
    ".cc.o.d",
    ".c.o.d",
    ".mm.o.d",
    ".m.o.d",
    ".cu.o.d",
    ".S.o.d",
    ".s.o.d",
    ".o.d",
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
    ".S.o",
    ".s.o",
    ".c.o",
    ".tests.cpp",
    ".test.cpp",
    ".cpp",
    ".cxx",
    ".cc",
    ".mm",
    ".cu",
    ".c",
    ".S",
    ".s",
    ".m",
    ".pch",
    ".gch",
    ".pcm",
    ".ifc",
    ".res",
    ".obj",
    ".lo",
    ".o",
    ".d",
    ".Td",
];

const SOURCE_EXTS: &[&str] = &[
    ".c", ".cc", ".cpp", ".cxx", ".C", ".S", ".s", ".m", ".mm", ".cu",
];

const OBJECT_EXTS: &[&str] = &[".o", ".obj", ".lo"];

// Compiler flags that consume the next standalone token as a non-translation-unit argument.
const SKIP_NEXT_ARG_FLAGS: &[&str] = &[
    "-I",
    "-isystem",
    "-iquote",
    "-idirafter",
    "-iprefix",
    "-iwithprefix",
    "-iwithprefixbefore",
    "-isysroot",
    "--sysroot",
    "-imacros",
    "-include",
    "-D",
    "-U",
    "-x",
    "-L",
    "-l",
    "-T",
    "-u",
    "-e",
    "-B",
    "-specs",
    "-param",
    "--param",
    "-target",
    "--target",
    "-arch",
    "-mllvm",
    "-Xclang",
    "-Xpreprocessor",
    "-Xassembler",
    "-Xlinker",
    "-dumpbase",
    "-dumpdir",
    "-auxbase",
    "-auxbase-strip",
];

#[derive(Clone, Copy)]
enum Segment<'a> {
    Word(&'a str),
    Spaces(&'a str),
}

struct FieldSpec<'a> {
    value: &'a str,
    is_numeric: bool,
}

enum ParsedKind {
    Compile {
        is_verbose: bool,
        has_ninja_step: bool,
    },
    PairedLabel,
}

// Invariant: every producer (`parse_compile`, `parse_percent_action`,
// `parse_make_tag`, and blank placeholders in `render`) emits
// `literals.len() == fields.len() + 1` so `render()` interleaving is total.
struct ParsedRow<'a> {
    kind: ParsedKind,
    literals: Vec<String>,
    fields: Vec<FieldSpec<'a>>,
}

struct Record {
    literals: Vec<String>,
    fields: Vec<String>,
    is_numeric: Vec<bool>,
}

/// Detects a verbose C/C++ compiler invocation stream from a bounded sample.
pub(super) fn detect(sample: &[String]) -> bool {
    scope::detect_sampled(
        sample,
        |head| scope::skip_leading_preambles(head, is_static_preamble),
        is_log_head,
        has_strong_evidence,
    )
}

/// Keeps compacted C/C++ compile outputs terminal across repeated passes outside Markdown fences.
pub(super) fn is_compacted(lines: &[&str]) -> bool {
    scope::is_compacted(lines, HEADER)
}

/// Counter steps outside compacted blocks that cannot be represented as compile/progress templates.
pub(super) fn is_unhandled_counter_step(line: &str) -> bool {
    cmake_build::is_counter_step(line) && parse_row(line).is_none()
}

/// Plans atomic lossless template edits across contiguous verbose C/C++ compile blocks,
/// skipping line ranges already claimed by earlier Stage 1 passes.
pub(super) fn plan(lines: &[&str], claimed: &[Range<usize>]) -> Vec<LayoutEdit> {
    if is_compacted(lines) || lines.iter().any(|line| line.len() > MAX_BYTES) {
        return Vec::new();
    }

    let mut edits = Vec::new();
    let mut active = true;
    let mut open_fence = None;
    let mut claimed = ClaimedCursor::new(claimed);
    let mut index = scope::leading_preamble_len(lines, is_static_preamble);

    while index < lines.len() {
        // Invariant: claimed spans come from verified unfenced lossless blocks, so skipping them
        // keeps `open_fence` intact; preserve `active` only when every claimed line stays inside
        // this pass's scope lifecycle, matching an unclaimed scan while closing on foreign lines.
        if let Some(next_index) = claimed.skip_if_claimed(index) {
            if active
                && !lines.get(index..next_index).is_some_and(|span| {
                    span.iter().all(|raw| {
                        complete(raw).is_some_and(|line| plain(line) && is_in_scope_lifecycle(line))
                    })
                })
            {
                active = false;
            }
            index = next_index;
            continue;
        }
        let row_limit = claimed.unclaimed_end(index, lines.len());
        let Some(line) = scope::enter_line_indexed(
            lines,
            index,
            row_limit,
            &mut active,
            &mut open_fence,
            |idx| parse_plan_row(lines, idx, row_limit).is_some(),
        ) else {
            index += 1;
            continue;
        };

        if parse_plan_row(lines, index, row_limit).is_none() {
            if !is_in_scope_lifecycle(line) {
                active = false;
            }
            index += 1;
            continue;
        }

        let start = index;
        let mut scan_end = index;
        let mut bytes = 0usize;
        let mut parsed_chunk = Vec::new();
        let chunk_limit = row_limit.min(start + MAX_ROWS);
        while scan_end < chunk_limit {
            let Some(next) = complete(lines[scan_end]) else {
                break;
            };
            if !plain(next) {
                break;
            }
            let entry = if next.is_empty() {
                None
            } else if let Some(parsed) = parse_plan_row(lines, scan_end, chunk_limit) {
                Some(parsed)
            } else {
                break;
            };
            let line_len = lines[scan_end].len();
            if bytes + line_len > MAX_BYTES {
                break;
            }
            bytes += line_len;
            parsed_chunk.push(entry);
            scan_end += 1;
        }

        let mut end = scan_end;
        while end > start
            && matches!(
                parsed_chunk.last(),
                Some(
                    None | Some(ParsedRow {
                        kind: ParsedKind::PairedLabel,
                        ..
                    })
                )
            )
        {
            parsed_chunk.pop();
            end -= 1;
        }
        let compile_rows = parsed_chunk
            .iter()
            .filter(|entry| {
                matches!(
                    entry,
                    Some(ParsedRow {
                        kind: ParsedKind::Compile { .. },
                        ..
                    })
                )
            })
            .count();
        let non_blank_rows = parsed_chunk.iter().filter(|entry| entry.is_some()).count();
        index = end.max(start + 1);

        if compile_rows >= 1 && non_blank_rows >= 2 && end > start {
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

fn split_ws_preserving(line: &str) -> Vec<Segment<'_>> {
    let mut segments = Vec::new();
    let bytes = line.as_bytes();
    let mut start = 0usize;
    while start < bytes.len() {
        let is_space = bytes[start] == b' ';
        let mut end = start + 1;
        while end < bytes.len() && (bytes[end] == b' ') == is_space {
            end += 1;
        }
        let slice = &line[start..end];
        if is_space {
            segments.push(Segment::Spaces(slice));
        } else {
            segments.push(Segment::Word(slice));
        }
        start = end;
    }
    segments
}

fn has_shell_meta(word: &str) -> bool {
    word.contains([';', '|', '&', '>', '<', '`', '\t']) || word.contains("$(")
}

fn is_clean_path_token(word: &str) -> bool {
    !word.is_empty()
        && !word.starts_with('-')
        && !word.contains(['\'', '"'])
        && !has_shell_meta(word)
}

fn is_cc_binary(word: &str) -> bool {
    if word.is_empty() || word.contains(['\'', '"']) || has_shell_meta(word) {
        return false;
    }
    let basename = word.rsplit('/').next().unwrap_or(word);
    if matches!(basename, "cc" | "gcc" | "clang" | "c++" | "g++" | "clang++") {
        return true;
    }
    for prefix in ["gcc-", "g++-", "clang-", "clang++-"] {
        if let Some(version) = basename.strip_prefix(prefix)
            && !version.is_empty()
            && version.bytes().all(|b| b.is_ascii_digit())
        {
            return true;
        }
    }
    false
}

fn ends_with_any(value: &str, suffixes: &[&str]) -> bool {
    suffixes.iter().any(|suffix| {
        let basename = value.rsplit('/').next().unwrap_or(value);
        value.ends_with(suffix) && basename.len() > suffix.len()
    })
}

fn parse_cd_prefix(segments: &[Segment<'_>]) -> Option<(usize, bool)> {
    // Expect: Word("cd"), Spaces, Word(dir), Spaces, Word("&&"), optional Spaces.
    let (Some(Segment::Word("cd")), Some(Segment::Spaces(_)), Some(Segment::Word(dir))) =
        (segments.first(), segments.get(1), segments.get(2))
    else {
        return None;
    };
    if !is_clean_path_token(dir) {
        return None;
    }
    if let (Some(Segment::Spaces(_)), Some(Segment::Word("&&"))) =
        (segments.get(3), segments.get(4))
    {
        let next = match segments.get(5) {
            Some(Segment::Spaces(_)) => 6,
            _ => 5,
        };
        return Some((next, true));
    }
    None
}

fn parse_compile(line: &str) -> Option<ParsedRow<'_>> {
    let mut literals = Vec::new();
    let mut fields = Vec::new();
    let mut current_lit = String::new();
    let mut ninja_step = false;

    let body = if line.starts_with('[')
        && let Some((Counter::Ninja { step, total }, rest)) = parse_counter(line)
    {
        ninja_step = true;
        current_lit.push('[');
        literals.push(std::mem::take(&mut current_lit));
        fields.push(FieldSpec {
            value: step,
            is_numeric: true,
        });
        current_lit.push('/');
        current_lit.push_str(total);
        current_lit.push_str("] ");
        rest
    } else {
        line
    };

    let segments = split_ws_preserving(body);
    let (comp_start, has_cd) = match parse_cd_prefix(&segments) {
        Some((idx, cd)) => (idx, cd),
        None => (0, false),
    };
    for seg in &segments[..comp_start] {
        match seg {
            Segment::Word(w) | Segment::Spaces(w) => current_lit.push_str(w),
        }
    }

    let Some(Segment::Word(compiler)) = segments.get(comp_start) else {
        return None;
    };
    if !is_cc_binary(compiler) {
        return None;
    }
    current_lit.push_str(compiler);

    let rest = &segments[comp_start + 1..];
    let mut has_c_flag = false;
    let mut has_dep_flag = false;
    let mut has_source_field = false;
    let mut var_word_indices = vec![false; rest.len()];

    let mut idx = 0usize;
    while idx < rest.len() {
        let Segment::Word(word) = rest[idx] else {
            idx += 1;
            continue;
        };
        if has_shell_meta(word) {
            return None;
        }
        if word == "-c" {
            has_c_flag = true;
            idx += 1;
            continue;
        }
        if matches!(word, "-MD" | "-MMD" | "-MT" | "-MQ" | "-MF") {
            has_dep_flag = true;
        }
        if matches!(word, "-o" | "-MT" | "-MQ" | "-MF") {
            if let (Some(Segment::Spaces(_)), Some(Segment::Word(arg))) =
                (rest.get(idx + 1), rest.get(idx + 2))
            {
                if has_shell_meta(arg) {
                    return None;
                }
                if is_clean_path_token(arg) {
                    var_word_indices[idx + 2] = true;
                    if ends_with_any(arg, SOURCE_EXTS) {
                        has_source_field = true;
                    }
                    idx += 3;
                    continue;
                }
            }
            idx += 1;
            continue;
        }
        if SKIP_NEXT_ARG_FLAGS.contains(&word) {
            if let (Some(Segment::Spaces(_)), Some(Segment::Word(arg))) =
                (rest.get(idx + 1), rest.get(idx + 2))
            {
                if has_shell_meta(arg) {
                    return None;
                }
                idx += 3;
                continue;
            }
            idx += 1;
            continue;
        }
        if is_clean_path_token(word)
            && (ends_with_any(word, SOURCE_EXTS) || ends_with_any(word, OBJECT_EXTS))
        {
            var_word_indices[idx] = true;
            if ends_with_any(word, SOURCE_EXTS) {
                has_source_field = true;
            }
        }
        idx += 1;
    }

    let var_count = var_word_indices.iter().filter(|&&marked| marked).count();
    if !has_c_flag || !has_source_field || var_count == 0 {
        return None;
    }

    for (seg_idx, seg) in rest.iter().enumerate() {
        match *seg {
            Segment::Spaces(s) => current_lit.push_str(s),
            Segment::Word(w) => {
                if var_word_indices[seg_idx] {
                    literals.push(std::mem::take(&mut current_lit));
                    fields.push(FieldSpec {
                        value: w,
                        is_numeric: false,
                    });
                } else {
                    current_lit.push_str(w);
                }
            }
        }
    }
    literals.push(current_lit);

    let is_verbose = ninja_step || has_cd || compiler.contains('/') || has_dep_flag;
    Some(ParsedRow {
        kind: ParsedKind::Compile {
            is_verbose,
            has_ninja_step: ninja_step,
        },
        literals,
        fields,
    })
}

fn parse_percent_action(line: &str) -> Option<ParsedRow<'_>> {
    if !line.starts_with('[') || line.contains('\t') {
        return None;
    }
    let (Counter::Percent(pct), body) = parse_counter(line)? else {
        return None;
    };
    let (verb, target) = parse_action(body)?;
    Some(ParsedRow {
        kind: ParsedKind::PairedLabel,
        literals: vec!["[".into(), format!("%] {verb}"), String::new()],
        fields: vec![
            FieldSpec {
                value: pct,
                is_numeric: true,
            },
            FieldSpec {
                value: target,
                is_numeric: false,
            },
        ],
    })
}

fn parse_make_tag(line: &str) -> Option<ParsedRow<'_>> {
    for tag in ["CC ", "CXX ", "AS "] {
        if let Some(target) = line.strip_prefix(tag)
            && is_object_path(target)
        {
            return Some(ParsedRow {
                kind: ParsedKind::PairedLabel,
                literals: vec![tag.into(), String::new()],
                fields: vec![FieldSpec {
                    value: target,
                    is_numeric: false,
                }],
            });
        }
    }
    None
}

fn parse_row(line: &str) -> Option<ParsedRow<'_>> {
    if !valid_line(line) || line.contains('\t') {
        return None;
    }
    parse_compile(line)
        .or_else(|| parse_percent_action(line))
        .or_else(|| parse_make_tag(line))
}

fn parse_compile_at<'a>(lines: &[&'a str], index: usize, limit: usize) -> Option<ParsedRow<'a>> {
    let line = complete(lines.get(..limit)?.get(index)?)?;
    if !valid_line(line) || line.contains('\t') {
        return None;
    }
    parse_compile(line)
}

fn parse_plan_row<'a>(lines: &[&'a str], index: usize, limit: usize) -> Option<ParsedRow<'a>> {
    let line = complete(lines.get(..limit)?.get(index)?)?;
    if !valid_line(line) || line.contains('\t') {
        return None;
    }
    if let Some(compile) = parse_compile(line) {
        return Some(compile);
    }
    if let Some(percent) = parse_percent_action(line) {
        return matches!(
            parse_compile_at(lines, index + 1, limit),
            Some(ParsedRow {
                kind: ParsedKind::Compile {
                    has_ninja_step: false,
                    ..
                },
                ..
            })
        )
        .then_some(percent);
    }
    parse_make_tag(line)
}

fn is_cmake_bin(word: &str) -> bool {
    !has_shell_meta(word) && word.rsplit('/').next().unwrap_or(word) == "cmake"
}

fn is_make_bin(word: &str) -> bool {
    !has_shell_meta(word)
        && matches!(
            word.rsplit('/').next().unwrap_or(word),
            "make" | "gmake" | "mingw32-make"
        )
}

fn is_cmake_driver_line(line: &str) -> bool {
    if !valid_line(line) {
        return false;
    }
    let after_cd = if let Some(rest) = line.strip_prefix("cd ")
        && let Some((dir, tail)) = rest.split_once(" && ")
        && is_clean_path_token(dir.trim_end())
    {
        tail.trim_start()
    } else {
        line
    };
    let Some((cmd, rest)) = after_cd.split_once(' ') else {
        return false;
    };
    if !is_cmake_bin(cmd) {
        return false;
    }
    let rest = rest.trim_start();
    (rest.starts_with("-S") && rest.contains(" --check-build-system "))
        || rest.starts_with("-E cmake_progress_start ")
        || rest.starts_with("-E cmake_depends ")
        || rest.starts_with("-E cmake_link_script ")
        || rest.starts_with("-E cmake_symlink_library ")
        || rest.starts_with("-E rm ")
        || rest.starts_with("-P ")
}

fn is_make_subinvocation(line: &str) -> bool {
    valid_line(line)
        && line
            .split_once(' ')
            .is_some_and(|(cmd, rest)| is_make_bin(cmd) && !rest.trim().is_empty())
}

fn is_archive_or_fs_step(line: &str) -> bool {
    if !valid_line(line) {
        return false;
    }
    let Some((cmd, rest)) = line.split_once(' ') else {
        return false;
    };
    let base = cmd.rsplit('/').next().unwrap_or(cmd);
    !has_shell_meta(cmd)
        && !rest.trim().is_empty()
        && matches!(base, "ar" | "ranlib" | "mkdir" | "ln" | "cp")
}

fn is_link_command(line: &str) -> bool {
    if !valid_line(line) {
        return false;
    }
    let body = if line.starts_with('[')
        && let Some((Counter::Ninja { .. }, rest)) = parse_counter(line)
    {
        rest
    } else {
        line
    };
    if let Some(after_colon) = body.strip_prefix(": && ") {
        return after_colon.contains(" -o ")
            || after_colon.contains("/ar ")
            || after_colon.contains("cmake_symlink_library");
    }
    let segments = split_ws_preserving(body);
    let comp_start = parse_cd_prefix(&segments).map_or(0, |(idx, _)| idx);
    let Some(Segment::Word(compiler)) = segments.get(comp_start) else {
        return false;
    };
    if !is_cc_binary(compiler) {
        return false;
    }
    let rest = &segments[comp_start + 1..];
    let has_c = rest
        .iter()
        .any(|seg| matches!(seg, Segment::Word(word) if *word == "-c"));
    let has_o = rest
        .iter()
        .any(|seg| matches!(seg, Segment::Word(word) if *word == "-o"));
    !has_c
        && has_o
        && !rest
            .iter()
            .any(|seg| matches!(seg, Segment::Word(w) if has_shell_meta(w)))
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
                "cc",
                "gcc",
                "clang",
                "c++",
                "g++",
                "clang++",
            ],
        )
        || is_ninja_dir_notice(line)
        || is_ninja_warning(line)
        || is_make_dir_notice(line)
        || is_cmake_driver_line(line)
        || is_make_subinvocation(line)
        || line.starts_with("mkdir -p ")
        || line.starts_with("Change Dir: ")
        || line.starts_with("Run Build Command(s): ")
}

fn is_in_scope_lifecycle(line: &str) -> bool {
    line.is_empty()
        || is_ninja_dir_notice(line)
        || is_make_dir_notice(line)
        || is_ninja_no_work(line)
        || is_cmake_regen_step(line)
        || is_cmake_driver_line(line)
        || is_make_subinvocation(line)
        || is_archive_or_fs_step(line)
        || is_link_command(line)
        || cmake_build::is_counter_step(line)
        || (valid_line(line)
            && (line.starts_with("Change Dir: ") || line.starts_with("Run Build Command(s): ")))
}

/// Detects unfenced strong evidence in a sample whose head was already verified by a sibling build-step pass.
pub(super) fn has_sampled_evidence(sample: &[String]) -> bool {
    scope::has_unfenced_evidence(sample, has_strong_evidence)
}

fn is_log_head(line: &str) -> bool {
    scope::is_compacted_header(line, HEADER)
        || scope::is_compacted_header(line, cmake_build::HEADER)
        || parse_row(line).is_some()
        || cmake_build::is_counter_step(line)
}

fn has_strong_evidence(run: &[String]) -> bool {
    let compacted = run
        .iter()
        .any(|line| scope::is_compacted_header(line, HEADER));
    if compacted {
        return true;
    }
    let mut compile_count = 0usize;
    let mut verbose_compile_count = 0usize;
    let mut paired_label_count = 0usize;
    let mut prev_was_label = false;
    for line in run {
        let Some(row) = parse_row(line) else {
            prev_was_label = false;
            continue;
        };
        match row.kind {
            ParsedKind::Compile {
                is_verbose,
                has_ninja_step,
            } => {
                compile_count += 1;
                if is_verbose {
                    verbose_compile_count += 1;
                }
                if prev_was_label && !has_ninja_step {
                    paired_label_count += 1;
                }
                prev_was_label = false;
            }
            ParsedKind::PairedLabel => {
                prev_was_label = true;
            }
        }
    }
    compile_count >= 2 && (verbose_compile_count >= 2 || paired_label_count >= 2)
}

fn split_ext(field: &str) -> (&str, &str) {
    if field.contains(' ') {
        return (field, "");
    }
    let basename = field.rsplit('/').next().unwrap_or(field);
    for ext in EXT_SUFFIXES {
        if let Some(stem) = field.strip_suffix(ext)
            && basename.len() > ext.len()
        {
            return (stem, ext);
        }
    }
    (field, "")
}

fn render(rows: &[Option<ParsedRow<'_>>]) -> String {
    let mut records = Vec::with_capacity(rows.len());
    for entry in rows {
        let Some(parsed) = entry else {
            records.push(Record {
                literals: vec![String::new()],
                fields: Vec::new(),
                is_numeric: Vec::new(),
            });
            continue;
        };
        debug_assert_eq!(parsed.literals.len(), parsed.fields.len() + 1);
        let mut literals = parsed.literals.clone();
        let mut fields = Vec::with_capacity(parsed.fields.len());
        let mut is_numeric = Vec::with_capacity(parsed.fields.len());
        for (idx, spec) in parsed.fields.iter().enumerate() {
            if spec.is_numeric {
                fields.push(spec.value.to_owned());
                is_numeric.push(true);
            } else {
                let (stem, ext) = split_ext(spec.value);
                if !ext.is_empty() {
                    literals[idx + 1] = format!("{ext}{}", literals[idx + 1]);
                }
                fields.push(stem.to_owned());
                is_numeric.push(false);
            }
        }
        records.push(Record {
            literals,
            fields,
            is_numeric,
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

    let mut prefix_lens: Vec<Vec<usize>> = templates
        .iter()
        .map(|template| vec![0usize; template.len().saturating_sub(1)])
        .collect();
    // Invariant: records sharing a template id share equal `literals` (hence
    // identical field counts and `is_numeric` flags), and `prefix_lens[id][f_idx]`
    // is a char-boundary byte prefix of every `records[idx].fields[f_idx]` in `group`.
    for (id, group) in groups.iter().enumerate() {
        if group.len() < 2 || records[group[0]].fields.is_empty() {
            continue;
        }
        let field_count = records[group[0]].fields.len();
        for f_idx in 0..field_count {
            if records[group[0]].is_numeric[f_idx] {
                continue;
            }
            let first = &records[group[0]].fields[f_idx];
            let common_bytes = group[1..]
                .iter()
                .map(|&index| {
                    first
                        .chars()
                        .zip(records[index].fields[f_idx].chars())
                        .take_while(|(a, b)| a == b)
                        .map(|(ch, _)| ch.len_utf8())
                        .sum::<usize>()
                })
                .min()
                .unwrap_or(0);
            if let Some(pos) = first[..common_bytes].rfind(['/', '_', '-']) {
                let prefix = &first[..=pos];
                templates[id][f_idx].push_str(prefix);
                prefix_lens[id][f_idx] = prefix.len();
            }
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
            output.push_str(&id.to_string());
            for (f_idx, field) in record.fields.iter().enumerate() {
                output.push('\t');
                output.push_str(&field[prefix_lens[id][f_idx]..]);
            }
            output.push('\n');
        }
    }
    output
}

#[cfg(test)]
mod tests;
