use super::super::scope::MAX_LINE_BYTES;
use super::super::{BuildLogCompressor, BuildLogOperation, sample_lines};
use super::*;
use crate::Recoverability;
use tokenless_ccr::{InMemoryStore, RecoveryMethod, StashStore};

#[path = "../../../tests/common/ctest_oracle.rs"]
mod oracle;

use oracle::{decode, strip_sgr};

fn format_test_ref(num: usize, width: usize) -> String {
    let tag = format!("#{num}");
    format!("{tag:>width$}")
}

fn synthetic_serial_ctest(count: usize) -> String {
    let total_str = count.to_string();
    let width = total_str.len().max(2);
    let mut output = String::from("Test project /work/build-ninja\n");
    for idx in 1..=count {
        let name = format!("suite/sub/test_case_{idx:02}");
        output.push_str(&format!("      Start {idx:>width$}: {name}\n"));
        let ref_tag = format_test_ref(idx, width + 1);
        let head = format!("{idx:>width$}/{total_str} Test {ref_tag}: {name} ");
        let dots_len = 79usize.saturating_sub(head.len()).max(3);
        output.push_str(&format!(
            "{head}{}   Passed    0.01 sec\n",
            ".".repeat(dots_len)
        ));
    }
    output.push_str(&format!(
        "\n100% tests passed, 0 tests failed out of {count}\n\nTotal Test time (real) =   {:.2} sec\n",
        count as f64 * 0.01
    ));
    output
}

fn synthetic_parallel_ctest(count: usize) -> String {
    let total_str = count.to_string();
    let width = total_str.len().max(2);
    let mut output = String::from("Test project /work/build-ninja\n");
    for pair in (1..=count).step_by(2) {
        let a = pair;
        let b = pair + 1;
        let name_a = format!("unit::group/case_{a:02}");
        output.push_str(&format!("      Start {a:>width$}: {name_a}\n"));
        if b <= count {
            let name_b = format!("unit::group/case_{b:02}");
            output.push_str(&format!("      Start {b:>width$}: {name_b}\n"));
            let ref_b = format_test_ref(b, width + 1);
            let head_b = format!("{a:>width$}/{total_str} Test {ref_b}: {name_b} ");
            let dots_b = 75usize.saturating_sub(head_b.len()).max(3);
            output.push_str(&format!(
                "{head_b}{}   Passed    0.02 sec\n",
                ".".repeat(dots_b)
            ));
            let ref_a = format_test_ref(a, width + 1);
            let head_a = format!("{b:>width$}/{total_str} Test {ref_a}: {name_a} ");
            let dots_a = 75usize.saturating_sub(head_a.len()).max(3);
            output.push_str(&format!(
                "{head_a}{}   Passed    0.03 sec\n",
                ".".repeat(dots_a)
            ));
        } else {
            let ref_a = format_test_ref(a, width + 1);
            let head_a = format!("{a:>width$}/{total_str} Test {ref_a}: {name_a} ");
            let dots_a = 75usize.saturating_sub(head_a.len()).max(3);
            output.push_str(&format!(
                "{head_a}{}   Passed    0.01 sec\n",
                ".".repeat(dots_a)
            ));
        }
    }
    output.push_str(&format!(
        "\n100% tests passed, 0 tests failed out of {count}\n\nTotal Test time (real) =   0.20 sec\n"
    ));
    output
}

fn compress_lossless(input: &str) -> super::super::BuildLogOutcome {
    BuildLogCompressor.compress_with_recovery(input, None, &RecoveryMethod::None)
}

