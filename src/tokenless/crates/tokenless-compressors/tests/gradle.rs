//! Independent reconstruction and boundary checks for Gradle structural compaction.

use tokenless_ccr::{InMemoryStore, RecoveryMethod, StashStore};
use tokenless_compressors::{BuildLogCompressor, BuildLogOperation, Recoverability};
use tokenless_protocol::estimate_tokens;

const HEADER: &str = "Gradle templates (id<TAB>fields; interleave with literals): ";
const HEAD: &str = "$ ./gradlew build\nWelcome to Gradle 8.14.2!\nStarting a Gradle Daemon (subsequent builds will be faster)\n";

#[path = "common/gradle_oracle.rs"]
mod oracle;
use oracle::decode;

fn tasks(count: usize, path: &str, status: &str) -> String {
    (0..count)
        .map(|index| format!("> Task {path}{index}{status}\n"))
        .collect()
}

fn check(input: &str, reduced: bool) -> String {
    let mut expected = None;
    for method in [RecoveryMethod::None, RecoveryMethod::Shell] {
        for store_enabled in [false, true] {
            let store = InMemoryStore::new();
            let outcome = BuildLogCompressor.compress_with_recovery(
                input,
                store_enabled.then_some(&store as &dyn StashStore),
                &method,
            );
            assert_eq!(decode(&outcome.output), input);
            assert_eq!(outcome.recoverability, Recoverability::Lossless);
            assert!(outcome.stash_writes.is_empty());
            assert_eq!(store.len(), 0);
            assert_eq!(outcome.metrics.omitted_blocks, 0);
            if reduced {
                assert!(outcome.output.chars().count() < input.chars().count());
                assert!(estimate_tokens(&outcome.output) < estimate_tokens(input));
                assert_eq!(outcome.operations, [BuildLogOperation::Reduction]);
            } else {
                assert_eq!(outcome.output, input);
                assert!(outcome.operations.is_empty());
            }
            let again =
                BuildLogCompressor.compress_with_recovery(&outcome.output, Some(&store), &method);
            assert_eq!(again.output, outcome.output);
            assert!(again.operations.is_empty());
            assert!(again.stash_writes.is_empty());
            if let Some(previous) = &expected {
                assert_eq!(&outcome.output, previous)
            }
            expected = Some(outcome.output);
        }
    }
    expected.unwrap()
}

#[test]
fn two_task_lines_are_the_only_detection_evidence_and_need_log_context() {
    let block = tasks(12, ":core:compileJava", " UP-TO-DATE");
    assert!(BuildLogCompressor::detect(&block));
    check(&format!("{HEAD}{block}"), true);
    // A single task line is not evidence.
    assert!(!BuildLogCompressor::detect(&tasks(1, ":build", "")));
    check(&tasks(1, ":build", ""), false);
    // Command/exit anchors alone do not turn task lines into a log.
    let single = tasks(1, ":build", "");
    assert!(!BuildLogCompressor::detect(&format!(
        "$ ./gradlew build\n{single}exit code: 0\n"
    )));
    // Quoted or fenced examples never claim the route.
    for prefix in ["Example output:\n", "```text\n", "~~~text\n"] {
        let fenced_block = tasks(4, ":core:compile", " UP-TO-DATE");
        assert!(!BuildLogCompressor::detect(&format!(
            "{prefix}{fenced_block}"
        )));
    }
}

#[test]
fn structured_preambles_preserve_gradle_compaction_within_head_budget() {
    let captured = tasks(12, ":core:compileJava", " UP-TO-DATE");
    for preamble in [
        "Picked up JAVA_TOOL_OPTIONS: -XX:ActiveProcessorCount=2\n",
        "OpenJDK 64-Bit Server VM warning: Sharing is only supported for boot loader classes because bootstrap classpath has been appended\n",
        "WARNING: An illegal reflective access operation has occurred\n\
         WARNING: Illegal reflective access by com.google.inject.internal.cglib.core.$ReflectUtils$1 (file:/opt/guice.jar) to method java.lang.ClassLoader.defineClass()\n\
         WARNING: All illegal access operations will be denied in a future release\n",
        "Downloading https://services.gradle.org/distributions/gradle-8.14.2-bin.zip\nUnzipping https://services.gradle.org/distributions/gradle-8.14.2-bin.zip\nWelcome to Gradle 8.14.2!\n",
        "Starting a Gradle Daemon (subsequent builds will be faster)\n",
        "Configuration cache is an incubating feature\nCalculating task graph as no cached configuration is available for tasks: build\n",
        "> Configure project :\n> Configure project :core\n",
        "+ ./gradlew build\n",
        "++ ./gradlew --parallel check\n",
    ] {
        let input = format!("{preamble}{captured}");
        assert!(BuildLogCompressor::detect(&input), "{preamble}");
        let outcome = BuildLogCompressor.compress(&input, None);
        assert_eq!(decode(&outcome.output), input);
        assert!(estimate_tokens(&outcome.output) < estimate_tokens(&input));
    }
    let rejected = "Downloading a random tarball from https://services.gradle.org/distributions/gradle-0.1-bin.zip\n";
    assert!(!BuildLogCompressor::detect(&format!(
        "{rejected}{}",
        tasks(4, ":build", "")
    )));
}

