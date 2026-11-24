//! Independent reconstruction and boundary checks for C/C++ compiler invocation compaction.

use std::path::PathBuf;

use tokenless_ccr::{InMemoryStore, RecoveryMethod, StashStore};
use tokenless_compressors::{BuildLogCompressor, Recoverability};
use tokenless_protocol::{RecoveryToolName, estimate_tokens};

mod common;
use common::build_log_harness as harness;
use common::literal_oracle as oracle;

const COMPILE_HEADER: &str = "C/C++ compile templates (id<TAB>fields; interleave with literals): ";

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build_logs/cc_compile")
}

fn decode(input: &str) -> String {
    oracle::decode(input, COMPILE_HEADER)
}

fn check(input: &str, expect_compacted: bool) -> String {
    harness::check_lossless(
        input,
        COMPILE_HEADER,
        expect_compacted,
        oracle::strip_sgr,
        decode,
    )
}

#[test]
fn provenance_matches_all_committed_fixtures() {
    harness::verify_provenance_digests(&fixtures_dir(), 8);
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

            let no_stash = BuildLogCompressor.compress_with_recovery(&raw, None, &recovery);
            assert_eq!(no_stash.output, outcome.output, "{name}");

            let again =
                BuildLogCompressor.compress_with_recovery(&outcome.output, Some(&store), &recovery);
            assert_eq!(again.output, outcome.output, "{name}");

            if exit_code == 0 {
                assert!(outcome.output.contains(COMPILE_HEADER), "{name}");
                assert!(
                    estimate_tokens(&outcome.output) < estimate_tokens(&cleaned),
                    "{name}"
                );
                assert!(
                    outcome.output.chars().count() < cleaned.chars().count(),
                    "{name}"
                );
            } else {
                assert_eq!(outcome.output, cleaned, "{name}");
                assert!(
                    outcome
                        .output
                        .contains("identifier '_kBrotliContextLookupTable' is reserved"),
                    "{name}"
                );
            }
        }

        if name == "fmt_ninja_verbose.txt" {
            let outcome =
                BuildLogCompressor.compress_with_recovery(&raw, None, &RecoveryMethod::None);
            assert!(outcome.output.contains("include/fmt/fmt-c.h:59:1: note:"));
        }

        if matches!(name, "brotli_make_verbose.txt" | "brotli_ninja_verbose.txt") {
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
    for fixture in ["brotli_ninja_verbose.txt", "zstd_make_lib_v1_gcc.txt"] {
        let raw = std::fs::read_to_string(fixtures_dir().join(fixture)).unwrap();
        let base_lines: Vec<&str> = raw.split_inclusive('\n').collect();
        let injections = [
            "FAILED: CMakeFiles/brotlienc.dir/c/enc/encode.c.o \n",
            "c/enc/encode.c:120:3: error: expected ';' before 'return'\n",
            "opaque redraw 50%\r",
            "\x1b[0Kunknown progress marker\n",
            "```text\n[1/2] /usr/bin/cc -O3 -c /work/a.c -o a.o\n[2/2] /usr/bin/cc -O3 -c /work/b.c -o b.o\n```\n",
            "unclassified stdout line\n",
        ];
        for pos in 0..=base_lines.len() {
            for injection in &injections {
                let mut mutated = String::new();
                for line in &base_lines[..pos] {
                    mutated.push_str(line);
                }
                mutated.push_str(injection);
                for line in &base_lines[pos..] {
                    mutated.push_str(line);
                }
                let cleaned = oracle::strip_sgr(&mutated);
                let outcome = BuildLogCompressor.compress_with_recovery(
                    &mutated,
                    None,
                    &RecoveryMethod::None,
                );
                assert_eq!(
                    decode(&outcome.output),
                    cleaned,
                    "fixture={fixture}, pos={pos}, inj={injection:?}"
                );
                assert!(
                    outcome.output.contains(&oracle::strip_sgr(injection)),
                    "fixture={fixture}, pos={pos}, inj={injection:?}"
                );
                assert!(outcome.stash_writes.is_empty());
                assert_eq!(outcome.recoverability, Recoverability::Lossless);
            }
        }
    }
}

