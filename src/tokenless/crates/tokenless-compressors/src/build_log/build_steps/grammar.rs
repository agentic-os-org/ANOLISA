//! Mutually exclusive line grammar, target pairing, and stream evidence for `build_steps`.

use std::borrow::Cow;
use std::collections::HashSet;

use super::super::scope::{self, MAX_LINE_BYTES, valid_line};
use super::{CMAKE_HEADER, COMPILE_HEADER};

const OBJECT_SUFFIXES: &[&str] = &[".o", ".obj", ".lo", ".pch", ".gch", ".pcm", ".ifc", ".res"];

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

#[derive(Clone, Copy)]
enum Quote {
    Single,
    Double,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Counter<'a> {
    Ninja { step: &'a str, total: &'a str },
    Percent(&'a str),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ProgressAction {
    BuildObject,
    Other,
}

#[derive(Clone, Copy)]
pub(super) struct ProgressRow<'a> {
    pub(super) counter: Counter<'a>,
    pub(super) action: ProgressAction,
    pub(super) verb: &'a str,
    pub(super) target: &'a str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FieldKind {
    Text,
    Numeric,
}

#[derive(Clone, Copy)]
pub(super) struct FieldSpec<'a> {
    pub(super) value: &'a str,
    pub(super) kind: FieldKind,
}

// Invariant: `literals.len() == fields.len() + 1` so `render_compile` interleaving is total.
pub(super) struct CompileRow<'a> {
    pub(super) has_ninja_step: bool,
    pub(super) is_verbose: bool,
    pub(super) mt_targets: Vec<&'a str>,
    pub(super) output_obj: Option<&'a str>,
    pub(super) literals: Vec<String>,
    pub(super) fields: Vec<FieldSpec<'a>>,
}

#[derive(Clone, Copy)]
pub(super) struct MakeTagRow<'a> {
    pub(super) tag: &'static str,
    pub(super) target: &'a str,
}

pub(super) enum LineKind<'a> {
    Blank,
    Compile(CompileRow<'a>),
    Progress(ProgressRow<'a>),
    MakeTag(MakeTagRow<'a>),
}

/// Fast index of object targets produced by `Compile` rows in a block or sample run.
#[derive(Default)]
pub(super) struct CompileTargetIndex<'a> {
    targets: HashSet<&'a str>,
}

impl<'a> CompileTargetIndex<'a> {
    pub(super) fn insert(&mut self, compile: &CompileRow<'a>) {
        for mt in &compile.mt_targets {
            self.targets.insert(strip_dot_slash(mt));
        }
        if let Some(obj) = compile.output_obj {
            self.targets.insert(strip_dot_slash(obj));
        }
    }

    pub(super) fn matches(&self, progress_target: &str) -> bool {
        self.targets.contains(strip_dot_slash(progress_target))
    }
}

fn strip_dot_slash(path: &str) -> &str {
    let mut cur = path;
    while let Some(rest) = cur.strip_prefix("./") {
        cur = rest;
    }
    cur
}

fn is_safe_grammar_line(line: &str) -> bool {
    valid_line(line) && !scope::is_fence_line(line)
}

/// Parses a single validated line into at most one mutually exclusive `LineKind`.
pub(super) fn parse_line(line: &str) -> Option<LineKind<'_>> {
    if !is_safe_grammar_line(line) {
        return None;
    }
    if line.is_empty() {
        return Some(LineKind::Blank);
    }
    if line.contains('\t') {
        return None;
    }
    if let Some(compile) = parse_compile(line) {
        return Some(LineKind::Compile(compile));
    }
    if let Some(progress) = parse_progress(line) {
        return Some(LineKind::Progress(progress));
    }
    parse_make_tag(line).map(LineKind::MakeTag)
}

