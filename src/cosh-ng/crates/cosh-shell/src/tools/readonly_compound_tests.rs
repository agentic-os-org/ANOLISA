use super::command_risk_parser::SegmentConnector;
use super::readonly_compound::*;
use super::readonly_pipeline::{limit_clean_text, ReadonlyPipelineConfig, ReadonlyPipelineOutput};
use std::path::Path;

use super::*;

fn plan(command: &str) -> ReadonlyCompoundPlan {
    build_readonly_compound_plan(command).expect("eligible compound")
}

/// Builds a plan directly, bypassing eligibility: executor-mechanism
/// tests need `true`/`false`/free-form argv that the readonly
/// allowlist (correctly) never grants. Programs resolve through the
/// trusted directories like a real plan; a name that does not resolve
/// keeps its bare form so spawn reports the command-not-found path.
fn raw_plan(steps: &[(&[&str], SegmentConnector)]) -> ReadonlyCompoundPlan {
    ReadonlyCompoundPlan {
        steps: steps
            .iter()
            .map(|(argv, connector)| ReadonlyCompoundStep {
                connector: *connector,
                program: resolve_trusted_executable(argv[0])
                    .unwrap_or_else(|| std::path::PathBuf::from(argv[0])),
                argv: argv.iter().map(ToString::to_string).collect(),
                suppress_stderr: false,
            })
            .collect(),
    }
}

fn run_plan(plan: &ReadonlyCompoundPlan) -> ReadonlyPipelineOutput {
    run_readonly_compound(plan, &ReadonlyPipelineConfig::default(), Path::new("/"))
        .expect("compound run")
}

use SegmentConnector::{And, Or, Seq};

#[test]
fn build_plan_keeps_connector_sequence() {
    let plan = plan("pwd && df -h; git status --short || pwd");
    assert_eq!(plan.steps.len(), 4);
    let connectors: Vec<SegmentConnector> = plan.steps.iter().map(|step| step.connector).collect();
    assert_eq!(
        connectors,
        vec![
            SegmentConnector::Seq,
            SegmentConnector::And,
            SegmentConnector::Seq,
            SegmentConnector::Or,
        ]
    );
    assert_eq!(plan.steps[1].argv, vec!["df", "-h"]);
    // R7: eligibility and executability are one verdict — every step
    // carries a trusted absolute program resolved at plan-build time.
    for step in &plan.steps {
        assert!(
            step.program.is_absolute() && step.program.is_file(),
            "{:?}",
            step.program
        );
    }
}

#[test]
fn build_plan_accepts_quoted_and_newline_forms() {
    let quoted = plan("ls 'my dir' && pwd");
    assert_eq!(quoted.steps[0].argv, vec!["ls", "my dir"]);
    let multiline = plan("pwd\ndf -h");
    assert_eq!(multiline.steps.len(), 2);
    assert_eq!(multiline.steps[1].connector, SegmentConnector::Seq);
}

#[test]
fn build_plan_fails_closed_for_ineligible_shapes() {
    for command in [
        // non-allowlisted segment
        "cd /tmp && git status",
        "touch /tmp/a && pwd",
        // pipeline segment
        "ps aux | head -5 && pwd",
        // null redirection stripped by the parser
        "pwd && df -h 2>/dev/null",
        // read redirection
        "wc -l < notes.txt && pwd",
        // write redirection / command substitution (dominant shapes)
        "pwd && echo x > f",
        "pwd && echo $(id)",
        "pwd && echo `id`",
        // issue #2184 vector-level pin: ANSI-C `$'...'` quoting is
        // rejected before any path check (rule 5 denies `$`), so the
        // quoting bypass cannot reach the executor
        "cat $'/proc/version' && pwd",
        "cat $'/etc/shadow' && pwd",
        // expansion intent the executor would not honor
        "pwd && echo $HOME",
        // complex shapes
        "(pwd) && df -h",
        "pwd & df -h",
        // doubled separator swallows an empty segment
        "pwd && && df -h",
        // explicitly quoted empty argument: a real shell passes it
        // through (`ls ''` fails on an empty path), so silently
        // dropping it would fork assessed argv from shell behavior
        "ls '' && pwd",
        "ls \"\" && pwd",
        // environment-observing commands: the executor's controlled
        // environment is not the interactive shell's live state (R8)
        "env && pwd",
        "pwd && printenv",
        "printenv HOME || pwd",
        // `which` resolves through the user's real PATH; the executor's
        // trusted PATH would answer differently (R9)
        "which cargo || pwd",
        "pwd && which git",
        // `tty` reports the terminal attached to stdin: exit 0 on the
        // interactive PTY, exit 1 on the executor's null stdin, so the
        // `||` branch would invert
        "tty || pwd",
        "pwd && tty",
        // stdin readers: the executor's null stdin would report an
        // instant empty-input result where the interactive shell
        // reads the terminal — a bare `-` operand (even behind `--`),
        // `tr` (a pure stdin filter), and default-to-stdin filters
        // without a file operand all stay on the AskUser path
        "cat -- - && pwd",
        "tr a b && pwd",
        "sort && pwd",
        "uniq || pwd",
        "cut -f1 && pwd",
        "fold -w 10 && pwd",
        "sort -r /etc/hosts && pwd",
        // trailing separator leaves a single segment
        "pwd &&",
        // not a compound at all
        "pwd",
    ] {
        assert!(
            build_readonly_compound_plan(command).is_none(),
            "{command} must stay ineligible"
        );
    }
}

