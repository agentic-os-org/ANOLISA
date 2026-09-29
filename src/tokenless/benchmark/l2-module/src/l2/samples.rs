// Copyright 2026 Alibaba Cloud
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Sample loading: static samples (`assets/samples/*.json`), command specs and
//! semantic-probe question sets, plus dynamic ground-truth extraction for
//! live-captured command output.
//!
//! Static JSON/code samples ship their ground truth in the asset file;
//! command/grep/diff samples cannot (their output depends on the repository
//! state at run time), so their ground truth is extracted from the raw
//! output of the very run being measured — guaranteeing the retention check
//! asserts facts that were actually present.

use crate::l2::{Category, GroundTruth, L2Error, SampleRecord};
use regex::Regex;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Maximum dynamically-extracted ground-truth items per sample. Keeps the
/// retention denominator comparable across samples of very different sizes.
const MAX_DYNAMIC_ITEMS: usize = 5;

#[derive(Deserialize)]
struct SampleFile {
    category: String,
    samples: Vec<RawSample>,
}

#[derive(Deserialize)]
struct RawSample {
    id: String,
    #[serde(default)]
    content_json: Option<serde_json::Value>,
    #[serde(default)]
    content_path: Option<String>,
    #[serde(default)]
    content_lines: Option<Vec<String>>,
    #[serde(default)]
    ground_truth: Vec<GroundTruth>,
}

/// One executable command spec from `samples/command_specs.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct CommandSpec {
    /// Stable id referenced by task simulations and report rows.
    pub id: String,
    /// One of `command` / `grep` / `diff`.
    pub category: String,
    /// Program + args, executed without a shell so quoting stays exact.
    pub argv: Vec<String>,
    /// Optional rtk entry point, when it differs from [`Self::argv`].
    ///
    /// rtk dispatches its filters by subcommand name, and some of those names
    /// (`read`, `smart`) are not executable programs, so the unfiltered
    /// baseline cannot reuse the same argv. Where this is set, `argv` produces
    /// the raw bytes and this produces the filtered ones; both must describe
    /// the same underlying content or the comparison is meaningless.
    #[serde(default)]
    pub rtk_argv: Option<Vec<String>>,
    /// Working directory relative to the repository root.
    pub cwd_rel: String,
    /// Always `"dynamic"` — kept explicit in the asset so a future static
    /// spec is a deliberate schema change, not an accident.
    pub ground_truth_source: String,
}

impl CommandSpec {
    /// The argv rtk is invoked with: [`Self::rtk_argv`] when set, else
    /// [`Self::argv`].
    pub fn rtk_invocation(&self) -> &[String] {
        self.rtk_argv.as_deref().unwrap_or(&self.argv)
    }
}

impl CommandSpec {
    /// Parses this spec's category string.
    ///
    /// # Errors
    ///
    /// Returns [`L2Error::InvalidSample`] for unknown category names.
    pub fn parsed_category(&self) -> Result<Category, L2Error> {
        Category::parse(&self.category).ok_or_else(|| {
            L2Error::InvalidSample(format!(
                "command spec {:?} has unknown category {:?}",
                self.id, self.category
            ))
        })
    }
}

/// One semantic-probe question with its literal answer check.
#[derive(Debug, Clone, Deserialize)]
pub struct ProbeQuestion {
    pub question: String,
    /// Substring the model's answer must contain to count as correct.
    pub expected_contains: String,
}

/// Loads the static `json` samples from `samples/json_api.json`.
///
/// `content_path` entries are resolved relative to the sample file itself so
/// the canonical fixture stays byte-identical to the L0/L1 suites.
///
/// # Errors
///
/// Fails on unreadable/malformed asset files or samples missing content.
pub fn load_json_samples(l2_dir: &Path) -> Result<Vec<SampleRecord>, L2Error> {
    load_static(l2_dir, "samples/json_api.json", Category::Json)
}