fn split_shell_preserving(line: &str) -> Option<Vec<Segment<'_>>> {
    let mut segments = Vec::new();
    let bytes = line.as_bytes();
    let mut idx = 0usize;
    while idx < bytes.len() {
        if bytes[idx] == b' ' {
            let start = idx;
            while idx < bytes.len() && bytes[idx] == b' ' {
                idx += 1;
            }
            segments.push(Segment::Spaces(&line[start..idx]));
            continue;
        }
        let start = idx;
        let mut quote: Option<Quote> = None;
        while idx < bytes.len() {
            let b = bytes[idx];
            match quote {
                Some(Quote::Single) => {
                    idx += 1;
                    if b == b'\'' {
                        quote = None;
                    }
                }
                Some(Quote::Double) => {
                    if b == b'\\' {
                        let next = line.get(idx + 1..)?.chars().next()?;
                        idx += 1 + next.len_utf8();
                    } else {
                        idx += 1;
                        if b == b'"' {
                            quote = None;
                        }
                    }
                }
                None => {
                    if b == b' ' {
                        break;
                    } else if b == b'\'' {
                        quote = Some(Quote::Single);
                        idx += 1;
                    } else if b == b'"' {
                        quote = Some(Quote::Double);
                        idx += 1;
                    } else if b == b'\\' {
                        let next = line.get(idx + 1..)?.chars().next()?;
                        idx += 1 + next.len_utf8();
                    } else {
                        idx += 1;
                    }
                }
            }
        }
        if quote.is_some() {
            return None;
        }
        segments.push(Segment::Word(&line[start..idx]));
    }
    Some(segments)
}

fn command_words<'a>(segments: &[Segment<'a>]) -> Vec<&'a str> {
    segments
        .iter()
        .filter_map(|seg| match *seg {
            Segment::Word(word) => Some(word),
            Segment::Spaces(_) => None,
        })
        .collect()
}

fn tokenized_command_words(body: &str, allow_cd_prefix: bool) -> Option<Vec<&str>> {
    let segments = split_shell_preserving(body)?;
    let cmd_start = if allow_cd_prefix {
        parse_cd_prefix(&segments).unwrap_or(0)
    } else {
        0
    };
    matches!(segments.get(cmd_start), Some(Segment::Word(_)))
        .then(|| command_words(&segments[cmd_start..]))
}

fn has_shell_meta(word: &str) -> bool {
    word.contains([';', '|', '&', '>', '<', '`', '\t']) || word.contains("$(")
}

fn has_shell_meta_or_comment(word: &str) -> bool {
    has_shell_meta(word) || word.starts_with('#')
}

fn is_safe_bin_token(word: &str) -> bool {
    !word.is_empty()
        && !scope::is_fence_line(word)
        && !word.contains(['\'', '"', '\\'])
        && !has_shell_meta(word)
}

fn is_clean_path_token(word: &str) -> bool {
    !word.is_empty()
        && !word.starts_with('-')
        && !word.contains(['\'', '"', '\\'])
        && !has_shell_meta(word)
}

fn unescape_shell_word(word: &str) -> Option<Cow<'_, str>> {
    if !word.contains(['\'', '"', '\\']) {
        return Some(Cow::Borrowed(word));
    }
    let mut out = String::with_capacity(word.len());
    let mut quote: Option<Quote> = None;
    let bytes = word.as_bytes();
    let mut idx = 0usize;
    while idx < bytes.len() {
        let b = bytes[idx];
        match quote {
            Some(Quote::Single) => {
                if b == b'\'' {
                    quote = None;
                    idx += 1;
                } else {
                    let ch = word.get(idx..)?.chars().next()?;
                    out.push(ch);
                    idx += ch.len_utf8();
                }
            }
            Some(Quote::Double) => {
                if b == b'\\' {
                    let next = word.get(idx + 1..)?.chars().next()?;
                    if !matches!(next, '"' | '\\' | '$' | '`') {
                        out.push('\\');
                    }
                    out.push(next);
                    idx += 1 + next.len_utf8();
                } else if b == b'"' {
                    quote = None;
                    idx += 1;
                } else {
                    let ch = word.get(idx..)?.chars().next()?;
                    out.push(ch);
                    idx += ch.len_utf8();
                }
            }
            None => {
                if b == b'\'' {
                    quote = Some(Quote::Single);
                    idx += 1;
                } else if b == b'"' {
                    quote = Some(Quote::Double);
                    idx += 1;
                } else if b == b'\\' {
                    let next = word.get(idx + 1..)?.chars().next()?;
                    out.push(next);
                    idx += 1 + next.len_utf8();
                } else {
                    let ch = word.get(idx..)?.chars().next()?;
                    out.push(ch);
                    idx += ch.len_utf8();
                }
            }
        }
    }
    quote.is_none().then_some(Cow::Owned(out))
}