#[test]
fn fenced_blocks_cannot_supply_or_bridge_gradle_evidence() {
    let body = tasks(12, ":core:compileJava", " UP-TO-DATE");
    let fenced = format!("```text\n{body}```\n");
    assert!(!BuildLogCompressor::detect(&fenced));
    check(&fenced, false);
    let split = "> Task :one\n```text\nquoted\n```\n> Task :two\n".to_string();
    assert!(!BuildLogCompressor::detect(&split));
    check(&split, false);
    // Two unfenced lines after the fence are evidence on their own; the
    // fence does not extend or bridge them.
    let after = "> Task :one\n```text\nquoted\n```\n> Task :two\n> Task :three\n".to_string();
    assert!(BuildLogCompressor::detect(&after));
    // Once unfenced strong evidence is present in the head, tail fences no
    // longer disqualify the log.
    let filler = "configuration status line\n".repeat(220);
    let head_evidence =
        "> Task :core:compileJava UP-TO-DATE\n> Task :core:test UP-TO-DATE\n".to_string();
    let with_fences = format!("{head_evidence}{filler}```\n");
    assert!(BuildLogCompressor::detect(&with_fences));
    let open_head = format!("```text\n{head_evidence}{filler}");
    assert!(!BuildLogCompressor::detect(&open_head));
}

#[test]
fn failed_tasks_stay_verbatim_adjacent_to_their_diagnostics() {
    let before = tasks(6, ":core:compileJava", " UP-TO-DATE");
    let failure = "> Task :core:test FAILED\n\norg.example.SampleTest > missingMethod FAILED\n    java.lang.NoSuchMethodError at SampleTest.java:12\n\n1 test completed, 1 failed\n";
    let after = tasks(6, ":core:jar", "");
    let input = format!("{HEAD}{before}{failure}{after}");
    let outcome = BuildLogCompressor.compress(&input, None);
    assert_eq!(decode(&outcome.output), input);
    assert!(outcome.output.contains(failure));
    // Compaction still happens outside the failure scope.
    assert!(estimate_tokens(&outcome.output) < estimate_tokens(&input));
    // The FAILED line and its diagnostics keep their exact adjacency.
    let position = outcome.output.find("> Task :core:test FAILED").unwrap();
    let decoded = decode(&outcome.output);
    let decoded_position = decoded.find("> Task :core:test FAILED").unwrap();
    assert_eq!(
        &decoded[decoded_position..decoded_position + failure.len()],
        failure
    );
    let _ = position;
}

#[test]
fn failure_and_unknown_output_close_scope_and_two_valid_lines_reopen() {
    for boundary in [
        "[directory] Some warning about deprecation\n",
        "The twCompileOptions property is deprecated.\n",
        "com.example.Test > case PASSED\n",
        "java.lang.IllegalStateException: boom\n\tat com.example.Boom.run(Boom.java:8)\n",
        "unrecognized plugin body\n",
        "Iteration 1\r\n",
    ] {
        let before = tasks(6, ":core:compileJava", " UP-TO-DATE");
        let after = tasks(6, ":core:jar", " FROM-CACHE");
        let input = format!("{HEAD}{before}{boundary}{after}");
        let output = check(&input, true);
        // The boundary and everything after it stays; compaction only
        // reopens after two consecutive valid non-failure task lines.
        assert!(
            output.ends_with(boundary.trim_end_matches('\n')) || output.contains(boundary),
            "{boundary}"
        );
    }
    // A single valid line after the boundary stays verbatim; a run of two
    // or more consecutive valid lines reopens compaction.
    let before = tasks(12, ":core:compileJava", " UP-TO-DATE");
    let single = format!("{HEAD}{before}warning body\n> Task :core:one\nBUILD SUCCESSFUL in 2s\n");
    let output = check(&single, true);
    assert!(output.contains("> Task :core:one"));
    let reopened = format!("{HEAD}{before}warning body\n{}", tasks(12, ":core:one", ""));
    let output = check(&reopened, true);
    assert!(!output.contains("> Task :core:one0"));
}