/// Loads the static `code` samples from `samples/source_code.json`.
///
/// # Errors
///
/// Fails on unreadable/malformed asset files or samples missing content.
pub fn load_code_samples(l2_dir: &Path) -> Result<Vec<SampleRecord>, L2Error> {
    load_static(l2_dir, "samples/source_code.json", Category::Code)
}

fn load_static(l2_dir: &Path, rel: &str, expect: Category) -> Result<Vec<SampleRecord>, L2Error> {
    let path = l2_dir.join(rel);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| L2Error::InvalidSample(format!("cannot read {}: {e}", path.display())))?;
    let file: SampleFile = serde_json::from_str(&text)?;
    if file.category != expect.name() {
        return Err(L2Error::InvalidSample(format!(
            "{rel} declares category {:?}, expected {:?}",
            file.category,
            expect.name()
        )));
    }
    let base = path.parent().map(PathBuf::from).unwrap_or_default();
    file.samples
        .into_iter()
        .map(|raw| {
            let content = resolve_content(&raw, &base)?;
            Ok(SampleRecord {
                id: raw.id,
                category: expect,
                content,
                ground_truth: raw.ground_truth,
            })
        })
        .collect()
}

// Content precedence: content_lines > content_json > content_path. Exactly
// one is expected per sample; the precedence only matters if an asset is
// over-specified, and then the most explicit (inline) form wins.
fn resolve_content(raw: &RawSample, base: &Path) -> Result<String, L2Error> {
    if let Some(lines) = &raw.content_lines {
        return Ok(lines.join("\n"));
    }
    if let Some(value) = &raw.content_json {
        return Ok(serde_json::to_string(value)?);
    }
    if let Some(rel) = &raw.content_path {
        let p = base.join(rel);
        let text = std::fs::read_to_string(&p).map_err(|e| {
            L2Error::InvalidSample(format!(
                "sample {:?}: cannot read content_path {}: {e}",
                raw.id,
                p.display()
            ))
        })?;
        // Re-serialize compactly when the referenced file is JSON so token
        // counts are measured on wire form, matching the L0/L1 convention.
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            return Ok(serde_json::to_string(&v)?);
        }
        return Ok(text);
    }
    Err(L2Error::InvalidSample(format!(
        "sample {:?} has no content_lines/content_json/content_path",
        raw.id
    )))
}