fn strip_simple_outer_quotes(word: &str) -> Option<&str> {
    let inner = word
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .or_else(|| word.strip_prefix('"').and_then(|s| s.strip_suffix('"')))?;
    (!inner.is_empty() && !inner.contains(['\'', '"', '\\'])).then_some(inner)
}

fn parse_path_arg(word: &str) -> Option<Cow<'_, str>> {
    if word.is_empty() || word.starts_with('-') || has_shell_meta(word) {
        return None;
    }
    let unescaped = unescape_shell_word(word)?;
    if unescaped.is_empty() || unescaped.starts_with('-') {
        return None;
    }
    Some(unescaped)
}

fn borrowed_clean_target(word: &str) -> Option<&str> {
    if is_clean_path_token(word) {
        Some(word)
    } else {
        strip_simple_outer_quotes(word).filter(|inner| is_clean_path_token(inner))
    }
}

fn split_joined_path_flag(word: &str) -> Option<(&'static str, &str)> {
    for flag in ["-MT", "-MQ", "-MF", "-o"] {
        let Some(arg) = word.strip_prefix(flag) else {
            continue;
        };
        let Some(unescaped) = parse_path_arg(arg) else {
            continue;
        };
        if flag == "-o" && !unescaped.contains(['/', '.']) {
            continue;
        }
        return Some((flag, arg));
    }
    None
}