#[test]
fn crlf_only_logs_stay_on_the_other_detectors() {
    let lf = format!("{HEAD}{}", tasks(6, ":core:compileJava", " UP-TO-DATE"));
    assert!(BuildLogCompressor::detect(&lf));
    let crlf = lf.replace('\n', "\r\n");
    assert!(!BuildLogCompressor::detect(&crlf));
    check(&crlf, false);
}

#[test]
fn unterminated_and_control_records_close_the_run() {
    // An unterminated final line is never a complete record. The head/tail
    // sample cannot see line terminators, so detection may still route the
    // log, but the planner works on split-inclusive lines and never edits
    // a run that ends in an unterminated record.
    let unterminated = format!("{HEAD}> Task :core:compileJava UP-TO-DATE\n> Task :core:jar");
    check(&unterminated, false);
    // A bounded run before an unterminated line still compacts; the
    // unterminated line itself stays verbatim.
    let before = tasks(12, ":core:compileJava", " UP-TO-DATE");
    let mixed = format!("{HEAD}{before}> Task :core:jar");
    let output = check(&mixed, true);
    assert!(output.ends_with("> Task :core:jar"));
    // SGR-only pollution is covered by the dedicated cleanup test.
    for bad in [
        "> Task :core:compileJava UP-TO-DATE\r\n",
        "> Task :core:compileJava\tUP-TO-DATE\n",
    ] {
        let tail = tasks(12, ":core:jar", "");
        let input = format!("{HEAD}{before}{bad}{tail}");
        let output = check(&input, true);
        assert!(output.contains(bad.trim_end_matches('\r')), "{bad:?}");
    }
}

#[test]
fn oversized_lines_and_blocks_are_rejected_atomically() {
    check(
        &format!("{HEAD}{}", tasks(256, ":core:compileJava", " UP-TO-DATE")),
        true,
    );
    check(
        &format!("{HEAD}{}", tasks(257, ":core:compileJava", " UP-TO-DATE")),
        false,
    );
    // One line over the per-line byte budget rejects every edit atomically,
    // including edits for bounded runs far away from the oversized line.
    let block = tasks(12, ":core:compileJava", " UP-TO-DATE");
    let base = "> Task :core:compileJava UP-TO-DATE\n";
    for line_bytes in [65536, 65537] {
        let giant = format!(
            "> Task :core:compileJava UP-TO-DATE{}\n",
            "x".repeat(line_bytes - base.len())
        );
        assert_eq!(giant.len(), line_bytes);
        check(&format!("{HEAD}{block}{giant}"), line_bytes == 65536);
    }
}

#[test]
fn duplicate_task_names_statuses_and_exact_order_are_preserved() {
    let rows = [
        (":core:compileJava", " UP-TO-DATE"),
        (":core:compileJava", " UP-TO-DATE"),
        (":core:compileJava", " FROM-CACHE"),
        (":core:test", " SKIPPED"),
        (":core:test", " NO-SOURCE"),
        (":core:test", ""),
    ];
    let input = format!(
        "{HEAD}{}",
        rows.iter()
            .map(|(path, status)| format!("> Task {path}{status}\n"))
            .collect::<String>()
            .repeat(6)
    );
    let output = check(&input, true);
    // Reconstructed order and every duplicate survive decode.
    assert_eq!(decode(&output), input);
    let decoded = decode(&output);
    for (path, status) in rows {
        assert!(decoded.contains(&format!("> Task {path}{status}")));
    }
}

#[test]
fn shared_module_prefixes_are_factored_at_path_boundaries() {
    let rows = (0..5)
        .map(|index| {
            format!(
                "> Task :library-core-module{:02}:compileJava UP-TO-DATE\n",
                index
            )
        })
        .collect::<String>();
    let input = format!("{HEAD}{rows}");
    let output = check(&input, true);
    assert!(output.contains(HEADER));
    // The shared prefix is stored in the literal, and the decoder alone
    // reassembles full paths.
    assert_eq!(decode(&output), input);
    let factor = output
        .lines()
        .find(|line| line.starts_with(HEADER))
        .unwrap();
    // The prefix ends at a `-` boundary, so the decoder reassembles the
    // remaining `module00..` segment from each row's tail.
    assert!(factor.contains("library-core-"), "{factor}");
    assert!(decode(&output).contains("> Task :library-core-module00:compileJava UP-TO-DATE"));
}