#[test]
fn handwritten_make_non_verbose_cmake_and_crlf_keep_existing_routes() {
    let handwritten = concat!(
        "make: Entering directory '/work'\n",
        "cc -O2 -Wall -c src/alpha.c -o build/alpha.o\n",
        "cc -O2 -Wall -c src/beta.c -o build/beta.o\n",
    );
    let make_store = InMemoryStore::new();
    let make_out = BuildLogCompressor.compress_with_recovery(
        handwritten,
        Some(&make_store),
        &RecoveryMethod::Shell,
    );
    assert!(!make_out.output.contains(COMPILE_HEADER));

    const CMAKE_HEADER: &str = "CMake/Ninja templates (id<TAB>fields; interleave with literals): ";
    let non_verbose_cmake = concat!(
        "[1/6] Building C object CMakeFiles/app.dir/alpha.c.o\n",
        "[2/6] Building C object CMakeFiles/app.dir/beta.c.o\n",
        "[3/6] Building C object CMakeFiles/app.dir/gamma.c.o\n",
        "[4/6] Building C object CMakeFiles/app.dir/delta.c.o\n",
        "[5/6] Building C object CMakeFiles/app.dir/epsilon.c.o\n",
        "[6/6] Linking C executable app\n",
    );
    let cmake_out = harness::check_lossless(
        non_verbose_cmake,
        CMAKE_HEADER,
        true,
        oracle::strip_sgr,
        |out| oracle::decode(out, CMAKE_HEADER),
    );
    assert!(!cmake_out.contains(COMPILE_HEADER));

    let crlf_verbose = concat!(
        "[1/2] /usr/bin/cc -O3 -MD -c /work/a.c -o a.o\r\n",
        "[2/2] /usr/bin/cc -O3 -MD -c /work/b.c -o b.o\r\n",
    );
    assert!(!BuildLogCompressor::detect(crlf_verbose));
    check(crlf_verbose, false);

    let raw_ninja =
        std::fs::read_to_string(fixtures_dir().join("brotli_ninja_verbose.txt")).unwrap();
    let compacted_ninja = check(&format!("$ ninja -v\n{raw_ninja}exit code: 0\n"), true);
    assert!(compacted_ninja.contains(COMPILE_HEADER));

    let oversized_compacted = format!("{}{compacted_ninja}", "$ echo step\n".repeat(120));
    let store = InMemoryStore::new();
    let terminal = BuildLogCompressor.compress_with_recovery(
        &oversized_compacted,
        Some(&store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(terminal.output, oversized_compacted);
    assert_eq!(terminal.recoverability, Recoverability::Lossless);
    assert!(terminal.stash_writes.is_empty());
}

#[test]
fn captured_verbose_corpus_round_trips_without_stash_and_saves_tokens() {
    let Some(root) = std::env::var_os("CPP_VERBOSE_CAPTURE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let entries: Vec<(&str, &str, &str, bool)> = vec![
        ("captures", "brotli", "make_verbose", true),
        ("captures", "catch2", "make_verbose", true),
        ("captures", "fmt", "make_verbose", true),
        ("slice_c_verbose", "brotli", "ninja_verbose", true),
        ("slice_c_verbose", "brotli", "clang_make_verbose", true),
        ("slice_c_verbose", "brotli", "clang_ninja_verbose", true),
        (
            "slice_c_verbose",
            "brotli",
            "clang_ninja_verbose_failure",
            false,
        ),
        ("slice_c_verbose", "catch2", "ninja_verbose", true),
        ("slice_c_verbose", "fmt", "ninja_verbose", true),
        ("slice_c_verbose", "fmt", "clang_make_verbose", true),
        ("slice_c_verbose", "fmt", "clang_ninja_verbose", true),
        (
            "slice_c_verbose",
            "fmt",
            "clang_make_verbose_failure",
            false,
        ),
        (
            "slice_c_verbose",
            "fmt",
            "clang_ninja_verbose_failure",
            false,
        ),
        ("slice_c_verbose", "zstd", "make_lib_v1_gcc", true),
        ("slice_c_verbose", "zstd", "make_lib_v1_clang", true),
        ("slice_c_verbose", "zstd", "make_all_v1_gcc", true),
    ];

    let mut checked = 0usize;
    for (bucket, project, run, expect_success_compaction) in entries {
        for stream in ["stdout", "stderr", "observed"] {
            let path = root
                .join(bucket)
                .join(project)
                .join(run)
                .join(format!("{stream}.txt"));
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

            let expect_compacted =
                expect_success_compaction && matches!(stream, "stdout" | "observed");
            if expect_compacted {
                assert!(
                    outcome.output.contains(COMPILE_HEADER),
                    "expected compaction on {}",
                    path.display()
                );
                assert!(
                    estimate_tokens(&outcome.output) < estimate_tokens(&cleaned),
                    "{}",
                    path.display()
                );
                assert!(
                    outcome.output.chars().count() < cleaned.chars().count(),
                    "{}",
                    path.display()
                );
            } else if (stream == "stderr" || !expect_success_compaction)
                && !outcome.output.contains(COMPILE_HEADER)
            {
                assert_eq!(outcome.output, cleaned, "{}", path.display());
            }
            println!(
                "CORPUS {project}/{run}/{stream}: {} -> {} tokens (compacted={})",
                estimate_tokens(&cleaned),
                estimate_tokens(&outcome.output),
                outcome.output.contains(COMPILE_HEADER),
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 48);
}

#[test]
#[ignore = "rewrites reviewed goldens; run explicitly after an intentional format change"]
fn regenerate_cc_compile_goldens() {
    let dir = fixtures_dir();
    for name in ["brotli_make_verbose.txt", "brotli_ninja_verbose.txt"] {
        let raw = std::fs::read_to_string(dir.join(name)).unwrap();
        let outcome = BuildLogCompressor.compress_with_recovery(&raw, None, &RecoveryMethod::None);
        std::fs::write(
            dir.join(name.replace(".txt", ".expected.txt")),
            outcome.output,
        )
        .unwrap();
    }
}