fn is_cc_binary(word: &str) -> bool {
    if !is_safe_bin_token(word) {
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
    let basename = value.rsplit('/').next().unwrap_or(value);
    suffixes
        .iter()
        .any(|suffix| value.ends_with(suffix) && basename.len() > suffix.len())
}

fn parse_cd_prefix(segments: &[Segment<'_>]) -> Option<usize> {
    let (Some(Segment::Word("cd")), Some(Segment::Spaces(_)), Some(Segment::Word(dir))) =
        (segments.first(), segments.get(1), segments.get(2))
    else {
        return None;
    };
    parse_path_arg(dir)?;
    if let (Some(Segment::Spaces(_)), Some(Segment::Word("&&"))) =
        (segments.get(3), segments.get(4))
    {
        let next = match segments.get(5) {
            Some(Segment::Spaces(_)) => 6,
            _ => 5,
        };
        return Some(next);
    }
    None
}

fn parse_compile(line: &str) -> Option<CompileRow<'_>> {
    let mut literals = Vec::new();
    let mut fields = Vec::new();
    let mut current_lit = String::new();
    let mut has_ninja_step = false;

    let body = if line.starts_with('[')
        && let Some((Counter::Ninja { step, total }, rest)) = parse_counter(line)
    {
        has_ninja_step = true;
        current_lit.push('[');
        literals.push(std::mem::take(&mut current_lit));
        fields.push(FieldSpec {
            value: step,
            kind: FieldKind::Numeric,
        });
        current_lit.push('/');
        current_lit.push_str(total);
        current_lit.push_str("] ");
        rest
    } else {
        line
    };

    let segments = split_shell_preserving(body)?;
    let cd_start = parse_cd_prefix(&segments);
    let comp_start = cd_start.unwrap_or(0);
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
    let mut mt_targets = Vec::new();
    let mut output_obj = None;
    let mut var_word_splits: Vec<Option<usize>> = vec![None; rest.len()];

    let mut idx = 0usize;
    while idx < rest.len() {
        let Segment::Word(word) = rest[idx] else {
            idx += 1;
            continue;
        };
        if has_shell_meta_or_comment(word) {
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
            if let (Some(&Segment::Spaces(_)), Some(&Segment::Word(arg))) =
                (rest.get(idx + 1), rest.get(idx + 2))
            {
                if has_shell_meta_or_comment(arg) {
                    return None;
                }
                if parse_path_arg(arg).is_some() {
                    var_word_splits[idx + 2] = Some(0);
                    if let Some(target) = borrowed_clean_target(arg) {
                        if matches!(word, "-MT" | "-MQ") && is_single_path(target) {
                            mt_targets.push(target);
                        } else if word == "-o" && is_object_path(target) {
                            output_obj = Some(target);
                        }
                    }
                }
                idx += 3;
                continue;
            }
            idx += 1;
            continue;
        }
        if let Some((flag, arg)) = split_joined_path_flag(word) {
            if matches!(flag, "-MT" | "-MQ" | "-MF") {
                has_dep_flag = true;
            }
            var_word_splits[idx] = Some(flag.len());
            if let Some(target) = borrowed_clean_target(arg) {
                if matches!(flag, "-MT" | "-MQ") && is_single_path(target) {
                    mt_targets.push(target);
                } else if flag == "-o" && is_object_path(target) {
                    output_obj = Some(target);
                }
            }
            idx += 1;
            continue;
        }
        if SKIP_NEXT_ARG_FLAGS.contains(&word) {
            if let (Some(&Segment::Spaces(_)), Some(&Segment::Word(arg))) =
                (rest.get(idx + 1), rest.get(idx + 2))
            {
                if has_shell_meta_or_comment(arg) {
                    return None;
                }
                idx += 3;
                continue;
            }
            idx += 1;
            continue;
        }
        if let Some(unescaped) = parse_path_arg(word)
            && (ends_with_any(&unescaped, SOURCE_EXTS) || ends_with_any(&unescaped, OBJECT_EXTS))
        {
            var_word_splits[idx] = Some(0);
            if ends_with_any(&unescaped, SOURCE_EXTS) {
                has_source_field = true;
            }
        }
        idx += 1;
    }

    let var_count = var_word_splits
        .iter()
        .filter(|split| split.is_some())
        .count();
    if !has_c_flag || !has_source_field || var_count == 0 {
        return None;
    }

    for (seg_idx, seg) in rest.iter().enumerate() {
        match *seg {
            Segment::Spaces(s) => current_lit.push_str(s),
            Segment::Word(w) => {
                if let Some(prefix_len) = var_word_splits[seg_idx] {
                    current_lit.push_str(&w[..prefix_len]);
                    literals.push(std::mem::take(&mut current_lit));
                    fields.push(FieldSpec {
                        value: &w[prefix_len..],
                        kind: FieldKind::Text,
                    });
                } else {
                    current_lit.push_str(w);
                }
            }
        }
    }
    literals.push(current_lit);

    let is_verbose = has_ninja_step || cd_start.is_some() || compiler.contains('/') || has_dep_flag;
    Some(CompileRow {
        has_ninja_step,
        is_verbose,
        mt_targets,
        output_obj,
        literals,
        fields,
    })
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

fn parse_action(body: &str) -> Option<(ProgressAction, &str, &str)> {
    if let Some(target) = body.strip_prefix("Building NVCC (Device) object ") {
        return is_object_path(target).then(|| {
            (
                ProgressAction::BuildObject,
                &body[..body.len() - target.len()],
                target,
            )
        });
    }
    if let Some(rest) = body
        .strip_prefix("Building ")
        .or_else(|| body.strip_prefix("Compiling "))
        && let Some((lang, target)) = rest.split_once(" object ")
        && is_valid_lang(lang)
        && is_object_path(target)
    {
        return Some((
            ProgressAction::BuildObject,
            &body[..body.len() - target.len()],
            target,
        ));
    }
    if let Some(target) = body
        .strip_prefix("Linking static target ")
        .or_else(|| body.strip_prefix("Linking target "))
    {
        return is_single_path(target).then(|| {
            (
                ProgressAction::Other,
                &body[..body.len() - target.len()],
                target,
            )
        });
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
                return Some((
                    ProgressAction::Other,
                    &body[..body.len() - target.len()],
                    target,
                ));
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
        return Some((
            ProgressAction::Other,
            &body[..body.len() - rest.len()],
            rest,
        ));
    }
    if let Some(target) = body.strip_prefix("Built target ") {
        return is_target_ident(target).then(|| {
            (
                ProgressAction::Other,
                &body[..body.len() - target.len()],
                target,
            )
        });
    }
    if let Some(outputs) = body.strip_prefix("Generating ") {
        let valid = !outputs.is_empty()
            && outputs
                .split(", ")
                .all(|path| is_single_path(path) && (path.contains('/') || path.contains('.')));
        return valid.then(|| {
            (
                ProgressAction::Other,
                &body[..body.len() - outputs.len()],
                outputs,
            )
        });
    }
    for prefix in [
        "Automatic MOC and UIC for target ",
        "Automatic MOC for target ",
        "Automatic UIC for target ",
        "Automatic RCC for target ",
    ] {
        if let Some(target) = body.strip_prefix(prefix) {
            return is_target_ident(target).then(|| {
                (
                    ProgressAction::Other,
                    &body[..body.len() - target.len()],
                    target,
                )
            });
        }
    }
    None
}

fn parse_progress(line: &str) -> Option<ProgressRow<'_>> {
    if line.contains('\t') {
        return None;
    }
    let (counter, body) = parse_counter(line)?;
    let (action, verb, target) = parse_action(body)?;
    Some(ProgressRow {
        counter,
        action,
        verb,
        target,
    })
}

fn parse_make_tag(line: &str) -> Option<MakeTagRow<'_>> {
    for tag in ["CC ", "CXX ", "AS "] {
        if let Some(target) = line.strip_prefix(tag)
            && is_object_path(target)
        {
            return Some(MakeTagRow { tag, target });
        }
    }
    None
}