/// Loads `samples/command_specs.json`.
///
/// # Errors
///
/// Fails on unreadable/malformed spec files.
pub fn load_command_specs(l2_dir: &Path) -> Result<Vec<CommandSpec>, L2Error> {
    let path = l2_dir.join("samples/command_specs.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| L2Error::InvalidSample(format!("cannot read {}: {e}", path.display())))?;
    Ok(serde_json::from_str(&text)?)
}

/// Loads the probe question set for one category from `probes/<name>.json`.
///
/// # Errors
///
/// Fails on unreadable/malformed probe files.
pub fn load_probe_questions(l2_dir: &Path, file_stem: &str) -> Result<Vec<ProbeQuestion>, L2Error> {
    let path = l2_dir.join(format!("probes/{file_stem}.json"));
    let text = std::fs::read_to_string(&path)
        .map_err(|e| L2Error::InvalidSample(format!("cannot read {}: {e}", path.display())))?;
    Ok(serde_json::from_str(&text)?)
}

/// The reply a diff probe expects when the token is gone.
///
/// Scoring is substring-based (`answer.contains(expected)`), so a negative reply
/// must never contain the fact it denies — otherwise a lost token scores as
/// retained, inflating the semantic score. `"absent"` failed this: it contains
/// `sent`, which a changed line like `+status = "sent"` yields as a fact.
///
/// Shortening the word is not by itself a guarantee. The definition branch of
/// `extract_diff_ground_truth` has no minimum name length, so `+fn o() {}`
/// yields the one-character fact `o`, which even `"no"` contains.
/// [`diff_probe_questions`] therefore drops any fact this word contains rather
/// than relying on a length coincidence.
pub const DIFF_PROBE_ABSENT: &str = "no";

/// Builds content-level probe questions for a diff from its extracted ground
/// truth, rather than loading fixed format questions from an asset.
///
/// The diff is generated at run time, so its content is not known ahead of
/// time and the answers cannot be written into a static probe file. Sharing the
/// changed-line facts from [`extract_dynamic_ground_truth`] keeps both the
/// deterministic retention check and the semantic probe anchored to the same
/// content — "can the model still see that `<ident>` was changed?" — instead
/// of asking whether the output still looks like a diff.
///
/// Returns an empty vector when nothing content-level could be extracted, so
/// the caller probes nothing rather than asking an unanswerable question.
///
/// Two kinds of fact are skipped, because a question built on them cannot be
/// scored honestly. Retention still checks them — only the probe declines:
///
/// * facts [`DIFF_PROBE_ABSENT`] contains, since the negative reply would match
///   them by substring and score a lost token as retained;
/// * facts containing whitespace, since the question asks for a one-word reply
///   and a model obeying that instruction cannot produce a multi-word answer.
///
/// Skipped facts leave the sample with fewer questions, and a sample left with
/// none counts as unprobed in the report rather than as a perfect score.
pub fn diff_probe_questions(ground_truth: &[GroundTruth]) -> Vec<ProbeQuestion> {
    let sentinel = DIFF_PROBE_ABSENT.to_lowercase();
    ground_truth
        .iter()
        .filter_map(|gt| match gt {
            GroundTruth::Substring(s) => Some(s.clone()),
            // Regex truths have no single literal answer to check against.
            GroundTruth::Pattern { .. } => None,
        })
        .filter(|fact| !sentinel.contains(&fact.to_lowercase()))
        .filter(|fact| !fact.chars().any(char::is_whitespace))
        .map(|ident| ProbeQuestion {
            question: format!(
                "Search the text below for the exact token '{ident}'. Reply with only \
                 one word and nothing else: reply '{ident}' if that token occurs \
                 in the text, or '{DIFF_PROBE_ABSENT}' if it does not."
            ),
            expected_contains: ident,
        })
        .collect()
}

/// Probe file stem for a category (matches `assets/probes/*.json` names).
///
/// `None` for `diff`, which builds its questions per sample from the content
/// extracted that run ([`diff_probe_questions`]) instead of loading a fixed
/// set. Returning `None` rather than a stem keeps the asset directory honest:
/// a stem here is a claim that the file exists and is used.
pub fn probe_file_stem(category: Category) -> Option<&'static str> {
    match category {
        Category::Json => Some("json_api"),
        Category::Command => Some("command_output"),
        Category::Grep => Some("grep_search"),
        Category::Code => Some("source_code"),
        Category::Diff => None,
    }
}

/// Extracts ground truth from live command output.
///
/// Patterns per category (first `MAX_DYNAMIC_ITEMS` hits, deduplicated):
/// * `command` — commit hashes at line start (`git log --oneline`,
///   `git show`), so retention asserts the hashes the agent would act on;
/// * `grep` — `file:line` prefixes from `rg -n` output;
/// * `diff` — **content-level facts from changed lines** (see
///   `extract_diff_ground_truth`): identifiers and code fragments that a
///   `+`/`-` line actually introduces or removes, not the `diff --git` file
///   headers. Header-only extraction rewarded a compressor for keeping the
///   diff's frame while dropping its content; asserting on changed-line
///   identifiers ties retention to what the change *says*.
///
/// Static categories return an empty vector: their truth ships in the asset.
///
/// # Errors
///
/// Returns [`L2Error::Regex`] only if a built-in pattern is invalid, which
/// the `l2_retention` test guards against.
pub fn extract_dynamic_ground_truth(
    category: Category,
    raw_output: &str,
) -> Result<Vec<GroundTruth>, L2Error> {
    let pattern = match category {
        Category::Command => r"(?m)^(?:commit )?([0-9a-f]{7,40})\b",
        Category::Grep => r"(?m)^([^:\s][^:\n]*:\d+)[:-]",
        Category::Diff => return extract_diff_ground_truth(raw_output),
        Category::Json | Category::Code => return Ok(Vec::new()),
    };
    let re = Regex::new(pattern)?;
    let mut seen = std::collections::HashSet::new();
    let mut items = Vec::new();
    for cap in re.captures_iter(raw_output) {
        if let Some(m) = cap.get(1) {
            let s = m.as_str().to_string();
            if seen.insert(s.clone()) {
                items.push(GroundTruth::Substring(s));
                if items.len() >= MAX_DYNAMIC_ITEMS {
                    break;
                }
            }
        }
    }
    Ok(items)
}

