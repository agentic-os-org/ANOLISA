//! Independent reconstruction and boundary checks for CTest structural compaction.

use std::path::PathBuf;

use tokenless_ccr::{InMemoryStore, RecoveryMethod, StashStore};
use tokenless_compressors::{BuildLogCompressor, Recoverability};
use tokenless_protocol::{RecoveryToolName, estimate_tokens};

mod common;
use common::build_log_harness as harness;
use common::ctest_oracle::{self as oracle, decode};

const CTEST_HEADER: &str =
    "CTest templates (kind<TAB>fields; SP/SK/SD=start+result, P/K/D=result, s/S=start, B=blank): ";

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build_logs/ctest")
}

fn check(input: &str, expect_compacted: bool) -> String {
    harness::check_lossless(
        input,
        CTEST_HEADER,
        expect_compacted,
        oracle::strip_sgr,
        decode,
    )
}

#[test]
fn provenance_matches_all_committed_fixtures() {
    harness::verify_provenance_digests(&fixtures_dir(), 6);
}

#[test]
fn committed_fixtures_round_trip_without_stash_and_match_goldens() {
    let dir = fixtures_dir();
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("provenance.json")).unwrap())
            .unwrap();

    for entry in &entries {
        let name = entry["file"].as_str().unwrap();
        let raw = std::fs::read_to_string(dir.join(name)).unwrap();
        let cleaned = oracle::strip_sgr(&raw);
        let exit_code = entry["capture"]["exit_code"].as_i64().unwrap();

        assert!(BuildLogCompressor::detect(&raw), "{name}");
        for recovery in [
            RecoveryMethod::None,
            RecoveryMethod::Shell,
            RecoveryMethod::Tool {
                name: RecoveryToolName::try_from("tokenless_retrieve".to_owned()).unwrap(),
            },
        ] {
            let store = InMemoryStore::new();
            let outcome = BuildLogCompressor.compress_with_recovery(&raw, Some(&store), &recovery);
            assert_eq!(decode(&outcome.output), cleaned, "{name}");
            assert!(outcome.stash_writes.is_empty(), "{name}");
            assert_eq!(store.len(), 0, "{name}");
            assert_eq!(outcome.recoverability, Recoverability::Lossless, "{name}");
            assert!(outcome.output.contains(CTEST_HEADER), "{name}");
            assert!(
                estimate_tokens(&outcome.output) < estimate_tokens(&cleaned),
                "{name}"
            );
            assert!(
                outcome.output.chars().count() < cleaned.chars().count(),
                "{name}"
            );

            let no_stash = BuildLogCompressor.compress_with_recovery(&raw, None, &recovery);
            assert_eq!(no_stash.output, outcome.output, "{name}");

            let again =
                BuildLogCompressor.compress_with_recovery(&outcome.output, Some(&store), &recovery);
            assert_eq!(again.output, outcome.output, "{name}");

            if exit_code != 0 {
                assert!(outcome.output.contains("The following tests FAILED:"));
                assert!(outcome.output.contains("online::customcert::file"));
                assert!(outcome.output.contains("***Failed"));
            }
        }

        if matches!(name, "brotli_serial.txt" | "brotli_parallel.txt") {
            let golden = dir.join(name.replace(".txt", ".expected.txt"));
            let outcome =
                BuildLogCompressor.compress_with_recovery(&raw, None, &RecoveryMethod::None);
            assert_eq!(
                outcome.output,
                std::fs::read_to_string(golden).unwrap(),
                "{name}"
            );
        }

        let outcome = BuildLogCompressor.compress_with_recovery(&raw, None, &RecoveryMethod::None);
        println!(
            "FIXTURE {name}: raw={} cleaned={} candidate={}",
            estimate_tokens(&raw),
            estimate_tokens(&cleaned),
            estimate_tokens(&outcome.output)
        );
    }
}