fn is_ninja_diagnostic(line: &str) -> bool {
    is_safe_grammar_line(line)
        && (line.starts_with("ninja: error: ") || line.starts_with("ninja: fatal: "))
}

fn is_ninja_warning(line: &str) -> bool {
    is_safe_grammar_line(line) && line.starts_with("ninja: warning: ")
}

fn is_ninja_no_work(line: &str) -> bool {
    is_safe_grammar_line(line) && line == "ninja: no work to do."
}

fn is_ninja_dir_notice(line: &str) -> bool {
    is_safe_grammar_line(line) && line.starts_with("ninja: Entering directory ")
}

fn is_non_control_line(line: &str) -> bool {
    line.len() <= MAX_LINE_BYTES && !line.chars().any(|ch| ch != '\t' && ch.is_control())
}

pub(super) fn is_counter_step(line: &str) -> bool {
    line.starts_with('[')
        && is_non_control_line(line)
        && parse_counter(line).is_some_and(|(_, body)| {
            let trimmed = body.trim_start();
            !trimmed.is_empty() && !trimmed.starts_with("FAILED:") && !trimmed.starts_with("ninja:")
        })
}

/// Counter steps outside standard CMake/Ninja progress rows stay protected from Stash;
/// compacted compile blocks are already covered by `protected_ranges`, while unprofitable
/// counter-prefixed compile commands remain verbatim rather than falling into Generic omission.
pub(super) fn is_protected_step(line: &str) -> bool {
    is_counter_step(line) && !matches!(parse_line(line), Some(LineKind::Progress(_)))
}

fn is_cmake_regen_step(line: &str) -> bool {
    is_safe_grammar_line(line)
        && (line == "Re-running CMake..."
            || parse_counter(line).is_some_and(|(_, body)| {
                matches!(
                    body,
                    "Re-running CMake..." | "Running CMake to regenerate build system..."
                )
            }))
}

fn is_cmake_bin(word: &str) -> bool {
    is_safe_bin_token(word) && word.rsplit('/').next().unwrap_or(word) == "cmake"
}

fn is_make_bin(word: &str) -> bool {
    is_safe_bin_token(word)
        && matches!(
            word.rsplit('/').next().unwrap_or(word),
            "make" | "gmake" | "mingw32-make"
        )
}

fn is_cmake_driver_words(words: &[&str]) -> bool {
    let Some(&cmd) = words.first() else {
        return false;
    };
    if !is_cmake_bin(cmd)
        || words.len() < 2
        || words[1..]
            .iter()
            .any(|&word| has_shell_meta_or_comment(word))
    {
        return false;
    }
    if words[1].starts_with("-S") && words[2..].contains(&"--check-build-system") {
        return true;
    }
    match (words[1], words.get(2).copied()) {
        (
            "-E",
            Some(
                "cmake_progress_start"
                | "cmake_depends"
                | "cmake_link_script"
                | "cmake_symlink_library"
                | "rm",
            ),
        ) => words.len() >= 4,
        ("-P", Some(_)) => words.len() >= 3,
        _ => false,
    }
}