/// Whether a token is ordinary English prose rather than a code fact.
///
/// Only used by the last-resort branch of [`extract_diff_ground_truth`]. A
/// documentation diff otherwise yields its longest English word as the
/// "content fact", and since compressors rarely drop mid-sentence words, that
/// would score as retained while the changed text was thrown away. The list is
/// deliberately short: it covers connectives and hedges that show up in prose
/// and comments, not a general dictionary, because over-filtering would discard
/// genuine identifiers that happen to be English words.
fn is_prose_word(token: &str) -> bool {
    const PROSE: &[&str] = &[
        "about",
        "after",
        "again",
        "against",
        "already",
        "also",
        "although",
        "always",
        "another",
        "available",
        "because",
        "been",
        "before",
        "being",
        "below",
        "between",
        "both",
        "cannot",
        "could",
        "does",
        "during",
        "each",
        "either",
        "enough",
        "every",
        "from",
        "have",
        "here",
        "however",
        "instead",
        "into",
        "itself",
        "just",
        "like",
        "made",
        "make",
        "many",
        "might",
        "more",
        "most",
        "much",
        "must",
        "never",
        "none",
        "only",
        "other",
        "over",
        "rather",
        "same",
        "should",
        "since",
        "some",
        "such",
        "than",
        "that",
        "their",
        "them",
        "then",
        "there",
        "these",
        "they",
        "this",
        "those",
        "through",
        "under",
        "until",
        "very",
        "were",
        "what",
        "when",
        "where",
        "which",
        "while",
        "will",
        "with",
        "would",
        "your",
    ];
    // Keywords and builtins across the languages these fixtures touch (Rust,
    // Python, Go, TypeScript). These tokens appear in almost any code output, so
    // they carry no discriminating power as a fact: asserting on one lets a
    // compressor pass while the changed line was discarded. A few (`print`,
    // `range`, `module`) are builtins or contextual keywords rather than strictly
    // reserved, so filtering them can in principle drop a user-chosen name of the
    // same spelling; that line then yields no fact and lands in
    // `unprobed_samples`, which the report marks, rather than producing a fact
    // that cannot fail.
    // Without this, a line like `-    return;` yields `return`, and retention
    // passes even though the whole line was dropped.
    const KEYWORDS: &[&str] = &[
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "crate",
        "default",
        "defer",
        "elif",
        "else",
        "enum",
        "except",
        "export",
        "false",
        "final",
        "finally",
        "func",
        "function",
        "impl",
        "import",
        "interface",
        "lambda",
        "loop",
        "match",
        "module",
        "null",
        "pass",
        "print",
        "raise",
        "range",
        "return",
        "self",
        "static",
        "struct",
        "super",
        "switch",
        "throw",
        "trait",
        "true",
        "type",
        "typeof",
        "unsafe",
        "using",
        "void",
        "yield",
    ];
    // Case-insensitive: prose capitalises at sentence start, and an identifier
    // that differs from a stop word only by case is not a distinctive fact.
    let lower = token.to_ascii_lowercase();
    PROSE.contains(&lower.as_str()) || KEYWORDS.contains(&lower.as_str())
}

