//! Independent reconstruction and boundary checks for Gradle structural compaction.

use std::path::PathBuf;

use tokenless_ccr::{InMemoryStore, RecoveryMethod, StashStore};
use tokenless_compressors::{BuildLogCompressor, Recoverability};
use tokenless_protocol::{RecoveryToolName, estimate_tokens};

mod common;
use common::build_log_harness as harness;
use common::gradle_oracle::{self as oracle, decode};

const TASK_HEADER: &str = "Gradle templates (id<TAB>fields; interleave with literals): ";

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build_logs/gradle")
}

fn check(input: &str, expect_compacted: bool) -> String {
    harness::check_lossless(
        input,
        TASK_HEADER,
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
            assert!(outcome.output.contains(TASK_HEADER), "{name}");
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
                assert!(
                    outcome
                        .output
                        .contains("Task 'buid' not found in root project 'caffeine'")
                );
            }
        }

        if matches!(
            name,
            "caffeine_incremental.txt" | "resilience4j_incremental.txt"
        ) {
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
    let raw = std::fs::read_to_string(fixtures_dir().join("mockito_cold_excerpt.txt")).unwrap();
    let sample_lines = raw
        .split_inclusive('\n')
        .skip(39)
        .take(28)
        .collect::<Vec<_>>();
    assert!(sample_lines.iter().all(|line| line.starts_with("> Task :")));

    let injections = [
        "warning: [unchecked] unchecked call\n",
        "w: file:///work/src/Main.kt:10:5: Variable 'x' is never used\n",
        "Note: Recompile with -Xlint:unchecked for details.\n",
        "java.lang.IllegalStateException: boom\n\tat com.example.App.main(App.java:12)\n",
        "> Task :mockito-core:test FAILED\n",
        "progress 50%\rprogress 100%\n",
        "  \x1b[0K\x1b[32m14 passing \x1b[90m(11.6s)\x1b[m\n",
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
fn routing_fences_crlf_and_edit_caps_keep_safety_boundaries() {
    for negative in [
        "> Task :app:compileJava\n",
        "> Task :app:test FAILED\n",
        "> Configure project :\nBuilding version '1.0'\n",
        "let text = \"> Task :app:compileJava UP-TO-DATE\";\n",
        "```text\n> Task :app:compileJava UP-TO-DATE\n> Task :app:classes UP-TO-DATE\n```\n",
        "~~~text\n> Task :app:compileJava UP-TO-DATE\n> Task :app:classes UP-TO-DATE\n~~~\n",
    ] {
        assert!(!BuildLogCompressor::detect(negative), "{negative}");
        check(negative, false);
    }

    let valid = "> Task :mod-1:compileJava UP-TO-DATE\n> Task :mod-1:classes UP-TO-DATE\n\
                 > Task :mod-2:compileJava UP-TO-DATE\n> Task :mod-2:classes UP-TO-DATE\n";
    let crlf = valid.replace('\n', "\r\n");
    assert!(!BuildLogCompressor::detect(&crlf));
    check(&crlf, false);

    let gapped_open_fence = format!("```text\n{}{valid}", "example line\n".repeat(120));
    assert!(!BuildLogCompressor::detect(&gapped_open_fence));

    let anchored_fallback = format!(
        "$ ./gradlew bundle\n> Task :app:runningPreprocess\n{}> Task :app:runningPostprocess\nexit code: 0\n",
        (1..=12)
            .map(|i| format!("processing asset item {i} of 12 in Gradle task\n"))
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

    let example: String = (0..12)
        .map(|i| format!("> Task :example-module-{i}:compileJava UP-TO-DATE\n"))
        .collect();
    let fenced = format!("```text\n{example}```\n");
    let fenced_in_build = format!(
        "> Task :app:prepare\n> Task :app:printDocumentation\n{fenced}\
         BUILD SUCCESSFUL in 1s\n2 actionable tasks: 2 executed\n"
    );
    let fenced_out = check(&fenced_in_build, false);
    assert!(fenced_out.contains(&fenced));

    let mut variant_log = "$ ./gradlew build\n".to_owned();
    for module in 0..3 {
        for variant in 0..70 {
            variant_log.push_str(&format!(
                "> Task :module-{module}:generateVariant{variant:02}ProductionReleaseResources UP-TO-DATE\n"
            ));
        }
    }
    for i in 1..=12 {
        variant_log.push_str(&format!(
            "processing custom asset bundle {i} of 12 inside Gradle custom generator\n"
        ));
    }
    variant_log
        .push_str("BUILD SUCCESSFUL in 2s\n210 actionable tasks: 210 up-to-date\nexit code: 0\n");
    let compacted_long = check(&variant_log, true);
    let header_line = compacted_long
        .lines()
        .find(|line| line.starts_with(TASK_HEADER))
        .unwrap();
    assert!(header_line.len() > 4096);

    let block_tasks: String = (0..12)
        .map(|i| format!("> Task :module-{i}:compileJava UP-TO-DATE\n"))
        .collect();
    let quoted_template =
        format!("```text\n{TASK_HEADER}{{\"rows\":1,\"literals\":[[\"example\"]]}}\n0\n```\n");
    let with_quoted_template = format!("{block_tasks}{quoted_template}{block_tasks}");
    let quoted_out = check(&with_quoted_template, true);
    assert!(quoted_out.contains(&quoted_template));
}

#[test]
fn captured_gradle_corpus_round_trips_without_stash_and_saves_tokens() {
    let Some(root) = std::env::var_os("GRADLE_CAPTURE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let projects = ["mockito", "caffeine", "resilience4j"];
    let runs = [
        "cold",
        "incremental",
        "clean_rebuild",
        "failure_unknown_task",
    ];
    let streams = ["stdout", "stderr", "observed"];
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
                if outcome.output != cleaned {
                    assert!(estimate_tokens(&outcome.output) < estimate_tokens(&cleaned));
                    assert!(outcome.output.chars().count() < cleaned.chars().count());
                }
                if project == "resilience4j" && run == "cold" && stream == "stderr" {
                    assert_eq!(outcome.output, cleaned);
                }
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 36);
}

#[test]
#[ignore = "rewrites reviewed goldens; run explicitly after an intentional format change"]
fn regenerate_gradle_goldens() {
    let dir = fixtures_dir();
    for name in ["caffeine_incremental.txt", "resilience4j_incremental.txt"] {
        let path = dir.join(name);
        let raw = std::fs::read_to_string(&path).unwrap();
        let outcome = BuildLogCompressor.compress_with_recovery(&raw, None, &RecoveryMethod::None);
        assert_eq!(decode(&outcome.output), oracle::strip_sgr(&raw));
        let expected = dir.join(name.replace(".txt", ".expected.txt"));
        std::fs::write(expected, outcome.output).unwrap();
    }
}

#[test]
fn deep_non_task_tail_after_compactable_run_never_aborts() {
    // Regression for the run scan: 50 000 non-task lines after a compactable
    // run must be closed iteratively, not per line, so deep tails cannot
    // exhaust the thread stack. The run before the noise still compacts and
    // the noise tail stays verbatim.
    let mut input = String::from("$ ./gradlew build\nWelcome to Gradle 8.14.2!\n");
    for index in 0..12 {
        input.push_str(&format!("> Task :core:compileJava{index} UP-TO-DATE\n"));
    }
    let noise = "Some deep diagnostic body line that is not a task record\n".repeat(50_000);
    input.push_str(&noise);
    let output = check(&input, true);
    assert!(output.contains(&noise[..80]));
}

#[test]
fn malformed_task_fragments_never_enter_generic_progress_templates() {
    // `> Task ` lines belong to the Gradle route even when the fragment is
    // not a valid task record: the generic progress rules must not turn
    // quoted or malformed headers into template rows.
    let anchor = "$ ./gradlew build\n";
    let tail = (1..=12)
        .map(|index| format!("processing asset {index} of 12\n"))
        .collect::<String>();
    for fragment in [
        // A path with dot-separated numeric segments that main's
        // `valid_task_path` accepts; the prefix rule rejects nothing wider.
        "> Task :1.2.3 UP-TO-DATE\n",
        // Quoted / malformed fragments that are not task records at all.
        "> Task > quoted fragment 12 running\n",
        "> Task :: UP-TO-DATE\n",
    ] {
        let repeats = fragment.repeat(12);
        let input = format!("{anchor}{tail}{repeats}");
        let store = InMemoryStore::new();
        let outcome =
            BuildLogCompressor.compress_with_recovery(&input, Some(&store), &RecoveryMethod::Shell);
        let cleaned = oracle::strip_sgr(&input);
        // The fragment itself must remain in the output verbatim.
        assert!(
            outcome
                .output
                .contains(&oracle::strip_sgr(fragment.trim_end())),
            "fragment text missing from output: {fragment:?}"
        );
        // Whichever lines were stashed must be exactly the retrievable
        // progress tail, never the fragment.
        for key in outcome.stash_writes.iter().map(|w| &w.key) {
            let payload = store.retrieve(key).unwrap().expect("stashed payload");
            assert!(
                !payload.contains("> Task "),
                "stashed payload contains a task fragment: {key}"
            );
        }
        assert!(matches!(
            outcome.recoverability,
            Recoverability::Lossless | Recoverability::Retrievable
        ));
        let _ = cleaned;
        // A second pass must be stable.
        let again = BuildLogCompressor.compress_with_recovery(
            &outcome.output,
            Some(&store),
            &RecoveryMethod::Shell,
        );
        assert_eq!(again.output, outcome.output, "{fragment:?}");
    }
}
