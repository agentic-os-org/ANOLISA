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

//! Unit checks for ground-truth retention: substring/regex item matching and
//! the dynamic extraction patterns for live command output.

use tokenless_l2_bench::l2::retention::check;
use tokenless_l2_bench::l2::samples::extract_dynamic_ground_truth;
use tokenless_l2_bench::l2::{Category, GroundTruth, L2Error};

#[test]
fn substring_items_are_checked_individually() {
    let truth = vec![
        GroundTruth::Substring("req-8f3a91".to_string()),
        GroundTruth::Substring("E_TIMEOUT".to_string()),
        GroundTruth::Substring("not-in-there".to_string()),
    ];
    let result = check(&truth, "error E_TIMEOUT for request req-8f3a91").expect("check");
    assert_eq!(result.passed, 2);
    assert_eq!(result.total, 3);
    assert_eq!(result.failures.len(), 1);
    assert!(result.failures[0].contains("not-in-there"));
    assert!((result.rate() - 2.0 / 3.0).abs() < 1e-9);
}

#[test]
fn substring_matching_is_case_sensitive() {
    // Ids and error codes are case-significant facts; a case-folded match
    // would hide a real information loss.
    let truth = vec![GroundTruth::Substring("E_TIMEOUT".to_string())];
    let result = check(&truth, "error e_timeout occurred").expect("check");
    assert_eq!(result.passed, 0);
}

#[test]
fn regex_items_match_and_miss() {
    let truth = vec![
        GroundTruth::Pattern {
            regex: r"req-[0-9a-f]{6}".to_string(),
        },
        GroundTruth::Pattern {
            regex: r"^HTTP/2 500$".to_string(),
        },
    ];
    let result = check(&truth, "trace req-8f3a91 finished").expect("check");
    assert_eq!(result.passed, 1);
    assert_eq!(result.total, 2);
}

#[test]
fn empty_ground_truth_scores_full_retention() {
    let result = check(&[], "anything").expect("check");
    assert_eq!(result.total, 0);
    assert_eq!(result.rate(), 1.0);
}

#[test]
fn invalid_regex_surfaces_as_error_not_a_miss() {
    let truth = vec![GroundTruth::Pattern {
        regex: "[unclosed".to_string(),
    }];
    let err = check(&truth, "text").expect_err("bad pattern must error");
    assert!(matches!(err, L2Error::Regex(_)), "got {err:?}");
}

#[test]
fn dynamic_extraction_command_pulls_commit_hashes() {
    let raw = "abc1234 fix parser edge case\n\
               deadbee5 add l2 harness\n\
               abc1234 duplicated line\n";
    let items = extract_dynamic_ground_truth(Category::Command, raw).expect("extract");
    // Duplicates collapse: retention should not double-count one hash.
    assert_eq!(items.len(), 2);
    assert!(matches!(&items[0], GroundTruth::Substring(s) if s == "abc1234"));
    assert!(matches!(&items[1], GroundTruth::Substring(s) if s == "deadbee5"));
}

#[test]
fn dynamic_extraction_command_accepts_git_show_headers() {
    let raw = "commit 0123456789abcdef0123456789abcdef01234567\nAuthor: someone\n";
    let items = extract_dynamic_ground_truth(Category::Command, raw).expect("extract");
    assert_eq!(items.len(), 1);
    assert!(matches!(
        &items[0],
        GroundTruth::Substring(s) if s == "0123456789abcdef0123456789abcdef01234567"
    ));
}

#[test]
fn dynamic_extraction_grep_pulls_file_line_prefixes() {
    let raw = "src/bin/l2_compare.rs:41:fn main() -> Result<()> {\n\
               src/lib.rs:20:pub fn main_entry() {\n";
    let items = extract_dynamic_ground_truth(Category::Grep, raw).expect("extract");
    assert_eq!(items.len(), 2);
    assert!(matches!(
        &items[0],
        GroundTruth::Substring(s) if s == "src/bin/l2_compare.rs:41"
    ));
}

#[test]
fn dynamic_extraction_diff_pulls_changed_line_content_not_paths() {
    // diff ground truth must reflect *what changed*, not the
    // diff frame. A definition on a changed line yields its name; the frame
    // lines (diff --git / index / +++ / --- / @@) yield nothing.
    let raw = "diff --git a/src/l2/samples.rs b/src/l2/samples.rs\n\
               index 111..222 100644\n\
               --- a/src/l2/samples.rs\n\
               +++ b/src/l2/samples.rs\n\
               @@ -1,3 +1,3 @@\n\
               -fn extract_old_way(input: &str) {\n\
               +fn extract_diff_ground_truth(input: &str) {\n\
               +const MAX_DYNAMIC_ITEMS: usize = 5;\n";
    let items = extract_dynamic_ground_truth(Category::Diff, raw).expect("extract");
    let facts: Vec<&str> = items
        .iter()
        .map(|gt| match gt {
            GroundTruth::Substring(s) => s.as_str(),
            GroundTruth::Pattern { regex } => regex.as_str(),
        })
        .collect();
    // The changed definitions and constant are captured.
    assert!(facts.contains(&"extract_old_way"), "got {facts:?}");
    assert!(
        facts.contains(&"extract_diff_ground_truth"),
        "got {facts:?}"
    );
    assert!(facts.contains(&"MAX_DYNAMIC_ITEMS"), "got {facts:?}");
    // The file path (frame) is NOT a ground-truth fact anymore.
    assert!(
        !facts.iter().any(|f| f.contains("samples.rs")),
        "file path leaked into ground truth: {facts:?}"
    );
}

