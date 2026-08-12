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

//! Asset sanity checks: every shipped sample, command spec, and probe file
//! must parse with all required fields — a malformed asset should fail here,
//! not mid-run on the remote host.

use std::path::PathBuf;
use tokenless_l2_bench::l2::Category;
use tokenless_l2_bench::l2::samples::{
    load_code_samples, load_command_specs, load_json_samples, load_probe_questions, probe_file_stem,
};

fn l2_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets")
}

#[test]
fn json_samples_load_with_content_and_ground_truth() {
    let samples = load_json_samples(&l2_dir()).expect("load json samples");
    assert_eq!(samples.len(), 3);
    for s in &samples {
        assert_eq!(s.category, Category::Json);
        assert!(!s.id.is_empty());
        assert!(!s.content.is_empty(), "sample {} has empty content", s.id);
        assert!(
            !s.ground_truth.is_empty(),
            "sample {} has no ground truth",
            s.id
        );
        // json samples must carry valid wire-form JSON — the tokenless side
        // parses them before compressing.
        serde_json::from_str::<serde_json::Value>(&s.content)
            .unwrap_or_else(|e| panic!("sample {} is not valid JSON: {e}", s.id));
    }
    // The canonical fixture arrives via content_path and must resolve.
    assert!(samples.iter().any(|s| s.id == "tool_response_main"));
}

#[test]
fn code_samples_load_with_content_and_ground_truth() {
    let samples = load_code_samples(&l2_dir()).expect("load code samples");
    assert_eq!(samples.len(), 9);
    for s in &samples {
        assert_eq!(s.category, Category::Code);
        assert!(!s.content.is_empty(), "sample {} has empty content", s.id);
        assert!(
            !s.ground_truth.is_empty(),
            "sample {} has no ground truth",
            s.id
        );
    }
}

#[test]
fn static_samples_retain_their_own_ground_truth() {
    // Every ground-truth item must hold against the uncompressed content,
    // otherwise the retention metric starts from a broken baseline.
    let mut all = load_json_samples(&l2_dir()).expect("json");
    all.extend(load_code_samples(&l2_dir()).expect("code"));
    for s in &all {
        let result =
            tokenless_l2_bench::l2::retention::check(&s.ground_truth, &s.content).expect("check");
        assert_eq!(
            result.passed, result.total,
            "sample {} loses items pre-compression: {:?}",
            s.id, result.failures
        );
    }
}

#[test]
fn retention_text_falls_back_to_the_wire_form_without_a_content_key() {
    // The wrapped-text branch reads the inner `content` string. If the compressor
    // ever rewrites the envelope so that key is gone, the fallback must hand back
    // the wire form rather than an empty string: an empty haystack would score
    // every ground-truth item as lost and report a harness failure as a product
    // defect. Exercised through the real compress path on inputs that stress the
    // envelope (empty text, and text that is itself JSON).
    use tokenless_l2_bench::l2::{Category, tokenless_side};
    for content in ["", "{\"content\": null}", "plain text"] {
        let out = tokenless_side::compress(Category::Code, content).expect("compress");
        assert!(
            !out.retention_text.is_empty() || content.is_empty(),
            "retention_text empty for {content:?}: every item would score as lost"
        );
        // The wire form always stays valid JSON, whichever branch was taken.
        serde_json::from_str::<serde_json::Value>(&out.compressed)
            .unwrap_or_else(|e| panic!("wire form is not JSON for {content:?}: {e}"));
    }
}

#[test]
fn retention_text_is_unescaped_for_wrapped_text() {
    // Guards the escaping fix: a code ground-truth item containing a quote must
    // match against retention_text. The wrapped envelope serializes content as
    // a JSON string, so checking the wire form saw SEC_CORE_RUST_TOOLCHAIN=\"..\"
    // and scored fully present content as a miss. retention_text hands back the
    // inner string un-escaped, independent of how much the body compressed.
    use tokenless_l2_bench::l2::{Category, GroundTruth, retention, tokenless_side};
    let content = "fn main() {\n    let v = \"quoted value\";\n    // marker LINE_END\n}";
    let out = tokenless_side::compress(Category::Code, content).expect("compress");
    let gt = vec![
        GroundTruth::Substring("let v = \"quoted value\"".to_string()),
        GroundTruth::Substring("LINE_END".to_string()),
    ];
    let result = retention::check(&gt, &out.retention_text).expect("check");
    assert_eq!(
        result.passed, result.total,
        "quote-bearing content wrongly scored as lost: {:?}",
        result.failures
    );
    // And the wire form still carries the escaped form, so token counts are
    // unaffected by the retention-text change.
    assert!(out.compressed.contains("\\\"quoted value\\\""));
}

#[test]
fn command_specs_are_well_formed() {
    let specs = load_command_specs(&l2_dir()).expect("load command specs");
    // 2 command + 2 grep + 5 diff (two live git ranges, plus one committed
    // fixture measured through three rtk entry points).
    assert_eq!(specs.len(), 9);
    for spec in &specs {
        assert!(!spec.id.is_empty());
        assert!(!spec.argv.is_empty(), "spec {} has empty argv", spec.id);
        // A spec may set rtk_argv to a different entry point, but never to an
        // empty list: rtk with no subcommand prints usage and exits 0, so the
        // run would measure the help text as if it were compressed output.
        assert!(
            !spec.rtk_invocation().is_empty(),
            "spec {} resolves to an empty rtk invocation",
            spec.id
        );
        assert_eq!(spec.ground_truth_source, "dynamic", "spec {}", spec.id);
        assert!(!spec.cwd_rel.is_empty(), "spec {}", spec.id);
        let category = spec.parsed_category().expect("category parses");
        assert!(
            category.is_dynamic(),
            "spec {} must be a dynamic category",
            spec.id
        );
    }
    // Ids must be unique — task simulations reference them by name.
    let mut ids: Vec<&str> = specs.iter().map(|s| s.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), specs.len(), "duplicate spec ids");
}

