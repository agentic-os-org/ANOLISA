//! Independent reconstruction and boundary checks for Maven structural compaction.

use tokenless_ccr::{InMemoryStore, RecoveryMethod, StashStore};
use tokenless_compressors::{BuildLogCompressor, BuildLogOperation, Recoverability};
use tokenless_protocol::estimate_tokens;

const HEADER: &str = "Maven templates (id<TAB>fields; interleave with literals): ";
const PHASE: &str = "[INFO] --- compiler:3.13.0:compile (default-compile) @ sample ---\n";
const CONTEXT: &str = "[INFO] --- compiler:3.13.0:compile (default-compile) @ sample ---\n[INFO] No sources to compile\n";

#[path = "common/maven_oracle.rs"]
mod oracle;
use oracle::decode;

fn downloads(rows: usize, prefix: &str) -> String {
    (0..rows).map(|index| {
        let action = if index % 3 == 0 { "Downloading" } else { "Downloaded" };
        let receipt = if action == "Downloaded" { " (22 kB at 34 kB/s)" } else { "" };
        format!("{prefix}{action} from central: https://repo.maven.apache.org/maven2/org/example/repeated/group/artifact-{index:03}/1.0/artifact-1.0.pom{receipt}\n")
    }).collect()
}

#[test]
fn downloads_and_isolated_status_markers_do_not_establish_maven_context() {
    for prefix in ["", "[INFO] "] {
        for count in [1, 2, 30, 400] {
            let input = downloads(count, prefix);
            assert!(!BuildLogCompressor::detect(&input));
            check(&input, false);
        }
    }
    for marker in [
        "[INFO] Scanning for projects...\n",
        "[INFO] BUILD SUCCESS\n",
        "[INFO] BUILD FAILURE\n",
        PHASE,
    ] {
        assert!(!BuildLogCompressor::detect(marker));
        check(marker, false);
    }
}

#[test]
fn maven_structure_needs_visible_head_context_and_contiguous_evidence() {
    let tail = format!("{PHASE}{}", downloads(30, "[INFO] "));
    for prefix in [format!("{}\n", "x".repeat(40000)), "\n".repeat(201)] {
        assert!(!BuildLogCompressor::detect(&format!("{prefix}{tail}")));
    }
    let before = "[INFO] status\n".repeat(98);
    let gap = "[INFO] unknown\n".repeat(20);
    let after = "[INFO] status\n".repeat(98);
    let input = format!(
        "{before}[INFO] ----< org.example:sample >----\n[INFO] Building Sample 1.0\n{gap}[INFO]   from pom.xml\n[INFO] ----[ jar ]----\n{after}"
    );
    assert!(!BuildLogCompressor::detect(&input));
}

#[test]
fn related_maven_structures_are_evidence_only_in_log_context() {
    for input in [
        format!("{PHASE}{}", downloads(1, "")),
        "[INFO] ----< org.example:sample >----\n[INFO] Building Sample 1.0\n[INFO]   from pom.xml\n[INFO] ----[ jar ]----\n".into(),
        "[INFO] -------------------------------------------------------\n[INFO]  T E S T S\n[INFO] -------------------------------------------------------\n".into(),
        "[INFO] BUILD SUCCESS\n[INFO] ------------------------------------------------------------------------\n[INFO] Total time:  1.234 s\n[INFO] Finished at: 2026-11-12T09:14:59Z\n".into(),
    ] {
        assert!(BuildLogCompressor::detect(&input), "{input}");
        assert!(!BuildLogCompressor::detect(&input.replace('\n', "\r\n")));
        for prefix in ["Quoted output:\n", "```text\n", "~~~text\n"] {
            assert!(!BuildLogCompressor::detect(&format!("{prefix}{input}")));
        }
    }
}

