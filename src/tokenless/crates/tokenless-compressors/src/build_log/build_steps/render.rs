//! Lossless template rendering, extension folding, directory bucketing, and prefix factoring.

use std::collections::HashMap;

use tokenless_protocol::estimate_tokens;

use super::grammar::{CompileRow, Counter, FieldKind, MakeTagRow, ProgressRow};
use super::{CMAKE_HEADER, COMPILE_HEADER};

// Match compound object extensions before bare `.o`/`.obj` so shared suffix templates stay maximal.
const CMAKE_EXT_SUFFIXES: &[&str] = &[
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

const COMPILE_EXT_SUFFIXES: &[&str] = &[
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

pub(super) enum CompileBlockItem<'a> {
    Blank,
    Compile(&'a CompileRow<'a>),
    Progress(&'a ProgressRow<'a>),
    MakeTag(&'a MakeTagRow<'a>),
}

struct ProgressRecord {
    literals: Vec<String>,
    fields: Vec<String>,
}

struct CompileRecord {
    literals: Vec<String>,
    fields: Vec<String>,
    kinds: Vec<FieldKind>,
}

fn split_ext<'a, 's>(target: &'a str, suffixes: &[&'s str]) -> (&'a str, &'s str) {
    if target.contains(' ') {
        return (target, "");
    }
    let basename = target.rsplit('/').next().unwrap_or(target);
    for &ext in suffixes {
        if let Some(stem) = target.strip_suffix(ext)
            && basename.len() > ext.len()
        {
            return (stem, ext);
        }
    }
    (target, "")
}