#[test]
fn diff_qa_rejects_frame_only_output() {
    // A compressor that keeps the diff frame but drops the changed lines must
    // now score a retention miss, which the header-only design could not catch.
    let raw = "diff --git a/src/l2/samples.rs b/src/l2/samples.rs\n\
               @@ -1,2 +1,2 @@\n\
               -fn removed_helper() {}\n\
               +fn added_helper() {}\n";
    let items = extract_dynamic_ground_truth(Category::Diff, raw).expect("extract");
    // Frame only: headers kept, changed lines gone.
    let frame_only = "diff --git a/src/l2/samples.rs b/src/l2/samples.rs\n@@ -1,2 +1,2 @@\n";
    let score = check(&items, frame_only).expect("check");
    assert!(
        score.passed < score.total,
        "frame-only output should lose content facts: {score:?}"
    );
    assert!(!score.failures.is_empty());
}

#[test]
fn diff_extraction_prefers_quoted_literals_and_paths_over_prose() {
    // Review follow-up: the longest-token fallback picked ordinary English words
    // on documentation and configuration diffs, and compressors keep mid-sentence
    // words, so retention passed while the changed text was discarded. Quoted
    // literals and paths come first now, and prose words are filtered out.
    let raw = "diff --git a/config b/config\n\
               @@ -1,4 +1,4 @@\n\
               +SEC_CORE_RUST_TOOLCHAIN=\"1.93.0\"\n\
               +  spec: src/l2/samples.rs\n\
               +# the policy lives in one reviewable place\n";
    let items = extract_dynamic_ground_truth(Category::Diff, raw).expect("extract");
    let facts: Vec<&str> = items
        .iter()
        .map(|gt| match gt {
            GroundTruth::Substring(s) => s.as_str(),
            GroundTruth::Pattern { regex } => regex.as_str(),
        })
        .collect();
    // A quoted value is taken over the surrounding identifier.
    assert!(facts.contains(&"1.93.0"), "got {facts:?}");
    // A path is taken whole rather than as its longest word fragment.
    assert!(facts.contains(&"src/l2/samples.rs"), "got {facts:?}");
}

#[test]
fn prose_only_and_keyword_only_lines_yield_no_fact() {
    // Review follow-up: the previous assertions here were vacuous — they named
    // words that either fall below the 4-character token floor or were never in
    // the filter list, so they passed whether or not is_prose_word worked. These
    // lines contain nothing BUT filtered tokens, so a fact appearing at all
    // proves the filter is off.
    let raw = "diff --git a/x b/x\n\
               @@ -1,2 +1,2 @@\n\
               +# because available\n\
               -    return;\n";
    let items = extract_dynamic_ground_truth(Category::Diff, raw).expect("extract");
    assert!(
        items.is_empty(),
        "prose-only and keyword-only lines must not yield facts, got {items:?}"
    );
}

#[test]
fn diff_extraction_yields_at_most_one_fact_per_changed_line() {
    // Review follow-up: a line with several identifiers must not produce several
    // probe questions for one change. Extraction takes one fact per changed
    // line, so the count can never exceed the number of changed lines.
    let raw = "diff --git a/x b/x\n\
               @@ -1,2 +1,2 @@\n\
               +let alpha_one = beta_two + gamma_three + delta_four;\n\
               -let epsilon_five = zeta_six;\n";
    let items = extract_dynamic_ground_truth(Category::Diff, raw).expect("extract");
    assert!(
        items.len() <= 2,
        "expected at most one fact per changed line, got {items:?}"
    );
}