fn is_cmake_driver_line(line: &str) -> bool {
    is_safe_grammar_line(line)
        && tokenized_command_words(line, true).is_some_and(|words| is_cmake_driver_words(&words))
}

fn is_make_subinvocation(line: &str) -> bool {
    is_safe_grammar_line(line)
        && tokenized_command_words(line, true).is_some_and(|words| {
            words.first().is_some_and(|&cmd| is_make_bin(cmd))
                && words.len() >= 2
                && !words[1..]
                    .iter()
                    .any(|&word| has_shell_meta_or_comment(word))
        })
}

fn is_archive_or_fs_words(words: &[&str]) -> bool {
    let Some(&cmd) = words.first() else {
        return false;
    };
    let base = cmd.rsplit('/').next().unwrap_or(cmd);
    is_safe_bin_token(cmd)
        && words.len() >= 2
        && matches!(base, "ar" | "ranlib" | "mkdir" | "ln" | "cp")
        && !words[1..]
            .iter()
            .any(|&word| has_shell_meta_or_comment(word))
}

fn is_archive_or_fs_step(line: &str) -> bool {
    is_safe_grammar_line(line)
        && tokenized_command_words(line, true).is_some_and(|words| is_archive_or_fs_words(&words))
}

fn is_mkdir_p_step(line: &str) -> bool {
    is_safe_grammar_line(line)
        && tokenized_command_words(line, false).is_some_and(|words| {
            words.first() == Some(&"mkdir")
                && words.get(1) == Some(&"-p")
                && words.len() >= 3
                && !words[2..]
                    .iter()
                    .any(|&word| has_shell_meta_or_comment(word))
        })
}

fn is_cc_link_words(words: &[&str]) -> bool {
    let Some(&compiler) = words.first() else {
        return false;
    };
    if !is_cc_binary(compiler)
        || words[1..]
            .iter()
            .any(|&word| has_shell_meta_or_comment(word))
    {
        return false;
    }
    let args = &words[1..];
    let mut has_c = false;
    let mut has_o = false;
    let mut idx = 0usize;
    while idx < args.len() {
        let word = args[idx];
        if word == "-c" {
            has_c = true;
            idx += 1;
        } else if matches!(word, "-o" | "-MT" | "-MQ" | "-MF")
            || SKIP_NEXT_ARG_FLAGS.contains(&word)
        {
            if word == "-o" && idx + 1 < args.len() {
                has_o = true;
            }
            idx += if idx + 1 < args.len() { 2 } else { 1 };
        } else if let Some(arg) = word.strip_prefix("-o")
            && parse_path_arg(arg).is_some()
        {
            has_o = true;
            idx += 1;
        } else {
            idx += 1;
        }
    }
    !has_c && has_o
}

fn is_link_command(line: &str) -> bool {
    if !is_safe_grammar_line(line) {
        return false;
    }
    let body = if line.starts_with('[')
        && let Some((Counter::Ninja { .. }, rest)) = parse_counter(line)
    {
        rest
    } else {
        line
    };
    let Some(segments) = split_shell_preserving(body) else {
        return false;
    };
    if !matches!(segments.first(), Some(Segment::Word(_))) {
        return false;
    }
    let words = command_words(&segments);
    if words.first() == Some(&":") && words.get(1) == Some(&"&&") {
        let mut saw_build_step = false;
        for subcmd in words[2..].split(|&word| word == "&&") {
            match subcmd {
                [] => return false,
                [":"] => {}
                ["cd", dir] if parse_path_arg(dir).is_some() => {}
                _ if is_cc_link_words(subcmd)
                    || is_archive_or_fs_words(subcmd)
                    || is_cmake_driver_words(subcmd) =>
                {
                    saw_build_step = true;
                }
                _ => return false,
            }
        }
        return saw_build_step;
    }
    let cmd_start = parse_cd_prefix(&segments).unwrap_or(0);
    matches!(segments.get(cmd_start), Some(Segment::Word(_)))
        && is_cc_link_words(&command_words(&segments[cmd_start..]))
}