/// Renders a pure CMake/Ninja progress block using `CMAKE_HEADER` (`dir_counts >= 3`).
pub(super) fn render_progress(rows: &[Option<ProgressRow<'_>>]) -> String {
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
        let (stem, ext) = split_ext(parsed.target, CMAKE_EXT_SUFFIXES);
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
            records.push(ProgressRecord {
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
        records.push(ProgressRecord {
            literals: vec!["[".into(), lit1, ext.into()],
            fields: vec![idx_field.into(), stem.into()],
        });
    }

    let (mut templates, groups, ids) =
        group_by_literals(&records, |record| record.literals.as_slice());

    let mut prefix_lens = vec![0usize; templates.len()];
    for (id, group) in groups.iter().enumerate() {
        // Blank rows carry no fields and must never participate in target-prefix sharing.
        if records[group[0]].fields.is_empty() || group.len() < 2 {
            continue;
        }
        let first = &records[group[0]].fields[1];
        let common_bytes = common_char_prefix_bytes(
            group[1..]
                .iter()
                .map(|&idx| (first.as_str(), records[idx].fields[1].as_str())),
        );
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
    let mut output = format!("{CMAKE_HEADER}{header}\n");
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

/// Renders a verbose C/C++ compile block (including paired `[PPP%]` / `MakeTag` labels)
/// with extension folding, cost-aware directory bucketing, and common-prefix factoring.
pub(super) fn render_compile(items: &[CompileBlockItem<'_>]) -> String {
    let mut records = build_compile_records(items);
    let (mut templates, mut groups, mut ids) =
        group_by_literals(&records, |record| record.literals.as_slice());

    if apply_cost_aware_directory_buckets(&mut records, &templates, &groups) {
        (templates, groups, ids) = group_by_literals(&records, |record| record.literals.as_slice());
    }

    let mut prefix_lens: Vec<Vec<usize>> = templates
        .iter()
        .map(|template| vec![0usize; template.len().saturating_sub(1)])
        .collect();

    for (id, group) in groups.iter().enumerate() {
        if group.len() < 2 || records[group[0]].fields.is_empty() {
            continue;
        }
        let field_count = records[group[0]].fields.len();
        for f_idx in 0..field_count {
            if !group
                .iter()
                .all(|&idx| records[idx].kinds.get(f_idx) == Some(&FieldKind::Text))
            {
                continue;
            }
            let first = &records[group[0]].fields[f_idx];
            let common_bytes = common_char_prefix_bytes(
                group[1..]
                    .iter()
                    .map(|&idx| (first.as_str(), records[idx].fields[f_idx].as_str())),
            );
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
    let mut output = format!("{COMPILE_HEADER}{header}\n");
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

fn build_compile_records(items: &[CompileBlockItem<'_>]) -> Vec<CompileRecord> {
    let mut records = Vec::with_capacity(items.len());
    for item in items {
        match item {
            CompileBlockItem::Blank => records.push(CompileRecord {
                literals: vec![String::new()],
                fields: Vec::new(),
                kinds: Vec::new(),
            }),
            CompileBlockItem::Compile(compile) => {
                debug_assert_eq!(compile.literals.len(), compile.fields.len() + 1);
                let mut literals = compile.literals.clone();
                let mut fields = Vec::with_capacity(compile.fields.len());
                let mut kinds = Vec::with_capacity(compile.fields.len());
                for (idx, spec) in compile.fields.iter().enumerate() {
                    if spec.kind == FieldKind::Numeric {
                        fields.push(spec.value.to_owned());
                        kinds.push(FieldKind::Numeric);
                    } else {
                        let (stem, ext) = split_ext(spec.value, COMPILE_EXT_SUFFIXES);
                        if !ext.is_empty() {
                            literals[idx + 1] = format!("{ext}{}", literals[idx + 1]);
                        }
                        fields.push(stem.to_owned());
                        kinds.push(FieldKind::Text);
                    }
                }
                records.push(CompileRecord {
                    literals,
                    fields,
                    kinds,
                });
            }
            CompileBlockItem::Progress(progress) => {
                let (lit1, idx_field) = match progress.counter {
                    Counter::Ninja { step, total } => {
                        (format!("/{total}] {}", progress.verb), step)
                    }
                    Counter::Percent(pct) => (format!("%] {}", progress.verb), pct),
                };
                let (stem, ext) = split_ext(progress.target, COMPILE_EXT_SUFFIXES);
                records.push(CompileRecord {
                    literals: vec!["[".into(), lit1, ext.into()],
                    fields: vec![idx_field.into(), stem.into()],
                    kinds: vec![FieldKind::Numeric, FieldKind::Text],
                });
            }
            CompileBlockItem::MakeTag(tag) => {
                let (stem, ext) = split_ext(tag.target, COMPILE_EXT_SUFFIXES);
                records.push(CompileRecord {
                    literals: vec![tag.tag.into(), ext.into()],
                    fields: vec![stem.into()],
                    kinds: vec![FieldKind::Text],
                });
            }
        }
    }
    records
}

fn apply_cost_aware_directory_buckets(
    records: &mut [CompileRecord],
    base_templates: &[Vec<String>],
    base_groups: &[Vec<usize>],
) -> bool {
    let mut mutated = false;
    for (base_lit, group) in base_templates.iter().zip(base_groups) {
        if group.len() < 2 || records[group[0]].fields.is_empty() {
            continue;
        }
        let mut sigs = Vec::with_capacity(group.len());
        let mut sig_counts: HashMap<Vec<Option<String>>, usize> = HashMap::new();
        for &rec_idx in group {
            let record = &records[rec_idx];
            let sig = record
                .fields
                .iter()
                .enumerate()
                .map(|(f_idx, field)| {
                    if record.kinds.get(f_idx) != Some(&FieldKind::Text) {
                        return None;
                    }
                    field.rfind('/').map(|pos| field[..=pos].to_owned())
                })
                .collect::<Vec<_>>();
            *sig_counts.entry(sig.clone()).or_insert(0) += 1;
            sigs.push(sig);
        }

        if sig_counts.len() < 2 {
            continue;
        }

        let field_count = records[group[0]].fields.len();
        let mut base_prefix_len = vec![0usize; field_count];
        for (f_idx, slot) in base_prefix_len.iter_mut().enumerate() {
            if !group
                .iter()
                .all(|&idx| records[idx].kinds.get(f_idx) == Some(&FieldKind::Text))
            {
                continue;
            }
            let first = &records[group[0]].fields[f_idx];
            let common_bytes = common_char_prefix_bytes(
                group[1..]
                    .iter()
                    .map(|&idx| (first.as_str(), records[idx].fields[f_idx].as_str())),
            );
            if let Some(pos) = first[..common_bytes].rfind(['/', '_', '-']) {
                *slot = pos + 1;
            }
        }

        let template_overhead =
            base_lit.iter().map(String::len).sum::<usize>() + 3 * base_lit.len() + 4;
        let template_overhead_tokens =
            estimate_tokens(&serde_json::to_string(base_lit).unwrap_or_default()) + 1;

        for (row_pos, &rec_idx) in group.iter().enumerate() {
            let sig = &sigs[row_pos];
            let count = sig_counts.get(sig).copied().unwrap_or(0);
            let extra_dir_bytes: usize = sig
                .iter()
                .enumerate()
                .map(|(f_idx, dir_opt)| {
                    dir_opt
                        .as_ref()
                        .map_or(0, |dir| dir.len().saturating_sub(base_prefix_len[f_idx]))
                })
                .sum();
            let extra_dir_tokens: usize = sig
                .iter()
                .enumerate()
                .map(|(f_idx, dir_opt)| {
                    dir_opt
                        .as_ref()
                        .and_then(|dir| dir.get(base_prefix_len[f_idx]..))
                        .map_or(0, estimate_tokens)
                })
                .sum();
            if count >= 2
                && (count - 1) * extra_dir_bytes > template_overhead
                && (count - 1) * extra_dir_tokens > template_overhead_tokens
            {
                let record = &mut records[rec_idx];
                for (f_idx, dir_opt) in sig.iter().enumerate() {
                    if let Some(dir) = dir_opt {
                        record.literals[f_idx].push_str(dir);
                        record.fields[f_idx].drain(..dir.len());
                    }
                }
                mutated = true;
            }
        }
    }
    mutated
}

fn group_by_literals<'a, T>(
    records: &'a [T],
    literals_of: impl Fn(&'a T) -> &'a [String],
) -> (Vec<Vec<String>>, Vec<Vec<usize>>, Vec<usize>) {
    let mut seen: HashMap<&'a [String], usize> = HashMap::new();
    let mut templates: Vec<Vec<String>> = Vec::new();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut ids = Vec::with_capacity(records.len());
    for (index, record) in records.iter().enumerate() {
        let lits = literals_of(record);
        let id = if let Some(&existing) = seen.get(lits) {
            existing
        } else {
            let new_id = templates.len();
            seen.insert(lits, new_id);
            templates.push(lits.to_vec());
            groups.push(Vec::new());
            new_id
        };
        groups[id].push(index);
        ids.push(id);
    }
    (templates, groups, ids)
}

fn common_char_prefix_bytes<'a>(pairs: impl Iterator<Item = (&'a str, &'a str)>) -> usize {
    pairs
        .map(|(first, other)| {
            first
                .chars()
                .zip(other.chars())
                .take_while(|(a, b)| a == b)
                .map(|(ch, _)| ch.len_utf8())
                .sum::<usize>()
        })
        .min()
        .unwrap_or(0)
}