#[test]
fn single_lines_prose_fences_and_crlf_do_not_trigger_ctest_detection() {
    for negative in [
        "Test project /work/build\n",
        "      Start  1: solo\n",
        " 1/1 Test  #1: solo .............................   Passed    0.00 sec\n",
        " 1/1 Test  #1: solo .............................***Failed    0.01 sec\n",
        "100% tests passed, 0 tests failed out of 1\n",
        "No tests were found!!!\n",
        "let example = \"1/2 Test #1: foo ... Passed 0.01 sec\";\n",
        "Test project /work/build\nRandom non-ctest text\n",
        "```text\nTest project /work/build\n 1/2 Test #1: a ...   Passed    0.01 sec\n 2/2 Test #2: b ...   Passed    0.01 sec\n```\n",
        "~~~text\nTest project /work/build\n 1/2 Test #1: a ...   Passed    0.01 sec\n 2/2 Test #2: b ...   Passed    0.01 sec\n~~~\n",
        "+++ b/ctest.log\n 1/2 Test #1: a ...   Passed    0.01 sec\n 2/2 Test #2: b ...   Passed    0.01 sec\n",
        "Exit code: 0 means ok\nTest project /work/build\n100% tests passed, 0 tests failed out of 1\n",
    ] {
        let sample = sample_lines(negative);
        assert!(!detect(&sample), "false positive detect on: {negative}");
        assert!(
            !BuildLogCompressor::detect(negative),
            "false positive BuildLogCompressor::detect on: {negative}"
        );
        let outcome = compress_lossless(negative);
        assert_eq!(outcome.output, negative);
        assert!(outcome.stash_writes.is_empty());
        assert_eq!(outcome.recoverability, Recoverability::Lossless);
    }

    let valid = synthetic_serial_ctest(8);
    let crlf = valid.replace('\n', "\r\n");
    assert!(!detect(&sample_lines(&crlf)));
    assert!(!BuildLogCompressor::detect(&crlf));
    assert_eq!(compress_lossless(&crlf).output, crlf);

    let head_padding = format!("{}{valid}", "unrelated status line\n".repeat(101));
    assert!(!detect(&sample_lines(&head_padding)));
    assert!(!BuildLogCompressor::detect(&head_padding));
    assert_eq!(compress_lossless(&head_padding).output, head_padding);

    let oversized_head = format!("{}\n{valid}", "x".repeat(33 * 1024));
    assert!(!detect(&sample_lines(&oversized_head)));
    assert!(!BuildLogCompressor::detect(&oversized_head));
    assert_eq!(compress_lossless(&oversized_head).output, oversized_head);

    let unclosed_fence_gap = format!("```text\n{}\n{valid}", "ignored example line\n".repeat(120));
    assert!(!detect(&sample_lines(&unclosed_fence_gap)));
    assert!(!BuildLogCompressor::detect(&unclosed_fence_gap));
}

#[test]
fn valid_preambles_preserve_ctest_detection_and_stay_verbatim() {
    let body = synthetic_serial_ctest(12);
    let preambles = [
        "$ ctest --test-dir /work/build --output-on-failure\n",
        "Exit code: 0\n",
        "exit status: 0\n",
        "+ ctest --output-on-failure\n",
        "++ CTEST_OUTPUT_ON_FAILURE=1 /usr/bin/ctest -j 2\n",
        "Internal ctest changing into directory: /work/build\n",
        "Cannot find file: /work/build/DartConfiguration.tcl\n",
        "UpdateCTestConfiguration  from :/work/build/DartConfiguration.tcl\nParse Config file:/work/build/DartConfiguration.tcl\n",
    ];
    for preamble in preambles {
        let input = format!("{preamble}{body}");
        assert!(
            BuildLogCompressor::detect(&input),
            "failed detect with preamble: {preamble}"
        );
        let outcome = compress_lossless(&input);
        assert!(
            outcome.output.starts_with(preamble),
            "preamble mutated: {preamble}"
        );
        assert_eq!(decode(&outcome.output), input);
        assert!(outcome.operations.contains(&BuildLogOperation::Reduction));
        assert!(
            tokenless_protocol::estimate_tokens(&outcome.output)
                < tokenless_protocol::estimate_tokens(&input)
        );
    }
}

#[test]
fn serial_and_parallel_statuses_and_special_names_round_trip() {
    let parallel = synthetic_parallel_ctest(14);
    let par_outcome = compress_lossless(&parallel);
    assert!(par_outcome.output.contains(HEADER));
    assert_eq!(decode(&par_outcome.output), parallel);
    assert_eq!(
        compress_lossless(&par_outcome.output).output,
        par_outcome.output
    );
    assert!(
        tokenless_protocol::estimate_tokens(&par_outcome.output)
            < tokenless_protocol::estimate_tokens(&parallel)
    );

    let tricky = concat!(
        "Internal ctest changing into directory: /work/b\n",
        "Test project /work/b\n",
        "    Start  1: @smoke\n",
        "1/8 Test  #1: @smoke .........................................................   Passed    0.00 sec\n",
        "    Start  2: @@double\n",
        "2/8 Test  #2: @@double .......................................................***Skipped   0.00 sec\n",
        "    Start  3: @0:fake_ref\n",
        "3/8 Test  #3: @0:fake_ref ....................................................***Not Run (Disabled)   0.00 sec\n",
        "    Start  4: scope/sub/@leaf_a\n",
        "4/8 Test  #4: scope/sub/@leaf_a ..............................................   Passed    0.01 sec\n",
        "    Start  5: scope/sub/@leaf_b\n",
        "5/8 Test  #5: scope/sub/@leaf_b ..............................................   Passed    123.45 sec\n",
        "    Start  6: suite/sub/very_long_test_name_that_exceeds_default_column_alignment_12345\n",
        "6/8 Test  #6: suite/sub/very_long_test_name_that_exceeds_default_column_alignment_12345 ...   Passed    0.00 sec\n",
        "    Start 16: sparse::suite::case_16\n",
        "7/8 Test #16: sparse::suite::case_16 .........................................   Passed    0.02 sec\n",
        "    Start  1: @smoke\n",
        "8/8 Test  #1: @smoke_repeat ..................................................   Passed  0.03 sec\n",
        "\n",
        "100% tests passed, 0 tests failed out of 7\n",
        "\n",
        "Total Test time (real) = 123.51 sec\n",
    );
    let outcome = compress_lossless(tricky);
    assert!(outcome.output.contains(HEADER));
    assert_eq!(decode(&outcome.output), tricky);
    assert_eq!(compress_lossless(&outcome.output).output, outcome.output);
    assert!(
        tokenless_protocol::estimate_tokens(&outcome.output)
            < tokenless_protocol::estimate_tokens(tricky)
    );
}

