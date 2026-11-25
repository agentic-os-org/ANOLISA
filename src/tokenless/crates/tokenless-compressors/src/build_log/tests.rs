use super::*;

mod pnpm;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokenless_ccr::{InMemoryStore, StashError, StashStore, StashWrite, extract_hash};

struct AlwaysFail;

impl StashStore for AlwaysFail {
    fn stash(&self, _payload: &str) -> Result<StashWrite, StashError> {
        Err(StashError::Backend("simulated".to_owned()))
    }

    fn retrieve(&self, _hash: &str) -> Result<Option<String>, StashError> {
        Ok(None)
    }

    fn len(&self) -> usize {
        0
    }

    fn evict_expired(&self) -> Result<usize, StashError> {
        Ok(0)
    }

    fn delete(&self, _hash: &str, _generation: u64) -> Result<bool, StashError> {
        Ok(false)
    }
}

/// Stash store whose first write succeeds and every later write fails,
/// simulating a backend that breaks mid-run (disk full, lock lost).
struct FailAfterFirst {
    inner: InMemoryStore,
    calls: AtomicUsize,
}

impl StashStore for FailAfterFirst {
    fn stash(&self, payload: &str) -> Result<StashWrite, StashError> {
        if self.calls.fetch_add(1, Ordering::Relaxed) > 0 {
            return Err(StashError::Backend("simulated mid-run failure".to_owned()));
        }
        self.inner.stash(payload)
    }

    fn retrieve(&self, hash: &str) -> Result<Option<String>, StashError> {
        self.inner.retrieve(hash)
    }

    fn len(&self) -> usize {
        self.inner.len()
    }

    fn evict_expired(&self) -> Result<usize, StashError> {
        self.inner.evict_expired()
    }

    fn delete(&self, hash: &str, generation: u64) -> Result<bool, StashError> {
        self.inner.delete(hash, generation)
    }
}

fn cargo_log(lines: usize) -> String {
    let mut output = "$ cargo build\n".to_owned();
    for index in 0..lines {
        output.push_str(&format!(
            "   Compiling package-{index:03} v0.1.{index} with a deliberately long progress suffix\n"
        ));
    }
    output.push_str("Finished `dev` profile [unoptimized] target(s) in 2.1s\n");
    output
}

fn cargo_test_log(passing: usize) -> String {
    let mut output = format!("$ cargo test\nrunning {passing} tests\n");
    for index in 0..passing {
        output.push_str(&format!(
            "test parser::case_{index:03}_with_a_descriptive_name ... ok\n"
        ));
    }
    output.push_str(&format!(
        "test result: ok. {passing} passed; 0 failed; finished in 0.30s\n"
    ));
    output
}

#[test]
fn static_tool_instructions_restore_exact_log_intervals() {
    use tokenless_ccr::{RecoveryMethod, recovery_hashes, recovery_instruction};
    let method = RecoveryMethod::tool("t".repeat(64)).unwrap();
    let store = InMemoryStore::new();
    let input = cargo_test_log(40);
    let outcome = BuildLogCompressor.compress_with_recovery(&input, Some(&store), &method);
    assert_eq!(outcome.metrics.omitted_blocks, 1);
    let hashes = recovery_hashes(&outcome.output, &method);
    assert_eq!(hashes.len(), 1);
    assert!(!outcome.output.contains("<<tokenless:"));
    let line = outcome
        .output
        .lines()
        .find(|line| line.contains(&recovery_instruction(hashes[0], &method)))
        .unwrap();
    let restored = outcome.output.replace(
        &format!("{line}\n"),
        &store.retrieve(hashes[0]).unwrap().unwrap(),
    );
    assert_eq!(restored, input);
    let none =
        BuildLogCompressor.compress_with_recovery(&input, Some(&store), &RecoveryMethod::None);
    assert_eq!(none.output, input);
    assert!(none.stash_writes.is_empty());
}

fn pytest_quiet_log(files: usize) -> String {
    let mut output = String::new();
    for index in 0..files {
        output.push_str(&format!(
            "tests/test_module_{index:03}.py ........ [ {:>3}%]\n",
            (index + 1) * 100 / files
        ));
    }
    output.push_str(&format!("{} passed in 2.34s\n", files * 8));
    output
}

fn unittest_log(passing: usize) -> String {
    let mut output = (0..passing)
        .map(|index| format!("test_case_{index:03} (__main__.ExampleTests) ... ok\n"))
        .collect::<String>();
    output.push_str(&format!(
        "\n----------------------------------------------------------------------\nRan {passing} tests in 0.295s\n\nOK\n"
    ));
    output
}

#[test]
fn unittest_success_reduces_and_restores_exactly() {
    for input in [
        unittest_log(83),
        unittest_log(83).replace("ExampleTests)", "ExampleTests.test_case)"),
        unittest_log(83).replace('\n', "\r\n"),
    ] {
        assert!(BuildLogCompressor::detect(&input));
        let store = InMemoryStore::new();
        let outcome = BuildLogCompressor.compress(&input, Some(&store));
        assert_eq!(outcome.operations, [BuildLogOperation::Reduction]);
        assert_eq!(outcome.metrics.omitted_lines, 79);
        assert!(outcome.output.contains("test_case_000"));
        assert!(outcome.output.contains("test_case_082"));
        assert!(outcome.output.contains("Ran 83 tests in 0.295s"));
        assert!(outcome.output.ends_with("OK\n") || outcome.output.ends_with("OK\r\n"));
        let write = &outcome.stash_writes[0];
        assert_eq!(
            replace_marker_line(
                &outcome.output,
                &write.key,
                &store.retrieve(&write.key).unwrap().unwrap(),
            ),
            input
        );
    }
    let input = unittest_log(83);
    assert_eq!(BuildLogCompressor.compress(&input, None).output, input);
    assert_eq!(
        BuildLogCompressor
            .compress(&input, Some(&AlwaysFail))
            .output,
        input
    );
}

#[test]
fn unittest_django_keeps_nonpassing_records_and_tracebacks() {
    let setup = "Creating test database for alias 'default'...\n\
                 Operations to perform:\n\
                   Apply all migrations: auth, contenttypes\n\
                 Running migrations:\n\
                   Applying auth.0001_initial... OK\n\
                 System check identified no issues (0 silenced).\n";
    let diagnostics = "test_skip (auth_tests.FormsTest) ... skipped 'database unavailable'\n\
                       test_expected (auth_tests.FormsTest) ... expected failure\n\
                       test_unexpected (auth_tests.FormsTest) ... unexpected success\n\
                       test_fail (auth_tests.FormsTest) ... FAIL\n\
                       test_error (auth_tests.FormsTest) ... ERROR\n\
                       test_documented (auth_tests.FormsTest)\n\
                       A description of the test ... ok\n\
                       ======================================================================\n\
                       ERROR: test_error (auth_tests.FormsTest)\n\
                       ----------------------------------------------------------------------\n\
                       Traceback (most recent call last):\n\
                       \x20 File \"test_forms.py\", line 42, in test_error\n\
                       \x20   raise ValueError('invalid form')\n\
                       ValueError: invalid form\n\n";
    let summary = "Ran 86 tests in 0.295s\n\n\
                   FAILED (failures=1, errors=1, skipped=1, expected failures=1, unexpected successes=1)\n\
                   Destroying test database for alias 'default'...\n";
    let mut input = format!("{setup}{}", unittest_log(80));
    input.insert_str(input.find("test_case_040").unwrap(), diagnostics);
    input = input.replace("Ran 80 tests in 0.295s\n\nOK\n", summary);
    let store = InMemoryStore::new();
    let outcome = BuildLogCompressor.compress(&input, Some(&store));
    assert_eq!(outcome.operations, [BuildLogOperation::Reduction]);
    assert_eq!(outcome.metrics.omitted_blocks, 2);
    assert!(outcome.output.starts_with(setup));
    assert!(outcome.output.contains(diagnostics));
    assert!(outcome.output.ends_with(summary));
    let mut restored = outcome.output.clone();
    for write in &outcome.stash_writes {
        restored = replace_marker_line(
            &restored,
            &write.key,
            &store.retrieve(&write.key).unwrap().unwrap(),
        );
    }
    assert_eq!(restored, input);
}

