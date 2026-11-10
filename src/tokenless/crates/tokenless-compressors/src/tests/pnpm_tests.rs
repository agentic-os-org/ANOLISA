use std::sync::atomic::{AtomicUsize, Ordering};

use tokenless_ccr::{InMemoryStore, StashError, recovery_hashes};

const LIVE: &str = include_str!("../../tests/fixtures/build_logs/pnpm_install_live.txt");
const DONE: &str = "Done in 13.8s using pnpm v10.33.4\n";
const NOTICE_BODY: &str = "Update available! 10.33.4 → 12.18.0.\n\
                          Changelog: https://pnpm.io/v/12.18.0\n\
                          To update, run: pnpm add -g pnpm\n";
const LIVE_DEPENDENCIES: &str = "devDependencies:\n\
                                + @types/node 25.9.9 (26.6.6 is available)\n\
                                + esbuild 0.28.2\n\
                                + typescript 5.9.3 (7.0.2 is available)\n\
                                + vitest 3.2.7 (5.1.0 is available)\n";
const COMPACT_DEPENDENCIES: &str = "devDependencies (version; available):\n\
                                   + @types/node 25.9.9; 26.6.6\n\
                                   + esbuild 0.28.2\n\
                                   + typescript 5.9.3; 7.0.2\n\
                                   + vitest 3.2.7; 5.1.0\n";

struct RecordingStore {
    inner: InMemoryStore,
    calls: AtomicUsize,
    fail_after: Option<usize>,
}