#[test]
fn failed_timeout_exception_and_trailing_starts_stay_verbatim() {
    let input = concat!(
        "Test project /work/build\n",
        "      Start  1: suite/pass_1\n",
        " 1/7 Test  #1: suite/pass_1 .....................................   Passed    0.01 sec\n",
        "      Start  2: suite/pass_2\n",
        " 2/7 Test  #2: suite/pass_2 .....................................   Passed    0.01 sec\n",
        "      Start  3: suite/pass_3\n",
        " 3/7 Test  #3: suite/pass_3 .....................................   Passed    0.01 sec\n",
        "      Start  4: suite/broken_fail\n",
        " 4/7 Test  #4: suite/broken_fail ................................***Failed    0.02 sec\n",
        "expected: 42\n",
        "actual:   41\n",
        "      Start  5: suite/timeout_case\n",
        " 5/7 Test  #5: suite/timeout_case ...............................***Timeout   1.00 sec\n",
        "      Start  6: suite/segfault_case\n",
        " 6/7 Test  #6: suite/segfault_case ..............................***Exception: SegFault  0.02 sec\n",
        "      Start  7: suite/missing_exec\n",
        "Could not find executable missing-cmd\n",
        " 7/7 Test  #7: suite/missing_exec ...............................***Not Run   0.00 sec\n",
        "\n",
        "43% tests passed, 4 tests failed out of 7\n",
        "\n",
        "Total Test time (real) =   1.06 sec\n",
        "\n",
        "The following tests FAILED:\n",
        "\t  4 - suite/broken_fail (Failed)\n",
        "\t  5 - suite/timeout_case (Timeout)\n",
        "\t  6 - suite/segfault_case (SEGFAULT)\n",
        "\t  7 - suite/missing_exec (Not Run)\n",
        "Errors while running CTest\n",
    );
    let outcome = compress_lossless(input);
    assert_eq!(decode(&outcome.output), input);
    for expected_verbatim in [
        "      Start  4: suite/broken_fail\n",
        " 4/7 Test  #4: suite/broken_fail ................................***Failed    0.02 sec\n",
        "expected: 42\n",
        "      Start  5: suite/timeout_case\n",
        " 5/7 Test  #5: suite/timeout_case ...............................***Timeout   1.00 sec\n",
        "      Start  6: suite/segfault_case\n",
        " 6/7 Test  #6: suite/segfault_case ..............................***Exception: SegFault  0.02 sec\n",
        "      Start  7: suite/missing_exec\n",
        "\t  4 - suite/broken_fail (Failed)\n",
        "Errors while running CTest\n",
    ] {
        assert!(
            outcome.output.contains(expected_verbatim),
            "missing verbatim diagnostic: {expected_verbatim:?}"
        );
    }
}