#[test]
fn non_task_output_between_two_task_lines_breaks_the_run() {
    let a = tasks(12, ":core:compileJava", " UP-TO-DATE");
    let b = tasks(12, ":core:jar", " FROM-CACHE");
    let input = format!("{HEAD}{a}BUILD SUCCESSFUL in 2s\n{b}");
    let output = check(&input, true);
    assert!(output.contains("BUILD SUCCESSFUL in 2s"));
    assert_eq!(output.matches(HEADER).count(), 2);
}

#[test]
fn unedited_gradle_logs_retain_anchored_generic_recovery() {
    let routine = (0..60)
        .map(|index| format!("compiling module {index} of the deployment with fairly long text\n"))
        .collect::<String>();
    let input = format!(
        "$ ./gradlew build\n{routine}{}exit code: 0\n",
        tasks(1, ":build", "")
    );
    for method in [
        RecoveryMethod::Shell,
        RecoveryMethod::tool("retrieve_log").unwrap(),
    ] {
        let store = InMemoryStore::new();
        let outcome = BuildLogCompressor.compress_with_recovery(&input, Some(&store), &method);
        assert_eq!(outcome.recoverability, Recoverability::Retrievable);
        assert_eq!(outcome.metrics.omitted_blocks, 1);
        assert_eq!(outcome.stash_writes.len(), 1);
        let payload = store
            .retrieve(&outcome.stash_writes[0].key)
            .unwrap()
            .unwrap();
        let marker = outcome
            .output
            .split_inclusive('\n')
            .find(|line| line.contains("lines omitted."))
            .unwrap();
        assert_eq!(outcome.output.replacen(marker, &payload, 1), input);
        assert!(estimate_tokens(&outcome.output) < estimate_tokens(&input));
    }
    for method in [RecoveryMethod::None, RecoveryMethod::Shell] {
        let store = InMemoryStore::new();
        let stash = (method == RecoveryMethod::None).then_some(&store as &dyn StashStore);
        let unanchored = input
            .strip_prefix("$ ./gradlew build\n")
            .unwrap()
            .rsplit_once("exit code:")
            .unwrap()
            .0
            .to_owned();
        for (outcome, expected) in [
            (
                BuildLogCompressor.compress_with_recovery(&input, stash, &method),
                &input,
            ),
            (
                BuildLogCompressor.compress(&unanchored, Some(&store)),
                &unanchored,
            ),
        ] {
            assert_eq!(outcome.output, *expected);
            assert!(outcome.stash_writes.is_empty());
        }
        assert_eq!(store.len(), 0);
    }
}

#[test]
fn gradle_task_lines_do_not_enter_generic_progress_templates() {
    // Repeated identical `> Task :` lines are not Generic progress evidence.
    let rows = "> Task :core:compileJava UP-TO-DATE\n".repeat(30);
    let input = format!("$ ./gradlew build\n{rows}exit code: 0\n");
    let outcome = check(&input, true);
    assert!(outcome.contains(HEADER));
}

#[test]
fn sgr_cleanup_is_the_only_change_to_protected_gradle_records() {
    let rows = tasks(6, ":core:compileJava", " UP-TO-DATE");
    let colored = format!("{HEAD}{rows}").replace("> Task", "\x1b[32m> Task\x1b[0m");
    let outcome = BuildLogCompressor.compress(&colored, None);
    assert_eq!(
        outcome.operations,
        [
            BuildLogOperation::TerminalCleanup,
            BuildLogOperation::Reduction
        ]
    );
    assert_eq!(decode(&outcome.output), format!("{HEAD}{rows}"));
    assert!(outcome.stash_writes.is_empty());
}

#[test]
fn malformed_task_records_are_never_edited() {
    for bad in [
        "> Task :\n",
        "> Task ::\n",
        "> Task :path extra\n",
        "> Task :path FAILED\n",
        "> Task :path UP-TO-DATE EXTRA\n",
        "> Task :path UNKNOWN\n",
    ] {
        let tail = tasks(12, ":core:jar", " UP-TO-DATE");
        let input = format!("{HEAD}{}{bad}{tail}", tasks(12, ":core:compileJava", ""));
        // The malformed record stays verbatim between two compacted runs.
        let output = check(&input, true);
        assert_eq!(decode(&output), input);
        assert!(decode(&output).contains(bad.trim_end()));
    }
    let input = format!("{HEAD}{}", tasks(6, ":core:compileJava", " UP-TO-DATE"));
    let truncated = input.trim_end_matches('\n');
    let outcome = BuildLogCompressor.compress(truncated, None);
    assert_eq!(decode(&outcome.output), truncated);
}