#[test]
fn build_plan_keeps_stdin_filters_with_a_file_operand_eligible() {
    // A default-to-stdin filter with a definite file operand never
    // touches stdin, so it keeps its compound grant; the conservative
    // operand detection only counts a non-flag token that does not
    // directly follow a flag-like token (which might consume it as a
    // value), so flag-adjacent forms fall back to AskUser instead.
    let plan = plan("sort /etc/hosts && pwd");
    assert_eq!(plan.steps[0].argv, vec!["sort", "/etc/hosts"]);
    assert!(build_readonly_compound_plan("uniq /etc/hosts || pwd").is_some());
}

#[test]
fn executor_short_circuits_like_bash() {
    // `&&` after success runs the next step.
    let output = run_plan(&raw_plan(&[
        (&["echo", "first"], Seq),
        (&["echo", "second"], And),
    ]));
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.stdout, "first\nsecond\n");
    // `&&` after failure skips it; the failing code is preserved.
    let output = run_plan(&raw_plan(&[(&["false"], Seq), (&["echo", "second"], And)]));
    assert_eq!(output.exit_code, Some(1));
    assert_eq!(output.stdout, "");
    // `||` after success skips the next step.
    let output = run_plan(&raw_plan(&[(&["true"], Seq), (&["echo", "second"], Or)]));
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.stdout, "");
    // `||` after failure runs it.
    let output = run_plan(&raw_plan(&[(&["false"], Seq), (&["echo", "second"], Or)]));
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.stdout, "second\n");
    // `;` always runs the next step.
    let output = run_plan(&raw_plan(&[(&["false"], Seq), (&["echo", "second"], Seq)]));
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.stdout, "second\n");
    // Left-associative chain: `false || false && echo ok` runs nothing
    // after the first failure pair, exit code is the last executed step.
    let output = run_plan(&raw_plan(&[
        (&["false"], Seq),
        (&["false"], Or),
        (&["echo", "ok"], And),
    ]));
    assert_eq!(output.exit_code, Some(1));
    assert_eq!(output.stdout, "");
    // `true && false || echo x` matches bash left-associativity.
    let output = run_plan(&raw_plan(&[
        (&["true"], Seq),
        (&["false"], And),
        (&["echo", "x"], Or),
    ]));
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.stdout, "x\n");
}

#[test]
fn executor_passes_tokens_verbatim() {
    // Every character class that a shell would expand (history
    // expansion trigger, glob, tilde, comment lead, spaces inside
    // quotes) reaches the process argv untouched.
    let output = run_plan(&raw_plan(&[(
        &["echo", "a b", "!-2", "*.log", "~", "#x"],
        Seq,
    )]));
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.stdout, "a b !-2 *.log ~ #x\n");
}

// The shell control group for the no-parsing-layer evidence lives in
// the integration layer (`tests/logic/tools.rs`): check-layout.sh
// forbids new subprocess spawns in `src` test code, and
// `executor_passes_tokens_verbatim` above covers the executor half.

#[test]
fn executor_bounds_aggregate_output() {
    let plan = raw_plan(&[
        (&["echo", "first"], Seq),
        (&["echo", "second"], Seq),
        (&["echo", "third"], Seq),
    ]);
    let output = run_readonly_compound(
        &plan,
        &ReadonlyPipelineConfig {
            output_limit_bytes: 12,
            ..ReadonlyPipelineConfig::default()
        },
        Path::new("/"),
    )
    .expect("bounded run");
    assert!(output.stdout.contains("<truncated>"), "{}", output.stdout);
    assert!(output.stdout.len() <= 32, "{}", output.stdout);
}

