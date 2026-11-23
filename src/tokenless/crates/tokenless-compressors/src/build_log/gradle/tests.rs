use super::super::scope::MAX_LINE_BYTES;
use super::super::{BuildLogCompressor, BuildLogOperation, sample_lines};
use super::*;
use crate::Recoverability;
use tokenless_ccr::{InMemoryStore, RecoveryMethod, StashStore};

#[path = "../../../tests/common/gradle_oracle.rs"]
mod oracle;

use oracle::{decode, strip_sgr};

fn synthetic_gradle_log(modules: usize) -> String {
    let mut output = String::new();
    for idx in 0..modules {
        output.push_str(&format!(
            "> Task :svc-module-{idx:02}:compileJava UP-TO-DATE\n\
             > Task :svc-module-{idx:02}:processResources NO-SOURCE\n\
             > Task :svc-module-{idx:02}:classes UP-TO-DATE\n"
        ));
    }
    output.push_str("BUILD SUCCESSFUL in 4s\n");
    output.push_str(&format!(
        "{total} actionable tasks: {total} up-to-date\n",
        total = modules * 3
    ));
    output
}

fn compress_lossless(input: &str) -> super::super::BuildLogOutcome {
    BuildLogCompressor.compress_with_recovery(input, None, &RecoveryMethod::None)
}

#[test]
fn static_grammars_compile() {
    assert!(BUILD_STATUS.is_ok(), "{BUILD_STATUS:?}");
    assert!(ACTIONABLE_TASKS.is_ok(), "{ACTIONABLE_TASKS:?}");
}