#[test]
fn edit_limit_retains_completed_edits_and_the_remaining_suffix() {
    let block = format!("{HEAD}{}", tasks(8, ":core:compileJava", " UP-TO-DATE"));
    for count in [255, 256, 257] {
        let input = block.repeat(count);
        let output = check(&input, true);
        assert_eq!(output.matches(HEADER).count(), count.min(256));
        if count > 256 {
            assert!(output.ends_with(&block));
        }
    }
}

#[test]
fn mutation_at_every_task_boundary_keeps_the_opaque_suffix() {
    let original = tasks(12, ":core:compileJava", " UP-TO-DATE");
    let rows = original.split_inclusive('\n').collect::<Vec<_>>();
    for index in 0..=rows.len() {
        for boundary in [
            "[WARN] injected\n",
            "java.lang.Exception: injected\n",
            "unrecognized text\n",
        ] {
            let suffix = rows[index..].concat();
            let input = format!("{HEAD}{}{boundary}{suffix}", rows[..index].concat());
            let outcome = BuildLogCompressor.compress(&input, None);
            // The boundary is never absorbed into a template block.
            assert_eq!(decode(&outcome.output), input);
            assert!(outcome.output.contains(boundary.trim_end()));
            assert_eq!(
                BuildLogCompressor.compress(&outcome.output, None).output,
                outcome.output
            );
        }
    }
}

#[test]
fn captured_streams_round_trip_without_stash_and_are_idempotent() {
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build_logs/gradle");
    let mut paths = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension().is_some_and(|extension| extension == "txt")
                && !path.to_string_lossy().ends_with(".expected.txt")
        })
        .collect::<Vec<_>>();
    // Optional complete local capture corpus; committed fixtures remain the CI gate.
    if let Ok(root) = std::env::var("GRADLE_CAPTURE_ROOT") {
        for entry in std::fs::read_dir(root).unwrap() {
            let directory = entry.unwrap().path();
            for mode in [
                "cold",
                "incremental",
                "clean_rebuild",
                "failure_unknown_task",
            ] {
                for stream in ["stdout", "stderr", "observed"] {
                    let path = directory.join(mode).join(format!("{stream}.txt"));
                    if path.exists() {
                        paths.push(path);
                    }
                }
            }
        }
    }
    paths.sort();
    for path in paths {
        let raw = std::fs::read_to_string(&path).unwrap();
        let cleaned = strip_sgr(&raw);
        let store = InMemoryStore::new();
        let outcome = BuildLogCompressor.compress(&raw, Some(&store));
        assert_eq!(decode(&outcome.output), cleaned, "{}", path.display());
        assert!(outcome.stash_writes.is_empty(), "{}", path.display());
        assert_eq!(store.len(), 0);
        assert_eq!(outcome.recoverability, Recoverability::Lossless);
        let again = BuildLogCompressor.compress(&outcome.output, Some(&store));
        assert_eq!(again.output, outcome.output, "{}", path.display());
        if outcome.output != raw {
            assert!(estimate_tokens(&outcome.output) < estimate_tokens(&raw));
            assert!(outcome.output.chars().count() < raw.chars().count());
        }
        println!(
            "{}: {} -> {}",
            path.display(),
            estimate_tokens(&raw),
            estimate_tokens(&outcome.output)
        );
    }
}

fn strip_sgr(input: &str) -> String {
    // Remove only independently specified SGR escapes before comparison,
    // mirroring the Maven oracle's contract; the Gradle grammar itself
    // never accepts control sequences.
    regex::Regex::new(r"\x1b\[[0-9;]*m")
        .unwrap()
        .replace_all(input, "")
        .into_owned()
}

#[test]
#[ignore = "rewrites reviewed goldens; run explicitly after an intentional format change"]
fn regenerate_gradle_goldens() {
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build_logs/gradle");
    let name = "mockito_incremental.txt";
    let path = directory.join(name);
    let raw = std::fs::read_to_string(&path).unwrap();
    let outcome = BuildLogCompressor.compress(&raw, None);
    assert_eq!(decode(&outcome.output), strip_sgr(&raw));
    std::fs::write(path.with_extension("expected.txt"), outcome.output).unwrap();
}