#[test]
fn parallel_unpaired_failure_starts_stay_verbatim_anywhere_in_run() {
    // Slow failing test #1 starts first, then 12 fast tests pass before #1 fails.
    let mut lead_fail = String::from("Test project /work/build\n      Start  1: suite/slow_fail\n");
    for id in 2..=13 {
        let ord = id - 1;
        let tag = format_test_ref(id, 3);
        let name = format!("suite/fast_{ord:02}");
        lead_fail.push_str(&format!("      Start {id:2}: {name}\n"));
        lead_fail.push_str(&format!(
            "{ord:2}/13 Test {tag}: {name} .................................   Passed    0.01 sec\n"
        ));
    }
    lead_fail.push_str(
        "13/13 Test  #1: suite/slow_fail ................................***Failed    1.25 sec\n\
         assertion failed: x == y\n\n\
         92% tests passed, 1 tests failed out of 13\n",
    );

    let first = compress_lossless(&lead_fail);
    assert!(first.output.contains(HEADER));
    assert_eq!(decode(&first.output), lead_fail);
    assert_eq!(compress_lossless(&first.output).output, first.output);
    assert!(first.output.contains("      Start  1: suite/slow_fail\n"));
    assert!(first.output.contains("***Failed"));
    assert!(!first.output.lines().any(|line| {
        line == "s\t1" || (line.starts_with("S\t") && line.contains("suite/slow_fail"))
    }));

    // Mid-run slow timeout (#7) starts while an earlier passing test (#6) is still in flight.
    let mut mid_timeout = String::from("Test project /work/build\n");
    for id in 1..=5 {
        let tag = format_test_ref(id, 3);
        let name = format!("suite/fast_{id:02}");
        mid_timeout.push_str(&format!("      Start {id:2}: {name}\n"));
        mid_timeout.push_str(&format!(
            "{id:2}/13 Test {tag}: {name} .................................   Passed    0.01 sec\n"
        ));
    }
    mid_timeout.push_str("      Start  6: suite/in_flight\n");
    mid_timeout.push_str("      Start  7: suite/mid_timeout\n");
    mid_timeout.push_str(
        " 6/13 Test  #6: suite/in_flight ................................   Passed    0.03 sec\n",
    );
    for (ord, id) in (7..=12).zip(8..=13) {
        let tag = format_test_ref(id, 3);
        let name = format!("suite/tail_{id:02}");
        mid_timeout.push_str(&format!("      Start {id:2}: {name}\n"));
        mid_timeout.push_str(&format!(
            "{ord:2}/13 Test {tag}: {name} .................................   Passed    0.01 sec\n"
        ));
    }
    mid_timeout.push_str(
        "13/13 Test  #7: suite/mid_timeout ..............................***Timeout   2.00 sec\n\
         92% tests passed, 1 tests failed out of 13\n",
    );

    let mid = compress_lossless(&mid_timeout);
    assert!(mid.output.contains(HEADER));
    assert_eq!(decode(&mid.output), mid_timeout);
    assert_eq!(compress_lossless(&mid.output).output, mid.output);
    // `#6` completed inside the contiguous region, so only unpaired `#7` keeps its literal Start.
    assert!(!mid.output.contains("      Start  6: suite/in_flight\n"));
    assert!(mid.output.contains("      Start  7: suite/mid_timeout\n"));
    assert!(!mid.output.lines().any(|line| {
        line == "s\t7" || (line.starts_with("S\t") && line.contains("suite/mid_timeout"))
    }));
}

#[test]
fn boundary_mutations_close_scope_and_round_trip_at_every_line() {
    let mut base_lines = Vec::new();
    for idx in 1..=16 {
        let name = format!("pkg/sub/test_{idx:02}");
        base_lines.push(format!("      Start {idx:2}: {name}\n"));
        let ref_tag = format_test_ref(idx, 3);
        base_lines.push(format!(
            "{idx:2}/16 Test {ref_tag}: {name} .................................   Passed    0.01 sec\n"
        ));
    }
    let injections = [
        " 99/99 Test #99: pkg/sub/injected_fail ...........................***Failed    0.02 sec\n",
        " 99/99 Test #99: pkg/sub/injected_timeout ........................***Timeout   1.00 sec\n",
        " 99/99 Test #99: pkg/sub/injected_segv ...........................***Exception: SegFault  0.01 sec\n",
        " 99/99 Test #99: pkg/sub/injected_not_run ........................***Not Run   0.00 sec\n",
        "Assertion failed at src/foo.cc:42\n",
        "progress 50%\rprogress 100%\n",
        "  \x1b[0K\x1b[32m14 passing\x1b[0m\n",
        "\t  99 - pkg/sub/injected_fail (Failed)\n",
        "```text\n 1/2 Test #1: quoted ...   Passed    0.01 sec\n```\n",
        "unclassified stdout notice\n",
    ];

    let mut mutation_count = 0usize;
    for pos in 0..=base_lines.len() {
        for injection in injections {
            let mut mutated = base_lines.clone();
            mutated.insert(pos, injection.to_owned());
            let raw = mutated.concat();
            let cleaned = strip_sgr(&raw);
            let outcome = compress_lossless(&raw);
            assert_eq!(
                decode(&outcome.output),
                cleaned,
                "pos={pos}, inj={injection:?}"
            );
            let injected_clean = strip_sgr(injection);
            assert!(
                outcome.output.contains(&injected_clean),
                "injected line lost at pos={pos}: {injected_clean:?}"
            );
            assert!(outcome.stash_writes.is_empty());
            assert_eq!(outcome.recoverability, Recoverability::Lossless);
            let again = compress_lossless(&outcome.output);
            assert_eq!(again.output, outcome.output, "not idempotent at pos={pos}");
            mutation_count += 1;
        }
    }
    assert!(mutation_count >= 300);
}

