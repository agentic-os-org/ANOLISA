//! Fixture goldens for build logs and content-domain compressors.
//! Run with `-- --nocapture --test-threads=1` to display input/output comparisons.
//!
//! Each fixture input has a committed `<name>.expected.txt` baseline.
//! To re-baseline after an intentional engine change:
//! `REGEN_GOLDENS=1 cargo test -p tokenless-compressors --test golden_test`
//! then review the diff.

use std::fs;
use std::path::PathBuf;

use tokenless_ccr::{
    InMemoryStore, RecoveryMethod, StashStore, compute_key, is_valid_hash, recovery_instruction,
};
use tokenless_compressors::{
    BuildLogCompressor, BuildLogOperation, BuildLogOutcome, HtmlExtractor, JsonCompressionContext,
    JsonCompressor, JsonOperation, SearchResultsCompressor, TabularCompressor, TabularOperation,
};

const FIXTURES: &[&str] = &[
    "cargo_success",
    "cargo_failure",
    "npm_success",
    "npm_failure",
    "jest_success",
    "jest_failure",
    "pytest_success",
    "pytest_failure",
    "go_success",
    "go_failure",
    "shell_success",
    "shell_failure",
];

/// Task facts (§6.1) that must survive compression verbatim: error identity,
/// file:line references, exit state, summaries.
const PROBES: &[(&str, &[&str])] = &[
    (
        "cargo_success",
        &["Finished `release` profile [optimized] target(s) in 42.18s"],
    ),
    (
        "cargo_failure",
        &[
            "error[E0308]",
            "--> src/main.rs:12:5",
            "error: could not compile `app` (bin \"app\") due to 1 previous error",
        ],
    ),
    (
        "npm_success",
        &[
            "added 41 packages, and audited 42 packages in 3s",
            "found 0 vulnerabilities",
        ],
    ),
    (
        "npm_failure",
        &[
            "npm ERR! code E404",
            "'left-padd@^1.0.0' is not in this registry.",
        ],
    ),
    ("jest_success", &["Test Suites: 12 passed, 12 total"]),
    (
        "jest_failure",
        &[
            "FAIL src/critical.test.js",
            "Error: expected true to be false",
            "at verify (/work/src/critical.test.js:14:9)",
            "Test Suites: 1 failed, 20 passed, 21 total",
        ],
    ),
    ("pytest_success", &["38 passed in 1.23s"]),
    (
        "pytest_failure",
        &[
            "FAILED tests/test_math.py::test_answer - assert 3 == 4",
            "E       assert 3 == 4",
            "1 failed, 33 passed in 2.14s",
        ],
    ),
    ("go_success", &["ok  \tgithub.com/acme/app\t0.123s"]),
    (
        "go_failure",
        &[
            "./main.go:10:2: undefined: fooBar",
            "FAIL\tgithub.com/acme/app [build failed]",
        ],
    ),
    (
        "shell_success",
        &["make: Leaving directory '/work/project'", "Exit code: 0"],
    ),
    (
        "shell_failure",
        &[
            "src/util.c:42:5: error: 'foo' undeclared (first use in this function)",
            "make: *** [Makefile:12: build/util.o] Error 1",
            "Exit code: 2",
        ],
    ),
];

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build_logs")
}