#[test]
fn single_lines_prose_quotes_fences_and_crlf_do_not_trigger_gradle_detection() {
    for negative in [
        "> Task :app:compileJava\n",
        "> Task :app:test FAILED\n",
        "> Configure project :app\n",
        "> Configure project :\nBuilding version '1.0.0'\n",
        "BUILD SUCCESSFUL in 1s\n",
        "1 actionable task: 1 executed\n",
        "Gradle BUILD SUCCESSFUL in 2s\n",
        "let log = \"> Task :app:compileJava UP-TO-DATE\";\n",
        "> > Task :app:compileJava UP-TO-DATE\n> > Task :app:test UP-TO-DATE\n",
        "```text\n> Task :app:compileJava UP-TO-DATE\n> Task :app:test UP-TO-DATE\n```\n",
        "~~~text\n> Task :app:compileJava UP-TO-DATE\n> Task :app:test UP-TO-DATE\n~~~\n",
        "+++ b/gradlew\n> Task :app:compileJava UP-TO-DATE\n> Task :app:test UP-TO-DATE\n",
        "Welcome to Gradle 8.14.2!\n\nStarting a Gradle Daemon\n",
        "$ ./gradlew build\nWelcome to Gradle 8.14.2!\n",
        "Exit code: 0 means ok\n> Task :app:compileJava UP-TO-DATE\n> Task :app:test UP-TO-DATE\n",
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

    let valid = synthetic_gradle_log(4);
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
fn valid_preambles_preserve_detection_and_stay_verbatim() {
    let body = synthetic_gradle_log(8);
    let welcome_preamble = concat!(
        "Fetching distribution (retrying 3 times, with an initial back off of 5000 ms).\n",
        "Downloading https://services.gradle.org/distributions/gradle-9.8.1-bin.zip\n",
        "..............10%..............20%..............100%\n",
        "\n",
        "Welcome to Gradle 9.8.1!\n",
        "\n",
        "Here are the highlights of this release:\n",
        " - Java 27 support\n",
        " - Maven mirror settings reuse\n",
        "\n",
        "For more details see https://docs.gradle.org/9.8.1/release-notes.html\n",
        "\n",
        "Starting a Gradle Daemon (subsequent builds will be faster)\n",
        "Parallel Configuration Cache is an incubating feature.\n",
        "Configuration on demand is an incubating feature.\n",
    );
    let preambles = [
        "$ ./gradlew build\n",
        "Exit code: 0\n",
        "exit status: 0\n",
        "+ ./gradlew clean build\n",
        "++ GRADLE_OPTS=-Xmx2g ./gradlew build\n",
        "Picked up JAVA_TOOL_OPTIONS: -Xmx2g\n",
        "OpenJDK 64-Bit Server VM warning: Sharing is only supported for boot loader classes\n",
        welcome_preamble,
        "> Configure project :app\n",
        "> Configure project :\nBuilding version '1.0.0'\n",
    ];
    for preamble in preambles {
        let input = format!("{preamble}{body}");
        assert!(
            BuildLogCompressor::detect(&input),
            "failed BuildLogCompressor::detect with preamble: {preamble}"
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
fn failed_tasks_stay_verbatim_outside_template_blocks() {
    let input = format!(
        "{}\
         > Task :svc-module-09:test FAILED\n\
         FAILURE: Build failed with an exception.\n\
         * What went wrong:\n\
         Execution failed for task ':svc-module-09:test'.\n\
         > There were failing tests.\n\
         > Task :svc-module-10:compileJava UP-TO-DATE\n\
         > Task :svc-module-10:classes UP-TO-DATE\n\
         > Task :svc-module-10:jar UP-TO-DATE\n\
         > Task :svc-module-10:assemble UP-TO-DATE\n",
        synthetic_gradle_log(8)
    );
    let outcome = compress_lossless(&input);
    assert_eq!(decode(&outcome.output), input);
    assert!(
        outcome
            .output
            .contains("> Task :svc-module-09:test FAILED\n")
    );
    assert!(
        outcome
            .output
            .contains("FAILURE: Build failed with an exception.\n")
    );
    assert!(
        outcome
            .output
            .contains("Execution failed for task ':svc-module-09:test'.\n")
    );
}

#[test]
fn boundary_mutations_close_scope_and_round_trip_at_every_line() {
    let mut base_lines = Vec::new();
    for idx in 0..10 {
        base_lines.push(format!(
            "> Task :org-app-mod-{idx:02}:compileJava UP-TO-DATE\n"
        ));
        base_lines.push(format!(
            "> Task :org-app-mod-{idx:02}:processResources NO-SOURCE\n"
        ));
        base_lines.push(format!("> Task :org-app-mod-{idx:02}:classes UP-TO-DATE\n"));
    }
    let injections = [
        "warning: [unchecked] unchecked call\n",
        "w: file:///work/src/Main.kt:10:5: Variable 'x' is never used\n",
        "Note: Recompile with -Xlint:unchecked for details.\n",
        "Deprecated Gradle features were used in this build, making it incompatible with Gradle 9.0.\n",
        "java.lang.IllegalStateException: boom\n\tat com.example.App.main(App.java:12)\n",
        "> Task :org-app-mod-99:test FAILED\n",
        "progress 50%\rprogress 100%\n",
        "  \x1b[0K\x1b[32m42 passing\x1b[0m\n",
        "\tindented tab output\n",
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
                "injected diagnostic lost at pos={pos}: {injected_clean:?}"
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
    let valid = synthetic_gradle_log(12);
    let truncated = valid.trim_end_matches('\n');
    let outcome = compress_lossless(truncated);
    assert_eq!(decode(&outcome.output), truncated);
    assert!(
        outcome
            .output
            .ends_with(truncated.rsplit('\n').next().unwrap())
    );

    let long_task = format!(
        "> Task :{}:compileJava UP-TO-DATE\n",
        "a".repeat(MAX_LINE_BYTES)
    );
    let with_long = format!(
        "{}{long_task}{}",
        synthetic_gradle_log(6),
        synthetic_gradle_log(6)
    );
    let long_outcome = compress_lossless(&with_long);
    assert_eq!(decode(&long_outcome.output), with_long);
    assert!(long_outcome.output.contains(&long_task));
    assert!(long_outcome.output.contains(HEADER));
    assert!(
        tokenless_protocol::estimate_tokens(&long_outcome.output)
            < tokenless_protocol::estimate_tokens(&with_long)
    );

    // Any single line exceeding MAX_BYTES (64 KiB) suppresses Gradle edits across the result.
    let huge_line = format!("diagnostic payload: {}\n", "x".repeat(MAX_BYTES + 1));
    let with_huge = format!(
        "{}{huge_line}{}",
        synthetic_gradle_log(6),
        synthetic_gradle_log(6)
    );
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

    // 300 and 420 contiguous tasks exceed MAX_ROWS (256) and must split cleanly into <=256-row blocks.
    for modules in [100, 140] {
        let large = synthetic_gradle_log(modules);
        let large_lines = large.split_inclusive('\n').collect::<Vec<_>>();
        let large_edits = plan(&large_lines);
        assert_eq!(large_edits.len(), 2);
        for edit in &large_edits {
            assert!(edit.end - edit.start <= MAX_ROWS);
            assert_eq!(
                decode(&edit.replacement),
                large_lines[edit.start..edit.end].concat()
            );
        }
        let large_outcome = compress_lossless(&large);
        assert_eq!(decode(&large_outcome.output), large);
        assert!(
            tokenless_protocol::estimate_tokens(&large_outcome.output)
                < tokenless_protocol::estimate_tokens(&large)
        );
    }

    // >64 KiB contiguous task block under 256 rows splits cleanly at MAX_BYTES.
    let wide_prefix = "wide-module-segment-".repeat(48);
    let mut wide_bytes_log = String::new();
    for idx in 0..80 {
        wide_bytes_log.push_str(&format!(
            "> Task :{wide_prefix}{idx:03}:compileJava UP-TO-DATE\n"
        ));
    }
    let wide_lines = wide_bytes_log.split_inclusive('\n').collect::<Vec<_>>();
    let wide_edits = plan(&wide_lines);
    assert_eq!(wide_edits.len(), 2);
    for edit in &wide_edits {
        let orig = wide_lines[edit.start..edit.end].concat();
        assert!(orig.len() <= MAX_BYTES);
        assert_eq!(decode(&edit.replacement), orig);
    }
    let wide_outcome = compress_lossless(&wide_bytes_log);
    assert_eq!(decode(&wide_outcome.output), wide_bytes_log);
    assert!(
        tokenless_protocol::estimate_tokens(&wide_outcome.output)
            < tokenless_protocol::estimate_tokens(&wide_bytes_log)
    );

    // Exceeding MAX_EDITS (256) compacts the first 256 blocks and leaves the rest verbatim.
    let mut many_blocks = String::new();
    for block in 0..260 {
        for sub in 0..4 {
            many_blocks.push_str(&format!(
                "> Task :svc-block-{block:03}-m{sub}:compileJava UP-TO-DATE\n\
                 > Task :svc-block-{block:03}-m{sub}:processResources NO-SOURCE\n\
                 > Task :svc-block-{block:03}-m{sub}:classes UP-TO-DATE\n"
            ));
        }
        many_blocks.push_str(&format!("custom boundary {block}\n"));
    }
    let many_lines = many_blocks.split_inclusive('\n').collect::<Vec<_>>();
    let many_edits = plan(&many_lines);
    assert_eq!(many_edits.len(), MAX_EDITS);
    let many_outcome = compress_lossless(&many_blocks);
    assert_eq!(decode(&many_outcome.output), many_blocks);
    assert!(
        many_outcome
            .output
            .contains("> Task :svc-block-259-m0:compileJava UP-TO-DATE\n")
    );
    assert_eq!(
        compress_lossless(&many_outcome.output).output,
        many_outcome.output
    );
}

#[test]
fn uneditable_anchored_gradle_retains_generic_recovery_while_task_headers_stay_protected() {
    let progress = (1..=12)
        .map(|i| {
            format!("processing custom asset bundle {i} of 12 inside Gradle custom generator\n")
        })
        .collect::<String>();
    let anchored = format!(
        "$ ./gradlew generateAssets\n\
         > Task :app:compilingStep1\n\
         {progress}\
         > Task :app:runningStep2\n\
         BUILD SUCCESSFUL in 2s\n\
         2 actionable tasks: 2 executed\n\
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

    // Failed or isolated numbered task headers must never trigger Generic omission.
    let failed_tasks = (1..=12)
        .map(|i| format!("> Task :svc-{i}:runningTests FAILED\n"))
        .collect::<String>();
    let failed_anchored = format!("$ ./gradlew check\n{failed_tasks}exit code: 1\n");
    let failed_store = InMemoryStore::default();
    let failed_outcome = BuildLogCompressor.compress_with_recovery(
        &failed_anchored,
        Some(&failed_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(failed_outcome.output, failed_anchored);
    assert!(failed_outcome.stash_writes.is_empty());

    // Normal multi-task run containing `running`/`building` names stays Lossless with 0 Stash.
    let named_tasks = (1..=12)
        .map(|i| format!("> Task :svc-{i}:runningTests UP-TO-DATE\n"))
        .collect::<String>();
    let named_log = format!("$ ./gradlew test\n{named_tasks}exit code: 0\n");
    let named_store = InMemoryStore::default();
    let named_outcome = BuildLogCompressor.compress_with_recovery(
        &named_log,
        Some(&named_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(named_outcome.recoverability, Recoverability::Lossless);
    assert!(named_outcome.stash_writes.is_empty());
    assert_eq!(decode(&named_outcome.output), named_log);
}

#[test]
fn fenced_task_examples_inside_valid_gradle_logs_stay_verbatim() {
    let example: String = (0..12)
        .map(|i| format!("> Task :example-module-{i}:compileJava UP-TO-DATE\n"))
        .collect();
    let fenced = format!("```text\n{example}```\n");
    let doc_log = format!(
        "> Task :app:prepare\n> Task :app:printDocumentation\n{fenced}\
         BUILD SUCCESSFUL in 1s\n2 actionable tasks: 2 executed\n"
    );
    assert!(BuildLogCompressor::detect(&doc_log));
    let doc_outcome = compress_lossless(&doc_log);
    assert!(doc_outcome.output.contains(&fenced));
    assert_eq!(decode(&doc_outcome.output), doc_log);

    // Unfenced task blocks before and after nested/info-less fences still compact, while
    // every line inside the fences (including inner fences and `\t` lines) stays verbatim.
    let before = synthetic_gradle_log(4);
    let after = synthetic_gradle_log(4);
    let md_fenced = format!("````markdown\n```text\n{example}\tindented task note\n```\n````\n");
    let mixed = format!("{before}{md_fenced}{after}");
    assert!(BuildLogCompressor::detect(&mixed));
    let mixed_outcome = compress_lossless(&mixed);
    assert!(mixed_outcome.output.contains(&md_fenced));
    assert!(mixed_outcome.output.contains(HEADER));
    assert_eq!(decode(&mixed_outcome.output), mixed);
    assert_eq!(
        compress_lossless(&mixed_outcome.output).output,
        mixed_outcome.output
    );

    // A fenced literal matching HEADER does not suppress compaction of surrounding tasks,
    // and the shared oracle keeps fenced literals verbatim during reconstruction.
    let quoted_header =
        format!("```text\n{HEADER}{{\"rows\":1,\"literals\":[[\"example\"]]}}\n0\n```\n");
    let with_quoted_header = format!("{before}{quoted_header}{after}");
    let quoted_outcome = compress_lossless(&with_quoted_header);
    assert!(quoted_outcome.output.contains(&quoted_header));
    assert!(quoted_outcome.output.matches(HEADER).count() >= 2);
    assert_eq!(decode(&quoted_outcome.output), with_quoted_header);
    assert_eq!(
        compress_lossless(&quoted_outcome.output).output,
        quoted_outcome.output
    );

    let malformed_quoted = format!("```text\n{HEADER}not-json\n\t0\n```\n");
    let with_malformed_quoted = format!("{before}{malformed_quoted}{after}");
    let malformed_outcome = compress_lossless(&with_malformed_quoted);
    assert!(malformed_outcome.output.contains(&malformed_quoted));
    assert_eq!(decode(&malformed_outcome.output), with_malformed_quoted);

    // An unclosed fence keeps all trailing task headers verbatim to EOF.
    let unclosed = format!("{before}~~~text\n{example}");
    let unclosed_outcome = compress_lossless(&unclosed);
    assert!(
        unclosed_outcome
            .output
            .contains(&format!("~~~text\n{example}"))
    );
    assert_eq!(decode(&unclosed_outcome.output), unclosed);
}

#[test]
fn over_4kib_generated_template_header_stays_idempotent_and_lossless() {
    let mut input = "$ ./gradlew build\n".to_owned();
    for module in 0..3 {
        for variant in 0..70 {
            let name = format!("generateVariant{variant:02}ProductionReleaseResources");
            input.push_str(&format!("> Task :module-{module}:{name} UP-TO-DATE\n"));
        }
    }
    let progress = (1..=12)
        .map(|i| {
            format!("processing custom asset bundle {i} of 12 inside Gradle custom generator\n")
        })
        .collect::<String>();
    input.push_str(&progress);
    input.push_str("BUILD SUCCESSFUL in 2s\n210 actionable tasks: 210 up-to-date\nexit code: 0\n");

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
    assert_eq!(second.recoverability, Recoverability::Lossless);
    assert!(second.stash_writes.is_empty());
    assert_eq!(store.len(), 0);
    assert_eq!(second.output, first.output);

    // Also keep idempotence when >100 preamble lines push the >4 KiB header out of sample_lines().
    let configure_head = (0..105)
        .map(|i| format!("> Configure project :sub-cfg-{i:03}\n"))
        .collect::<String>();
    let gapped = format!(
        "$ ./gradlew build\n{configure_head}{}",
        input.strip_prefix("$ ./gradlew build\n").unwrap()
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