#[test]
fn executor_enforces_stage_timeout() {
    let plan = raw_plan(&[(&["sleep", "2"], Seq), (&["echo", "second"], Seq)]);
    let err = run_readonly_compound(
        &plan,
        &ReadonlyPipelineConfig {
            stage_timeout: std::time::Duration::from_millis(20),
            total_timeout: std::time::Duration::from_secs(10),
            ..ReadonlyPipelineConfig::default()
        },
        Path::new("/"),
    )
    .expect_err("stage must time out");
    assert_eq!(err.reason, "stage-timeout");
}

#[cfg(unix)]
#[test]
fn executor_ignores_path_shadowing_of_allowlisted_names() {
    // R6 P1 regression: a user-writable directory at the front of
    // the inherited PATH provides an executable that would create a
    // marker file if executed. PATH-based lookup would find and run
    // it; the executor resolves from the trusted system directories
    // only, so the step reports 127, the marker never appears, and
    // list evaluation continues.
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!(
        "cosh-compound-shadow-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::create_dir_all(&dir).expect("create shadow dir");
    let probe = format!("cosh-shadow-probe-{}", std::process::id());
    let marker = dir.join("shadow-executed");
    let fake = dir.join(&probe);
    std::fs::write(
        &fake,
        format!("#!/bin/sh\ntouch {}\necho SHADOWED\n", marker.display()),
    )
    .expect("write fake probe");
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fake probe");
    let original_path = std::env::var_os("PATH");
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            dir.display(),
            original_path
                .as_deref()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default()
        ),
    );
    let output = run_readonly_compound(
        &raw_plan(&[(&[probe.as_str()], Seq), (&["echo", "fallback"], Or)]),
        &ReadonlyPipelineConfig::default(),
        Path::new("/"),
    );
    match original_path {
        Some(path) => std::env::set_var("PATH", path),
        None => std::env::remove_var("PATH"),
    }
    let output = output.expect("compound run");
    let marker_created = marker.exists();
    std::fs::remove_dir_all(&dir).expect("clean shadow dir");
    assert!(!marker_created, "PATH-resolved probe must never execute");
    assert!(!output.stdout.contains("SHADOWED"), "{}", output.stdout);
    assert!(
        output.stderr.contains("command not found"),
        "{}",
        output.stderr
    );
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.stdout, "fallback\n");
}

#[test]
fn executor_caps_capture_of_oversized_step_output() {
    // R6 P1 regression: capture is pipe-drained under the budget,
    // so a step emitting far more than the budget still yields a
    // bounded aggregate, later steps still run, and no temp file is
    // involved (nothing to grow with the source size).
    let output = run_readonly_compound(
        &raw_plan(&[
            (&["yes", "budget-overflow-line"], Seq),
            (&["echo", "after-large"], Seq),
        ]),
        &ReadonlyPipelineConfig {
            output_limit_bytes: 4096,
            stage_timeout: std::time::Duration::from_millis(300),
            total_timeout: std::time::Duration::from_secs(10),
            ..ReadonlyPipelineConfig::default()
        },
        Path::new("/"),
    );
    // `yes` never exits on its own; the stage timeout ends it, and
    // the run reports the timeout error contract. What must hold is
    // that the drain kept the capture bounded rather than buffering
    // the unbounded stream.
    let err = output.expect_err("yes must hit the stage timeout");
    assert_eq!(err.reason, "stage-timeout");
    // Bounded-capture happy path: a finite oversized emitter.
    let big = "x".repeat(512 * 1024).into_bytes();
    let chunk = limit_clean_text(&big, false, 4096, 200);
    assert!(chunk.len() <= 4096 + "\n<truncated>".len());
    let output = run_readonly_compound(
        &raw_plan(&[
            (&["head", "-c", "524288", "/dev/zero"], Seq),
            (&["echo", "after-large"], Seq),
        ]),
        &ReadonlyPipelineConfig {
            output_limit_bytes: 4096,
            ..ReadonlyPipelineConfig::default()
        },
        Path::new("/"),
    )
    .expect("bounded oversized run");
    assert!(output.stdout.contains("<truncated>"));
    assert!(
        output.stdout.len() <= 4096 + 2 * "\n<truncated>".len() + "after-large\n".len(),
        "aggregate must stay near the budget, got {}",
        output.stdout.len()
    );
    assert_eq!(output.exit_code, Some(0));
}