#[test]
fn unittest_detection_accepts_partial_runs_and_rejects_prose() {
    let partial = "test_a (tests.Example) ... ok\n\
                   test_b (tests.Example) ... expected failure\n\
                   test_c (tests.Example) ... skipped 'not supported'\n";
    assert!(BuildLogCompressor::detect(partial));
    let prefaced = format!("{}{partial}", "Preparing database...\n".repeat(140));
    assert!(BuildLogCompressor::detect(&prefaced));
    for summary in ["Ran 1 test in 0.001s", "Ran 83 tests in 0.295 s"] {
        let input = format!("{summary}\n\nOK\n");
        assert!(BuildLogCompressor::detect(&input));
        assert_eq!(
            BuildLogCompressor
                .compress(&input, Some(&InMemoryStore::new()))
                .output,
            input
        );
    }
    for prose in [
        "The unittest runner reports tests as ok, skipped, FAIL or ERROR.\n",
        "OK\n",
        "Ran tests in development.\n",
        "Ran 83 tests in a temporary database.\n",
        "Example: test_case (tests.Example) ... ok\n"
            .repeat(20)
            .as_str(),
        "test prose (not a test identifier) ... ok\n"
            .repeat(20)
            .as_str(),
    ] {
        assert!(!BuildLogCompressor::detect(prose), "detected: {prose}");
        assert_eq!(
            BuildLogCompressor
                .compress(prose, Some(&InMemoryStore::new()))
                .output,
            prose
        );
    }
}

#[test]
fn detects_six_supported_dialects() {
    for log in [
        "   Compiling a v1\n   Compiling b v1\n   Compiling c v1\n",
        "===== test session starts =====\ntest_a::case PASSED\n",
        "npm ERR! code E404\n",
        "PASS src/a.test.js\nPASS src/b.test.js\nPASS src/c.test.js\n",
        "=== RUN   TestOne\n--- PASS: TestOne (0.00s)\n",
        "make: Entering directory '/work'\ncc -c a.c -o a.o\n",
    ] {
        assert!(BuildLogCompressor::detect(log), "undetected log: {log}");
    }
}

#[test]
fn detects_strong_signal_in_the_tail_sample() {
    let mut log = (0..140)
        .map(|index| format!("ordinary preface {index}\n"))
        .collect::<String>();
    log.push_str("npm ERR! code E404\n");
    assert!(BuildLogCompressor::detect(&log));
}

#[test]
fn rejects_prose_and_source_that_merely_name_log_words() {
    let prose = "This document discusses npm ERR! and test result: as examples.\n";
    let source = r#"const example = \"npm ERR! code E404\";
const summary = \"test result: ok\";
"#;
    assert!(!BuildLogCompressor::detect(prose));
    assert!(!BuildLogCompressor::detect(source));
}

#[test]
fn terminal_cleanup_is_lossless_when_no_progress_run_exists() {
    let input = (0..20)
        .map(|index| format!("\x1b[1m\x1b[31merror[E{index:04}]\x1b[0m: diagnostic {index}\n"))
        .collect::<String>();
    let store = InMemoryStore::new();
    let outcome = BuildLogCompressor.compress(&input, Some(&store));

    assert_eq!(outcome.operations, [BuildLogOperation::TerminalCleanup]);
    assert_eq!(outcome.recoverability, crate::Recoverability::Lossless);
    assert!(!outcome.output.contains('\x1b'));
    assert!(outcome.stash_writes.is_empty());
}

#[test]
fn colored_progress_continues_to_recoverable_reduction() {
    let input = cargo_log(30).replace("   Compiling", "   \x1b[1m\x1b[32mCompiling\x1b[0m");
    let store = InMemoryStore::new();
    let outcome = BuildLogCompressor.compress(&input, Some(&store));

    assert_eq!(
        outcome.operations,
        [
            BuildLogOperation::TerminalCleanup,
            BuildLogOperation::Reduction,
        ]
    );
    assert_eq!(outcome.recoverability, crate::Recoverability::Retrievable);
    assert!(!outcome.output.contains('\x1b'));
    assert_eq!(outcome.metrics.omitted_blocks, 1);
    assert_eq!(outcome.stash_writes.len(), 1);
}

#[test]
fn reduction_keeps_edges_diagnostics_summary_and_unknown_lines() {
    let mut input = cargo_log(30);
    input.insert_str(
        input.find("Finished ").unwrap(),
        "an unexplained but important line\nerror[E0308]: mismatched types\n  --> src/main.rs:3:4\n",
    );
    let store = InMemoryStore::new();
    let outcome = BuildLogCompressor.compress(&input, Some(&store));

    assert_eq!(outcome.operations, [BuildLogOperation::Reduction]);
    assert_eq!(outcome.recoverability, crate::Recoverability::Retrievable);
    assert_eq!(outcome.metrics.omitted_blocks, 1);
    assert!(outcome.output.contains("package-000"));
    assert!(outcome.output.contains("package-001"));
    assert!(outcome.output.contains("package-028"));
    assert!(outcome.output.contains("package-029"));
    assert!(outcome.output.contains("error[E0308]"));
    assert!(outcome.output.contains("src/main.rs:3:4"));
    assert!(outcome.output.contains("an unexplained but important line"));
    assert!(outcome.output.contains("Finished `dev` profile"));
}

#[test]
fn cargo_test_reduction_keeps_failure_and_summary() {
    let mut input = cargo_test_log(30);
    let position = input.find("test parser::case_015").unwrap();
    let end = input[position..].find('\n').unwrap() + position + 1;
    input.replace_range(
        position..end,
        "test parser::case_015_with_a_descriptive_name ... FAILED\nthread 'parser::case_015_with_a_descriptive_name' panicked at src/parser.rs:15:5:\nassertion failed: parsed.is_valid()\n",
    );
    input = input.replace(
        "test result: ok. 30 passed; 0 failed; finished in 0.30s",
        "test result: FAILED. 29 passed; 1 failed; finished in 0.30s",
    );

    let store = InMemoryStore::new();
    let outcome = BuildLogCompressor.compress(&input, Some(&store));

    assert_eq!(outcome.operations, [BuildLogOperation::Reduction]);
    assert_eq!(outcome.metrics.omitted_blocks, 2);
    assert!(
        outcome
            .output
            .contains("case_015_with_a_descriptive_name ... FAILED")
    );
    assert!(
        outcome
            .output
            .contains("assertion failed: parsed.is_valid()")
    );
    assert!(outcome.output.contains("test result: FAILED"));
    let mut restored = outcome.output.clone();
    for write in &outcome.stash_writes {
        let payload = store.retrieve(&write.key).unwrap().unwrap();
        restored = replace_marker_line(&restored, &write.key, &payload);
    }
    assert_eq!(restored, input);
}