fn make_notice_tail(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("mingw32-make")
        .or_else(|| line.strip_prefix("gmake"))
        .or_else(|| line.strip_prefix("make"))?;
    if let Some(after_open) = rest.strip_prefix('[') {
        let (digits, tail) = after_open.split_once(']')?;
        (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then_some(tail)
    } else {
        Some(rest)
    }
}

pub(super) fn is_make_dir_notice(line: &str) -> bool {
    is_safe_grammar_line(line)
        && make_notice_tail(line).is_some_and(|tail| {
            tail.starts_with(": Entering directory ") || tail.starts_with(": Leaving directory ")
        })
}

pub(super) fn is_static_preamble(line: &str) -> bool {
    if !is_safe_grammar_line(line) {
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
        || is_mkdir_p_step(line)
        || line.starts_with("Change Dir: ")
        || line.starts_with("Run Build Command(s): ")
}

pub(super) fn is_in_scope_lifecycle(line: &str) -> bool {
    if scope::is_fence_line(line) {
        return false;
    }
    line.is_empty()
        || is_ninja_dir_notice(line)
        || is_make_dir_notice(line)
        || is_ninja_no_work(line)
        || is_cmake_regen_step(line)
        || is_cmake_driver_line(line)
        || is_make_subinvocation(line)
        || is_archive_or_fs_step(line)
        || is_link_command(line)
        || is_counter_step(line)
        || (is_safe_grammar_line(line)
            && (line.starts_with("Change Dir: ") || line.starts_with("Run Build Command(s): ")))
}

pub(super) fn is_strict_log_head(line: &str) -> bool {
    if scope::is_fence_line(line) {
        return false;
    }
    if scope::is_compacted_header(line, CMAKE_HEADER)
        || scope::is_compacted_header(line, COMPILE_HEADER)
        || is_counter_step(line)
        || is_ninja_no_work(line)
        || is_ninja_diagnostic(line)
        || is_cmake_regen_step(line)
    {
        return true;
    }
    match parse_line(line) {
        Some(LineKind::Progress(_) | LineKind::MakeTag(_)) => true,
        Some(LineKind::Compile(compile)) => compile.is_verbose,
        Some(LineKind::Blank) | None => false,
    }
}

pub(super) fn is_log_head(line: &str) -> bool {
    is_strict_log_head(line)
        || matches!(parse_line(line), Some(LineKind::Compile(_)))
        || is_link_command(line)
        || is_archive_or_fs_step(line)
}

pub(super) fn has_strong_evidence(run: &[String], allow_compacted_only: bool) -> bool {
    if allow_compacted_only
        && run.iter().any(|line| {
            scope::is_compacted_header(line, CMAKE_HEADER)
                || scope::is_compacted_header(line, COMPILE_HEADER)
        })
    {
        return true;
    }

    let mut progress_count = 0usize;
    let mut compile_count = 0usize;
    let mut verbose_compile_count = 0usize;
    let mut compile_targets = CompileTargetIndex::default();
    let mut label_targets = HashSet::new();
    let mut ninja_outcome = false;
    let mut stopped_marker = false;

    for line in run {
        if is_ninja_no_work(line) || is_ninja_diagnostic(line) {
            ninja_outcome = true;
        }
        if is_safe_grammar_line(line)
            && (line.starts_with("ninja: build stopped:") || line.starts_with("FAILED: "))
        {
            stopped_marker = true;
        }
        match parse_line(line) {
            Some(LineKind::Progress(progress)) => {
                progress_count += 1;
                if matches!(progress.counter, Counter::Percent(_))
                    && progress.action == ProgressAction::BuildObject
                {
                    label_targets.insert(progress.target);
                }
            }
            Some(LineKind::Compile(compile)) => {
                compile_count += 1;
                if compile.is_verbose {
                    verbose_compile_count += 1;
                }
                if !compile.has_ninja_step {
                    compile_targets.insert(&compile);
                }
            }
            Some(LineKind::MakeTag(tag)) => {
                label_targets.insert(tag.target);
            }
            Some(LineKind::Blank) | None => {}
        }
    }

    let paired_label_count = label_targets
        .iter()
        .filter(|target| compile_targets.matches(target))
        .count();

    let single_step_stopped = (progress_count >= 1 || compile_count >= 1) && stopped_marker;
    progress_count >= 2
        || ninja_outcome
        || single_step_stopped
        || verbose_compile_count >= 2
        || paired_label_count >= 2
}
