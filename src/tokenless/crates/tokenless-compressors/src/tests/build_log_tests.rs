use std::sync::Arc;

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
            BuildLogOperation::ProgressReduction,
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

    assert_eq!(outcome.operations, [BuildLogOperation::ProgressReduction]);
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

    assert_eq!(outcome.operations, [BuildLogOperation::ProgressReduction]);
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

    assert_eq!(outcome.operations, [BuildLogOperation::ProgressReduction]);
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
fn locally_unprofitable_gap_stays_verbatim() {
    let input = "$ cargo build\nCompiling a\nCompiling b\nCompiling c\nCompiling d\nCompiling e\nCompiling f\nCompiling g\nCompiling h\nCompiling i\n";
    let store = InMemoryStore::new();
    let outcome = BuildLogCompressor.compress(input, Some(&store));
    assert_eq!(outcome.output, input);
    assert!(outcome.operations.is_empty());
    assert_eq!(store.len(), 0);
}

#[test]
fn ninth_profitable_gap_abandons_reduction_before_stashing() {
    let mut input = "$ cargo build\n".to_owned();
    for group in 0..9 {
        for index in 0..12 {
            input.push_str(&format!(
                "Compiling group-{group}-package-{index} with a long repeated compilation description\n"
            ));
        }
        input.push_str(&format!("phase boundary {group}\n"));
    }
    let store = InMemoryStore::new();
    let outcome = BuildLogCompressor.compress(&input, Some(&store));
    assert_eq!(outcome.output, input);
    assert!(outcome.operations.is_empty());
    assert_eq!(store.len(), 0);
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
    assert_eq!(outcome.operations, [BuildLogOperation::ProgressReduction]);
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