#[test]
fn structured_launcher_and_jvm_preambles_preserve_maven_compaction_within_head_budget() {
    let captured = include_str!("fixtures/build_logs/maven/jsoup_batch.txt");
    let cleaned_captured = oracle::strip_sgr(captured);
    for preamble in [
        "OpenJDK 64-Bit Server VM warning: Sharing is only supported for boot loader classes because bootstrap classpath has been appended\n",
        "Java HotSpot(TM) 64-Bit Server VM warning: Options -Xverify:none and -noverify were deprecated in JDK 13\n",
        "WARNING: An illegal reflective access operation has occurred\n\
         WARNING: Illegal reflective access by com.google.inject.internal.cglib.core.$ReflectUtils$1 (file:/opt/guice.jar) to method java.lang.ClassLoader.defineClass()\n\
         WARNING: Please consider reporting this to the maintainers of com.google.inject.internal.cglib.core.$ReflectUtils$1\n\
         WARNING: Use --illegal-access=warn to enable warnings of further illegal reflective access operations\n\
         WARNING: All illegal access operations will be denied in a future release\n",
        "WARNING: A terminally deprecated method in sun.misc.Unsafe has been called\n\
         WARNING: sun.misc.Unsafe::objectFieldOffset has been called by com.google.common.util.concurrent.AbstractFuture$UnsafeAtomicHelper\n\
         WARNING: Please consider reporting this to the maintainers of com.google.common.util.concurrent.AbstractFuture$UnsafeAtomicHelper\n\
         WARNING: sun.misc.Unsafe::objectFieldOffset will be removed in a future release\n",
        "WARNING: A restricted method in java.lang.System has been called\n\
         WARNING: java.lang.System::load has been called by org.fusesource.jansi.internal.JansiLoader in an unnamed module\n\
         WARNING: Use --enable-native-access=ALL-UNNAMED to avoid a warning for callers in this module\n\
         WARNING: Restricted methods will be blocked in a future release unless native access is enabled\n",
        "Apache Maven 3.9.6 (bc0240f3c744dd6b6ec2920b3cd08dcc295161ae)\n\
         Maven home: /opt/maven\n\
         Java version: 21.0.2, vendor: Eclipse Adoptium, runtime: /opt/java/openjdk\n\
         Java home: /opt/java/openjdk\n\
         Default locale: en_US, platform encoding: UTF-8\n\
         OS name: \"linux\", version: \"6.8.0\", arch: \"amd64\", family: \"unix\"\n",
        "+ mvn -B verify\n",
        "++ ./mvnw clean verify\n",
        "+ JAVA_HOME=/opt/jdk MAVEN_OPTS=-Xmx2g /usr/local/bin/mvn verify\n",
        "[mvnd] Starting daemon\n",
    ] {
        let input = format!("{preamble}{captured}");
        assert!(BuildLogCompressor::detect(&input), "{preamble}");
        let outcome = BuildLogCompressor.compress(&input, None);
        assert_eq!(
            decode(&outcome.output),
            format!("{preamble}{cleaned_captured}")
        );
        assert!(outcome.output.starts_with(preamble));
        assert!(estimate_tokens(&outcome.output) < estimate_tokens(&input));
    }

    let valid_log = format!("{PHASE}{}", downloads(10, "[INFO] "));
    let pre_reactor_failure = format!(
        "$ mvn -B verify\n\
         Picked up JAVA_TOOL_OPTIONS: -Xmx2g\n\
         [INFO] Scanning for projects...\n\
         {}\
         [INFO] BUILD FAILURE\n\
         [INFO] ------------------------------------------------------------------------\n\
         [INFO] Total time:  0.812 s\n\
         [INFO] Finished at: 2026-11-12T09:14:59Z\n\
         [ERROR] Non-resolvable parent POM\n",
        downloads(20, "[INFO] ")
    );
    check(&pre_reactor_failure, true);
    for rejected in [
        "+ /usr/bin/cat README.md\n".to_owned(),
        "+ env cat README.md\n".to_owned(),
        "+++ b/mvnw\n".to_owned(),
        "WARNING: Example output below, do not run:\n".to_owned(),
        "Java version: 21\n".to_owned(),
        "[INFO](https://example.org)\n".to_owned(),
        "[WARNING]: note\n".to_owned(),
        "2026-11-12T09:14:59Z [INFO] Scanning for projects...\n".to_owned(),
        "Picked up JAVA_TOOL_OPTIONS: -Xmx2g\n".repeat(201),
    ] {
        let input = format!("{rejected}{valid_log}");
        assert!(!BuildLogCompressor::detect(&input), "{rejected}");
        check(&input, false);
    }
}