#[test]
fn pytest_quiet_progress_reduces_but_xpass_remains_visible() {
    let mut input = pytest_quiet_log(30);
    let position = input.find("tests/test_module_015.py").unwrap();
    let end = input[position..].find('\n').unwrap() + position + 1;
    input.replace_range(
        position..end,
        "tests/test_module_015.py ....X... [ 53%]\nXPASS tests/test_module_015.py::test_changed - behavior changed\n",
    );
    input = input.replace("240 passed in 2.34s", "239 passed, 1 xpassed in 2.34s");

    let store = InMemoryStore::new();
    let outcome = BuildLogCompressor.compress(&input, Some(&store));

    assert_eq!(outcome.operations, [BuildLogOperation::Reduction]);
    assert_eq!(outcome.metrics.omitted_blocks, 2);
    assert!(outcome.output.contains("....X... [ 53%]"));
    assert!(
        outcome
            .output
            .contains("XPASS tests/test_module_015.py::test_changed")
    );
    assert!(outcome.output.contains("239 passed, 1 xpassed in 2.34s"));
    let mut restored = outcome.output.clone();
    for write in &outcome.stash_writes {
        let payload = store.retrieve(&write.key).unwrap().unwrap();
        restored = replace_marker_line(&restored, &write.key, &payload);
    }
    assert_eq!(restored, input);
}

#[test]
fn cargo_test_and_pytest_progress_need_recovery_and_a_long_run() {
    for input in [cargo_test_log(30), pytest_quiet_log(30)] {
        let outcome = BuildLogCompressor.compress(&input, None);
        assert_eq!(outcome.output, input);
        assert!(outcome.operations.is_empty());
    }

    for input in [cargo_test_log(8), pytest_quiet_log(8)] {
        let store = InMemoryStore::new();
        let outcome = BuildLogCompressor.compress(&input, Some(&store));
        assert_eq!(outcome.output, input);
        assert!(outcome.operations.is_empty());
        assert_eq!(store.len(), 0);
    }
}

#[test]
fn every_marker_restores_its_cleaned_gap_exactly() {
    let input = cargo_log(30);
    let store = InMemoryStore::new();
    let outcome = BuildLogCompressor.compress(&input, Some(&store));
    let mut restored = outcome.output.clone();
    for write in &outcome.stash_writes {
        let payload = store.retrieve(&write.key).unwrap().unwrap();
        restored = replace_marker_line(&restored, &write.key, &payload);
    }
    assert_eq!(restored, input);
}

#[test]
fn no_store_allows_cleanup_but_not_progress_reduction() {
    let input = cargo_log(30);
    let outcome = BuildLogCompressor.compress(&input, None);
    assert_eq!(outcome.output, input);
    assert!(outcome.operations.is_empty());
    assert_eq!(outcome.metrics.omitted_blocks, 0);
}

#[test]
fn stash_failure_is_reported_without_emitting_a_marker() {
    let input = cargo_log(30);
    let outcome = BuildLogCompressor.compress(&input, Some(&AlwaysFail));
    assert_eq!(outcome.output, input);
    assert!(outcome.operations.is_empty());
    assert_eq!(outcome.metrics.stash_errors, 1);
    assert!(outcome.stash_writes.is_empty());
}

#[test]
fn partial_stash_failure_returns_tentative_writes_with_lossless_output() {
    // First omission block stashes, the second fails mid-run: the candidate
    // must fall back to the lossless log without a marker, while the one
    // tentative write is still returned so the runtime ledger can delete the
    // orphaned row.
    let mut input = cargo_log(30);
    let position = input.find("Compiling package-015").unwrap();
    input.insert_str(
        position,
        "error[E0308]: mismatched types\n  --> src/main.rs:3:4\n",
    );
    let store = FailAfterFirst {
        inner: InMemoryStore::new(),
        calls: AtomicUsize::new(0),
    };
    let outcome = BuildLogCompressor.compress(&input, Some(&store));

    assert_eq!(outcome.recoverability, crate::Recoverability::Lossless);
    assert_eq!(outcome.output, input);
    assert!(!outcome.output.contains("lines omitted"));
    assert_eq!(outcome.metrics.stash_errors, 1);
    assert_eq!(outcome.stash_writes.len(), 1);
    assert!(outcome.stash_writes[0].created);
    assert_eq!(store.len(), 1);
}

#[test]
fn duplicate_omission_payloads_share_one_key_across_markers() {
    // Two omission blocks with identical payloads: the first stash creates
    // the row, the second refreshes it, and both markers carry the same key —
    // one retrievable row backs two markers.
    let mut input = "$ cargo build\n".to_owned();
    for group in 0..2 {
        for _ in 0..12 {
            input.push_str(
                "Compiling shared-package v0.1.0 with a deliberately long progress suffix\n",
            );
        }
        input.push_str(&format!("phase boundary {group}\n"));
    }
    input.push_str("Finished `dev` profile [unoptimized] target(s) in 2.1s\n");
    let store = InMemoryStore::new();
    let outcome = BuildLogCompressor.compress(&input, Some(&store));

    assert_eq!(outcome.recoverability, crate::Recoverability::Retrievable);
    assert_eq!(outcome.metrics.omitted_blocks, 2);
    assert_eq!(outcome.stash_writes.len(), 2);
    let (first, second) = (&outcome.stash_writes[0], &outcome.stash_writes[1]);
    assert!(first.created);
    assert!(!second.created);
    assert_eq!(first.key, second.key);
    assert_eq!(second.previous_generation, Some(first.generation));
    assert_eq!(store.len(), 1, "one row backs both markers");
    let markers = outcome
        .output
        .matches(&recovery_instruction(&first.key, &RecoveryMethod::Shell))
        .count();
    assert_eq!(markers, 2);
}

#[test]
fn locally_unprofitable_gap_stays_verbatim() {
    let input = "$ cargo build\nCompiling a\nCompiling b\nCompiling c\nCompiling d\nCompiling e\nCompiling f\nCompiling g\nCompiling h\nCompiling i\n";
    let store = InMemoryStore::new();
    let outcome = BuildLogCompressor.compress(input, Some(&store));
    assert_eq!(outcome.output, input);
    assert!(outcome.operations.is_empty());
    assert_eq!(store.len(), 0);
}