#[test]
fn truncated_tail_and_oversized_or_many_chunks_respect_bounds() {
    let valid = synthetic_serial_ctest(12);
    let truncated = valid.trim_end_matches('\n');
    let outcome = compress_lossless(truncated);
    assert_eq!(decode(&outcome.output), truncated);
    assert!(
        outcome
            .output
            .ends_with(truncated.rsplit('\n').next().unwrap())
    );

    let long_start = format!("      Start 99: {}\n", "a".repeat(MAX_LINE_BYTES));
    let half = synthetic_serial_ctest(6);
    let half_body = half
        .strip_prefix("Test project /work/build-ninja\n")
        .unwrap()
        .split("\n100% tests passed")
        .next()
        .unwrap();
    let half_full = half
        .strip_prefix("Test project /work/build-ninja\n")
        .unwrap();
    let with_long = format!("Test project /work/build-ninja\n{half_body}{long_start}{half_full}");
    let long_outcome = compress_lossless(&with_long);
    assert_eq!(decode(&long_outcome.output), with_long);
    assert!(long_outcome.output.contains(&long_start));
    assert!(long_outcome.output.contains(HEADER));

    // Any single line exceeding MAX_BYTES (64 KiB) suppresses CTest edits across the result.
    let huge_line = format!("diagnostic payload: {}\n", "x".repeat(MAX_BYTES + 1));
    let with_huge = format!("{huge_line}{}", synthetic_serial_ctest(10));
    let huge_lines = with_huge.split_inclusive('\n').collect::<Vec<_>>();
    assert!(plan(&huge_lines).is_empty());
    let huge_store = InMemoryStore::default();
    let huge_outcome = BuildLogCompressor.compress_with_recovery(
        &with_huge,
        Some(&huge_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(huge_outcome.output, with_huge);
    assert!(huge_outcome.stash_writes.is_empty());

    // 140 serial tests = 280 lines (> MAX_ROWS = 256); the split must never cleave a Start+Result pair.
    let large = synthetic_serial_ctest(140);
    let large_lines = large.split_inclusive('\n').collect::<Vec<_>>();
    let large_edits = plan(&large_lines);
    assert_eq!(large_edits.len(), 2);
    for edit in &large_edits {
        assert!(edit.end - edit.start <= MAX_ROWS);
        assert_eq!(
            decode(&edit.replacement),
            large_lines[edit.start..edit.end].concat()
        );
        assert!((edit.end - edit.start).is_multiple_of(2));
    }
    let large_outcome = compress_lossless(&large);
    assert_eq!(decode(&large_outcome.output), large);
    assert!(
        tokenless_protocol::estimate_tokens(&large_outcome.output)
            < tokenless_protocol::estimate_tokens(&large)
    );

    // >64 KiB contiguous CTest block under 256 rows splits cleanly at MAX_BYTES.
    let wide_prefix = "wide/ctest/prefix/".repeat(48);
    let mut wide_log = String::from("Test project /work/build-ninja\n");
    for idx in 1..=80 {
        let name = format!("{wide_prefix}case_{idx:02}");
        wide_log.push_str(&format!("      Start {idx:2}: {name}\n"));
        let ref_tag = format_test_ref(idx, 3);
        wide_log.push_str(&format!(
            "{idx:2}/80 Test {ref_tag}: {name} ...   Passed    0.01 sec\n"
        ));
    }
    wide_log.push_str(
        "\n100% tests passed, 0 tests failed out of 80\nTotal Test time (real) = 0.80 sec\n",
    );
    let wide_lines = wide_log.split_inclusive('\n').collect::<Vec<_>>();
    let wide_edits = plan(&wide_lines);
    assert!(wide_edits.len() >= 2);
    for edit in &wide_edits {
        let orig = wide_lines[edit.start..edit.end].concat();
        assert!(orig.len() <= MAX_BYTES);
        assert_eq!(decode(&edit.replacement), orig);
    }
    let wide_outcome = compress_lossless(&wide_log);
    assert_eq!(decode(&wide_outcome.output), wide_log);

    // Exceeding MAX_EDITS (256) compacts the first 256 blocks and leaves trailing blocks verbatim.
    let mut many_blocks = String::from("Test project /work/build-ninja\n");
    for block in 0..260 {
        for sub in 1..=4 {
            let idx = block * 4 + sub;
            let name = format!("blk{block:03}/sub/case_{sub}");
            many_blocks.push_str(&format!("      Start {idx:4}: {name}\n"));
            let ref_tag = format_test_ref(idx, 5);
            many_blocks.push_str(&format!(
                "{idx:4}/1040 Test {ref_tag}: {name} ...............   Passed    0.01 sec\n"
            ));
        }
        many_blocks.push_str(&format!("custom boundary {block}\n"));
    }
    let many_lines = many_blocks.split_inclusive('\n').collect::<Vec<_>>();
    let many_edits = plan(&many_lines);
    assert_eq!(many_edits.len(), MAX_EDITS);
    let many_outcome = compress_lossless(&many_blocks);
    assert_eq!(decode(&many_outcome.output), many_blocks);
    assert!(many_outcome.output.contains("blk259/sub/case_1"));
    assert_eq!(
        compress_lossless(&many_outcome.output).output,
        many_outcome.output
    );
}

#[test]
fn uneditable_anchored_ctest_retains_generic_recovery_while_ctest_frames_stay_protected() {
    let progress = (1..=12)
        .map(|i| {
            format!("processing custom CTest fixture bundle {i} of 12 inside integration harness\n")
        })
        .collect::<String>();
    let anchored = format!(
        "$ ctest --test-dir /work/build\n\
         Test project /work/build\n\
               Start  1: runningStep\n\
         {progress}\
          1/1 Test  #1: runningStep ......................   Passed    0.05 sec\n\
         \n\
         100% tests passed, 0 tests failed out of 1\n\
         exit code: 0\n"
    );
    assert!(BuildLogCompressor::detect(&anchored));
    assert_eq!(compress_lossless(&anchored).output, anchored);

    let store = InMemoryStore::default();
    let outcome =
        BuildLogCompressor.compress_with_recovery(&anchored, Some(&store), &RecoveryMethod::Shell);
    assert_eq!(outcome.recoverability, Recoverability::Retrievable);
    assert_eq!(outcome.stash_writes.len(), 1);
    let marker = outcome
        .output
        .split_inclusive('\n')
        .find(|line| line.contains("lines omitted."))
        .unwrap();
    let payload = store
        .retrieve(&outcome.stash_writes[0].key)
        .unwrap()
        .unwrap();
    assert_eq!(outcome.output.replacen(marker, &payload, 1), anchored);

    // Failed numbered CTest frames containing `running`/`compiling` must never trigger Generic omission.
    let failed_cases = (1..=12)
        .map(|i| {
            let ref_tag = format_test_ref(i, 3);
            format!(
                " {i:2}/12 Test {ref_tag}: runningSuite ....................***Failed    0.02 sec\n"
            )
        })
        .collect::<String>();
    let failed_anchored =
        format!("$ ctest\nTest project /work/build\n{failed_cases}exit code: 1\n");
    let failed_store = InMemoryStore::default();
    let failed_outcome = BuildLogCompressor.compress_with_recovery(
        &failed_anchored,
        Some(&failed_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(failed_outcome.output, failed_anchored);
    assert!(failed_outcome.stash_writes.is_empty());
}

#[test]
fn fenced_ctest_examples_and_over_4kib_generated_header_keep_guarantees() {
    let example: String = (1..=12)
        .map(|i| {
            let ref_tag = format_test_ref(i, 3);
            format!(
                "      Start {i:2}: example/suite/case_{i:02}\n\
                 {i:2}/12 Test {ref_tag}: example/suite/case_{i:02} ........   Passed    0.01 sec\n"
            )
        })
        .collect();
    let body = synthetic_serial_ctest(6);
    let quoted_header = format!(
        "```text\n{HEADER}{{\"rows\":1,\"total\":\"1\",\"start_indent\":6,\"idx_width\":1,\"id_width\":1,\"col\":40,\"prefixes\":[]}}\nSP\t1\t\tex\t0.01\n```\n"
    );
    let mixed = format!(
        "Test project /work/build-ninja\n{}{quoted_header}```text\n{example}```\n",
        body.strip_prefix("Test project /work/build-ninja\n")
            .unwrap()
    );
    assert!(BuildLogCompressor::detect(&mixed));
    let mixed_outcome = compress_lossless(&mixed);
    assert!(mixed_outcome.output.contains(&quoted_header));
    assert!(mixed_outcome.output.contains(&example));
    assert_eq!(decode(&mixed_outcome.output), mixed);
    assert_eq!(
        compress_lossless(&mixed_outcome.output).output,
        mixed_outcome.output
    );

    // Force >4 KiB generated JSON header inside one <=256-row block via ~60 distinct long namespaces.
    let mut body_log = String::from("Test project /work/build-ninja\n");
    for ns in 0..60 {
        for sub in 1..=2 {
            let idx = ns * 2 + sub;
            let name = format!(
                "long_component_namespace_segment_padding_alpha_beta_gamma_{ns:03}_module/case_{sub}"
            );
            body_log.push_str(&format!("      Start {idx:3}: {name}\n"));
            let ref_tag = format_test_ref(idx, 4);
            body_log.push_str(&format!(
                "{idx:3}/120 Test {ref_tag}: {name} ...   Passed    0.01 sec\n"
            ));
        }
    }
    let trailing_progress = (1..=12)
        .map(|i| {
            format!("processing custom CTest fixture bundle {i} of 12 inside integration harness\n")
        })
        .collect::<String>();
    let input =
        format!("$ ctest --test-dir /work/build\n{body_log}{trailing_progress}exit code: 0\n");

    let store = InMemoryStore::default();
    let first =
        BuildLogCompressor.compress_with_recovery(&input, Some(&store), &RecoveryMethod::Shell);
    let first_header = first
        .output
        .lines()
        .find(|line| line.starts_with(HEADER))
        .unwrap();
    assert!(first_header.len() > MAX_LINE_BYTES);
    assert_eq!(first.recoverability, Recoverability::Lossless);
    assert!(first.stash_writes.is_empty());

    let second = BuildLogCompressor.compress_with_recovery(
        &first.output,
        Some(&store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(second.output, first.output);
    assert!(second.stash_writes.is_empty());
    assert_eq!(store.len(), 0);

    // Keep idempotence when >100 lifecycle lines push the >4 KiB header out of sample_lines().
    let cfg_head = (0..105)
        .map(|i| format!("Test project /work/build/sub_{i:03}\n"))
        .collect::<String>();
    let gapped = format!(
        "$ ctest --test-dir /work/build\n{cfg_head}{body_log}{trailing_progress}exit code: 0\n"
    );
    let gapped_first =
        BuildLogCompressor.compress_with_recovery(&gapped, Some(&store), &RecoveryMethod::Shell);
    assert!(gapped_first.output.contains(HEADER));
    let gapped_second = BuildLogCompressor.compress_with_recovery(
        &gapped_first.output,
        Some(&store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(gapped_second.recoverability, Recoverability::Lossless);
    assert!(gapped_second.stash_writes.is_empty());
    assert_eq!(gapped_second.output, gapped_first.output);
}

fn synthetic_staggered_parallel_ctest(count: usize, jobs: usize, fail_first: bool) -> String {
    let total_str = count.to_string();
    let width = total_str.len().max(2);
    let mut output = String::from("Test project /work/build\n");
    let first_name = |id: usize| {
        if fail_first && id == 1 {
            "pkg/alpha/sub/slow_fail".to_owned()
        } else {
            format!("pkg/alpha/sub/case_{id:04}")
        }
    };

    let mut next_start = 1usize;
    let mut active = Vec::new();
    while active.len() < jobs && next_start <= count {
        let name = first_name(next_start);
        output.push_str(&format!("      Start {next_start:>width$}: {name}\n"));
        active.push(next_start);
        next_start += 1;
    }

    let mut ord = 0usize;
    while !active.is_empty() {
        let pick = if fail_first && active[0] == 1 && active.len() > 1 {
            1
        } else {
            usize::from(active.len() > 1 && ord % 2 == 1)
        };
        let done_id = active.remove(pick);
        ord += 1;
        let name = first_name(done_id);
        let ref_tag = format_test_ref(done_id, width + 1);
        let head = format!("{ord:>width$}/{total_str} Test {ref_tag}: {name} ");
        let dots_len = 79usize.saturating_sub(head.len()).max(3);
        let status = if fail_first && done_id == 1 {
            "***Failed"
        } else {
            "   Passed"
        };
        output.push_str(&format!(
            "{head}{}{status}    0.01 sec\n",
            ".".repeat(dots_len)
        ));
        if next_start <= count {
            let new_name = first_name(next_start);
            output.push_str(&format!("      Start {next_start:>width$}: {new_name}\n"));
            active.push(next_start);
            next_start += 1;
        }
    }

    if fail_first {
        let pct = count.saturating_sub(1).saturating_mul(100) / count.max(1);
        output.push_str(&format!(
            "assertion failed: expected ok\n\n\
             {pct}% tests passed, 1 tests failed out of {count}\n"
        ));
    } else {
        output.push_str(&format!(
            "\n100% tests passed, 0 tests failed out of {count}\nTotal Test time (real) =   1.20 sec\n"
        ));
    }
    output
}

#[test]
fn multi_chunk_parallel_ctest_stays_compact_across_max_rows_boundaries() {
    for jobs in [2usize, 4] {
        let raw = synthetic_staggered_parallel_ctest(400, jobs, false);
        let outcome = compress_lossless(&raw);
        assert_eq!(decode(&outcome.output), raw, "jobs={jobs}");
        assert_eq!(
            compress_lossless(&outcome.output).output,
            outcome.output,
            "jobs={jobs} not idempotent"
        );
        assert!(outcome.stash_writes.is_empty());
        assert!(outcome.output.matches(HEADER).count() >= 4);
        assert!(
            !outcome
                .output
                .lines()
                .any(|line| line.trim_start().starts_with("Start ")),
            "jobs={jobs} left raw Start lines uncompacted"
        );
        let before_tokens = tokenless_protocol::estimate_tokens(&raw);
        let after_tokens = tokenless_protocol::estimate_tokens(&outcome.output);
        assert!(
            after_tokens * 100 < before_tokens * 35,
            "jobs={jobs} expected >65% savings, got {before_tokens} -> {after_tokens}"
        );
    }

    // One leading slow failure stays verbatim while the remaining 399 parallel passes compact.
    let fail_raw = synthetic_staggered_parallel_ctest(400, 2, true);
    let fail_outcome = compress_lossless(&fail_raw);
    assert_eq!(decode(&fail_outcome.output), fail_raw);
    assert_eq!(
        compress_lossless(&fail_outcome.output).output,
        fail_outcome.output
    );
    assert!(
        fail_outcome
            .output
            .contains("      Start   1: pkg/alpha/sub/slow_fail\n")
    );
    assert!(fail_outcome.output.contains("***Failed"));
    assert!(
        !fail_outcome
            .output
            .lines()
            .any(|line| line.trim_start().starts_with("Start ") && !line.contains("slow_fail"))
    );
    let fail_before = tokenless_protocol::estimate_tokens(&fail_raw);
    let fail_after = tokenless_protocol::estimate_tokens(&fail_outcome.output);
    assert!(
        fail_after * 100 < fail_before * 35,
        "expected >65% savings with slow failure, got {fail_before} -> {fail_after}"
    );
}

#[test]
fn mixed_width_and_total_rows_round_trip_through_fallback_r_tag() {
    let mut mixed = String::from("Test project /work/build\n");
    for idx in 1..=5 {
        let name = format!("pkg/sub/case_{idx:02}");
        mixed.push_str(&format!("      Start {idx:2}: {name}\n"));
        let tag = format_test_ref(idx, 3);
        mixed.push_str(&format!(
            "{idx:2}/6 Test {tag}: {name} ............................   Passed    0.01 sec\n"
        ));
    }
    mixed.push_str("      Start  6: pkg/sub/case_06\n");
    mixed.push_str("      Start  7: pkg/sub/extra_case\n");
    mixed.push_str(
        " 6/6 Test  #6: pkg/sub/case_06 ............................   Passed    0.01 sec\n",
    );
    mixed.push_str(
        " 1/1 Test  #7: pkg/sub/extra_case .........................   Passed    0.02 sec\n",
    );
    mixed.push_str("      Start 100: pkg/sub/hundred\n");
    mixed.push_str(
        "100/100 Test #100: pkg/sub/hundred .........................   Passed    0.03 sec\n",
    );
    mixed.push_str("\n100% tests passed, 0 tests failed out of 6\n");

    let outcome = compress_lossless(&mixed);
    assert!(outcome.output.contains("\nR\t"));
    assert!(outcome.output.contains("\ns\t7\n"));
    assert_eq!(decode(&outcome.output), mixed);
    assert_eq!(compress_lossless(&outcome.output).output, outcome.output);
}
