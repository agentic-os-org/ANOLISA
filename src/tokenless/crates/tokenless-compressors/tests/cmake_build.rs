//! Independent reconstruction and boundary checks for CMake and Ninja structural compaction.

use std::path::PathBuf;

use tokenless_ccr::{InMemoryStore, RecoveryMethod, StashStore};
use tokenless_compressors::{BuildLogCompressor, Recoverability};
use tokenless_protocol::{RecoveryToolName, estimate_tokens};

mod common;
use common::build_log_harness as harness;
use common::literal_oracle as oracle;

const BUILD_HEADER: &str = "CMake/Ninja templates (id<TAB>fields; interleave with literals): ";

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build_logs/cmake_build")
}

fn decode(input: &str) -> String {
    oracle::decode(input, BUILD_HEADER)
}

fn check(input: &str, expect_compacted: bool) -> String {
    harness::check_lossless(
        input,
        BUILD_HEADER,
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
                assert!(outcome.output.contains(BUILD_HEADER), "{name}");
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
                        .contains("unknown target 'tokenless_nonexistent_target'"),
                    "{name}"
                );
            }
        }

        if matches!(name, "fmt_ninja_cold.txt" | "fmt_make_cold.txt") {
            let outcome =
                BuildLogCompressor.compress_with_recovery(&raw, None, &RecoveryMethod::None);
            assert!(outcome.output.contains("include/fmt/fmt-c.h:59:1: note:"));
        }

        if matches!(name, "brotli_ninja_cold.txt" | "brotli_make_cold.txt") {
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
    let raw = std::fs::read_to_string(fixtures_dir().join("brotli_ninja_cold.txt")).unwrap();
    let base_lines: Vec<&str> = raw.split_inclusive('\n').collect();
    let injections = [
        "FAILED: CMakeFiles/brotlienc.dir/c/enc/encode.c.o \n",
        "c/enc/encode.c:120:3: error: expected ';' before 'return'\n",
        "opaque redraw 50%\r",
        "\x1b[0Kunknown progress marker\n",
        "```text\n[1/2] Building C object a.c.o\n[2/2] Built target a\n```\n",
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
        "[1/1] Building CXX object CMakeFiles/app.dir/main.cpp.o\n",
        "[100%] Built target app\n",
        "let text = \"[1/2] Building CXX object a.cpp.o\";\n",
        "```text\n[1/2] Building CXX object a.cpp.o\n[2/2] Linking CXX executable app\n```\n",
        "~~~text\n[1/2] Building CXX object a.cpp.o\n[2/2] Linking CXX executable app\n~~~\n",
    ] {
        assert!(!BuildLogCompressor::detect(negative), "{negative}");
        check(negative, false);
    }

    let valid = "[1/2] Building CXX object CMakeFiles/app.dir/alpha.cpp.o\n\
                 [2/2] Building CXX object CMakeFiles/app.dir/beta.cpp.o\n";
    let crlf = valid.replace('\n', "\r\n");
    assert!(!BuildLogCompressor::detect(&crlf));
    check(&crlf, false);

    let gapped_open_fence = format!("```text\n{}{valid}", "example line\n".repeat(120));
    assert!(!BuildLogCompressor::detect(&gapped_open_fence));

    let anchored_fallback = format!(
        "$ cmake --build /work/build\n\
         [1/2] Building CXX object CMakeFiles/app.dir/main.cpp.o\n\
         {}\
         [2/2] Linking CXX executable app\n\
         exit code: 0\n",
        (1..=12)
            .map(|i| format!("processing asset item {i} of 12 in custom build step\n"))
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
        .map(|i| format!("[{i}/10] Building CXX object CMakeFiles/ex.dir/file_{i:02}.cpp.o\n"))
        .collect();
    let fenced = format!("```text\n{example}```\n");
    let fenced_in_build = format!(
        "[1/2] Building CXX object CMakeFiles/app.dir/a.cpp.o\n{fenced}\
         [2/2] Linking CXX executable app\n"
    );
    let fenced_out = check(&fenced_in_build, false);
    assert!(fenced_out.contains(&fenced));

    let block_rows: String = (1..=10)
        .map(|i| format!("[{i}/10] Building CXX object CMakeFiles/app.dir/mod_{i:02}.cpp.o\n"))
        .collect();
    let quoted_template = format!(
        "```text\n{BUILD_HEADER}{{\"rows\":1,\"literals\":[[\"[\",\"/1] Built target \",\"\"]]}}\n0\t1\tex\n```\n"
    );
    let with_quoted = format!("{block_rows}{quoted_template}{block_rows}");
    let out = check(&with_quoted, true);
    assert!(out.contains(&quoted_template));

    let verbose_interleaved = (1..=8)
        .map(|i| {
            let pct = i * 12;
            format!(
                "[{pct:3}%] Building CXX object CMakeFiles/app.dir/file_{i:02}.cpp.o\n\
                 /usr/bin/c++ -O2 -o CMakeFiles/app.dir/file_{i:02}.cpp.o -c /work/src/file_{i:02}.cpp\n"
            )
        })
        .collect::<String>();
    let verbose_stream = format!(
        "make[1]: Entering directory '/work/build'\n{verbose_interleaved}make[1]: Leaving directory '/work/build'\n"
    );
    let verbose_store = InMemoryStore::new();
    let verbose_out = BuildLogCompressor.compress_with_recovery(
        &verbose_stream,
        Some(&verbose_store),
        &RecoveryMethod::Shell,
    );
    assert!(!verbose_out.output.contains(BUILD_HEADER));
    assert!(verbose_out.output.contains("C/C++ compile templates"));
    assert_eq!(verbose_out.recoverability, Recoverability::Lossless);
    assert!(verbose_out.stash_writes.is_empty());
    assert_eq!(
        oracle::decode(
            &verbose_out.output,
            "C/C++ compile templates (id<TAB>fields; interleave with literals): "
        ),
        verbose_stream
    );

    let no_work_then_make = format!(
        "ninja: no work to do.\n{}",
        (1..=16)
            .map(|i| format!("cc -O2 -c src/mod_{i:02}.c -o build/mod_{i:02}.o\n"))
            .collect::<String>()
    );
    let make_store = InMemoryStore::new();
    let make_fallback = BuildLogCompressor.compress_with_recovery(
        &no_work_then_make,
        Some(&make_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(make_fallback.recoverability, Recoverability::Retrievable);
    assert_eq!(make_fallback.stash_writes.len(), 1);
    assert!(make_fallback.output.starts_with("ninja: no work to do.\n"));

    let ninja_head = (1..=9)
        .map(|i| format!("[{i}/9] Building CXX object CMakeFiles/app.dir/src/mod_{i:02}.cpp.o\n"))
        .collect::<String>();
    let make_body = (1..=24)
        .map(|i| format!("cc -O2 -Wall -Iinclude -c src/item_{i:02}.c -o build/item_{i:02}.o\n"))
        .collect::<String>();
    let ninja_then_make = format!(
        "{ninja_head}make: Entering directory '/work/make'\n{make_body}make: Leaving directory '/work/make'\n"
    );
    let ninja_only =
        BuildLogCompressor.compress_with_recovery(&ninja_then_make, None, &RecoveryMethod::None);
    assert_eq!(ninja_only.recoverability, Recoverability::Lossless);
    assert!(ninja_only.stash_writes.is_empty());

    let make_only_store = InMemoryStore::new();
    let make_only = BuildLogCompressor.compress_with_recovery(
        &format!("make: Entering directory '/work/make'\n{make_body}make: Leaving directory '/work/make'\n"),
        Some(&make_only_store),
        &RecoveryMethod::Shell,
    );

    let composed_store = InMemoryStore::new();
    let composed = BuildLogCompressor.compress_with_recovery(
        &ninja_then_make,
        Some(&composed_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(composed.recoverability, Recoverability::Retrievable);
    assert_eq!(composed.stash_writes.len(), 1);
    assert!(composed.output.contains(BUILD_HEADER));
    assert!(composed.output.contains("lines omitted."));
    assert!(estimate_tokens(&composed.output) < estimate_tokens(&ninja_only.output));
    assert!(
        estimate_tokens(&composed.output)
            < estimate_tokens(&ninja_head) + estimate_tokens(&make_only.output)
    );
    let marker = composed
        .output
        .split_inclusive('\n')
        .find(|line| line.contains("lines omitted."))
        .unwrap();
    let stashed = composed_store
        .retrieve(&composed.stash_writes[0].key)
        .unwrap()
        .unwrap();
    assert_eq!(
        decode(&composed.output.replacen(marker, &stashed, 1)),
        ninja_then_make
    );
    let second_pass = BuildLogCompressor.compress_with_recovery(
        &composed.output,
        Some(&composed_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(second_pass.output, composed.output);
    assert_eq!(composed_store.len(), 1);

    let fenced_make = format!("{ninja_head}```text\n{make_body}```\n");
    let fenced_store = InMemoryStore::new();
    let fenced_make_out = BuildLogCompressor.compress_with_recovery(
        &fenced_make,
        Some(&fenced_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(fenced_make_out.recoverability, Recoverability::Lossless);
    assert!(fenced_make_out.stash_writes.is_empty());
    assert!(fenced_make_out.output.contains(&make_body));
    assert_eq!(decode(&fenced_make_out.output), fenced_make);

    let fmt_inc = std::fs::read_to_string(fixtures_dir().join("fmt_make_incremental.txt")).unwrap();
    let fmt_plus_wheels = format!(
        "$ cmake --build /work/fmt-build && python wheel.py\n\
         {fmt_inc}\
         {}\
         exit code: 0\n",
        (1..=60)
            .map(|i| format!("Processing wheel {i} of 60\n"))
            .collect::<String>()
    );
    let fmt_lossless =
        BuildLogCompressor.compress_with_recovery(&fmt_plus_wheels, None, &RecoveryMethod::None);
    let fmt_wheel_store = InMemoryStore::new();
    let fmt_wheel_out = BuildLogCompressor.compress_with_recovery(
        &fmt_plus_wheels,
        Some(&fmt_wheel_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(fmt_wheel_out.recoverability, Recoverability::Retrievable);
    assert_eq!(fmt_wheel_out.stash_writes.len(), 1);
    assert!(fmt_wheel_out.output.contains(BUILD_HEADER));
    assert!(fmt_wheel_out.output.contains("lines omitted."));
    assert!(
        estimate_tokens(&fmt_wheel_out.output) < estimate_tokens(&fmt_lossless.output),
        "expected Generic tail omission to improve on lossless {} vs {}",
        estimate_tokens(&fmt_wheel_out.output),
        estimate_tokens(&fmt_lossless.output)
    );
    let wheel_marker = fmt_wheel_out
        .output
        .split_inclusive('\n')
        .find(|line| line.contains("lines omitted."))
        .unwrap();
    let wheel_stashed = fmt_wheel_store
        .retrieve(&fmt_wheel_out.stash_writes[0].key)
        .unwrap()
        .unwrap();
    assert_eq!(
        decode(
            &fmt_wheel_out
                .output
                .replacen(wheel_marker, &wheel_stashed, 1)
        ),
        fmt_plus_wheels
    );

    let custom_steps = format!(
        "$ cmake --build /work/build\n\
         {}\
         [21/22] Building C object CMakeFiles/app.dir/main.c.o\n\
         [22/22] Linking C static library libapp.a\n\
         exit code: 0\n",
        (1..=20)
            .map(|i| format!(
                "[{i}/22] Building generated review artifact {i:02} for the project metadata index\n"
            ))
            .collect::<String>()
    );
    let custom_store = InMemoryStore::new();
    let custom_steps_out = BuildLogCompressor.compress_with_recovery(
        &custom_steps,
        Some(&custom_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(custom_steps_out.recoverability, Recoverability::Lossless);
    assert!(custom_steps_out.stash_writes.is_empty());
    assert_eq!(custom_steps_out.output, custom_steps);

    let ctrl_cc = "cc -O2 -Wall -Iinclude -c src/item_12.c\x1b[0K -o build/item_12.o\n";
    let make_ctrl_body = (1..=24)
        .map(|i| {
            if i == 12 {
                ctrl_cc.to_owned()
            } else {
                format!("cc -O2 -Wall -Iinclude -c src/item_{i:02}.c -o build/item_{i:02}.o\n")
            }
        })
        .collect::<String>();
    let ninja_then_ctrl_make = format!(
        "{ninja_head}make: Entering directory '/work/make'\n{make_ctrl_body}make: Leaving directory '/work/make'\n"
    );
    let ctrl_store = InMemoryStore::new();
    let ctrl_out = BuildLogCompressor.compress_with_recovery(
        &ninja_then_ctrl_make,
        Some(&ctrl_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(ctrl_out.recoverability, Recoverability::Retrievable);
    // Keeping row 12 verbatim splits the 24-row Make run into 11- and 12-row Stash blocks.
    assert_eq!(ctrl_out.stash_writes.len(), 2);
    assert!(ctrl_out.output.contains(ctrl_cc));
    let ctrl_markers = ctrl_out
        .output
        .split_inclusive('\n')
        .filter(|line| line.contains("lines omitted."))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(ctrl_markers.len(), 2);
    let mut ctrl_restored = ctrl_out.output.clone();
    for (marker, write) in ctrl_markers.iter().zip(&ctrl_out.stash_writes) {
        let payload = ctrl_store.retrieve(&write.key).unwrap().unwrap();
        assert!(!payload.contains("\x1b[0K"));
        ctrl_restored = ctrl_restored.replacen(marker, &payload, 1);
    }
    assert_eq!(decode(&ctrl_restored), ninja_then_ctrl_make);

    let interleaved_30 = format!(
        "$ cmake --build /work/build && python postprocess.py\n\
         {}\
         exit code: 0\n",
        (1..=30)
            .map(|i| format!(
                "[{i}/30] Building CXX object CMakeFiles/app.dir/src/unit_{i:02}.cpp.o\n\
                 Processing item {i} of 30\n"
            ))
            .collect::<String>()
    );
    let interleaved_store = InMemoryStore::new();
    let interleaved_out = BuildLogCompressor.compress_with_recovery(
        &interleaved_30,
        Some(&interleaved_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(interleaved_out.recoverability, Recoverability::Retrievable);
    assert_eq!(interleaved_out.stash_writes.len(), 1);
    assert!(estimate_tokens(&interleaved_out.output) < 120);
    let interleaved_marker = interleaved_out
        .output
        .split_inclusive('\n')
        .find(|line| line.contains("lines omitted."))
        .unwrap();
    let interleaved_payload = interleaved_store
        .retrieve(&interleaved_out.stash_writes[0].key)
        .unwrap()
        .unwrap();
    assert_eq!(
        interleaved_out
            .output
            .replacen(interleaved_marker, &interleaved_payload, 1),
        interleaved_30
    );

    let c_tail = (21..=32)
        .map(|i| format!("[{i}/32] Building C object CMakeFiles/lib.dir/src/unit_{i:02}.c.o\n"))
        .collect::<String>();
    let unicode_anchored = format!(
        "$ cmake --build /work/build\n\
         [1/32] Building generated review artifact 01 for the project metadata index\n\
         {}\
         {c_tail}\
         exit code: 0\n",
        (2..=20)
            .map(|i| format!(
                "[{i}/32] \u{1f527} Building generated review artifact {i:02} for the project metadata index\n"
            ))
            .collect::<String>()
    );
    let unicode_head = format!(
        "{}{c_tail}",
        (1..=20)
            .map(|i| format!(
                "[{i}/32] \u{1f527} Building generated review artifact {i:02} for the project metadata index\n"
            ))
            .collect::<String>()
    );
    for unicode_case in [unicode_anchored, unicode_head] {
        let store = InMemoryStore::new();
        let out = BuildLogCompressor.compress_with_recovery(
            &unicode_case,
            Some(&store),
            &RecoveryMethod::Shell,
        );
        assert_eq!(out.recoverability, Recoverability::Lossless);
        assert!(out.stash_writes.is_empty());
        assert!(out.output.contains(BUILD_HEADER));
        assert_eq!(decode(&out.output), unicode_case);
    }

    let tab_custom_anchored = format!(
        "$ cmake --build /work/build\n{}{}",
        (1..=24)
            .map(|i| {
                format!("[{i}/41] Processing\tgenerated build resource {i:02} for the project metadata index\n")
            })
            .collect::<String>(),
        (25..=41)
            .map(|i| {
                let idx = i - 24;
                format!("[{i}/41] Building C object CMakeFiles/app.dir/src/unit_{idx:02}.c.o\n")
            })
            .collect::<String>(),
    );
    let tab_store = InMemoryStore::new();
    let tab_out = BuildLogCompressor.compress_with_recovery(
        &tab_custom_anchored,
        Some(&tab_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(tab_out.recoverability, Recoverability::Lossless);
    assert!(tab_out.stash_writes.is_empty());
    assert!(tab_out.output.contains(BUILD_HEADER));
    assert!(
        tab_out
            .output
            .contains("[18/41] Processing\tgenerated build resource 18")
    );
    assert_eq!(decode(&tab_out.output), tab_custom_anchored);

    assert!(!BuildLogCompressor::detect(
        "ninja: warning: premature end of file; recovering\n"
    ));
    let leading_ninja_warn = format!(
        "ninja: warning: premature end of file; recovering\n{}",
        (1..=16)
            .map(|i| format!("[{i}/16] Building C object CMakeFiles/app.dir/src/unit_{i:02}.c.o\n"))
            .collect::<String>(),
    );
    let warn_store = InMemoryStore::new();
    let warn_out = BuildLogCompressor.compress_with_recovery(
        &leading_ninja_warn,
        Some(&warn_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(warn_out.recoverability, Recoverability::Lossless);
    assert!(warn_out.stash_writes.is_empty());
    assert!(
        warn_out
            .output
            .starts_with("ninja: warning: premature end of file; recovering\n")
    );
    assert!(warn_out.output.contains(BUILD_HEADER));
    assert_eq!(decode(&warn_out.output), leading_ninja_warn);

    let make_tab_prefix = (1..=24)
        .map(|i| {
            let pct = (i * 100) / 40;
            format!(
                "[{pct:>3}%] \tProcessing generated build resource {i:02} for the project metadata index\n"
            )
        })
        .collect::<String>();
    let make_compile_tail = (25..=40)
        .map(|i| {
            let pct = (i * 100) / 40;
            let idx = i - 24;
            format!("[{pct:>3}%] Building C object CMakeFiles/app.dir/src/unit_{idx:02}.c.o\n")
        })
        .collect::<String>();
    for make_tab_case in [
        format!("$ cmake --build /work/build\n{make_tab_prefix}{make_compile_tail}"),
        format!("{make_tab_prefix}{make_compile_tail}"),
    ] {
        assert!(BuildLogCompressor::detect(&make_tab_case));
        let store = InMemoryStore::new();
        let out = BuildLogCompressor.compress_with_recovery(
            &make_tab_case,
            Some(&store),
            &RecoveryMethod::Shell,
        );
        assert_eq!(out.recoverability, Recoverability::Lossless);
        assert!(out.stash_writes.is_empty());
        assert!(out.output.contains(BUILD_HEADER));
        assert!(
            out.output
                .contains("] \tProcessing generated build resource 18")
        );
        assert_eq!(decode(&out.output), make_tab_case);
    }
}

#[test]
fn captured_cmake_build_corpus_round_trips_without_stash_and_saves_tokens() {
    let Some(root) = std::env::var_os("CPP_CAPTURE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let runs = [
        "ninja_cold",
        "ninja_incremental",
        "make_cold",
        "make_incremental",
        "failure_unknown_target",
    ];
    let streams = ["stdout", "stderr", "observed"];
    let projects = ["brotli", "catch2", "fmt"];
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

                let expect_compacted = matches!(
                    (project, run, stream),
                    (_, "ninja_cold" | "make_cold", "stdout" | "observed")
                        | ("fmt", "make_incremental", "stdout" | "observed")
                );
                if expect_compacted {
                    assert!(
                        outcome.output.contains(BUILD_HEADER),
                        "expected compaction on {}",
                        path.display()
                    );
                    assert!(estimate_tokens(&outcome.output) < estimate_tokens(&cleaned));
                    assert!(outcome.output.chars().count() < cleaned.chars().count());
                } else {
                    assert_eq!(outcome.output, cleaned, "{}", path.display());
                }
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 45);
}

#[test]
#[ignore = "rewrites reviewed goldens; run explicitly after an intentional format change"]
fn regenerate_cmake_build_goldens() {
    let dir = fixtures_dir();
    for name in ["brotli_ninja_cold.txt", "brotli_make_cold.txt"] {
        let raw = std::fs::read_to_string(dir.join(name)).unwrap();
        let outcome = BuildLogCompressor.compress_with_recovery(&raw, None, &RecoveryMethod::None);
        std::fs::write(
            dir.join(name.replace(".txt", ".expected.txt")),
            outcome.output,
        )
        .unwrap();
    }
}