/// Content-level ground truth for a unified diff.
///
/// Reads only the changed lines — those starting with a single `+` or `-`,
/// excluding the `+++`/`---` file headers — and extracts what the change
/// introduces or removes, in descending order of how specific the fact is:
///
/// 1. a definition name (`fn`/`def`/`func`/`struct`/`enum`/`const`/`class`);
/// 2. a quoted string literal, which is where error codes and messages live;
/// 3. a path-like token, so a moved or renamed file registers as content;
/// 4. otherwise the longest identifier-like token that is not a common English
///    word.
///
/// The stop-word filter in step 4 matters on documentation and configuration
/// diffs: without it the "longest token" is a word like `because` or `available`,
/// which almost any compressor keeps, so retention would pass while the actual
/// change was discarded.
///
/// These are facts about *what changed*, so a compressor that keeps the
/// `diff --git` frame but discards the body no longer scores as lossless.
///
/// # Errors
///
/// Returns [`L2Error::Regex`] if a built-in pattern is invalid.
fn extract_diff_ground_truth(raw_output: &str) -> Result<Vec<GroundTruth>, L2Error> {
    // Definition-introducing keywords across the languages these fixtures
    // touch (Rust/Python/Go/TS). The captured name is the identifier.
    let def_re = Regex::new(
        r"\b(?:fn|def|func|struct|enum|const|class|type|impl)\s+([A-Za-z_][A-Za-z0-9_]*)",
    )?;
    // Quoted literals: error codes, messages and versions travel this way, and
    // they are the content a reader of the diff would quote back.
    let quoted_re = Regex::new(r#"["']([^"'\n]{3,})["']"#)?;
    // Path-like tokens. The regex alone also matches pure slash/punctuation
    // runs (`///` from doc comments), so the use site below skips matches
    // without an alphanumeric character instead of taking the leftmost one.
    let path_re = Regex::new(r"[A-Za-z0-9_.-]*/[A-Za-z0-9_./-]{2,}")?;
    // Fallback: the longest identifier-like token on a changed line, so a line
    // that changes an expression rather than a definition still yields a fact.
    let ident_re = Regex::new(r"[A-Za-z_][A-Za-z0-9_]{3,}")?;

    let mut seen = std::collections::HashSet::new();
    let mut items = Vec::new();
    for line in raw_output.lines() {
        // Changed lines only, excluding file headers (+++/---) and hunk
        // headers (@@), which are frame rather than content.
        let is_change = (line.starts_with('+') && !line.starts_with("+++"))
            || (line.starts_with('-') && !line.starts_with("---"));
        if !is_change {
            continue;
        }
        let body = &line[1..];

        let fact = def_re
            .captures(body)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .or_else(|| {
                quoted_re
                    .captures(body)
                    .and_then(|c| c.get(1))
                    .map(|m| m.as_str().to_string())
            })
            .or_else(|| {
                path_re
                    .find_iter(body)
                    // A match without any alphanumeric is a separator or
                    // doc-comment run, not a path: it would occupy a fact
                    // slot while passing retention almost anywhere. Scanning
                    // rather than filtering the leftmost match keeps a real
                    // path that follows one — `/// see docs/notes.md` yields
                    // the path, not the identifier fallback.
                    .find(|m| m.as_str().chars().any(char::is_alphanumeric))
                    .map(|m| m.as_str().to_string())
            })
            .or_else(|| {
                ident_re
                    .find_iter(body)
                    .map(|m| m.as_str())
                    .filter(|s| !is_prose_word(s))
                    .max_by_key(|s| s.len())
                    .map(str::to_string)
            });

        if let Some(fact) = fact
            && seen.insert(fact.clone())
        {
            items.push(GroundTruth::Substring(fact));
            if items.len() >= MAX_DYNAMIC_ITEMS {
                break;
            }
        }
    }
    Ok(items)
}