#[test]
fn fenced_blocks_cannot_supply_or_bridge_maven_evidence_but_do_not_veto_confirmed_logs() {
    let body = format!("{PHASE}{}", downloads(4, "[INFO] "));
    for head in ["[INFO] Scanning for projects...\n", "[INFO]\n"] {
        let fenced = format!("{head}```text\n{body}```\n");
        assert!(!BuildLogCompressor::detect(&fenced), "{fenced}");
        check(&fenced, false);
    }

    let split_phase = format!("{PHASE}```text\nquoted\n```\n{}", downloads(2, ""));
    assert!(!BuildLogCompressor::detect(&split_phase));
    check(&split_phase, false);

    let split_banner = "[INFO] ----< org.example:sample >----\n[INFO] Building Sample 1.0\n```text\nquoted\n```\n[INFO]   from pom.xml\n[INFO] ----[ jar ]----\n";
    assert!(!BuildLogCompressor::detect(split_banner));
    check(split_banner, false);

    for unclosed in [
        format!("[INFO] Scanning for projects...\n```text\nquoted\n~~~\n{body}"),
        format!("[INFO] Scanning for projects...\n````text\nquoted\n```\n{body}"),
    ] {
        assert!(!BuildLogCompressor::detect(&unclosed));
        check(&unclosed, false);
    }

    // Across a head/tail sample gap, neither an unclosed head fence nor a closing
    // tail fence can validate tail-only Maven evidence.
    let footer = "[INFO] BUILD SUCCESS\n[INFO] ------------------------------------------------------------------------\n[INFO] Total time:  1.234 s\n[INFO] Finished at: 2026-11-12T09:14:59Z\n";
    let neutral_head = format!(
        "[INFO] Scanning for projects...\n{}",
        "[INFO] status\n".repeat(220)
    );
    assert!(BuildLogCompressor::detect(&format!(
        "{neutral_head}{footer}"
    )));
    assert!(!BuildLogCompressor::detect(&format!(
        "{neutral_head}{footer}```\n"
    )));
    let open_head = format!(
        "[INFO] Scanning for projects...\n```text\n{}",
        "[INFO] status\n".repeat(220)
    );
    assert!(!BuildLogCompressor::detect(&format!("{open_head}{footer}")));

    // Once unfenced strong evidence is present in the head, fences in test/plugin
    // output or at the tail no longer disqualify the Maven log.
    let captured = include_str!("fixtures/build_logs/maven/jsoup_batch.txt");
    let lines = captured.split_inclusive('\n').collect::<Vec<_>>();
    for (position, snippet) in [
        (10, "```json\n{\"status\":\"ok\"}\n```\n"),
        (lines.len() / 2, "~~~markdown\n# Plugin report\n~~~\n"),
        (lines.len() - 3, "~~~\n"),
        (lines.len(), "```text\n"),
    ] {
        let mut mutated = lines[..position].concat();
        mutated.push_str(snippet);
        mutated.push_str(&lines[position..].concat());
        assert!(BuildLogCompressor::detect(&mutated), "pos={position}");
        let outcome = BuildLogCompressor.compress(&mutated, None);
        assert_eq!(decode(&outcome.output), oracle::strip_sgr(&mutated));
        assert!(outcome.output.contains(snippet));
        assert!(estimate_tokens(&outcome.output) < estimate_tokens(&mutated));
    }
}

#[test]
fn native_cargo_download_progress_keeps_its_existing_recovery() {
    let records = (0..30)
        .map(|index| format!("  Downloaded sample-{index} v1.0.0 (registry crates-io)\n"))
        .collect::<String>();
    let input = format!(
        "$ cargo build\n{records}    Finished `dev` profile [unoptimized] target(s) in 1.0s\n"
    );
    assert!(BuildLogCompressor::detect(&input));
    let store = InMemoryStore::new();
    let outcome = BuildLogCompressor.compress(&input, Some(&store));
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
    assert_eq!(outcome.output.replacen(marker, &payload, 1), input);
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
fn default_and_batch_downloads_preserve_actions_receipts_duplicates_and_prefixes() {
    for prefix in ["", "[INFO] "] {
        let input = format!("{CONTEXT}{}", downloads(30, prefix).repeat(2));
        assert!(BuildLogCompressor::detect(&input));
        let output = check(&input, true);
        assert!(output.contains("central"));
        assert!(output.contains("Downloading"));
        assert!(output.contains("Downloaded"));
        assert!(output.contains("(22 kB at 34 kB/s)"));
        if !prefix.is_empty() {
            assert!(output.contains("[INFO] "))
        }
    }
    check(
        &format!("{CONTEXT}{}{}", downloads(30, ""), downloads(30, "[INFO] ")),
        true,
    );
}

#[test]
fn singleton_downloads_require_corroborating_maven_evidence() {
    for prefix in ["", "[INFO] "] {
        let input = downloads(1, prefix);
        assert!(!BuildLogCompressor::detect(&input));
        check(&input, false);
        assert!(BuildLogCompressor::detect(&format!("{PHASE}{input}")));
        check(&format!("{input}{PHASE}{}", downloads(30, prefix)), true);
    }
}

#[test]
fn unedited_maven_logs_retain_existing_recovery() {
    let routine = (0..60)
        .map(|index| format!("[INFO] Compiling module {index} of the project with a fairly long descriptive line\n"))
        .collect::<String>();
    // The phase forces Maven detection even after singleton detection is tightened.
    for evidence in [downloads(1, ""), format!("{PHASE}{}", downloads(1, ""))] {
        let input = format!("$ mvn -q verify\n{routine}{evidence}exit code: 0\n");
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
            let outcome = BuildLogCompressor.compress_with_recovery(&input, stash, &method);
            assert_eq!(outcome.output, input);
            assert!(outcome.stash_writes.is_empty());
            assert_eq!(store.len(), 0);
        }
    }
    // A usable structural layout remains lossless even with Generic evidence.
    let input = format!(
        "$ mvn verify\n{routine}{PHASE}{}exit code: 0\n",
        downloads(30, "")
    );
    check(&input, true);
}