#[test]
fn missing_binary_error_lines_pay_into_the_stderr_budget() {
    // R6 P2 regression: the synthetic `command not found` lines go
    // through the same bounded append as real step output, so a
    // tiny budget truncates them instead of growing unbounded.
    let output = run_readonly_compound(
        &raw_plan(&[
            (&["cosh-compound-missing-first"], Seq),
            (&["cosh-compound-missing-second"], Seq),
        ]),
        &ReadonlyPipelineConfig {
            output_limit_bytes: 8,
            ..ReadonlyPipelineConfig::default()
        },
        Path::new("/"),
    )
    .expect("missing-binary run");
    assert_eq!(output.exit_code, Some(127));
    assert!(output.stderr.contains("<truncated>"), "{}", output.stderr);
    assert!(
        !output.stderr.contains("cosh-compound-missing-second"),
        "second error must not exceed the exhausted budget: {}",
        output.stderr
    );
}

#[test]
fn executor_runs_steps_in_the_requested_working_directory() {
    let dir = std::env::temp_dir().join(format!(
        "cosh-compound-cwd-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::create_dir_all(&dir).expect("create cwd fixture");
    let output = run_readonly_compound(
        &raw_plan(&[(&["pwd"], Seq)]),
        &ReadonlyPipelineConfig::default(),
        &dir,
    )
    .expect("compound run");
    // macOS resolves /tmp symlinks in child processes; compare
    // canonical forms on both sides.
    let reported = std::fs::canonicalize(output.stdout.trim()).expect("reported cwd");
    let expected = std::fs::canonicalize(&dir).expect("expected cwd");
    std::fs::remove_dir_all(&dir).expect("clean cwd fixture");
    assert_eq!(reported, expected);
}

#[test]
fn executor_maps_missing_binary_to_127_and_continues_list() {
    // bash semantics: command-not-found is step status 127, not a
    // plan-level executor error, so `||` still runs the fallback
    // (e.g. `sw_vers || uname` on a platform without `sw_vers`).
    let output = run_plan(&raw_plan(&[
        (&["cosh-compound-definitely-missing-binary"], Seq),
        (&["echo", "fallback"], Or),
    ]));
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.stdout, "fallback\n");
    assert!(
        output
            .stderr
            .contains("cosh-compound-definitely-missing-binary: command not found"),
        "{}",
        output.stderr
    );
    // `&&` after a 127 step short-circuits, preserving the 127.
    let output = run_plan(&raw_plan(&[
        (&["cosh-compound-definitely-missing-binary"], Seq),
        (&["echo", "skipped"], And),
    ]));
    assert_eq!(output.exit_code, Some(127));
    assert_eq!(output.stdout, "");
}

#[cfg(unix)]
#[test]
fn executor_maps_signal_death_to_128_plus_signum() {
    // A step killed by a signal must evaluate as failed for `&&`
    // (bash reports 128+SIGTERM=143), never as success.
    let output = run_plan(&raw_plan(&[
        (&["sh", "-c", "kill -TERM $$"], Seq),
        (&["echo", "skipped"], And),
    ]));
    assert_eq!(output.exit_code, Some(143));
    assert_eq!(output.stdout, "");
    // `||` treats the signal death as failure and runs the fallback.
    let output = run_plan(&raw_plan(&[
        (&["sh", "-c", "kill -TERM $$"], Seq),
        (&["echo", "fallback"], Or),
    ]));
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.stdout, "fallback\n");
}

#[test]
fn executor_runs_steps_in_a_controlled_environment() {
    // R7 regression: steps must not observe the cosh process
    // environment (provider credentials would leak into auto-executed
    // output); only the pass-through keys and the trusted PATH are
    // present. `env` is used via raw_plan as a mechanism probe — the
    // eligibility gate itself never grants environment-observing
    // commands (R8, covered by the fail-closed cases above).
    let probe_key = format!("COSH_COMPOUND_ENV_PROBE_{}", std::process::id());
    std::env::set_var(&probe_key, "leaky-secret");
    let output = run_plan(&raw_plan(&[(&["env"], Seq)]));
    std::env::remove_var(&probe_key);
    assert_eq!(output.exit_code, Some(0));
    assert!(
        !output.stdout.contains(&probe_key),
        "cosh process env must not reach the step: {}",
        output.stdout
    );
    assert!(
        output.stdout.contains("PATH=/usr/bin:/bin:/usr/sbin:/sbin"),
        "{}",
        output.stdout
    );
}