#[test]
fn probe_skips_facts_the_sentinel_contains_or_that_hold_whitespace() {
    // Review follow-up: `def_re` has no minimum name length, so `+fn o() {}`
    // yields the one-character fact `o` — which even the two-character sentinel
    // contains, reviving the false positive that the sentinel change fixed. And
    // `quoted_re` can yield a multi-word literal, which contradicts the question's
    // "reply with only one word" instruction. Both must be dropped at question
    // construction, not left to a length coincidence.
    use tokenless_l2_bench::l2::samples::{DIFF_PROBE_ABSENT, diff_probe_questions};

    let gt = vec![
        GroundTruth::Substring("o".to_string()), // sentinel substring
        GroundTruth::Substring("NO".to_string()), // sentinel, other case
        GroundTruth::Substring("file not found".to_string()), // multi-word
        GroundTruth::Substring("extract_diff".to_string()), // keeper
    ];
    let qs = diff_probe_questions(&gt);
    let asked: Vec<&str> = qs.iter().map(|q| q.expected_contains.as_str()).collect();
    assert_eq!(asked, vec!["extract_diff"], "got {asked:?}");

    // The surviving question's answer can never be matched by the negative reply.
    let sentinel = DIFF_PROBE_ABSENT.to_lowercase();
    for q in &qs {
        assert!(
            !sentinel.contains(&q.expected_contains.to_lowercase()),
            "sentinel {sentinel:?} contains {:?}",
            q.expected_contains
        );
        assert!(
            !q.expected_contains.chars().any(char::is_whitespace),
            "one-word question asks for multi-word answer {:?}",
            q.expected_contains
        );
    }
}

#[test]
fn negative_sentinel_cannot_contain_a_fact() {
    // Review follow-up: scoring is substring-based, so if the negative reply
    // contains the fact it denies, a lost token is scored as retained. That is
    // what "absent" did — it contains "sent", which a line like
    // `+status = "sent"` yields as a fact. Assert the invariant directly instead
    // of trusting that the sentinel happens to be short.
    use tokenless_l2_bench::l2::samples::DIFF_PROBE_ABSENT;

    // Facts that the extractor can really produce, including the collision case.
    let raw = "diff --git a/x b/x\n\
               @@ -1,3 +1,3 @@\n\
               +status = \"sent\"\n\
               +fn absentee_check() {}\n\
               +  ent = 1\n";
    let items = extract_dynamic_ground_truth(Category::Diff, raw).expect("extract");
    let facts: Vec<&str> = items
        .iter()
        .filter_map(|gt| match gt {
            GroundTruth::Substring(s) => Some(s.as_str()),
            GroundTruth::Pattern { .. } => None,
        })
        .collect();
    assert!(
        facts.contains(&"sent"),
        "expected the collision fact: {facts:?}"
    );

    let sentinel = DIFF_PROBE_ABSENT.to_lowercase();
    for fact in &facts {
        assert!(
            !sentinel.contains(&fact.to_lowercase()),
            "negative reply {sentinel:?} contains fact {fact:?}: a dropped token \
             would score as retained"
        );
    }
}

#[test]
fn diff_probe_questions_built_from_content_facts() {
    use tokenless_l2_bench::l2::samples::diff_probe_questions;
    let gt = vec![
        GroundTruth::Substring("extract_diff_ground_truth".to_string()),
        GroundTruth::Substring("MAX_DYNAMIC_ITEMS".to_string()),
    ];
    let qs = diff_probe_questions(&gt);
    assert_eq!(qs.len(), 2);
    // Each question checks a content identifier, not a diff-format property.
    assert_eq!(qs[0].expected_contains, "extract_diff_ground_truth");
    assert!(qs[0].question.contains("extract_diff_ground_truth"));
    assert!(
        !qs.iter().any(|q| q.question.contains("diff --git")
            || q.question.contains("@@")
            || q.question.to_lowercase().contains("unified diff")),
        "probe question still asks about format"
    );
}

#[test]
fn diff_probe_questions_empty_when_no_content() {
    use tokenless_l2_bench::l2::samples::diff_probe_questions;
    assert!(diff_probe_questions(&[]).is_empty());
}

#[test]
fn dynamic_extraction_caps_item_count() {
    // 8 distinct hashes in, at most 5 items out — keeps the retention
    // denominator comparable across differently sized outputs.
    let raw: String = (0..8)
        .map(|i| format!("{i}{i}{i}abcd subject {i}\n"))
        .collect();
    let items = extract_dynamic_ground_truth(Category::Command, &raw).expect("extract");
    assert_eq!(items.len(), 5);
}

#[test]
fn dynamic_extraction_is_empty_for_static_categories() {
    for category in [Category::Json, Category::Code] {
        let items = extract_dynamic_ground_truth(category, "abc1234 anything").expect("extract");
        assert!(items.is_empty(), "{category} must ship truth in its asset");
    }
}

#[test]
fn extracted_items_round_trip_through_retention_check() {
    let raw = "abc1234 fix parser\ndeadbee5 add harness\n";
    let items = extract_dynamic_ground_truth(Category::Command, raw).expect("extract");
    // The raw output trivially retains its own extracted facts.
    let full = check(&items, raw).expect("check");
    assert_eq!(full.passed, full.total);
    // A compressed view that drops one hash is caught.
    let partial = check(&items, "abc1234 fix parser").expect("check");
    assert_eq!(partial.passed, 1);
    assert_eq!(partial.total, 2);
}