impl RecordingStore {
    fn new(fail_after: Option<usize>) -> Self {
        Self {
            inner: InMemoryStore::new(),
            calls: AtomicUsize::new(0),
            fail_after,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl StashStore for RecordingStore {
    fn stash(&self, payload: &str) -> Result<StashWrite, StashError> {
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        if self.fail_after.is_some_and(|limit| call >= limit) {
            return Err(StashError::Backend("simulated pnpm stash failure".into()));
        }
        self.inner.stash(payload)
    }

    fn retrieve(&self, key: &str) -> Result<Option<String>, StashError> {
        self.inner.retrieve(key)
    }

    fn len(&self) -> usize {
        self.inner.len()
    }

    fn evict_expired(&self) -> Result<usize, StashError> {
        self.inner.evict_expired()
    }

    fn delete(&self, key: &str, generation: u64) -> Result<bool, StashError> {
        self.inner.delete(key, generation)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct DependencyFact {
    category: String,
    sign: String,
    name: String,
    version: String,
    available: Option<String>,
}

// Independent presentation oracle; production grammar validation is intentionally excluded.
fn dependency_facts(text: &str) -> Vec<DependencyFact> {
    let mut category = None;
    let mut facts = Vec::new();
    for line in text.lines() {
        if let Some(heading) = line.strip_suffix(':') {
            let heading = heading
                .strip_suffix(" (version; available)")
                .unwrap_or(heading);
            category = match heading {
                "dependencies" | "devDependencies" | "optionalDependencies" => Some(heading),
                _ => None,
            };
            continue;
        }
        let Some(current) = category else {
            continue;
        };
        if !(line.starts_with("+ ") || line.starts_with("- ")) {
            category = None;
            continue;
        }
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        let (version, available) = match fields.as_slice() {
            [_, _, version] => (*version, None),
            [_, _, version, available] => (
                version.strip_suffix(';').unwrap(),
                Some((*available).to_owned()),
            ),
            [_, _, version, available, "is", "available)"] => (
                *version,
                Some(available.strip_prefix('(').unwrap().to_owned()),
            ),
            _ => panic!("unexpected dependency presentation: {line}"),
        };
        facts.push(DependencyFact {
            category: current.to_owned(),
            sign: fields[0].to_owned(),
            name: fields[1].to_owned(),
            version: version.to_owned(),
            available,
        });
    }
    facts
}

fn captured_notice() -> String {
    LIVE.split_inclusive('\n').skip(2).take(7).collect()
}

fn live_layout() -> String {
    // These fixed edits form an oracle independent of the pnpm scanner.
    LIVE.replace(&captured_notice(), NOTICE_BODY)
        .replace(
            "Packages: +53\n+++++++++++++++++++++++++++++++++++++++++++++++++++++\n",
            "Packages: +53\n",
        )
        .replace(LIVE_DEPENDENCIES, COMPACT_DEPENDENCIES)
}

// All generated logs below are synthetic; LIVE is the only installation capture.
fn progress(counters: [usize; 4], done: bool) -> String {
    let [resolved, reused, downloaded, added] = counters;
    let suffix = if done { ", done" } else { "" };
    format!(
        "Progress: resolved {resolved}, reused {reused}, downloaded {downloaded}, added {added}{suffix}\n"
    )
}

fn run(start: usize, count: usize) -> String {
    (start..start + count)
        .map(|value| progress([value, 0, value, 0], false))
        .collect()
}

fn middle(text: &str) -> String {
    let lines = text.split_inclusive('\n').collect::<Vec<_>>();
    lines[2..lines.len() - 2].concat()
}

fn marker(payload: &str, method: &RecoveryMethod) -> String {
    format!(
        "{} package progress lines omitted. {}\n",
        payload.lines().count(),
        recovery_instruction(&compute_key(payload.as_bytes()), method)
    )
}

fn expand_markers(
    outcome: &BuildLogOutcome,
    store: &dyn StashStore,
    method: &RecoveryMethod,
) -> String {
    let mut restored = String::new();
    let mut references = 0;
    for line in outcome.output.split_inclusive('\n') {
        let hashes = recovery_hashes(line, method);
        if hashes.is_empty() {
            restored.push_str(line);
            continue;
        }
        assert_eq!(hashes.len(), 1);
        references += 1;
        let payload = store.retrieve(hashes[0]).unwrap().unwrap();
        assert_eq!(compute_key(payload.as_bytes()), hashes[0]);
        assert_eq!(line, marker(&payload, method));
        assert!(
            outcome
                .stash_writes
                .iter()
                .any(|write| write.key == hashes[0])
        );
        restored.push_str(&payload);
    }
    assert_eq!(references, outcome.metrics.omitted_blocks);
    restored
}

fn assert_unchanged(input: &str) {
    let store = RecordingStore::new(None);
    let outcome =
        BuildLogCompressor.compress_with_recovery(input, Some(&store), &RecoveryMethod::Shell);
    assert_eq!(outcome.output, input);
    assert!(outcome.operations.is_empty());
    assert_eq!(outcome.recoverability, crate::Recoverability::Lossless);
    assert_eq!(outcome.metrics.omitted_blocks, 0);
    assert_eq!(outcome.metrics.omitted_lines, 0);
    assert!(outcome.stash_writes.is_empty());
    assert_eq!(store.calls(), 0);
}

#[test]
fn captured_fixture_and_layout_have_independent_cost_and_fact_oracles() {
    assert_eq!(LIVE.chars().count(), 1326);
    assert_eq!(estimate_tokens(LIVE), 332);
    assert!(BuildLogCompressor::detect(LIVE));
    let expected = live_layout();
    assert_eq!(expected.chars().count(), 1022);
    assert_eq!(estimate_tokens(&expected), 256);
    let outcome = BuildLogCompressor.compress_with_recovery(LIVE, None, &RecoveryMethod::None);
    assert_eq!(outcome.output, expected);
    assert_eq!(outcome.operations, [BuildLogOperation::Reduction]);
    assert_eq!(outcome.recoverability, crate::Recoverability::Lossless);
    let before = dependency_facts(LIVE);
    assert_eq!(before.len(), 4);
    assert_eq!(before[1].available, None);
    assert_eq!(dependency_facts(&outcome.output), before);
}

#[test]
fn layout_needs_neither_a_store_nor_recovery_and_never_stashes() {
    let store = RecordingStore::new(Some(0));
    for method in [
        RecoveryMethod::None,
        RecoveryMethod::Shell,
        RecoveryMethod::tool("pnpm_retrieve").unwrap(),
    ] {
        let outcome = BuildLogCompressor.compress_with_recovery(LIVE, None, &method);
        assert_eq!(outcome.output, live_layout());
        assert_eq!(outcome.operations, [BuildLogOperation::Reduction]);
        assert!(outcome.stash_writes.is_empty());
        assert_eq!(outcome.metrics.omitted_lines, 0);
    }
    let outcome =
        BuildLogCompressor.compress_with_recovery(LIVE, Some(&store), &RecoveryMethod::None);
    assert_eq!(outcome.output, live_layout());
    assert!(outcome.stash_writes.is_empty());
    assert_eq!(outcome.metrics.stash_errors, 0);
    assert_eq!(store.calls(), 0);
}

#[test]
fn captured_shell_and_tool_markers_restore_only_the_cleaned_progress_gap() {
    let payload = LIVE
        .split_inclusive('\n')
        .skip(12)
        .take(7)
        .collect::<String>();
    assert_eq!(payload.chars().count(), 391);
    assert_eq!(estimate_tokens(&payload), 98);
    for method in [
        RecoveryMethod::Shell,
        RecoveryMethod::tool("pnpm_retrieve").unwrap(),
        RecoveryMethod::tool("t".repeat(64)).unwrap(),
    ] {
        let store = RecordingStore::new(None);
        let outcome = BuildLogCompressor.compress_with_recovery(LIVE, Some(&store), &method);
        assert_eq!(
            outcome.output,
            live_layout().replace(&payload, &marker(&payload, &method))
        );
        assert_eq!(outcome.operations, [BuildLogOperation::Reduction]);
        assert_eq!(outcome.recoverability, crate::Recoverability::Retrievable);
        assert_eq!(outcome.metrics.omitted_blocks, 1);
        assert_eq!(outcome.metrics.omitted_lines, 7);
        assert_eq!(outcome.stash_writes.len(), 1);
        let key = &outcome.stash_writes[0].key;
        assert_ne!(key, "000000000000000000000000");
        assert_eq!(store.retrieve(key).unwrap().unwrap(), payload);
        assert_eq!(expand_markers(&outcome, &store, &method), live_layout());
        assert_eq!(dependency_facts(&outcome.output), dependency_facts(LIVE));
        if method == RecoveryMethod::Shell {
            assert_eq!(outcome.output.chars().count(), 734);
            assert_eq!(estimate_tokens(&outcome.output), 184);
            let progress_only = LIVE.replace(&payload, &marker(&payload, &method));
            assert_eq!(progress_only.chars().count(), 1038);
            assert_eq!(estimate_tokens(&progress_only), 260);
        }
        assert!(outcome.output.chars().count() < live_layout().chars().count());
        assert!(estimate_tokens(&outcome.output) < estimate_tokens(&live_layout()));
    }
}

#[test]
fn layout_and_progress_share_one_reduction_label_after_sgr_cleanup() {
    let colored = LIVE
        .split_inclusive('\n')
        .map(|line| format!("\x1b[1;32m{}\x1b[0m\n", line.strip_suffix('\n').unwrap()))
        .collect::<String>();
    for method in [RecoveryMethod::None, RecoveryMethod::Shell] {
        let store = RecordingStore::new(None);
        let outcome = BuildLogCompressor.compress_with_recovery(&colored, Some(&store), &method);
        let operations = [
            BuildLogOperation::TerminalCleanup,
            BuildLogOperation::Reduction,
        ];
        if method == RecoveryMethod::Shell {
            assert_eq!(expand_markers(&outcome, &store, &method), live_layout());
            let payload = store
                .retrieve(&outcome.stash_writes[0].key)
                .unwrap()
                .unwrap();
            assert!(!payload.contains('\x1b'));
            assert!(LIVE.contains(&payload));
        } else {
            assert_eq!(outcome.output, live_layout());
            assert_eq!(store.calls(), 0);
        }
        assert_eq!(outcome.operations, operations);
    }
}

#[test]
fn second_pass_is_identical_without_any_additional_stash_writes() {
    for method in [
        RecoveryMethod::None,
        RecoveryMethod::Shell,
        RecoveryMethod::tool("pnpm_retrieve").unwrap(),
    ] {
        let store = RecordingStore::new(None);
        let first = BuildLogCompressor.compress_with_recovery(LIVE, Some(&store), &method);
        let calls = store.calls();
        let second =
            BuildLogCompressor.compress_with_recovery(&first.output, Some(&store), &method);
        assert_eq!(second.output, first.output);
        assert!(second.operations.is_empty());
        assert!(second.stash_writes.is_empty());
        assert_eq!(second.metrics.omitted_lines, 0);
        assert_eq!(store.calls(), calls);
    }
}

#[test]
fn detection_requires_pnpm_completion_with_structure_or_three_progress_rows_and_packages() {
    for input in [
        format!("{}{DONE}", run(0, 1)),
        format!("{}{DONE}", captured_notice()),
        format!("{LIVE_DEPENDENCIES}\n{DONE}"),
        format!("{}Packages: +3\n", run(0, 3)),
    ] {
        assert!(BuildLogCompressor::detect(&input), "undetected: {input}");
    }
    for input in [
        DONE.to_owned(),
        run(0, 12),
        format!("{}Packages: +3\n", run(0, 2)),
        "dependencies:\n".to_owned(),
        "Packages: +3\n+++\n".to_owned(),
        captured_notice(),
        "Progress is not a pnpm record.\n".to_owned(),
    ] {
        assert!(!BuildLogCompressor::detect(&input), "detected: {input}");
        assert_unchanged(&input);
    }
}

#[test]
fn malformed_counters_and_completion_lines_cannot_supply_positive_evidence() {
    let good = progress([1, 0, 1, 0], false);
    for bad in [
        good.replace("resolved 1", "resolved -1"),
        good.replace("reused 0", "reused 18446744073709551616"),
        good.replace("downloaded 1", "downloaded 1.0"),
        good.replace("added 0", "added +0"),
        good.replace(", reused", "; reused"),
        good.replace('\n', ", done later\n"),
        good.replace('\n', " trailing\n"),
        format!(" {good}"),
        format!("> {good}"),
        format!("\"{}\"\n", good.trim_end()),
    ] {
        let input = format!("{bad}{DONE}");
        assert!(!BuildLogCompressor::detect(&input), "detected: {input}");
        assert_unchanged(&input);
        assert_unchanged(&format!("{}{bad}{LIVE}", run(0, 1)));
    }
    for done in [
        "Done in soon using pnpm v10.33.4\n",
        "Done in 13.8s using pnpm v10.33.4 trailing\n",
        "Done in 13.8s using npm v10.33.4\n",
        " Done in 13.8s using pnpm v10.33.4\n",
    ] {
        assert!(!BuildLogCompressor::detect(&format!("{good}{done}")));
    }
}

#[test]
fn detection_uses_bounded_head_and_tail_without_compressing_an_unknown_preface() {
    let preface = "unrecognized preface\n".repeat(110);
    let input = format!("{preface}{LIVE}");
    assert!(BuildLogCompressor::detect(&input));
    assert_unchanged(&input);
    let middle_only = format!("{preface}{LIVE}{preface}");
    assert!(!BuildLogCompressor::detect(&middle_only));
    let oversized = format!("{}\n{LIVE}{}\n", "x".repeat(32769), "y".repeat(32769));
    assert!(!BuildLogCompressor::detect(&oversized));
}

#[test]
fn progress_threshold_preserves_two_edges_done_totals_and_actual_recovery_cost() {
    for count in [8, 9, 12] {
        let rows = run(0, count);
        let ending = format!("Packages: +9\n{}{DONE}", progress([20, 0, 20, 9], true));
        let input = format!("{rows}{ending}");
        for method in [
            RecoveryMethod::Shell,
            RecoveryMethod::tool("t".repeat(64)).unwrap(),
        ] {
            let store = RecordingStore::new(None);
            let outcome = BuildLogCompressor.compress_with_recovery(&input, Some(&store), &method);
            if count < 9 {
                assert_eq!(outcome.output, input);
                assert_eq!(store.calls(), 0);
            } else {
                let payload = middle(&rows);
                assert_eq!(
                    outcome.output,
                    input.replace(&payload, &marker(&payload, &method))
                );
                assert_eq!(outcome.metrics.omitted_lines, count - 4);
                assert_eq!(outcome.operations, [BuildLogOperation::Reduction]);
                assert!(marker(&payload, &method).chars().count() < payload.chars().count());
                assert!(estimate_tokens(&marker(&payload, &method)) < estimate_tokens(&payload));
                assert_eq!(expand_markers(&outcome, &store, &method), input);
            }
            assert!(outcome.output.ends_with(&ending));
        }
    }
}

#[test]
fn any_counter_decrease_starts_a_new_segment_with_its_own_visible_edges() {
    for reset in 0..4 {
        let first = (100..112)
            .map(|value| progress([value; 4], false))
            .collect::<String>();
        let second = (112..124)
            .map(|value| {
                let mut counters = [value; 4];
                counters[reset] = value - 112;
                progress(counters, false)
            })
            .collect::<String>();
        let input = format!("{first}{second}{DONE}");
        let store = RecordingStore::new(None);
        let method = RecoveryMethod::Shell;
        let outcome = BuildLogCompressor.compress_with_recovery(&input, Some(&store), &method);
        let expected = input
            .replace(&middle(&first), &marker(&middle(&first), &method))
            .replace(&middle(&second), &marker(&middle(&second), &method));
        assert_eq!(outcome.output, expected, "counter index {reset}");
        assert_eq!(outcome.metrics.omitted_blocks, 2);
        assert_eq!(outcome.metrics.omitted_lines, 16);
        assert_eq!(expand_markers(&outcome, &store, &method), input);
    }
}

#[test]
fn done_records_and_original_layout_boundaries_never_join_short_runs() {
    for (boundary, replacement) in [
        (progress([6, 0, 6, 0], true), progress([6, 0, 6, 0], true)),
        ("Already up to date\n".into(), "Already up to date\n".into()),
        (
            "Lockfile is up to date, resolution step is skipped\n".into(),
            "Lockfile is up to date, resolution step is skipped\n".into(),
        ),
        (captured_notice(), NOTICE_BODY.into()),
        ("Packages: +3\n+++\n".into(), "Packages: +3\n".into()),
        (
            "dependencies:\n+ a 1.0.0\n\n".into(),
            "dependencies:\n+ a 1.0.0\n\n".into(),
        ),
    ] {
        let input = format!("{}{boundary}{}{DONE}", run(0, 5), run(10, 5));
        let expected = format!("{}{replacement}{}{DONE}", run(0, 5), run(10, 5));
        let store = RecordingStore::new(None);
        let outcome =
            BuildLogCompressor.compress_with_recovery(&input, Some(&store), &RecoveryMethod::Shell);
        assert_eq!(outcome.output, expected);
        assert_eq!(outcome.metrics.omitted_blocks, 0);
        assert_eq!(store.calls(), 0);
    }
}

#[test]
fn eight_gaps_are_recoverable_but_a_ninth_falls_back_to_layout_before_writing() {
    for groups in [8, 9] {
        let mut input = String::new();
        for group in 0..groups {
            input.push_str(&run(group * 100, 12));
            input.push_str("Already up to date\n");
        }
        input.push_str(&format!("Packages: +3\n+++\n{DONE}"));
        let layout = input.replace("Packages: +3\n+++\n", "Packages: +3\n");
        let store = RecordingStore::new(None);
        let outcome =
            BuildLogCompressor.compress_with_recovery(&input, Some(&store), &RecoveryMethod::Shell);
        if groups == 8 {
            assert_eq!(outcome.metrics.omitted_blocks, 8);
            assert_eq!(store.calls(), 8);
            assert_eq!(
                expand_markers(&outcome, &store, &RecoveryMethod::Shell),
                layout
            );
        } else {
            assert_eq!(outcome.output, layout);
            assert_eq!(outcome.operations, [BuildLogOperation::Reduction]);
            assert!(outcome.stash_writes.is_empty());
            assert_eq!(store.calls(), 0);
        }
    }
}

#[test]
fn duplicate_payloads_preserve_every_marker_and_generation_write() {
    let rows = run(0, 12);
    let input = format!("{rows}Already up to date\n{rows}{DONE}");
    let store = RecordingStore::new(None);
    let method = RecoveryMethod::Shell;
    let first = BuildLogCompressor.compress_with_recovery(&input, Some(&store), &method);
    assert_eq!(first.metrics.omitted_blocks, 2);
    assert_eq!(first.stash_writes.len(), 2);
    let a = &first.stash_writes[0];
    let b = &first.stash_writes[1];
    assert_eq!(a.key, b.key);
    assert!(a.created);
    assert!(!b.created);
    assert_eq!(b.previous_generation, Some(a.generation));
    assert_eq!(store.len(), 1);
    assert_eq!(expand_markers(&first, &store, &method), input);
    let replay = BuildLogCompressor.compress_with_recovery(&input, Some(&store), &method);
    assert_eq!(replay.output, first.output);
    assert_eq!(replay.stash_writes.len(), 2);
    assert!(replay.stash_writes.iter().all(|write| !write.created));
}

#[test]
fn failed_or_partial_stashing_returns_layout_and_all_tentative_writes() {
    for successful_writes in [0, 1] {
        let extra = format!("{}Already up to date\n", run(200, 12));
        let input = format!("{extra}{LIVE}");
        let store = RecordingStore::new(Some(successful_writes));
        let outcome =
            BuildLogCompressor.compress_with_recovery(&input, Some(&store), &RecoveryMethod::Shell);
        assert_eq!(outcome.output, format!("{extra}{}", live_layout()));
        assert_eq!(outcome.operations, [BuildLogOperation::Reduction]);
        assert_eq!(outcome.recoverability, crate::Recoverability::Lossless);
        assert_eq!(outcome.metrics.stash_errors, 1);
        assert_eq!(outcome.metrics.omitted_blocks, 0);
        assert_eq!(outcome.stash_writes.len(), successful_writes);
        assert_eq!(store.calls(), successful_writes + 1);
        if successful_writes == 1 {
            let write = &outcome.stash_writes[0];
            assert!(write.created);
            assert_eq!(
                store.retrieve(&write.key).unwrap().unwrap(),
                middle(&run(200, 12))
            );
        }
    }
}

#[test]
fn first_unknown_warning_error_or_script_keeps_the_entire_suffix_opaque() {
    for boundary in [
        "unrecognized status with 50% and │ decoration\n",
        "WARN deprecated package: 50% │ keep this explanation\n",
        "ERR_PNPM_FETCH_404 GET https://registry.example.invalid/missing\n",
        "Ignored build scripts: esbuild. Run pnpm approve-builds to choose allowed scripts.\n",
        "╭ Warning ─────────────────────────────────────────────╮\n│ Ignored build scripts: esbuild. Run pnpm approve-builds │\n╰───────────────────────────────────────────────────────╯\n",
        "> synthetic-app@1.0.0 postinstall /work/synthetic-app\n> node build.js\n",
        "node_modules/esbuild postinstall$ node install.js\n",
        "Error: lifecycle failed\n    at main (/work/install.js:10:2)\n",
        "   Compiling child-script v1.0.0\n",
        "PASS src/child.test.js\n",
    ] {
        let prefix = run(0, 12);
        let child_cargo = "   Compiling child-script v1.0.0\n".repeat(12);
        let suffix = format!("{boundary}\n{LIVE}{child_cargo}{}\n", run(0, 12));
        let input = format!("{prefix}{suffix}");
        let store = RecordingStore::new(None);
        let method = RecoveryMethod::Shell;
        let outcome = BuildLogCompressor.compress_with_recovery(&input, Some(&store), &method);
        let expected_prefix = prefix.replace(&middle(&prefix), &marker(&middle(&prefix), &method));
        assert_eq!(outcome.output, format!("{expected_prefix}{suffix}"));
        assert_eq!(outcome.operations, [BuildLogOperation::Reduction]);
        assert_eq!(outcome.metrics.omitted_blocks, 1);
        assert_eq!(expand_markers(&outcome, &store, &method), input);
    }
}

#[test]
fn sgr_cleanup_preserves_warning_suffix_bytes_after_removing_only_inserted_sgr() {
    let suffix = format!("WARN 75% │ ignored scripts; run pnpm approve-builds\n\n{LIVE}");
    let colored_suffix = format!("\x1b[33m{suffix}\x1b[0m");
    let prefix = run(0, 12);
    let input = format!("{prefix}{colored_suffix}");
    let store = RecordingStore::new(None);
    let method = RecoveryMethod::Shell;
    let outcome = BuildLogCompressor.compress_with_recovery(&input, Some(&store), &method);
    assert_eq!(
        outcome.output,
        format!(
            "{}{suffix}",
            prefix.replace(&middle(&prefix), &marker(&middle(&prefix), &method))
        )
    );
    assert_eq!(
        outcome.operations,
        [
            BuildLogOperation::TerminalCleanup,
            BuildLogOperation::Reduction
        ]
    );
    assert_eq!(
        expand_markers(&outcome, &store, &method),
        format!("{prefix}{suffix}")
    );
}

#[test]
fn any_non_sgr_control_disables_all_new_pnpm_transformations() {
    let mut inputs = vec![LIVE.replace('\n', "\r\n"), LIVE.replace('\n', "\r")];
    for control in [
        "\t",
        "\r",
        "\x1b[2K",
        "\x1b[1A",
        "\x1b]8;;https://example.invalid\x07label\x1b]8;;\x07",
        "\x1b",
        "\x1b[31",
        "\x07",
    ] {
        // A trailing control must suppress transformations earlier in the input too.
        inputs.push(format!("{LIVE}{control}"));
    }
    for input in inputs {
        assert_unchanged(&input);
        let colored = format!("\x1b[32m{input}\x1b[0m");
        let store = RecordingStore::new(None);
        let outcome = BuildLogCompressor.compress_with_recovery(
            &colored,
            Some(&store),
            &RecoveryMethod::Shell,
        );
        assert_eq!(outcome.output, input);
        assert_eq!(outcome.operations, [BuildLogOperation::TerminalCleanup]);
        assert_eq!(store.calls(), 0);
    }
}

#[test]
fn captured_and_synthetic_unequal_width_notices_preserve_the_same_body() {
    let captured = captured_notice();
    let unequal = captured.replace("│   Changelog:", "│ Changelog:");
    let widths = unequal
        .lines()
        .map(|line| line.chars().count())
        .collect::<Vec<_>>();
    assert!(widths.windows(2).any(|pair| pair[0] != pair[1]));
    for notice in [captured, unequal] {
        let input = format!("{notice}{DONE}");
        let outcome =
            BuildLogCompressor.compress_with_recovery(&input, None, &RecoveryMethod::None);
        assert_eq!(outcome.output, format!("{NOTICE_BODY}{DONE}"));
        assert_eq!(outcome.operations, [BuildLogOperation::Reduction]);
        assert!(outcome.stash_writes.is_empty());
    }
}

#[test]
fn unknown_incomplete_nested_or_over_budget_notices_are_atomic_and_stop_the_scan() {
    let notice = captured_notice();
    let lines = notice.split_inclusive('\n').collect::<Vec<_>>();
    let wide_notice = notice.replace(&"─".repeat(42), &"─".repeat(1100)).replace(
        "   │                                          │\n",
        &format!("   │{}│\n", " ".repeat(1100)),
    );
    assert!(wide_notice.len() > 8 * 1024);
    assert!(wide_notice.lines().all(|line| line.len() < 4096));
    for broken in [
        wide_notice,
        lines[1..].concat(),
        lines[..6].concat(),
        notice.replace("   ╰", "  ╰"),
        notice.replace('╭', "┌"),
        notice.replace("Changelog:", "Release notes:"),
        notice.replace(
            "   │     To update",
            "   │ extra payload\n   │     To update",
        ),
        format!("{}{notice}{}", lines[0], lines[6]),
        notice.replace(
            "   │                                          │\n",
            &format!("   │{}│\n", " ".repeat(4096)),
        ),
    ] {
        let input = format!("{}{broken}{LIVE}", run(0, 1));
        assert_unchanged(&input);
    }
}

#[test]
fn only_adjacent_exact_same_sign_package_bars_are_removed() {
    for (total, bar) in [("+7", "+++++++"), ("-7", "-------")] {
        let prefix = run(0, 1);
        let input = format!("{prefix}Packages: {total}\n{bar}\n{DONE}");
        let outcome =
            BuildLogCompressor.compress_with_recovery(&input, None, &RecoveryMethod::None);
        assert_eq!(outcome.output, format!("{prefix}Packages: {total}\n{DONE}"));
        assert_eq!(outcome.operations, [BuildLogOperation::Reduction]);
    }
    for body in [
        "Packages: +7\n++++++\n",
        "Packages: -7\n+++++++\n",
        "Packages: +7\n++++++-\n",
        "Packages: +7\n+++++++ text\n",
        "Packages: +7\n +++++++\n",
        "Packages: +7\n\n+++++++\n",
        "Packages: +7 -2\n+++++++--\n",
        "Packages: +18446744073709551616\n+++++++\n",
        "+++++++\n",
    ] {
        assert_unchanged(&format!("{}{body}{DONE}", run(0, 1)));
    }
}

#[test]
fn categories_preserve_order_sign_duplicates_names_versions_and_missing_annotations() {
    let rows = "+ @scope/pkg 1.2.3-beta.1+build.7 (2.0.0-rc.1 is available)\n\
                - ordinary-name 0.1.0 (1.0.0+build.2 is available)\n\
                + @scope/pkg 1.2.3-beta.1+build.7 (2.0.0-rc.1 is available)\n\
                + no-annotation 9.8.7\n";
    let compact_rows = "+ @scope/pkg 1.2.3-beta.1+build.7; 2.0.0-rc.1\n\
                        - ordinary-name 0.1.0; 1.0.0+build.2\n\
                        + @scope/pkg 1.2.3-beta.1+build.7; 2.0.0-rc.1\n\
                        + no-annotation 9.8.7\n";
    let mut input = String::new();
    let mut expected = String::new();
    for category in ["dependencies", "devDependencies", "optionalDependencies"] {
        input.push_str(&format!("{category}:\n{rows}"));
        expected.push_str(&format!("{category} (version; available):\n{compact_rows}"));
    }
    input.push_str(DONE);
    expected.push_str(DONE);
    let outcome = BuildLogCompressor.compress_with_recovery(&input, None, &RecoveryMethod::None);
    assert_eq!(outcome.output, expected);
    let facts = dependency_facts(&input);
    assert_eq!(facts.len(), 12);
    assert_eq!(dependency_facts(&outcome.output), facts);
    assert_eq!(facts[0], facts[2]);
    assert_eq!(facts[3].available, None);
    assert_eq!(outcome.operations, [BuildLogOperation::Reduction]);
}

#[test]
fn an_unknown_dependency_row_preserves_the_whole_category_and_later_suffix() {
    for bad_row in [
        "+ alias npm:actual@1.2.3\n",
        "+ local workspace:*\n",
        "+ local file:../local\n",
        "+ local link:../local\n",
        "+ remote git+https://example.invalid/repo.git\n",
        "+ /absolute/path 1.2.3\n",
        "+ ../relative 1.2.3\n",
        "+ @missing-name 1.2.3\n",
        "+ @scope/name/extra 1.2.3\n",
        "+ name 1.2\n",
        "+ name v1.2.3\n",
        "+ name 1.2.3-\n",
        "+ name 1.2.3 (2.0.0 is deprecated)\n",
        "+ name 1.2.3 (latest is available)\n",
        "+ name 1.2.3 extra\n",
        " + indented 1.2.3\n",
        "continuation without a package sign\n",
    ] {
        let suffix = format!("{LIVE_DEPENDENCIES}{bad_row}\n{LIVE}");
        let input = format!("{}{suffix}", run(0, 1));
        assert_unchanged(&input);
    }
    assert_unchanged(&format!(
        "{}peerDependencies:\n{LIVE_DEPENDENCIES}\n{LIVE}",
        run(0, 1)
    ));
}

#[test]
fn short_dependency_lists_and_token_tied_layout_remain_verbatim() {
    for category in [
        "dependencies:\n+ a 1.0.0\n\n".to_owned(),
        "devDependencies:\n+ a 1.0.0 (2.0.0 is available)\n\n".to_owned(),
        format!("optionalDependencies:\n{}\n", "+ a 1.0.0\n".repeat(40)),
    ] {
        assert_unchanged(&format!("{category}{DONE}"));
    }
    let input = format!("{}Packages: +1\n+\n{DONE}", run(0, 1));
    let candidate = input.replace("Packages: +1\n+\n", "Packages: +1\n");
    assert!(candidate.chars().count() < input.chars().count());
    assert_eq!(estimate_tokens(&candidate), estimate_tokens(&input));
    assert_unchanged(&input);
}

#[test]
fn unterminated_relevant_records_and_dependency_categories_are_not_partially_compacted() {
    for input in [
        format!("{DONE}{}", run(0, 9).trim_end_matches('\n')),
        format!("{DONE}Packages: +7\n+++++++"),
        format!("{DONE}{}", captured_notice().trim_end_matches('\n')),
        format!("{DONE}{LIVE_DEPENDENCIES}"),
        format!("{DONE}{}", LIVE_DEPENDENCIES.trim_end_matches('\n')),
    ] {
        assert_unchanged(&input);
    }
}

#[test]
fn markdown_quotes_source_and_script_prefixes_never_promote_embedded_records() {
    for input in [
        format!("```text\n{LIVE}```\n"),
        format!("const captured = `\n{LIVE}`;\n"),
        LIVE.lines().map(|line| format!("> {line}\n")).collect(),
        LIVE.lines().map(|line| format!("    {line}\n")).collect(),
        LIVE.lines()
            .map(|line| format!("script: {line}\n"))
            .collect(),
    ] {
        assert_unchanged(&input);
    }
}

#[test]
fn recognition_line_budget_preserves_overlong_bar_and_every_later_structure() {
    for size in [4095, 4097] {
        let bar = format!("{}\n", "+".repeat(size));
        let prefix = format!("{}Packages: +{size}\n", run(0, 1));
        let input = format!("{prefix}{bar}{LIVE}");
        let outcome =
            BuildLogCompressor.compress_with_recovery(&input, None, &RecoveryMethod::None);
        if size < 4096 {
            assert_eq!(outcome.output, format!("{prefix}{}", live_layout()));
        } else {
            assert_eq!(outcome.output, input);
            assert!(outcome.operations.is_empty());
        }
    }
}

#[test]
fn dependency_row_budget_is_atomic_at_256_rows() {
    for count in [256, 257] {
        let category = format!(
            "dependencies:\n{}\n",
            "+ a 1.0.0 (2.0.0 is available)\n".repeat(count)
        );
        let input = format!("{}{category}{DONE}", run(0, 1));
        let outcome =
            BuildLogCompressor.compress_with_recovery(&input, None, &RecoveryMethod::None);
        if count == 256 {
            let expected = format!(
                "{}dependencies (version; available):\n{}\n{DONE}",
                run(0, 1),
                "+ a 1.0.0; 2.0.0\n".repeat(count)
            );
            assert_eq!(outcome.output, expected);
            assert_eq!(dependency_facts(&outcome.output), dependency_facts(&input));
        } else {
            assert_eq!(outcome.output, input);
            assert!(outcome.operations.is_empty());
        }
    }
}

#[test]
fn dependency_byte_budget_is_atomic_on_both_sides_of_64_kib() {
    for row_bytes in [511, 513] {
        let base = "+ a 1.0.0- (2.0.0 is available)\n";
        let row = base.replace(
            "1.0.0-",
            &format!("1.0.0-{}", "a".repeat(row_bytes - base.len())),
        );
        assert_eq!(row.len(), row_bytes);
        let category = format!("dependencies:\n{}\n", row.repeat(128));
        let within_budget = row_bytes == 511;
        assert_eq!(category.len() <= 64 * 1024, within_budget);
        let input = format!("{}{category}{DONE}", run(0, 1));
        let outcome =
            BuildLogCompressor.compress_with_recovery(&input, None, &RecoveryMethod::None);
        if within_budget {
            assert_eq!(outcome.operations, [BuildLogOperation::Reduction]);
            assert_eq!(dependency_facts(&outcome.output), dependency_facts(&input));
            assert_eq!(dependency_facts(&outcome.output).len(), 128);
            assert!(outcome.output.len() < input.len());
        } else {
            assert_eq!(outcome.output, input);
            assert!(outcome.operations.is_empty());
        }
    }
}

#[test]
fn repeated_bars_cannot_promote_unknown_content_on_a_second_pass() {
    let input = format!("{}Packages: +7\n+++++++\n+++++++\n{DONE}", run(0, 1));
    assert_unchanged(&input);
}

#[test]
fn sampling_windows_do_not_fabricate_a_complete_dependency_section() {
    let input = format!(
        "Packages: +7\n+++++++\n{}dependencies:\n{}{}\n{DONE}{}",
        "\n".repeat(97),
        "unrecognized middle content\n".repeat(100),
        "+ a 1.0.0 (2.0.0 is available)\n".repeat(3),
        "\n".repeat(95),
    );
    assert!(!BuildLogCompressor::detect(&input));
    assert_unchanged(&input);
}

#[test]
fn compacted_layout_retains_pnpm_identity_before_opaque_script_output() {
    let suffix = format!(
        "WARN child script follows\n$ node script.js\n{}",
        (0..12)
            .map(|index| format!("processing file {index:03}: preserve script output details\n"))
            .collect::<String>()
    );
    for (original, compact) in [
        (captured_notice(), NOTICE_BODY.to_owned()),
        (
            format!("{LIVE_DEPENDENCIES}\n"),
            format!("{COMPACT_DEPENDENCIES}\n"),
        ),
    ] {
        let input = format!("{original}{DONE}{suffix}");
        let store = RecordingStore::new(None);
        let first =
            BuildLogCompressor.compress_with_recovery(&input, Some(&store), &RecoveryMethod::Shell);
        assert_eq!(first.output, format!("{compact}{DONE}{suffix}"));
        assert_eq!(first.operations, [BuildLogOperation::Reduction]);
        let second = BuildLogCompressor.compress_with_recovery(
            &first.output,
            Some(&store),
            &RecoveryMethod::Shell,
        );
        assert_eq!(second.output, first.output);
        assert!(second.operations.is_empty());
        assert_eq!(store.calls(), 0);
    }
}

#[test]
fn layout_edit_budget_never_exceeds_256_replacements() {
    for count in [256, 257] {
        let prefix = run(0, 1);
        let input = format!("{prefix}{}{DONE}", "Packages: +7\n+++++++\n".repeat(count));
        let store = RecordingStore::new(None);
        let outcome =
            BuildLogCompressor.compress_with_recovery(&input, Some(&store), &RecoveryMethod::Shell);
        let remaining_bars = outcome
            .output
            .lines()
            .filter(|line| *line == "+++++++")
            .count();
        assert!(count - remaining_bars <= 256);
        assert_eq!(outcome.output.matches("Packages: +7\n").count(), count);
        assert!(outcome.output.starts_with(&prefix));
        assert!(outcome.output.ends_with(DONE));
        assert_eq!(store.calls(), 0);
        if count == 256 {
            assert_eq!(remaining_bars, 0);
            assert_eq!(
                outcome.output,
                format!("{prefix}{}{DONE}", "Packages: +7\n".repeat(count))
            );
        } else {
            // Budget overflow must leave the remaining records intact.
            assert!(
                outcome
                    .output
                    .ends_with(&format!("Packages: +7\n+++++++\n{DONE}"))
            );
        }
    }
}