#[test]
fn more_than_twenty_four_profitable_gaps_retains_top_twenty_four() {
    let mut input = "$ cargo build\n".to_owned();
    // 24 large profitable groups + 2 smaller (still profitable) groups at the end.
    for group in 0..24 {
        for index in 0..20 {
            input.push_str(&format!(
                "Compiling large-group-{group}-package-{index} with a long repeated compilation description\n"
            ));
        }
        input.push_str(&format!("phase boundary large-{group}\n"));
    }
    for group in 0..2 {
        for index in 0..12 {
            input.push_str(&format!(
                "Compiling small-group-{group}-package-{index} with a long repeated compilation description\n"
            ));
        }
        input.push_str(&format!("phase boundary small-{group}\n"));
    }
    let store = InMemoryStore::new();
    let outcome = BuildLogCompressor.compress(&input, Some(&store));
    assert_eq!(outcome.operations, vec![BuildLogOperation::Reduction]);
    assert_eq!(outcome.stash_writes.len(), 24);
    assert_eq!(store.len(), 24);
    assert!(outcome.output.contains("Compiling small-group-0-package-0"));
    assert!(outcome.output.contains("Compiling small-group-1-package-0"));
    // Leading/trailing context lines of an omitted block remain visible; middle lines are omitted.
    assert!(
        !outcome
            .output
            .contains("Compiling large-group-0-package-10")
    );
}

#[test]
fn diagnostic_failure_mode_disables_generic_and_preserves_failure_anchors() {
    let mut input = String::from("$ cargo test\n");
    for index in 0..20 {
        input.push_str(&format!(
            "   Compiling crate-{index:02} v0.1.0 (/work/workspace/packages/crate-{index:02}) with extended build output\n"
        ));
    }
    for step in 1..=14 {
        input.push_str(&format!("progress: custom stage {step} complete\n"));
    }
    input.push_str("test passed_case ... ok\n");
    input.push_str("thread 'failing_case' panicked at src/lib.rs:42:9:\n");
    input.push_str("assertion `left == right` failed\n");
    input.push_str("  left: 1\n");
    input.push_str(" right: 2\n");
    input.push_str("   |\n");
    input.push_str("42 | assert_eq!(1, 2);\n");
    input.push_str("   | ^^^^^^^^^^^^^^^\n");
    input.push_str("= note: run with `RUST_BACKTRACE=1` for a backtrace\n");
    input.push_str(
        "test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n",
    );

    let store = InMemoryStore::new();
    let outcome = BuildLogCompressor.compress_with_options(
        &input,
        Some(&store),
        &RecoveryMethod::Shell,
        BuildLogMode::DiagnosticFailure,
    );
    assert!(outcome.output.contains("lines omitted."));
    assert!(!outcome.output.contains("Compiling crate-10"));
    // Generic template lines remain verbatim in DiagnosticFailure mode.
    for step in 1..=14 {
        assert!(
            outcome
                .output
                .contains(&format!("progress: custom stage {step} complete\n"))
        );
    }
    for required in [
        "thread 'failing_case' panicked at src/lib.rs:42:9:",
        "assertion `left == right` failed",
        "  left: 1",
        " right: 2",
        "42 | assert_eq!(1, 2);",
        "   | ^^^^^^^^^^^^^^^",
        "= note: run with `RUST_BACKTRACE=1` for a backtrace",
        "test result: FAILED.",
    ] {
        assert!(
            outcome.output.contains(required),
            "missing anchor: {required}"
        );
    }
}

#[test]
fn maven_over_64kib_line_only_isolates_adjacent_runs() {
    let mut input = String::from(
        "[INFO] Scanning for projects...\n\
         [INFO] --- compiler:3.13.0:compile (default-compile) @ sample ---\n",
    );
    let downloading = |start: usize| -> String {
        (start..start + 6)
            .map(|i| {
                format!(
                    "[INFO] Downloading from central: https://repo.maven.apache.org/maven2/org/example/mod-{i:02}/1.0/mod-{i:02}-1.0.pom\n\
                     [INFO] Downloaded from central: https://repo.maven.apache.org/maven2/org/example/mod-{i:02}/1.0/mod-{i:02}-1.0.pom (1.2 kB at 45 kB/s)\n"
                )
            })
            .collect()
    };
    let huge_line = format!(
        "[INFO] Diagnostic payload: {}\n",
        "x".repeat(scope::MAX_BYTES + 1)
    );
    input.push_str(&downloading(0));
    input.push_str(&huge_line);
    input.push_str(&downloading(10));
    input.push_str("[INFO] BUILD SUCCESS\n");

    let store = InMemoryStore::new();
    let outcome =
        BuildLogCompressor.compress_with_recovery(&input, Some(&store), &RecoveryMethod::Shell);
    assert!(outcome.output.contains(&huge_line));
    assert_ne!(outcome.output, input);
    assert!(outcome.output.len() < input.len());
}

#[test]
fn anchored_generic_log_reduces_only_its_dominant_template() {
    let mut input = "$ custom-builder\n".to_owned();
    for index in 0..20 {
        input.push_str(&format!(
            "progress: item {index} completed with stable generic output\n"
        ));
    }
    input.push_str("unique final observation\nExit code: 0\n");
    assert!(BuildLogCompressor::detect(&input));

    let store = InMemoryStore::new();
    let outcome = BuildLogCompressor.compress(&input, Some(&store));
    assert_eq!(outcome.operations, [BuildLogOperation::Reduction]);
    assert!(outcome.output.contains("unique final observation"));
    assert!(outcome.output.contains("Exit code: 0"));
}

#[test]
fn anchored_generic_output_does_not_reduce_repeated_data_rows() {
    let mut input = "$ custom-export\n".to_owned();
    for index in 0..20 {
        input.push_str(&format!("customer record {index} has stable output\n"));
    }
    input.push_str("Exit code: 0\n");

    assert!(!BuildLogCompressor::detect(&input));
    let store = InMemoryStore::new();
    let outcome = BuildLogCompressor.compress(&input, Some(&store));
    assert_eq!(outcome.output, input);
    assert!(outcome.operations.is_empty());
    assert_eq!(store.len(), 0);
}

#[test]
fn trace_regions_are_never_cut_by_progress_reduction() {
    let mut input = cargo_log(20);
    input.push_str(
        "Error: build script failed\n    at compile (/work/build.js:10:2)\n    at main (/work/index.js:3:1)\n",
    );
    input.push_str(&cargo_log(20));
    let outcome = BuildLogCompressor.compress(&input, Some(&InMemoryStore::new()));
    assert!(outcome.output.contains("at compile (/work/build.js:10:2)"));
    assert!(outcome.output.contains("at main (/work/index.js:3:1)"));
}

fn replace_marker_line(output: &str, key: &str, payload: &str) -> String {
    let marker = recovery_instruction(key, &RecoveryMethod::Shell);
    let marker_position = output.find(&marker).unwrap();
    assert_eq!(extract_hash(&output[marker_position..]), Some(key));
    let line_start = output[..marker_position]
        .rfind('\n')
        .map_or(0, |position| position + 1);
    let line_end = output[marker_position..]
        .find('\n')
        .map_or(output.len(), |position| marker_position + position + 1);
    format!(
        "{}{}{}",
        &output[..line_start],
        payload,
        &output[line_end..]
    )
}

#[test]
fn duplicate_payload_writes_remain_visible_to_the_runtime_ledger() {
    let store = Arc::new(InMemoryStore::new());
    let input = cargo_log(30);
    let first = BuildLogCompressor.compress(&input, Some(store.as_ref()));
    let second = BuildLogCompressor.compress(&input, Some(store.as_ref()));
    assert_eq!(first.stash_writes.len(), 1);
    assert_eq!(second.stash_writes.len(), 1);
    assert!(!second.stash_writes[0].created);
}