#[cfg(unix)]
#[test]
fn executor_join_does_not_wait_for_descendants_holding_the_pipe() {
    // R7/R8 regression: a descendant keeping the pipe write end open
    // (background job, fsmonitor daemon) must not stall the run past
    // its stage deadline — and on expiry the step's whole process
    // group is terminated, so the descendant is reaped instead of
    // accumulating across auto-executions.
    let started = std::time::Instant::now();
    let output = run_readonly_compound(
        &raw_plan(&[(&["sh", "-c", "sleep 5 & echo started $!"], Seq)]),
        &ReadonlyPipelineConfig {
            stage_timeout: std::time::Duration::from_millis(400),
            total_timeout: std::time::Duration::from_secs(10),
            ..ReadonlyPipelineConfig::default()
        },
        Path::new("/"),
    )
    .expect("compound run");
    let elapsed = started.elapsed();
    assert_eq!(output.exit_code, Some(0));
    assert!(output.stdout.starts_with("started "), "{}", output.stdout);
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "join must not wait for the descendant: {elapsed:?}"
    );
    let descendant: i32 = output
        .stdout
        .trim()
        .rsplit(' ')
        .next()
        .unwrap()
        .parse()
        .expect("descendant pid");
    // The group kill reaps the descendant; give the signal a moment.
    let reaped_by = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let alive = unsafe { nix::libc::kill(descendant, 0) } == 0;
        if !alive {
            break;
        }
        assert!(
            std::time::Instant::now() < reaped_by,
            "descendant {descendant} must not outlive the deadline"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(unix)]
#[test]
fn executor_joins_readers_when_a_setsid_descendant_escapes_the_group() {
    // A descendant that calls `setsid` leaves the step's process
    // group, so the group kill cannot reach it and it keeps the pipe
    // write end open indefinitely. The drain cancellation must still
    // reclaim both reader threads within the deadline — no reader may
    // be dropped unjoined — and the run must return without waiting
    // for the escaped descendant.
    let pidfile = std::env::temp_dir().join(format!(
        "cosh-compound-setsid-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    ));
    let script = format!(
        "setsid sh -c 'echo $$ > {pidfile}; exec sleep 3600' & sleep 0.2; echo leader-done",
        pidfile = pidfile.display()
    );
    let baseline = LIVE_READER_THREADS.load(std::sync::atomic::Ordering::SeqCst);
    let started = std::time::Instant::now();
    let output = run_readonly_compound(
        &raw_plan(&[(&["sh", "-c", script.as_str()], Seq)]),
        &ReadonlyPipelineConfig {
            stage_timeout: std::time::Duration::from_millis(500),
            total_timeout: std::time::Duration::from_secs(10),
            ..ReadonlyPipelineConfig::default()
        },
        Path::new("/"),
    )
    .expect("compound run");
    let elapsed = started.elapsed();
    assert_eq!(output.exit_code, Some(0));
    assert!(output.stdout.contains("leader-done"), "{}", output.stdout);
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "run must not wait for the escaped descendant: {elapsed:?}"
    );
    // Both reader threads must be joined by the time the run returns;
    // concurrent tests may transiently hold readers of their own, so
    // poll for a sample at or below the pre-run tally.
    let reaped_by = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if LIVE_READER_THREADS.load(std::sync::atomic::Ordering::SeqCst) <= baseline {
            break;
        }
        assert!(
            std::time::Instant::now() < reaped_by,
            "drain readers must be joined, not dropped"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // Clean up the escaped descendant so it cannot outlive the test.
    if let Ok(pid_text) = std::fs::read_to_string(&pidfile) {
        if let Ok(pid) = pid_text.trim().parse::<i32>() {
            unsafe {
                nix::libc::kill(pid, nix::libc::SIGKILL);
            }
        }
    }
    let _ = std::fs::remove_file(&pidfile);
}

#[cfg(unix)]
#[test]
fn executor_reaps_descendants_that_redirected_their_output() {
    // R9 regression: a background descendant that redirects its output
    // away from the pipes lets both drains complete normally, so group
    // cleanup must not be keyed off a pending drain — the group is
    // terminated unconditionally once the direct child is done.
    let output = run_plan(&raw_plan(&[(
        &["sh", "-c", "sleep 3600 >/dev/null 2>&1 & echo bg $!"],
        Seq,
    )]));
    assert_eq!(output.exit_code, Some(0));
    assert!(output.stdout.starts_with("bg "), "{}", output.stdout);
    let descendant: i32 = output
        .stdout
        .trim()
        .rsplit(' ')
        .next()
        .unwrap()
        .parse()
        .expect("descendant pid");
    let reaped_by = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let alive = unsafe { nix::libc::kill(descendant, 0) } == 0;
        if !alive {
            break;
        }
        assert!(
            std::time::Instant::now() < reaped_by,
            "redirected descendant {descendant} must not survive the step"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