#[test]
fn real_excerpt_boundary_mutations_keep_diagnostics_and_control_sequences_verbatim() {
    let raw = std::fs::read_to_string(fixtures_dir().join("brotli_parallel.txt")).unwrap();
    let sample_lines = raw
        .split_inclusive('\n')
        .skip(2)
        .take(26)
        .collect::<Vec<_>>();

    let injections = [
        " 99/99 Test #99: injected_failure ......................***Failed    0.05 sec\n",
        " 99/99 Test #99: injected_timeout ......................***Timeout   1.00 sec\n",
        " 99/99 Test #99: injected_segfault .....................***Exception: SegFault  0.02 sec\n",
        "Assertion failed: expected 1 == 2\n",
        "progress 50%\rprogress 100%\n",
        "  \x1b[0K\x1b[32m14 passing \x1b[90m(11.6s)\x1b[m\n",
        "\t  99 - injected_failure (Failed)\n",
        "unclassified stdout notice\n",
    ];
    for pos in 0..=sample_lines.len() {
        for injection in injections {
            let mut lines = sample_lines
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>();
            lines.insert(pos, injection.to_owned());
            let mutated = lines.concat();
            let cleaned = oracle::strip_sgr(&mutated);
            let outcome =
                BuildLogCompressor.compress_with_recovery(&mutated, None, &RecoveryMethod::None);
            assert_eq!(
                decode(&outcome.output),
                cleaned,
                "pos={pos}, inj={injection:?}"
            );
            assert!(
                outcome.output.contains(&oracle::strip_sgr(injection)),
                "pos={pos}, inj={injection:?}"
            );
            assert!(outcome.stash_writes.is_empty());
            assert_eq!(outcome.recoverability, Recoverability::Lossless);
        }
    }
}