#[test]
fn build_steps_respects_diagnostic_mask_and_retains_protected_ranges() {
    let mut input = String::new();
    for i in 1..=4 {
        input.push_str(&format!(
            "[{i}/8] Building CXX object src/CMakeFiles/app.dir/front_{i:02}.cpp.o\n"
        ));
    }
    input.push_str("Traceback (most recent call last):\n");
    input.push_str("  File \"gen.py\", line 12, in <module>\n");
    input.push_str("RuntimeError: boom\n");
    for i in 5..=8 {
        input.push_str(&format!(
            "[{i}/8] Building CXX object src/CMakeFiles/app.dir/tail_{i:02}.cpp.o\n"
        ));
    }
    let lines = input.split_inclusive('\n').collect::<Vec<_>>();
    let stripped = lines
        .iter()
        .map(|line| line.trim_end_matches('\n').to_owned())
        .collect::<Vec<_>>();
    let mut diagnostic_mask = vec![false; lines.len()];
    for region in trace_regions(&stripped) {
        diagnostic_mask[region].fill(true);
    }
    assert!(diagnostic_mask[4..7].iter().all(|&flag| flag));

    let plan = plan_stage1(
        DetectedRoute::Stage1(Stage1Selection::BuildSteps),
        &lines,
        &diagnostic_mask,
    );
    assert_eq!(plan.layouts.len(), 2);
    assert_eq!((plan.layouts[0].start, plan.layouts[0].end), (0, 4));
    assert_eq!((plan.layouts[1].start, plan.layouts[1].end), (7, 11));

    let mut masked_diag = vec![false; lines.len()];
    masked_diag[0] = true;
    let masked_plan = plan_stage1(
        DetectedRoute::Stage1(Stage1Selection::BuildSteps),
        &lines,
        &masked_diag,
    );
    assert_eq!(masked_plan.layouts.len(), 1);
    assert_eq!(
        (masked_plan.layouts[0].start, masked_plan.layouts[0].end),
        (7, 11)
    );
    let ResidualPolicy::ScopedMixed {
        protected_ranges, ..
    } = masked_plan.residual
    else {
        panic!("expected ScopedMixed residual policy");
    };
    assert_eq!(protected_ranges, vec![0..4, 7..11]);
}

#[test]
fn scoped_compacted_header_guards_scored_routes_and_ignores_fences() {
    let store = InMemoryStore::new();
    let cmake_block = "\
        CMake/Ninja templates (id<TAB>fields; interleave with literals): {\"literals\":[[\"[\",\"/2] Building CXX object \",\".cpp.o\"]],\"rows\":2}\n\
        0\t1\ta\n\
        0\t2\tb\n";
    let compile_block = "\
        C/C++ compile templates (id<TAB>fields; interleave with literals): {\"literals\":[[\"[\",\"/2] /usr/bin/cc -c \",\".c -o \",\".o\"]],\"rows\":2}\n\
        0\t1\ta\ta\n\
        0\t2\tb\tb\n";
    let gradle_block = "\
        Gradle templates (id<TAB>fields; interleave with literals): {\"literals\":[[\"> Task :\",\" UP-TO-DATE\"]],\"rows\":2}\n\
        0\tcompileJava\n\
        0\tclasses\n";
    let ctest_block = "\
        CTest templates (kind<TAB>fields; SP/SK/SD=start+result, P/K/D=result, s/S=start, B=blank): {\"rows\":2,\"total\":\"2\",\"start_indent\":6,\"idx_width\":1,\"id_width\":1,\"col\":40,\"prefixes\":[]}\n\
        P\t1\t\ta\t0.01\n\
        P\t2\t\tb\t0.01\n";

    let mut unsampled_head = String::from("make: Entering directory '/work'\n");
    for i in 0..105 {
        unsampled_head.push_str(&format!("echo preamble step {i}\n"));
    }
    let mut unsampled_tail = String::new();
    for i in 0..95 {
        unsampled_tail.push_str(&format!("echo trailing step {i}\n"));
    }
    for i in 0..15 {
        unsampled_tail.push_str(&format!(
            "gcc -O2 -c src/file_{i:02}.c -o build/file_{i:02}.o\n"
        ));
    }
    let unguarded_make = format!("{unsampled_head}{unsampled_tail}");
    let unguarded_outcome = BuildLogCompressor.compress(&unguarded_make, Some(&store));
    assert_eq!(
        unguarded_outcome.recoverability,
        crate::Recoverability::Retrievable
    );
    assert!(!unguarded_outcome.stash_writes.is_empty());

    for block in [cmake_block, compile_block, gradle_block, ctest_block] {
        // Push the valid compacted block into the unsampled middle beyond both 100-line ends.
        let input = format!("{unsampled_head}{block}{unsampled_tail}");
        assert!(matches!(
            detect_log(&input).route,
            Some(DetectedRoute::Scored(BuildLogFormat::Make))
        ));
        let outcome = BuildLogCompressor.compress(&input, Some(&store));
        assert_eq!(outcome.output, input);
        assert_eq!(outcome.recoverability, crate::Recoverability::Lossless);
        assert!(outcome.stash_writes.is_empty());
    }

    let raw_ninja = (1..=6)
        .map(|i| format!("[{i}/8] Building CXX object src/CMakeFiles/app.dir/unit_{i:02}.cpp.o\n"))
        .collect::<String>();
    let raw_outcome = BuildLogCompressor.compress(&raw_ninja, Some(&store));
    assert_ne!(raw_outcome.output, raw_ninja);
    assert!(raw_outcome.output.contains(build_steps::CMAKE_HEADER));

    let foreign_suffixes = [
        format!("{}unavailable\n", gradle::HEADER),
        format!("{}{{\"rows\":1}}\nordinary text\n", gradle::HEADER),
        format!(
            "{}{{\"literals\":[[\"> Task :\",\" FAILED\"]],\"rows\":2}}\n0\ta\n0\tb\n",
            gradle::HEADER
        ),
        gradle_block.to_owned(),
    ];
    for foreign in foreign_suffixes {
        let combined = format!("{raw_ninja}{foreign}");
        assert!(matches!(
            detect_log(&combined).route,
            Some(DetectedRoute::Stage1(Stage1Selection::BuildSteps))
        ));
        let outcome = BuildLogCompressor.compress(&combined, Some(&store));
        assert_ne!(outcome.output, combined, "{foreign}");
        assert!(
            outcome.output.contains(build_steps::CMAKE_HEADER),
            "{foreign}"
        );
        assert!(outcome.output.contains(&foreign), "{foreign}");
        assert_eq!(outcome.recoverability, crate::Recoverability::Lossless);
        assert!(outcome.stash_writes.is_empty());

        let again = BuildLogCompressor.compress(&outcome.output, Some(&store));
        assert_eq!(again.output, outcome.output, "{foreign}");
        assert_eq!(again.recoverability, crate::Recoverability::Lossless);
        assert!(again.stash_writes.is_empty());
    }

    let mut fenced_input = String::from(
        "[1/8] Building CXX object src/CMakeFiles/app.dir/lead.cpp.o\n\
         ```text\n",
    );
    fenced_input.push_str(cmake_block);
    fenced_input.push_str("```\n");
    for i in 2..=8 {
        fenced_input.push_str(&format!(
            "[{i}/8] Building CXX object src/CMakeFiles/app.dir/unit_{i:02}.cpp.o\n"
        ));
    }
    let fenced_lines = fenced_input.split_inclusive('\n').collect::<Vec<_>>();
    assert!(!has_scoped_compacted_header(&fenced_lines));
    let outcome = BuildLogCompressor.compress(&fenced_input, Some(&store));
    assert_ne!(outcome.output, fenced_input);
    assert_eq!(outcome.output.matches(build_steps::CMAKE_HEADER).count(), 2);

    let fenced_gradle = format!("```text\n{gradle_block}```\n");
    let mut fenced_make = String::from("make: Entering directory '/work'\n");
    fenced_make.push_str(&fenced_gradle);
    for i in 0..16 {
        fenced_make.push_str(&format!(
            "gcc -O2 -c src/file_{i:02}.c -o build/file_{i:02}.o\n"
        ));
    }
    assert!(matches!(
        detect_log(&fenced_make).route,
        Some(DetectedRoute::Scored(BuildLogFormat::Make))
    ));
    let fenced_make_lines = fenced_make.split_inclusive('\n').collect::<Vec<_>>();
    assert!(!has_scoped_compacted_header(&fenced_make_lines));
    let make_outcome = BuildLogCompressor.compress(&fenced_make, Some(&store));
    assert_eq!(
        make_outcome.recoverability,
        crate::Recoverability::Retrievable
    );
    assert!(!make_outcome.stash_writes.is_empty());
    assert!(make_outcome.output.contains(&fenced_gradle));
}