#[test]
fn scoped_default_transfers_recover_when_unedited_and_keep_opaque_or_bad_cr_boundaries() {
    let banner = "[INFO] ----------------< org.example:sample >----------------\n\
                  [INFO] Building Sample 1.0\n\
                  [INFO]   from pom.xml\n\
                  [INFO] ----------------[ jar ]----------------\n";
    let footer = "[INFO] BUILD SUCCESS\n\
                  [INFO] ------------------------------------------------------------------------\n\
                  [INFO] Total time:  2.345 s\n\
                  [INFO] Finished at: 2026-11-12T09:14:59Z\n";
    let groups = [
        "org/slf4j/slf4j-api",
        "com/google/guava/guava",
        "commons-io/commons-io",
        "org/jsoup/jsoup",
    ];
    let default_transfers = (0..20)
        .map(|index| {
            let coord = groups[index % groups.len()];
            let artifact = coord.rsplit('/').next().unwrap();
            let redraws = (1..=(index % 7 + 2))
                .map(|step| format!("Progress (1): {step}.0/20 kB\r"))
                .collect::<String>();
            format!(
                "Downloading from central: https://repo.maven.apache.org/maven2/{coord}/1.{index}/{artifact}-1.{index}.jar\n\
                 {redraws}                    \rDownloaded from central: https://repo.maven.apache.org/maven2/{coord}/1.{index}/{artifact}-1.{index}.jar (20 kB at 50 kB/s)\n"
            )
        })
        .collect::<String>();

    for anchored in [
        format!("$ mvn verify\n{banner}{PHASE}{default_transfers}exit code: 0\n"),
        format!("$ mvn verify\n{banner}{PHASE}{default_transfers}{footer}exit code: 0\n"),
        format!(
            "$ mvn verify\n[INFO] Scanning for projects...\n{default_transfers}{footer}[ERROR] Non-resolvable parent POM\nexit code: 1\n"
        ),
    ] {
        for method in [
            RecoveryMethod::Shell,
            RecoveryMethod::tool("retrieve_log").unwrap(),
        ] {
            let store = InMemoryStore::new();
            let outcome =
                BuildLogCompressor.compress_with_recovery(&anchored, Some(&store), &method);
            assert_eq!(outcome.recoverability, Recoverability::Retrievable);
            assert_eq!(outcome.metrics.omitted_blocks, 1);
            assert_eq!(outcome.stash_writes.len(), 1);
            if anchored.contains("[ERROR]") {
                assert!(outcome.output.contains("[ERROR] Non-resolvable parent POM"));
            }
            let payload = store
                .retrieve(&outcome.stash_writes[0].key)
                .unwrap()
                .unwrap();
            let marker = outcome
                .output
                .split_inclusive('\n')
                .find(|line| line.contains("lines omitted."))
                .unwrap();
            assert_eq!(outcome.output.replacen(marker, &payload, 1), anchored);
            assert!(estimate_tokens(&outcome.output) * 3 < estimate_tokens(&anchored));
        }

        // Without recovery capability or Stash, or without command/exit anchors, no lines are omitted.
        let unanchored = anchored
            .strip_prefix("$ mvn verify\n")
            .unwrap()
            .rsplit_once("exit code:")
            .unwrap()
            .0
            .to_owned();
        let store = InMemoryStore::new();
        for (outcome, expected) in [
            (
                BuildLogCompressor.compress_with_recovery(
                    &anchored,
                    Some(&store),
                    &RecoveryMethod::None,
                ),
                &anchored,
            ),
            (
                BuildLogCompressor.compress_with_recovery(&anchored, None, &RecoveryMethod::Shell),
                &anchored,
            ),
            (
                BuildLogCompressor.compress(&unanchored, Some(&store)),
                &unanchored,
            ),
        ] {
            assert_eq!(outcome.recoverability, Recoverability::Lossless);
            assert_eq!(&outcome.output, expected);
            assert!(outcome.stash_writes.is_empty());
        }
        assert_eq!(store.len(), 0);
    }

    // Diagnostics, exceptions, malformed transfers and unknown lines close the scope;
    // valid transfer records after them must remain untouched even with Stash enabled.
    for boundary in [
        "[WARNING] diagnostic output precedes raw transfers\n",
        "[ERROR] retain everything below\n",
        "java.lang.Exception: retain me\n\tat x.y(z.java:1)\n",
        "unrecognized plugin body\n",
        "[INFO] unknown text\n",
        "Downloaded from central: https://repo.example/missing-receipt.pom\n",
    ] {
        for tail in [&default_transfers, &downloads(30, "")] {
            let input = format!("$ mvn verify\n{CONTEXT}{boundary}{tail}exit code: 0\n");
            let store = InMemoryStore::new();
            let outcome = BuildLogCompressor.compress(&input, Some(&store));
            assert_eq!(
                outcome.recoverability,
                Recoverability::Lossless,
                "{boundary}"
            );
            assert_eq!(outcome.output, input, "{boundary}");
            assert!(outcome.stash_writes.is_empty());
            assert_eq!(store.len(), 0);
        }
    }

    // Malformed CR records must neither qualify as Maven transfers nor re-enter Generic templates.
    for bad_cr in [
        "Progress (0): 1 kB\r\n",
        "Progress (2): 1/2 kB\r\n",
        "Progress (1): 1 kB2 kB\r\n",
        "Progress (1): invalid\r\n",
        "Progress (1): 1/2 kB\ropaque diagnostic\r\n",
        "progress: item 1\ropaque redraw 2\n",
    ] {
        let input = format!("$ mvn verify\n{CONTEXT}{}exit code: 0\n", bad_cr.repeat(30));
        let store = InMemoryStore::new();
        let outcome = BuildLogCompressor.compress(&input, Some(&store));
        assert_eq!(
            outcome.recoverability,
            Recoverability::Lossless,
            "{bad_cr:?}"
        );
        assert_eq!(outcome.output, input, "{bad_cr:?}");
        assert!(outcome.stash_writes.is_empty());
        assert_eq!(store.len(), 0);
    }

    // Download-only fragments remain outside BuildLog even when command-wrapped.
    for wrapped_downloads in [
        format!("$ mvn verify\n{}exit code: 0\n", downloads(60, "")),
        format!(
            "$ mvn verify\n{}exit code: 0\n",
            "Downloaded from central: https://repo.maven.apache.org/maven2/org/example/a/1.0/a-1.0.jar (10 kB at 20 kB/s)\n".repeat(60)
        ),
    ] {
        assert!(!BuildLogCompressor::detect(&wrapped_downloads));
        let store = InMemoryStore::new();
        let outcome = BuildLogCompressor.compress(&wrapped_downloads, Some(&store));
        assert_eq!(outcome.output, wrapped_downloads);
        assert_eq!(store.len(), 0);
    }

    // Complete default-mode captures with valid structural edits stay lossless with zero Stash writes.
    let jsoup_default = include_str!("fixtures/build_logs/maven/jsoup_default.txt");
    let wrapped_default = format!("$ mvn verify\n{jsoup_default}exit code: 0\n");
    let store = InMemoryStore::new();
    let outcome = BuildLogCompressor.compress(&wrapped_default, Some(&store));
    assert_eq!(outcome.recoverability, Recoverability::Lossless);
    assert_eq!(decode(&outcome.output), oracle::strip_sgr(&wrapped_default));
    assert!(outcome.stash_writes.is_empty());
    assert_eq!(store.len(), 0);
}