fn load(name: &str) -> String {
    let path = fixtures_dir().join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn run_chain(input: &str, store: &InMemoryStore) -> (String, BuildLogOutcome) {
    let cleaned = strip_sgr_for_expected_reassembly(input);
    let outcome = BuildLogCompressor.compress(input, Some(store));
    (cleaned, outcome)
}

#[test]
fn outputs_match_committed_goldens_and_are_deterministic() {
    let regen = std::env::var("REGEN_GOLDENS").is_ok();
    for name in FIXTURES {
        let input = load(&format!("{name}.txt"));
        let (_, first) = run_chain(&input, &InMemoryStore::new());
        let (_, second) = run_chain(&input, &InMemoryStore::new());
        assert_eq!(
            first.output, second.output,
            "{name}: non-deterministic output"
        );

        print_case("BUILD LOG", name, &input, &first.output);

        let expected_path = fixtures_dir().join(format!("{name}.expected.txt"));
        if regen {
            fs::write(&expected_path, &first.output).unwrap();
            continue;
        }
        let expected = fs::read_to_string(&expected_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", expected_path.display()));
        assert_eq!(
            first.output, expected,
            "{name}: output diverged from golden"
        );
    }
}

#[test]
fn markers_are_valid_and_keys_match_payloads() {
    for name in FIXTURES {
        let input = load(&format!("{name}.txt"));
        let store = InMemoryStore::new();
        let (_, outcome) = run_chain(&input, &store);
        for write in &outcome.stash_writes {
            assert!(is_valid_hash(&write.key), "{name}: bad key {}", write.key);
            assert!(
                outcome
                    .output
                    .contains(&recovery_instruction(&write.key, &RecoveryMethod::Shell)),
                "{name}: marker for {} missing from output",
                write.key
            );
            let payload = store.retrieve(&write.key).unwrap().expect("payload");
            assert_eq!(
                compute_key(payload.as_bytes()),
                write.key,
                "{name}: key mismatch"
            );
        }
    }
}

#[test]
fn reassembly_reproduces_the_lossy_stage_input_byte_exactly() {
    for name in FIXTURES {
        let input = load(&format!("{name}.txt"));
        let store = InMemoryStore::new();
        let (cleaned, outcome) = run_chain(&input, &store);
        let reassembled = reassemble(&outcome.output, &store);
        assert_eq!(reassembled, cleaned, "{name}: reassembly diverged");
    }
}

#[test]
fn task_facts_survive_compression() {
    for (name, probes) in PROBES {
        let input = load(&format!("{name}.txt"));
        let (_, outcome) = run_chain(&input, &InMemoryStore::new());
        for probe in *probes {
            assert!(
                outcome.output.contains(probe),
                "{name}: task fact missing from output: {probe}"
            );
        }
    }
}

#[test]
fn every_compressing_fixture_actually_saves() {
    // Success fixtures dominated by signal (pytest verbose) legitimately
    // pass through; the rest must shrink.
    for name in FIXTURES {
        let input = load(&format!("{name}.txt"));
        let store = InMemoryStore::new();
        let (cleaned, outcome) = run_chain(&input, &store);
        if outcome
            .operations
            .contains(&BuildLogOperation::ProgressReduction)
        {
            assert!(
                outcome.output.chars().count() < cleaned.chars().count(),
                "{name}: markers without net savings"
            );
        } else {
            assert_eq!(
                outcome.output, cleaned,
                "{name}: no blocks but output changed?"
            );
        }
    }
}

fn reassemble(output: &str, store: &InMemoryStore) -> String {
    let mut result = String::new();
    for line in output.split_inclusive('\n') {
        if line.contains("If needed, run in shell: tokenless retrieve ") {
            let hash = tokenless_ccr::extract_hash(line).expect("marker on omission line");
            result.push_str(&store.retrieve(hash).unwrap().expect("stashed payload"));
        } else {
            result.push_str(line);
        }
    }
    result
}

fn strip_sgr_for_expected_reassembly(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("\x1b[") {
        output.push_str(&rest[..start]);
        let sequence = &rest[start + 2..];
        let Some(end) = sequence.find('m') else {
            output.push_str(&rest[start..]);
            return output;
        };
        if !sequence[..end]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b';')
        {
            output.push_str(&rest[start..start + 1]);
            rest = &rest[start + 1..];
            continue;
        }
        rest = &sequence[end + 1..];
    }
    output.push_str(rest);
    output
}

fn print_case(category: &str, name: &str, input: &str, output: &str) {
    let border = "=".repeat(88);
    let divider = "-".repeat(88);
    let mut display = format!("\n\n{border}\n  GOLDEN CASE | {category} | {name}\n{border}\n");
    for (label, text) in [("INPUT / BEFORE", input), ("OUTPUT / AFTER", output)] {
        display.push_str(&format!(
            "\n  {label}  [{} bytes, {} lines]\n{divider}\n",
            text.len(),
            text.lines().count()
        ));
        for line in text.lines() {
            display.push_str("  | ");
            display.push_str(line);
            display.push('\n');
        }
        display.push_str(&format!("{divider}\n  END {label}\n"));
    }
    display.push_str(&format!("\n  END CASE | {category} | {name}\n{border}\n"));
    print!("{display}");
}

fn check(name: &str, input: &str, output: &str) {
    assert!(
        tokenless_protocol::estimate_tokens(output) < tokenless_protocol::estimate_tokens(input),
        "{name}: no estimated token savings"
    );
    check_snapshot(name, input, output);
}

fn check_snapshot(name: &str, input: &str, output: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/content")
        .join(format!("{name}.expected.txt"));
    let category = if name.starts_with("json_toon") {
        "TOON"
    } else if name.starts_with("json_") {
        "JSON"
    } else if name.starts_with("html_") {
        "HTML"
    } else if name.ends_with(".tsv") {
        "TSV"
    } else if name.ends_with(".csv") {
        "CSV"
    } else {
        "SEARCH"
    };
    print_case(category, name, input, output);
    if std::env::var_os("REGEN_GOLDENS").is_some() {
        fs::write(path, output).unwrap();
    } else {
        let expected = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        assert_eq!(output, expected, "{name}");
    }
}

fn input(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/content")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[test]
fn json_goldens() {
    for (name, operation) in [
        ("json_cleanup", Some(JsonOperation::Cleanup)),
        ("json_records", Some(JsonOperation::RecordReduction)),
        ("json_toon", Some(JsonOperation::Toon)),
        ("json_already_compact_passthrough", None),
        ("json_records_anomaly", Some(JsonOperation::RecordReduction)),
        ("json_toon_unicode", Some(JsonOperation::Toon)),
    ] {
        let source = input(&format!("{name}.json"));
        let run = || {
            let store = InMemoryStore::new();
            let result = JsonCompressor::default()
                .compress(
                    &source,
                    &JsonCompressionContext {
                        recovery: &RecoveryMethod::Shell,
                        stash: Some(&store),
                        allow_toon: operation == Some(JsonOperation::Toon),
                        min_toon_chars: 0,
                        allow_unrecoverable: false,
                    },
                )
                .unwrap();
            if let Some(operation) = operation {
                assert!(result.operations.contains(&operation), "{name}");
            } else {
                assert!(result.operations.is_empty(), "{name}");
                assert_eq!(result.output, source, "{name}");
            }
            if operation != Some(JsonOperation::RecordReduction) {
                let decoded: serde_json::Value = if operation == Some(JsonOperation::Toon) {
                    toon_format::decode_default(&result.output).unwrap()
                } else {
                    serde_json::from_str(&result.output).unwrap()
                };
                assert_eq!(
                    decoded,
                    serde_json::from_str::<serde_json::Value>(&source).unwrap()
                );
            }
            if name == "json_records_anomaly" {
                assert!(result.output.contains("deployment failed: timeout"));
            }
            assert_eq!(
                result.stash_writes.len(),
                usize::from(operation == Some(JsonOperation::RecordReduction)),
                "{name}: unexpected stash writes"
            );
            for write in &result.stash_writes {
                assert!(is_valid_hash(&write.key), "{name}: invalid stash key");
                assert!(
                    result
                        .output
                        .contains(&recovery_instruction(&write.key, &RecoveryMethod::Shell)),
                    "{name}: missing recovery instruction for {}",
                    write.key
                );
                let recovered = store.retrieve(&write.key).unwrap().unwrap();
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&recovered).unwrap(),
                    serde_json::from_str::<serde_json::Value>(&source).unwrap()
                );
            }
            result.output
        };
        let output = run();
        assert_eq!(output, run(), "{name}: non-deterministic output");
        if operation.is_some() {
            check(name, &source, &output);
        } else {
            check_snapshot(name, &source, &output);
        }
    }
}