#[test]
fn trace_overlapping_cmake_layout_keeps_remaining_rows_out_of_stash() {
    let store = InMemoryStore::new();
    let mut input = String::from(
        "$ ninja -C /work/build\n\
         [1/13] cd /work/build && /usr/bin/cmake -E touch generated.h\n\
         thread 'main' panicked at src/main.rs:1:1:\n",
    );
    for i in 2..=13 {
        input.push_str(&format!(
            "[{i}/13] Building C object CMakeFiles/review_target.dir/src/module_unit_file_{i:02}.c.o\n"
        ));
    }
    input.push_str("exit code: 0\n");

    let stripped = input
        .split_inclusive('\n')
        .map(|line| line.trim_end_matches('\n').to_owned())
        .collect::<Vec<_>>();
    // Line 3 is `[2/13] ...`, absorbed by `rust_trace` from the panic header on line 2.
    assert!(
        trace_regions(&stripped)
            .iter()
            .any(|region| region.contains(&3))
    );

    let outcome =
        BuildLogCompressor.compress_with_recovery(&input, Some(&store), &RecoveryMethod::Shell);
    assert_eq!(outcome.recoverability, crate::Recoverability::Lossless);
    assert!(outcome.stash_writes.is_empty());
    assert_eq!(outcome.output, input);
}

#[test]
fn pnpm_suffix_with_foreign_compacted_header_preserves_prefix_compaction() {
    let store = InMemoryStore::new();
    let input = "\
Progress: resolved 1, reused 1, downloaded 0, added 0\n\
Progress: resolved 5, reused 5, downloaded 0, added 0\n\
Progress: resolved 5, reused 5, downloaded 0, added 5, done\n\
Packages: +5\n\
+++++\n\
Gradle templates (id<TAB>fields; interleave with literals): unavailable\n\
Done in 1.2s using pnpm v9.0.0\n";

    // Prove this input directly exercises the Stage 0 Pnpm exemption rather than failing header match.
    assert!(matches!(
        detect_log(input).route,
        Some(DetectedRoute::Stage1(Stage1Selection::Pnpm))
    ));
    let lines = input.split_inclusive('\n').collect::<Vec<_>>();
    assert!(has_scoped_compacted_header(&lines));

    let outcome = BuildLogCompressor.compress(input, Some(&store));
    assert!(!outcome.output.contains("+++++\n"));
    assert!(
        outcome
            .output
            .contains("Gradle templates (id<TAB>fields; interleave with literals): unavailable\n")
    );
    assert!(outcome.output.contains("Done in 1.2s using pnpm v9.0.0\n"));
}

#[test]
fn clipped_compacted_logs_stay_out_of_stash_fallback() {
    let store = InMemoryStore::new();
    let generic_progress = (1..=16)
        .map(|i| format!("progress: background step {i:02} completed cleanly during build\n"))
        .collect::<String>();
    let make_progress = (1..=16)
        .map(|i| format!("cc -O2 -Wall -Iinclude -c src/item_{i:02}.c -o build/item_{i:02}.o\n"))
        .collect::<String>();

    let gradle_base = format!(
        "$ ./gradlew build\n\
         > Configure project :app\n\
         > Task :app:compileJava UP-TO-DATE\n\
         BUILD SUCCESSFUL in 1s\n\
         1 actionable task: 1 executed\n\
         {generic_progress}\
         exit code: 0\n"
    );
    let gradle_clipped_block = format!(
        "{}{{\
            \"literals\":[[\"> Task :\",\" UP-TO-DATE\"]],\"rows\":20\
         }}\n\
         0\tcompileJava\n\
         0\tclasses\n",
        gradle::HEADER
    );
    let gradle_clipped = format!(
        "$ ./gradlew build\n\
         > Configure project :app\n\
         {gradle_clipped_block}\
         {generic_progress}\
         exit code: 0\n"
    );

    let ctest_base = format!(
        "$ ctest\n\
         Test project /work/build\n\
          1/1 Test  #1: solo .............................   Passed    0.00 sec\n\
         {generic_progress}\
         exit code: 0\n"
    );
    let ctest_clipped_block = format!(
        "{}{{\
            \"rows\":20,\"total\":\"20\",\"start_indent\":6,\"idx_width\":2,\"id_width\":2,\"col\":40,\"prefixes\":[]\
         }}\n\
         P\t1\t\ta\t0.01\n\
         P\t2\t\tb\t0.01\n",
        ctest::HEADER
    );
    let ctest_clipped = format!(
        "$ ctest\n\
         Test project /work/build\n\
         {ctest_clipped_block}\
         {generic_progress}\
         exit code: 0\n"
    );

    let cmake_base = format!(
        "$ ninja -C /work/build\n\
         ninja: no work to do.\n\
         {make_progress}\
         exit code: 0\n"
    );
    let cmake_clipped_block = format!(
        "{}{{\
            \"literals\":[[\"[\",\"/20] Building CXX object \",\".cpp.o\"]],\"rows\":20\
         }}\n\
         0\t1\ta\n\
         0\t2\tb\n",
        build_steps::CMAKE_HEADER
    );
    let compile_clipped_block = format!(
        "{}{{\
            \"literals\":[[\"[\",\"/20] /usr/bin/cc -c \",\".c -o \",\".o\"]],\"rows\":20\
         }}\n\
         0\t1\ta\ta\n\
         0\t2\tb\tb\n",
        build_steps::COMPILE_HEADER
    );
    let cmake_clipped = format!(
        "$ ninja -C /work/build\n\
         ninja: no work to do.\n\
         {make_progress}\
         {cmake_clipped_block}"
    );
    let cmake_head_clipped = format!(
        "$ ninja -C /work/build\n\
         {cmake_clipped_block}\
         {make_progress}"
    );
    let compile_head_clipped = format!(
        "$ ninja -C /work/build\n\
         {compile_clipped_block}\
         {make_progress}"
    );

    for (base, clipped) in [
        (gradle_base, gradle_clipped),
        (ctest_base, ctest_clipped),
        (cmake_base.clone(), cmake_clipped),
        (cmake_base.clone(), cmake_head_clipped),
        (cmake_base, compile_head_clipped),
    ] {
        let base_out =
            BuildLogCompressor.compress_with_recovery(&base, Some(&store), &RecoveryMethod::Shell);
        assert_eq!(base_out.recoverability, crate::Recoverability::Retrievable);
        assert!(!base_out.stash_writes.is_empty());

        assert!(matches!(
            detect_log(&clipped).route,
            Some(DetectedRoute::Stage1(_))
        ));
        let clipped_out = BuildLogCompressor.compress_with_recovery(
            &clipped,
            Some(&store),
            &RecoveryMethod::Shell,
        );
        assert_eq!(clipped_out.output, clipped);
        assert_eq!(clipped_out.recoverability, crate::Recoverability::Lossless);
        assert!(clipped_out.stash_writes.is_empty());
    }

    let make_only = format!(
        "make: Entering directory '/work/make'\n\
         {make_progress}\
         make: Leaving directory '/work/make'\n"
    );
    let make_only_out =
        BuildLogCompressor.compress_with_recovery(&make_only, Some(&store), &RecoveryMethod::Shell);
    assert_eq!(
        make_only_out.recoverability,
        crate::Recoverability::Retrievable
    );
    assert!(!make_only_out.stash_writes.is_empty());

    for clipped_block in [
        gradle_clipped_block,
        ctest_clipped_block,
        cmake_clipped_block,
        compile_clipped_block,
    ] {
        let scored_clipped = format!(
            "make: Entering directory '/work/make'\n\
             {make_progress}\
             {clipped_block}"
        );
        assert!(matches!(
            detect_log(&scored_clipped).route,
            Some(DetectedRoute::Scored(BuildLogFormat::Make))
        ));
        let outcome = BuildLogCompressor.compress_with_recovery(
            &scored_clipped,
            Some(&store),
            &RecoveryMethod::Shell,
        );
        assert_eq!(outcome.output, scored_clipped);
        assert_eq!(outcome.recoverability, crate::Recoverability::Lossless);
        assert!(outcome.stash_writes.is_empty());
    }
}