#[test]
fn crlf_only_maven_logs_do_not_claim_the_lossless_route() {
    let lf = format!("{PHASE}{}[INFO] BUILD SUCCESS\n", downloads(30, "[INFO] "));
    assert!(BuildLogCompressor::detect(&lf));
    let crlf = lf.replace('\n', "\r\n");
    assert!(!BuildLogCompressor::detect(&crlf));
    check(&crlf, false);
    let routine = (0..60)
        .map(|index| format!("[INFO] Compiling module {index} with stable verbose progress\r\n"))
        .collect::<String>();
    let wrapped = format!("$ mvn verify\r\n{routine}{crlf}exit code: 0\r\n");
    let store = InMemoryStore::new();
    let outcome = BuildLogCompressor.compress(&wrapped, Some(&store));
    assert_eq!(outcome.recoverability, Recoverability::Retrievable);
    assert!(!outcome.stash_writes.is_empty());
}

#[test]
fn edit_limit_retains_completed_edits_and_the_remaining_suffix() {
    let block = format!("{PHASE}{}", downloads(8, "[INFO] "));
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
fn unknown_warning_and_exception_never_reopen_on_transfer_text() {
    for boundary in [
        "[WARNING] retain this warning\n",
        "java.lang.Exception: retain me\n\tat x.y(z.java:1)\n",
        "unrecognized plugin body\n",
        "\tDownloading from central: https://a/b\n",
        "[INFO] unknown text\n",
    ] {
        let tail = downloads(30, "[INFO] ");
        let input = format!("{CONTEXT}{boundary}{tail}");
        check(&input, false);
        let input = format!(
            "{CONTEXT}{}{boundary}{tail}{PHASE}{tail}",
            downloads(30, "")
        );
        let output = check(&input, true);
        assert!(output.contains(&format!("{boundary}{tail}")));
    }
}

#[test]
fn mutation_at_every_download_boundary_keeps_the_opaque_suffix() {
    for original in [
        downloads(30, "[INFO] "),
        include_str!("fixtures/build_logs/maven/downloads_batch.txt").to_owned(),
    ] {
        let rows = original.split_inclusive('\n').collect::<Vec<_>>();
        for index in 0..=rows.len() {
            for boundary in [
                "[WARNING] injected\n",
                "java.lang.Exception: injected\n",
                "unrecognized text\n",
            ] {
                let suffix = rows[index..].concat();
                let input = format!("{CONTEXT}{}{boundary}{suffix}", rows[..index].concat());
                let outcome = BuildLogCompressor.compress(&input, None);
                assert_eq!(decode(&outcome.output), input);
                assert!(outcome.output.ends_with(&format!("{boundary}{suffix}")));
                assert_eq!(
                    BuildLogCompressor.compress(&outcome.output, None).output,
                    outcome.output
                );
            }
        }
    }
}

#[test]
fn cr_records_are_exact_boundaries_and_only_valid_continuations_keep_the_scope() {
    for progress in [
        "Progress (1): 1.2/3.4 kB\rProgress (1): 3.4 kB\r   \r\n",
        "Progress (2): 1/2 kB | 2/3 MB\r\n",
    ] {
        let input = format!(
            "{CONTEXT}{}{progress}{}",
            downloads(30, ""),
            downloads(30, "")
        );
        let output = check(&input, true);
        assert!(output.contains(progress));
        assert_eq!(output.matches(HEADER).count(), 2);
    }
    for bad in [
        "Progress (2): 1/2 kB\r\n",
        "Progress (1): 1 kB2 kB\r\n",
        "Progress (1): 1/2 kB\rwarning: do not discard\r\n",
        "Progress (1): invalid\r\n",
        "[INFO] unrecognized\r\n",
        "Progress (0): 1 kB\r\n",
    ] {
        let tail = downloads(30, "");
        let input = format!("{CONTEXT}{}{bad}{tail}", downloads(30, ""));
        assert!(check(&input, true).ends_with(&format!("{bad}{tail}")));
    }
}

#[test]
fn sgr_cleanup_is_the_only_change_to_protected_terminal_records() {
    let progress = "Progress (1): 1/2 kB\rProgress (1): 2 kB\r\n";
    let source = format!(
        "{CONTEXT}{}{progress}{}",
        downloads(30, "[INFO] "),
        downloads(30, "[INFO] ")
    );
    let colored = source.replace("[INFO]", "\x1b[32m[INFO]\x1b[0m");
    let outcome = BuildLogCompressor.compress(&colored, None);
    assert_eq!(
        outcome.operations,
        [
            BuildLogOperation::TerminalCleanup,
            BuildLogOperation::Reduction
        ]
    );
    assert_eq!(decode(&outcome.output), source);
    assert!(outcome.output.contains(progress));
    assert!(outcome.stash_writes.is_empty());
    for boundary in [
        "\x1b[2Kuntouched\n",
        "\x1b]8;;https://example.org\x07link\n",
    ] {
        let input = format!("{boundary}{}", downloads(30, "[INFO] "));
        check(&input, false);
    }
}

#[test]
fn reopening_requires_a_complete_module_or_test_banner() {
    let banner = "[INFO] ----------------< org.example:sample >----------------\n[INFO] Building Sample 1.0 [1/1]\n[INFO]   from pom.xml\n[INFO] ----------------[ jar ]----------------\n";
    let tests = "[INFO] -------------------------------------------------------\n[INFO]  T E S T S\n[INFO] -------------------------------------------------------\n";
    for anchor in [banner, tests] {
        let input = format!("{CONTEXT}unknown body\n{anchor}{}", downloads(30, ""));
        check(&input, true);
        let incomplete = anchor
            .rsplit_once('\n')
            .unwrap()
            .0
            .rsplit_once('\n')
            .unwrap()
            .0;
        check(
            &format!("{CONTEXT}unknown body\n{incomplete}\n{}", downloads(30, "")),
            false,
        );
    }
}

#[test]
fn bounded_blocks_are_rejected_atomically() {
    check(&format!("{CONTEXT}{}", downloads(256, "")), true);
    check(&format!("{CONTEXT}{}", downloads(257, "")), false);
    let row = downloads(1, "");
    for bytes in [65536, 65537] {
        let mut input = row.repeat(200);
        let length = bytes - input.len();
        input.insert_str(input.len() - 1, &"x".repeat(length));
        check(&format!("{CONTEXT}{input}"), bytes == 65536);
    }
    let input = format!("{CONTEXT}{}{}", downloads(20, ""), "x".repeat(65537));
    check(&input, false);
}

#[test]
fn malformed_urls_receipts_and_truncated_records_are_never_edited() {
    for bad in [
        "Downloaded from central: https://repo.example/a.pom\n",
        "Downloading from central: https://repo.example/a.pom (2 kB at 3 kB/s)\n",
        "Downloading from central: https://user:pass@repo.example/a.pom\n",
        "Downloading from central: https://repo.example/a.pom?token=secret\n",
        "Downloading from central: https://repo.example/a.pom#fragment\n",
        "Downloading from central: https://repo.example/a%2Fb.pom\n",
        "Downloading from central: https://repo.example/a.pom\t\n",
    ] {
        check(&format!("{CONTEXT}{bad}{}", downloads(30, "")), false);
    }
    let input = format!("{CONTEXT}{}", downloads(30, ""));
    let truncated = input.trim_end_matches('\n');
    let outcome = BuildLogCompressor.compress(truncated, None);
    assert_eq!(decode(&outcome.output), truncated);
    assert!(
        outcome
            .output
            .ends_with(truncated.rsplit('\n').next().unwrap())
    );
}

#[test]
fn test_records_preserve_every_class_method_count_and_duration() {
    let mut input = PHASE.to_owned();
    for index in 0..24 {
        input.push_str(&format!("[INFO] Running com.example.Test{index}\n[INFO] Tests run: 12, Failures: 0, Errors: 0, Skipped: 1, Time elapsed: 0.123 s -- in com.example.Test{index}\n"));
    }
    input.push_str(
        "[INFO] Tests run: 288, Failures: 0, Errors: 0, Skipped: 24\n[INFO] BUILD SUCCESS\n",
    );
    check(&input, true);
    let methods = (0..30)
        .map(|index| {
            format!(
                "[INFO] com.example.AVeryLongTestClass.method{index} -- Time elapsed: 0.001 s\n"
            )
        })
        .collect::<String>();
    check(&format!("{PHASE}{methods}"), true);
}

#[test]
fn bounded_detection_does_not_promote_prose_source_or_unknown_prefaces() {
    for input in [
        "Maven BUILD SUCCESS",
        "let text = \"[INFO] BUILD SUCCESS\";",
        "> [INFO] Scanning for projects...",
        "[INFO] Downloading some prose",
        "[INFO] --- malformed plugin ---",
    ] {
        assert!(!BuildLogCompressor::detect(input), "{input}");
        check(input, false);
    }
    check(&format!("unknown preface\n{}", downloads(30, "")), false);
}

#[test]
fn unicode_method_parameters_and_nonzero_failure_counts_are_preserved() {
    let methods = (0..30)
        .map(|index| {
            format!("[INFO] org.example.Test.method(参数{index}) -- Time elapsed: 0.001 s\n")
        })
        .collect::<String>();
    check(&format!("{PHASE}{methods}"), true);
    let failure = "[INFO] Tests run: 3, Failures: 1, Errors: 0, Skipped: 0, Time elapsed: 0.003 s -- in org.example.Test\n";
    check(&format!("{PHASE}{failure}{}", downloads(30, "")), false);
}

#[test]
fn captured_streams_round_trip_without_stash_and_are_idempotent() {
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build_logs/maven");
    let mut paths = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension().is_some_and(|extension| extension == "txt")
                && !path.to_string_lossy().ends_with(".expected.txt")
        })
        .collect::<Vec<_>>();
    // Optional complete local capture corpus; committed fixtures remain the CI gate.
    if let Ok(root) = std::env::var("MAVEN_CAPTURE_ROOT") {
        for entry in std::fs::read_dir(root).unwrap() {
            let directory = entry.unwrap().path();
            if !directory
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("tokenless-maven-9966-")
            {
                continue;
            }
            for cache in ["cold", "warm"] {
                for stream in ["stdout", "stderr", "observed"] {
                    let path = directory.join(cache).join(format!("{stream}.txt"));
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
        let cleaned = oracle::strip_sgr(&raw);
        let store = InMemoryStore::new();
        let outcome = BuildLogCompressor.compress(&raw, Some(&store));
        assert_eq!(decode(&outcome.output), cleaned, "{}", path.display());
        assert_eq!(
            outcome
                .output
                .split_inclusive('\n')
                .filter(|line| line.contains('\r'))
                .collect::<Vec<_>>(),
            cleaned
                .split_inclusive('\n')
                .filter(|line| line.contains('\r'))
                .collect::<Vec<_>>()
        );
        assert!(outcome.stash_writes.is_empty(), "{}", path.display());
        assert_eq!(store.len(), 0);
        assert_eq!(outcome.recoverability, Recoverability::Lossless);
        let again = BuildLogCompressor.compress(&outcome.output, Some(&store));
        assert_eq!(again.output, outcome.output, "{}", path.display());
        if outcome.output != raw {
            assert!(estimate_tokens(&outcome.output) < estimate_tokens(&raw));
            assert!(outcome.output.chars().count() < raw.chars().count());
        }
        if path.file_name().unwrap() == "commons_warm.txt" {
            let golden = path.with_extension("expected.txt");
            assert_eq!(outcome.output, std::fs::read_to_string(golden).unwrap());
        }
        if path.file_name().unwrap() == "downloads_batch.txt" {
            assert_eq!(outcome.output, raw);
            let contextual = format!("{CONTEXT}{raw}");
            let output = check(&contextual, true);
            let golden = std::fs::read_to_string(path.with_extension("expected.txt")).unwrap();
            assert_eq!(output, format!("{CONTEXT}{golden}"));
        }
        println!(
            "{}: {} -> {}",
            path.display(),
            estimate_tokens(&raw),
            estimate_tokens(&outcome.output)
        );
    }
}

#[test]
#[ignore = "rewrites reviewed goldens; run explicitly after an intentional format change"]
fn regenerate_maven_goldens() {
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build_logs/maven");
    for name in ["commons_warm.txt", "downloads_batch.txt"] {
        let path = directory.join(name);
        let raw = std::fs::read_to_string(&path).unwrap();
        let prefix = if name == "downloads_batch.txt" {
            CONTEXT
        } else {
            ""
        };
        let input = format!("{prefix}{raw}");
        let outcome = BuildLogCompressor.compress(&input, None);
        assert_eq!(decode(&outcome.output), oracle::strip_sgr(&input));
        std::fs::write(
            path.with_extension("expected.txt"),
            outcome.output.strip_prefix(prefix).unwrap(),
        )
        .unwrap();
    }
}
