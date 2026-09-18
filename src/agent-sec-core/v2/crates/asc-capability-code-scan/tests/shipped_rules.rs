//! Every shipped rule must load and compile under the engine that will run it.
//!
//! The V1 patterns were authored against Python's `re`, which accepts syntax the
//! Rust engines do not. Loading the YAML only proves it parses, so each pattern
//! is compiled here: a rule the engine rejects cannot reach a release quietly.

use asc_capability_code_scan::{Language, load_rules};
use fancy_regex::Regex;

/// Rule ids the loader returns for `language`.
fn loaded_ids(language: Language) -> Vec<String> {
    load_rules(language)
        .expect("rule set loads")
        .into_iter()
        .map(|rule| rule.rule_id)
        .collect()
}

/// Rule file stems present on disk for `language`, excluding shared data files.
fn stems_on_disk(language: Language) -> Vec<String> {
    let directory = format!("{}/rules/{}", env!("CARGO_MANIFEST_DIR"), language.as_str());
    let mut stems: Vec<String> = std::fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("cannot read {directory}: {error}"))
        .map(|entry| entry.expect("directory entry is readable").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter_map(|name| name.strip_suffix(".yaml").map(str::to_owned))
        .filter(|stem| !stem.starts_with('_'))
        .collect();
    stems.sort();
    stems
}

/// Compiles `pattern`, attributing any failure to the rule that carried it.
fn assert_compiles(rule_id: &str, label: &str, pattern: &str) {
    assert!(
        Regex::new(pattern).is_ok(),
        "{rule_id}: {label} rejected by fancy-regex: {pattern}"
    );
}

#[test]
fn every_shipped_rule_compiles() {
    for language in [Language::Bash, Language::Python] {
        let rules = load_rules(language).unwrap_or_else(|error| {
            panic!("{}: rule set failed to load: {error}", language.as_str())
        });
        assert!(
            !rules.is_empty(),
            "{}: rule set is empty",
            language.as_str()
        );
        for rule in &rules {
            assert_compiles(&rule.rule_id, "regex", &rule.regex);
            for (index, target) in rule.target_regexes.iter().flatten().enumerate() {
                assert_compiles(&rule.rule_id, &format!("target_regexes[{index}]"), target);
            }
        }
    }
}

#[test]
fn every_rule_file_on_disk_is_embedded() {
    // The `include_str!` tables are hand-maintained, so a new YAML file is only
    // one edit away from shipping as dead weight: present in the tree, absent
    // from the binary, and undetectable at runtime. Comparing against the
    // directory is what makes that omission fail the build instead.
    for language in [Language::Bash, Language::Python] {
        assert_eq!(
            loaded_ids(language),
            stems_on_disk(language),
            "{}: embedded rule table and rules/ directory disagree",
            language.as_str()
        );
    }
}

#[test]
fn rule_order_follows_file_names() {
    // Finding order, and therefore the rule-id list inside the result summary,
    // is this order. Sorting is explicit rather than left to a directory walk.
    for language in [Language::Bash, Language::Python] {
        let rules = load_rules(language).expect("rule set loads");
        let ids: Vec<&str> = rules.iter().map(|rule| rule.rule_id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "{}: rules are out of order", language.as_str());
    }
}

#[test]
fn shared_references_resolve_to_non_empty_lists() {
    // A ref that resolved to an empty list would silently disable the
    // segment-level path for that rule, which no shipped rule intends.
    for language in [Language::Bash, Language::Python] {
        for rule in load_rules(language).expect("rule set loads") {
            if let Some(targets) = &rule.target_regexes {
                assert!(
                    !targets.is_empty(),
                    "{}: target_regexes resolved empty",
                    rule.rule_id
                );
            }
        }
    }
}

#[test]
fn embedded_regexes_carry_no_authoring_newlines() {
    // Rules are authored as block scalars; the loader joins them back into one
    // pattern. A surviving newline would change what the pattern matches.
    for language in [Language::Bash, Language::Python] {
        for rule in load_rules(language).expect("rule set loads") {
            assert!(
                !rule.regex.contains('\n'),
                "{}: regex still contains a newline",
                rule.rule_id
            );
        }
    }
}

#[test]
fn unsupported_language_is_rejected_by_name() {
    // Language parsing is exact: V1 does not fold case.
    assert!(Language::parse("bash").is_ok());
    assert!(Language::parse("python").is_ok());
    for rejected in ["Bash", "PYTHON", "ruby", ""] {
        assert!(
            Language::parse(rejected).is_err(),
            "{rejected:?} was accepted as a language"
        );
    }
}

/// Expectations for `shell-disk-wipe`, produced by running the V1 pattern under
/// Python `re` over these exact inputs.
///
/// The V2 copy of that rule writes the closing-quote check as a numeric
/// conditional because fancy-regex misreads the named form V1 uses. These cases
/// are what makes the two forms provably equivalent rather than assumed so, and
/// they are the reason quoted device paths stay detected.
const DISK_WIPE_CASES: &[(&str, bool)] = &[
    ("dd if=/dev/zero of=/dev/sda", true),
    ("dd if=/dev/zero of=\"/dev/sda\"", true),
    ("dd if=/dev/zero of='/dev/sda'", true),
    ("dd if=/dev/zero of=\"/dev/sda", false),
    ("dd if=/dev/zero of='/dev/sda", false),
    ("dd if=/dev/zero of=/dev/sda\"", false),
    ("dd if=/dev/zero of=\"/dev/sda'", false),
    ("dd if=/dev/zero of=/dev/nvme0n1p3", true),
    ("dd if=/dev/zero of=\"/dev/mapper/vg-root\"", true),
    ("dd if=/dev/zero of=/dev/sda bs=1M", true),
    ("dd if=/dev/zero of=\"/dev/sda\" bs=1M", true),
    ("dd if=/dev/zero of=/dev/sdaX", false),
    ("dd if=/dev/zero of=/tmp/file", false),
    ("mkfs.ext4 /dev/sda1", true),
    ("mkfs /dev/sdb", true),
    ("wipefs -a /dev/sda", true),
    ("shred -u /dev/sda", true),
];

#[test]
fn disk_wipe_matches_v1_behaviour() {
    let rule = load_rules(Language::Bash)
        .expect("rule set loads")
        .into_iter()
        .find(|rule| rule.rule_id == "shell-disk-wipe")
        .expect("shell-disk-wipe is shipped");
    let regex = Regex::new(&rule.regex).expect("pattern compiles");
    for (case, expected) in DISK_WIPE_CASES {
        let matched = regex
            .is_match(case)
            .expect("match does not exhaust backtracking");
        assert_eq!(matched, *expected, "diverged from V1 on: {case}");
    }
}