#[test]
fn unsampled_middle_build_step_header_keeps_residual_out_of_stash() {
    let store = InMemoryStore::new();
    let trailing = format!(
        "{}exit code: 0\n",
        (1..=16)
            .map(|i| format!("progress: downloading artifact_{i:02}.tar.gz from mirror\n"))
            .collect::<String>()
    );
    let ninja_row =
        |i: usize| format!("[{i}/230] Building C object CMakeFiles/app.dir/unit_{i:03}.c.o\n");

    let control = format!(
        "$ ninja -C /work/build\n{}{trailing}",
        (1..=230).map(ninja_row).collect::<String>()
    );
    assert!(matches!(
        detect_log(&control).route,
        Some(DetectedRoute::Stage1(Stage1Selection::BuildSteps))
    ));
    let control_out =
        BuildLogCompressor.compress_with_recovery(&control, Some(&store), &RecoveryMethod::Shell);
    assert!(control_out.output.contains(build_steps::CMAKE_HEADER));
    assert_eq!(
        control_out.recoverability,
        crate::Recoverability::Retrievable
    );
    assert!(!control_out.stash_writes.is_empty());

    let compile_compacted = format!(
        "{}{{\
            \"literals\":[[\"[\",\"/20] /usr/bin/cc -c \",\".c -o \",\".o\"]],\"rows\":2\
         }}\n\
         0\t1\ta\ta\n\
         0\t2\tb\tb\n",
        build_steps::COMPILE_HEADER
    );
    let cmake_compacted = format!(
        "{}{{\
            \"literals\":[[\"[\",\"/20] Building CXX object \",\".cpp.o\"]],\"rows\":2\
         }}\n\
         0\t1\ta\n\
         0\t2\tb\n",
        build_steps::CMAKE_HEADER
    );
    let compile_row = |i: usize| {
        format!(
            "[{i}/230] /usr/bin/cc -O3 -I/work/include -MD -MT CMakeFiles/app.dir/src/f_{i:03}.c.o -MF CMakeFiles/app.dir/src/f_{i:03}.c.o.d -o CMakeFiles/app.dir/src/f_{i:03}.c.o -c /work/src/f_{i:03}.c\n"
        )
    };

    for input in [
        format!(
            "$ ninja -C /work/build\n{}{compile_compacted}{}{trailing}",
            (1..=115).map(ninja_row).collect::<String>(),
            (116..=230).map(ninja_row).collect::<String>()
        ),
        format!(
            "$ ninja -v\n{}{cmake_compacted}{}{trailing}",
            (1..=115).map(compile_row).collect::<String>(),
            (116..=230).map(compile_row).collect::<String>()
        ),
    ] {
        assert!(matches!(
            detect_log(&input).route,
            Some(DetectedRoute::Stage1(Stage1Selection::BuildSteps))
        ));
        let out =
            BuildLogCompressor.compress_with_recovery(&input, Some(&store), &RecoveryMethod::Shell);
        assert_eq!(out.output, input);
        assert_eq!(out.recoverability, crate::Recoverability::Lossless);
        assert!(out.stash_writes.is_empty());
    }
}