#[test]
fn routing_fences_crlf_and_anchored_fallback_keep_safety_boundaries() {
    for negative in [
        "      Start  1: solo\n",
        " 1/1 Test  #1: solo .............................   Passed    0.00 sec\n",
        "let text = \"1/2 Test #1: foo ... Passed 0.01 sec\";\n",
        "```text\nTest project /work/build\n 1/2 Test #1: a ...   Passed    0.01 sec\n 2/2 Test #2: b ...   Passed    0.01 sec\n```\n",
        "~~~text\nTest project /work/build\n 1/2 Test #1: a ...   Passed    0.01 sec\n 2/2 Test #2: b ...   Passed    0.01 sec\n~~~\n",
    ] {
        assert!(!BuildLogCompressor::detect(negative), "{negative}");
        check(negative, false);
    }

    let valid = "Test project /work/build\n\
                 1/2 Test #1: alpha ............................   Passed    0.01 sec\n\
                 2/2 Test #2: beta .............................   Passed    0.01 sec\n";
    let crlf = valid.replace('\n', "\r\n");
    assert!(!BuildLogCompressor::detect(&crlf));
    check(&crlf, false);

    let gapped_open_fence = format!("```text\n{}{valid}", "example line\n".repeat(120));
    assert!(!BuildLogCompressor::detect(&gapped_open_fence));

    let anchored_fallback = format!(
        "$ ctest\nTest project /work/build\n      Start  1: runningSuite\n{} 1/1 Test  #1: runningSuite .....................   Passed    0.01 sec\nexit code: 0\n",
        (1..=12)
            .map(|i| format!("processing asset item {i} of 12 in CTest suite\n"))
            .collect::<String>()
    );
    let store = InMemoryStore::new();
    let fallback = BuildLogCompressor.compress_with_recovery(
        &anchored_fallback,
        Some(&store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(fallback.recoverability, Recoverability::Retrievable);
    assert_eq!(fallback.stash_writes.len(), 1);
    let marker = fallback
        .output
        .split_inclusive('\n')
        .find(|line| line.contains("lines omitted."))
        .unwrap();
    let recovered = store
        .retrieve(&fallback.stash_writes[0].key)
        .unwrap()
        .unwrap();
    assert_eq!(
        fallback.output.replacen(marker, &recovered, 1),
        anchored_fallback
    );

    let example: String = (1..=10)
        .map(|i| {
            let tag = format!("#{i}");
            format!(" {i:2}/10 Test {tag:>3}: example/case_{i:02} ....................   Passed    0.01 sec\n")
        })
        .collect();
    let fenced = format!("```text\n{example}```\n");
    let fenced_in_build = format!(
        "Test project /work/build\n 1/2 Test #1: outer_a .........................   Passed    0.01 sec\n 2/2 Test #2: outer_b .........................   Passed    0.01 sec\n{fenced}100% tests passed, 0 tests failed out of 2\n"
    );
    let fenced_out = check(&fenced_in_build, false);
    assert!(fenced_out.contains(&fenced));

    let block_tests: String = (1..=10)
        .map(|i| {
            let tag = format!("#{i}");
            format!(" {i:2}/10 Test {tag:>3}: suite/case_{i:02} .....................   Passed    0.01 sec\n")
        })
        .collect();
    let quoted_template = format!(
        "```text\n{CTEST_HEADER}{{\"rows\":1,\"total\":\"1\",\"start_indent\":6,\"idx_width\":1,\"id_width\":1,\"col\":40,\"prefixes\":[]}}\nSP\t1\t\tex\t0.01\n```\n"
    );
    let with_quoted =
        format!("Test project /work/build\n{block_tests}{quoted_template}{block_tests}");
    let out = check(&with_quoted, true);
    assert!(out.contains(&quoted_template));

    let mut parallel_slow_fail = String::from(concat!(
        "$ ctest -j 2 --output-on-failure || true\n",
        "Test project /work/build\n",
        "      Start  1: suite/slow_fail\n",
    ));
    for id in 2..=13 {
        let ord = id - 1;
        let tag = format!("#{id}");
        let name = format!("suite/fast_{ord:02}");
        parallel_slow_fail.push_str(&format!("      Start {id:2}: {name}\n"));
        parallel_slow_fail.push_str(&format!(
            "{ord:2}/13 Test {tag:>3}: {name} .................................   Passed    0.01 sec\n"
        ));
    }
    parallel_slow_fail.push_str(
        "13/13 Test  #1: suite/slow_fail ................................***Failed    1.25 sec\n\
         assertion failed: expected ok\nexit code: 0\n",
    );
    let parallel_out = check(&parallel_slow_fail, true);
    assert!(parallel_out.contains("      Start  1: suite/slow_fail\n"));
    assert!(parallel_out.contains("***Failed"));
    assert!(!parallel_out.lines().any(|line| {
        line == "s\t1" || (line.starts_with("S\t") && line.contains("suite/slow_fail"))
    }));
}

#[test]
fn captured_ctest_corpus_round_trips_without_stash_and_saves_tokens() {
    let Some(root) = std::env::var_os("CTEST_CAPTURE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let runs = ["ctest_serial", "ctest_parallel"];
    let streams = ["stdout", "stderr", "observed"];
    let projects = ["brotli", "catch2", "fmt", "libgit2"];
    let mut checked = 0usize;

    for project in projects {
        for run in runs {
            for stream in streams {
                let path = root.join(project).join(run).join(format!("{stream}.txt"));
                let raw = std::fs::read_to_string(&path).unwrap();
                let cleaned = oracle::strip_sgr(&raw);
                let store = InMemoryStore::new();
                let outcome = BuildLogCompressor.compress_with_recovery(
                    &raw,
                    Some(&store),
                    &RecoveryMethod::Shell,
                );
                assert_eq!(decode(&outcome.output), cleaned, "{}", path.display());
                assert!(outcome.stash_writes.is_empty(), "{}", path.display());
                assert_eq!(store.len(), 0, "{}", path.display());
                assert_eq!(outcome.recoverability, Recoverability::Lossless);
                let again = BuildLogCompressor.compress_with_recovery(
                    &outcome.output,
                    Some(&store),
                    &RecoveryMethod::Shell,
                );
                assert_eq!(again.output, outcome.output, "{}", path.display());
                if stream != "stderr" {
                    assert!(
                        outcome.output.contains(CTEST_HEADER),
                        "expected CTest compaction on {}",
                        path.display()
                    );
                    assert!(estimate_tokens(&outcome.output) < estimate_tokens(&cleaned));
                    assert!(outcome.output.chars().count() < cleaned.chars().count());
                } else {
                    assert_eq!(outcome.output, cleaned);
                }
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 24);
}

#[test]
#[ignore = "rewrites reviewed goldens; run explicitly after an intentional format change"]
fn regenerate_ctest_goldens() {
    let dir = fixtures_dir();
    for name in ["brotli_serial.txt", "brotli_parallel.txt"] {
        let path = dir.join(name);
        let raw = std::fs::read_to_string(&path).unwrap();
        let outcome = BuildLogCompressor.compress_with_recovery(&raw, None, &RecoveryMethod::None);
        assert_eq!(decode(&outcome.output), oracle::strip_sgr(&raw));
        let expected = dir.join(name.replace(".txt", ".expected.txt"));
        std::fs::write(expected, outcome.output).unwrap();
    }
}