#[test]
fn html_goldens() {
    for (name, retained) in [
        ("html_page", "service deploy --environment staging"),
        ("html_article", "Connection timeout after 30 seconds."),
        ("html_table", "FAILED"),
        ("html_entities", "timeout=30"),
    ] {
        let source = input(&format!("{name}.html"));
        let output = HtmlExtractor.render(&source).unwrap().output;
        assert_eq!(output, HtmlExtractor.render(&source).unwrap().output);
        assert!(output.contains(retained));
        assert!(!output.contains("trackVisitor"));
        check(name, &source, &output);
    }
}

#[test]
fn tabular_goldens() {
    for (name, expect_compaction) in [
        ("table_compaction.csv", true),
        ("table_compaction.tsv", true),
        ("table_complete_regression.csv", false),
        ("table_special.csv", true),
        ("table_special.tsv", true),
        ("table_complete_regression.tsv", false),
    ] {
        let source = input(name);
        let run = || {
            let result = TabularCompressor.compress(&source);
            if !expect_compaction {
                assert!(result.output.contains("FAILED"));
                assert_eq!(result.output, source);
                assert!(result.operations.is_empty());
            } else {
                assert_eq!(result.operations, [TabularOperation::Compaction]);
            }
            let delimiter = if name.ends_with(".tsv") { b'\t' } else { b',' };
            let cells = |text: &str| {
                csv::ReaderBuilder::new()
                    .delimiter(delimiter)
                    .has_headers(false)
                    .from_reader(text.as_bytes())
                    .records()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap()
            };
            assert_eq!(cells(&source), cells(&result.output));
            assert!(result.stash_writes.is_empty());
            assert_eq!(result.metrics.original_rows, result.metrics.retained_rows);
            assert_eq!(
                result.recoverability,
                tokenless_compressors::Recoverability::Lossless
            );
            result.output
        };
        let output = run();
        assert_eq!(output, run(), "{name}: non-deterministic output");
        if !expect_compaction {
            check_snapshot(name, &source, &output);
        } else {
            check(name, &source, &output);
        }
    }
}

#[test]
fn search_goldens() {
    for name in [
        "search_paths",
        "search_multiple_files",
        "search_unicode",
        "search_line_endings",
    ] {
        let source = input(&format!("{name}.txt"));
        let output = SearchResultsCompressor.compress(&source).unwrap();
        assert_eq!(output, SearchResultsCompressor.compress(&source).unwrap());
        let mut path = String::new();
        let mut restored = String::new();
        let mut lines = output.split_inclusive('\n');
        assert_eq!(
            lines.next(),
            Some("[Search results grouped by file; all received lines retained]\n"),
            "{name}: unexpected search header"
        );
        for line in lines {
            if let Some(quoted) = line.strip_prefix("File=") {
                path = serde_json::from_str(quoted).unwrap();
            } else {
                assert!(!path.is_empty(), "{name}: record before file header");
                restored.push_str(&format!("{path}:{line}"));
            }
        }
        assert_eq!(restored, source);
        check(name, &source, &output);
    }
}