#[test]
fn every_category_has_a_probe_file_with_enough_questions() {
    for category in Category::ALL {
        // diff has no probe asset: its questions come from the content extracted
        // per run. Asserting a file for it would keep a dead asset alive.
        let Some(stem) = probe_file_stem(category) else {
            continue;
        };
        let questions = load_probe_questions(&l2_dir(), stem)
            .unwrap_or_else(|e| panic!("probes/{stem}.json failed to load: {e}"));
        assert!(
            questions.len() >= 8,
            "probes/{stem}.json has only {} questions, need >= 8",
            questions.len()
        );
        for (i, q) in questions.iter().enumerate() {
            assert!(!q.question.trim().is_empty(), "{stem}[{i}] question empty");
            assert!(
                !q.expected_contains.trim().is_empty(),
                "{stem}[{i}] expected_contains empty"
            );
        }
    }
}

/// `JsonCompressor` drops noise fields (`logs`, `debug`, `trace`, ...)
/// by design, so JSON ground truth must never reference their contents —
/// otherwise retention would penalise the compressor for doing its job
/// (the miscalibration behind an early 40/60-vs-60/60 smoke reading).
#[test]
fn json_ground_truth_never_references_droppable_noise() {
    use tokenless_l2_bench::l2::GroundTruth;

    let noise_markers = ["log entry", "logs", "debug", "trace", "stacktrace"];
    let samples = load_json_samples(&l2_dir()).expect("load json samples");
    for sample in &samples {
        for item in &sample.ground_truth {
            let needle = match item {
                GroundTruth::Substring(s) => s.as_str(),
                GroundTruth::Pattern { regex } => regex.as_str(),
            };
            let lower = needle.to_lowercase();
            for marker in noise_markers {
                assert!(
                    !lower.contains(marker),
                    "{}: ground truth {needle:?} references droppable noise ({marker})",
                    sample.id
                );
            }
        }
    }
}

/// The path-like pattern also matches a pure slash run (`///`, a Rust doc
/// comment), which is syntax, not content. Every extracted diff fact must
/// carry at least one alphanumeric character.
#[test]
fn diff_ground_truth_never_yields_pure_punctuation_facts() {
    use tokenless_l2_bench::l2::GroundTruth;
    use tokenless_l2_bench::l2::samples::extract_dynamic_ground_truth;

    let diff = concat!(
        "--- a/src/lib.rs\n",
        "+++ b/src/lib.rs\n",
        "@@ -1,2 +1,2 @@\n",
        "-/// Auto-fix missing dependencies via env-fix.sh.\n",
        "+/// Ordered candidate list for the fix script lookup.\n",
    );
    let facts = extract_dynamic_ground_truth(Category::Diff, diff).expect("extract diff facts");
    assert!(!facts.is_empty(), "doc-comment diff produced no facts");
    for item in &facts {
        let GroundTruth::Substring(fact) = item else {
            continue;
        };
        assert!(
            fact.chars().any(char::is_alphanumeric),
            "fact {fact:?} is pure punctuation"
        );
    }
}

/// A doc-comment marker is a path-like match with no alphanumeric, and it sits
/// to the left of any real path on the same line. Rejecting only the leftmost
/// match would drop the line to the identifier fallback, which yields the bare
/// stem instead of the path the change is about.
#[test]
fn diff_ground_truth_prefers_a_real_path_over_a_leading_doc_comment() {
    use tokenless_l2_bench::l2::GroundTruth;
    use tokenless_l2_bench::l2::samples::extract_dynamic_ground_truth;

    let diff = concat!(
        "--- a/src/lib.rs\n",
        "+++ b/src/lib.rs\n",
        "@@ -1,1 +1,1 @@\n",
        "+/// see docs/design_notes.md for details\n",
    );
    let facts = extract_dynamic_ground_truth(Category::Diff, diff).expect("extract diff facts");
    let extracted: Vec<&str> = facts
        .iter()
        .filter_map(|item| match item {
            GroundTruth::Substring(fact) => Some(fact.as_str()),
            GroundTruth::Pattern { .. } => None,
        })
        .collect();
    assert!(
        extracted.contains(&"docs/design_notes.md"),
        "expected the path, got {extracted:?}"
    );
}

/// The committed `static_code_diff.diff` fixture backs the three
/// `static_code_diff*` samples. If extraction ever regresses to an empty
/// result on it, those samples would silently become invalid (empty ground
/// truth, retention always 1.0) with no warning in the report.
#[test]
fn static_code_diff_fixture_yields_probeable_facts() {
    use tokenless_l2_bench::l2::GroundTruth;
    use tokenless_l2_bench::l2::samples::{diff_probe_questions, extract_dynamic_ground_truth};

    let diff = std::fs::read_to_string(l2_dir().join("fixtures/static_code_diff.diff"))
        .expect("read fixtures/static_code_diff.diff");
    let facts = extract_dynamic_ground_truth(Category::Diff, &diff).expect("extract diff facts");
    assert!(!facts.is_empty(), "fixture produced no ground truth");
    assert!(
        !diff_probe_questions(&facts).is_empty(),
        "fixture facts produced no probe questions"
    );
    for item in &facts {
        let GroundTruth::Substring(fact) = item else {
            continue;
        };
        assert!(
            fact.chars().any(char::is_alphanumeric),
            "fact {fact:?} is pure punctuation"
        );
    }
}