#[test]
fn gmake_wrappers_preserve_main_cargo_and_generic_routing() {
    let compiling = (0..40)
        .map(|i| {
            format!("   Compiling crate-{i:02} v0.1.{i} (/work/workspace/crates/crate-{i:02})\n")
        })
        .collect::<String>();
    let finished = "    Finished `dev` profile [unoptimized + debuginfo] target(s) in 12.34s\n";

    let enter = "gmake: Entering directory '/work/workspace'\n";
    let echo = "$ cargo build --workspace\n";
    let leave = "gmake: Leaving directory '/work/workspace'\n";

    let wrapped_with_summary = format!("{enter}{echo}{compiling}{finished}{leave}");
    assert!(matches!(
        detect_log(&wrapped_with_summary).route,
        Some(DetectedRoute::Scored(BuildLogFormat::Cargo))
    ));
    let store = InMemoryStore::new();
    let outcome = BuildLogCompressor.compress_with_recovery(
        &wrapped_with_summary,
        Some(&store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(outcome.recoverability, crate::Recoverability::Retrievable);
    assert_eq!(outcome.stash_writes.len(), 1);
    assert!(outcome.output.len() < wrapped_with_summary.len());
    assert!(outcome.output.contains(enter));
    assert!(outcome.output.contains(finished));
    assert!(outcome.output.contains(leave));
    let payload = store
        .retrieve(&outcome.stash_writes[0].key)
        .unwrap()
        .unwrap();
    assert_eq!(
        replace_marker_line(&outcome.output, &outcome.stash_writes[0].key, &payload),
        wrapped_with_summary
    );

    let wrapped_weak_only = format!("{enter}{echo}{compiling}{leave}");
    assert!(matches!(
        detect_log(&wrapped_weak_only).route,
        Some(DetectedRoute::Scored(BuildLogFormat::Cargo))
    ));

    let make_failure = "gmake: *** [Makefile:12: build] Error 101\n";
    let rust_error = "error: could not compile `crate-39` (lib) due to 1 previous error\n";
    let wrapped_with_failure = format!("{enter}{echo}{compiling}{rust_error}{make_failure}{leave}");
    assert!(matches!(
        detect_log(&wrapped_with_failure).route,
        Some(DetectedRoute::Scored(BuildLogFormat::Cargo))
    ));
    let fail_store = InMemoryStore::new();
    let fail_outcome = BuildLogCompressor.compress_with_recovery(
        &wrapped_with_failure,
        Some(&fail_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(
        fail_outcome.recoverability,
        crate::Recoverability::Retrievable
    );
    assert_eq!(fail_outcome.stash_writes.len(), 1);
    assert!(fail_outcome.output.len() < wrapped_with_failure.len());
    assert!(fail_outcome.output.contains(rust_error));
    assert!(fail_outcome.output.contains(make_failure));
    let fail_payload = fail_store
        .retrieve(&fail_outcome.stash_writes[0].key)
        .unwrap()
        .unwrap();
    assert_eq!(
        replace_marker_line(
            &fail_outcome.output,
            &fail_outcome.stash_writes[0].key,
            &fail_payload,
        ),
        wrapped_with_failure
    );

    let probe_cc = "cc -c probe.c -o probe.o\n";
    let plain_rust_error = "error: compile_error! triggered\n";
    let wrapped_cc_cargo_failure =
        format!("{enter}{probe_cc}{echo}{compiling}{plain_rust_error}{make_failure}{leave}");
    assert!(matches!(
        detect_log(&wrapped_cc_cargo_failure).route,
        Some(DetectedRoute::Scored(BuildLogFormat::Cargo))
    ));
    let cc_cargo_store = InMemoryStore::new();
    let cc_cargo_out = BuildLogCompressor.compress_with_recovery(
        &wrapped_cc_cargo_failure,
        Some(&cc_cargo_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(
        cc_cargo_out.recoverability,
        crate::Recoverability::Retrievable
    );
    assert_eq!(cc_cargo_out.stash_writes.len(), 1);
    assert!(cc_cargo_out.output.len() < wrapped_cc_cargo_failure.len());
    assert!(cc_cargo_out.output.contains(probe_cc));
    assert!(cc_cargo_out.output.contains(plain_rust_error));
    assert!(cc_cargo_out.output.contains(make_failure));
    let cc_cargo_payload = cc_cargo_store
        .retrieve(&cc_cargo_out.stash_writes[0].key)
        .unwrap()
        .unwrap();
    assert_eq!(
        replace_marker_line(
            &cc_cargo_out.output,
            &cc_cargo_out.stash_writes[0].key,
            &cc_cargo_payload,
        ),
        wrapped_cc_cargo_failure
    );

    let generic_progress = (1..=40)
        .map(|i| format!("Processing package asset {i} of 40 for the release archive\n"))
        .collect::<String>();
    let gmake_generic = format!("$ gmake -w package\n{enter}{generic_progress}{leave}");
    assert!(matches!(
        detect_log(&gmake_generic).route,
        Some(DetectedRoute::Scored(BuildLogFormat::Generic))
    ));
    let generic_store = InMemoryStore::new();
    let generic_out = BuildLogCompressor.compress_with_recovery(
        &gmake_generic,
        Some(&generic_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(
        generic_out.recoverability,
        crate::Recoverability::Retrievable
    );
    assert_eq!(generic_out.stash_writes.len(), 1);
    assert!(generic_out.output.len() < gmake_generic.len());
    assert!(
        generic_out
            .output
            .contains("repeated progress lines omitted.")
    );
    assert!(generic_out.output.contains(enter));
    assert!(generic_out.output.contains(leave));
    let generic_payload = generic_store
        .retrieve(&generic_out.stash_writes[0].key)
        .unwrap()
        .unwrap();
    assert_eq!(
        replace_marker_line(
            &generic_out.output,
            &generic_out.stash_writes[0].key,
            &generic_payload,
        ),
        gmake_generic
    );

    let ninja_prefix = (1..=8)
        .map(|i| format!("[{i}/8] Building C object CMakeFiles/app.dir/unit_{i:02}.c.o\n"))
        .collect::<String>();
    let mixed_residual_generic = format!(
        "$ ninja -C /work/build\n{ninja_prefix}{enter}{generic_progress}{leave}exit code: 0\n"
    );
    assert!(matches!(
        detect_log(&mixed_residual_generic).route,
        Some(DetectedRoute::Stage1(Stage1Selection::BuildSteps))
    ));
    let residual_store = InMemoryStore::new();
    let residual_out = BuildLogCompressor.compress_with_recovery(
        &mixed_residual_generic,
        Some(&residual_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(
        residual_out.recoverability,
        crate::Recoverability::Retrievable
    );
    assert_eq!(residual_out.stash_writes.len(), 1);
    assert!(residual_out.output.contains(build_steps::CMAKE_HEADER));
    assert!(
        residual_out
            .output
            .contains("repeated progress lines omitted.")
    );
    assert!(residual_out.output.contains(enter));
    assert!(residual_out.output.contains(leave));
    let residual_payload = residual_store
        .retrieve(&residual_out.stash_writes[0].key)
        .unwrap()
        .unwrap();
    let residual_restored = replace_marker_line(
        &residual_out.output,
        &residual_out.stash_writes[0].key,
        &residual_payload,
    );
    assert_eq!(
        oracle::decode(&residual_restored, build_steps::CMAKE_HEADER),
        mixed_residual_generic
    );

    let standalone_make_failure = "\
make: Entering directory '/work'\n\
make: *** [Makefile:12: all] Error 2\n\
make: Leaving directory '/work'\n";
    assert!(matches!(
        detect_log(standalone_make_failure).route,
        Some(DetectedRoute::Scored(BuildLogFormat::Make))
    ));

    let make_compile_failure = concat!(
        "gmake: Entering directory '/work/c_app'\n",
        "cc -O2 -Wall -c src/a.c -o build/a.o\n",
        "src/a.c:12:5: error: expected ';'\n",
        "gmake: *** [Makefile:12: build/a.o] Error 1\n",
        "gmake: Leaving directory '/work/c_app'\n",
    );
    assert!(matches!(
        detect_log(make_compile_failure).route,
        Some(DetectedRoute::Scored(BuildLogFormat::Make))
    ));

    let make_compile = format!(
        "gmake: Entering directory '/work/c_app'\n{}gmake: Leaving directory '/work/c_app'\n",
        (0..16)
            .map(|i| format!("cc -O2 -Wall -c src/unit_{i:02}.c -o build/unit_{i:02}.o\n"))
            .collect::<String>()
    );
    assert!(matches!(
        detect_log(&make_compile).route,
        Some(DetectedRoute::Scored(BuildLogFormat::Make))
    ));
    let make_store = InMemoryStore::new();
    let make_out = BuildLogCompressor.compress_with_recovery(
        &make_compile,
        Some(&make_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(make_out.recoverability, crate::Recoverability::Retrievable);
    assert_eq!(make_out.stash_writes.len(), 1);
}
