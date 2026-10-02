//! Fixture goldens for the build-log domain compressor.
//!
//! Each `<name>.txt` input has a committed `<name>.expected.txt` baseline.
//! To re-baseline after an intentional engine change:
//! `REGEN_GOLDENS=1 cargo test -p tokenless-compressors --test golden_test`
//! then review the diff.

use std::fs;
use std::path::PathBuf;

use tokenless_ccr::{
    InMemoryStore, RecoveryMethod, StashStore, compute_key, is_valid_hash, recovery_instruction,
};
use tokenless_compressors::{BuildLogCompressor, BuildLogOperation, BuildLogOutcome};

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
