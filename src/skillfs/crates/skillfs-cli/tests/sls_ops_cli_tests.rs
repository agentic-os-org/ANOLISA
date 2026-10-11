//! CLI-level tests for SLS ops JSONL logging.
//!
//! Each `skillfs` subcommand appends one ops record to the deployment-owned
//! ops log. Tests point the writer at a temp file via `SKILLFS_SLS_OPS_PATH`
//! (validated to live under `/tmp/`) and assert the appended record.

use std::os::fd::{FromRawFd, OwnedFd};
use std::path::Path;
use std::process::{Command, Stdio};

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_skillfs")
}

/// Skip guard for record-expecting tests.
///
/// The CLI subprocess checks the real `/etc/anolisa/.telemetry_disabled` at
/// runtime, and there is deliberately no production override to relocate it.
/// When telemetry is disabled on the host, the CLI writes zero ops records, so
/// the "expect one record" assertions below cannot hold — skip rather than
/// fail. The enabled write path stays covered by the unit tests, which inject a
/// temp sentinel. Returns `true` (and prints a SKIP line) when disabled.
fn skip_if_telemetry_disabled() -> bool {
    if skillfs_fuse::security::telemetry_allowed() {
        return false;
    }
    eprintln!("SKIP: telemetry disabled on host (/etc/anolisa/.telemetry_disabled present)");
    true
}

const VALID_SKILL: &str = r#"---
name: good-skill
description: A valid skill
version: "1.0"
---
# Good Skill

This skill works correctly.
"#;

/// SKILL.md with invalid YAML frontmatter → ParseStatus::Error.
const ERROR_SKILL: &str = r#"---
name: [invalid yaml
  broken: {{{}
---
Body text.
"#;

fn create_skill_dir(parent: &Path, name: &str, content: &str) {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).expect("create skill dir");
    std::fs::write(dir.join("SKILL.md"), content).expect("write SKILL.md");
}

/// Create the deployment-owned ops log under /tmp so the CLI (which never
/// creates the file) will append to it.
fn make_ops_log(dir: &Path) -> std::path::PathBuf {
    let ops_log = dir.join("skillfs-ops.jsonl");
    std::fs::File::create(&ops_log).expect("pre-create ops log");
    ops_log
}

/// Read all JSONL records from the ops log.
fn read_records(ops_log: &Path) -> Vec<serde_json::Value> {
    let content = std::fs::read_to_string(ops_log).unwrap_or_default();
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSON record"))
        .collect()
}

fn run_skillfs(args: &[&str], ops_log: &Path) -> std::process::Output {
    Command::new(bin_path())
        .args(args)
        // Temp dirs live under /tmp on Linux, an allowed override prefix.
        .env("SKILLFS_SLS_OPS_PATH", ops_log)
        .output()
        .expect("invoke skillfs")
}

/// Create a pipe and close its read end, returning the write end. A child that
/// inherits it gets EPIPE on its first write regardless of timing.
/// Close-on-exec prevents subprocesses spawned by parallel tests from
/// accidentally inheriting either endpoint during setup.
fn readerless_pipe() -> OwnedFd {
    let mut fds = [0_i32; 2];
    // SAFETY: `pipe2` writes two fresh, close-on-exec fds into `fds` on success.
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    assert_eq!(rc, 0, "pipe2() failed: {}", std::io::Error::last_os_error());
    // SAFETY: a successful `pipe2` returned two fresh fds owned by this test.
    let read_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    // SAFETY: as above.
    let write_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    drop(read_fd);
    write_fd
}

/// Spawn `skillfs` with a reader-less stdout pipe, so the first `println!`
/// fails with EPIPE and the process panics — the broken-pipe scenario from
/// issue #1506 (`skillfs list | head -5`). The read end is closed before the
/// spawn: closing it afterwards races the child, which can write its whole
/// output into the pipe buffer and exit 0 first. stderr is discarded so
/// tracing and the panic message never block.
fn run_with_broken_stdout(args: &[&str], ops_log: &Path) -> std::process::ExitStatus {
    let mut child = Command::new(bin_path())
        .args(args)
        .env("SKILLFS_SLS_OPS_PATH", ops_log)
        .stdout(Stdio::from(readerless_pipe()))
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn skillfs");
    child.wait().expect("wait for skillfs")
}

/// Reproduce `skillfs <cmd> 2>&1 | <reader that closes immediately>` (the
/// issue's own repro). Creates a pipe and closes its read end *before*
/// spawning, so the child inherits a reader-less pipe on both stdout and
/// stderr. The first tracing write in `main` hits EPIPE; tracing's default
/// internal error report then writes to the closed stderr and panics before
/// the command body runs. This has no dependence on scheduling or output size
/// and covers both the earliest possible close and the guard armed before
/// logging.
fn run_with_merged_output_closed(args: &[&str], ops_log: &Path) -> std::process::ExitStatus {
    let write_fd = readerless_pipe();
    let write_dup = write_fd.try_clone().expect("clone pipe write fd");

    let mut child = Command::new(bin_path())
        .args(args)
        .env("SKILLFS_SLS_OPS_PATH", ops_log)
        .stdout(Stdio::from(write_fd))
        .stderr(Stdio::from(write_dup))
        .spawn()
        .expect("spawn skillfs");
    child.wait().expect("wait for skillfs")
}

#[test]
fn list_appends_ops_record_on_success() {
    if skip_if_telemetry_disabled() {
        return;
    }
    // /tmp-based temp dir required so the override prefix check accepts it.
    let dir = tempfile::tempdir_in("/tmp").expect("tempdir");
    let ops_log = make_ops_log(dir.path());

    let source = tempfile::tempdir_in("/tmp").expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);

    let out = run_skillfs(&["list", source.path().to_str().unwrap()], &ops_log);
    assert!(out.status.success(), "list should succeed");

    let records = read_records(&ops_log);
    assert_eq!(records.len(), 1, "expected one ops record");
    assert_eq!(records[0]["component.name"], "skillfs");
    assert_eq!(records[0]["component.agent_name"], "cli");
    assert_eq!(records[0]["ops_name"], "list");
    assert_eq!(records[0]["err_reason"], "none");
    assert!(
        !records[0]["component.version"].as_str().unwrap().is_empty(),
        "component.version must be populated"
    );
}

#[test]
fn validate_appends_ops_record_on_success() {
    if skip_if_telemetry_disabled() {
        return;
    }
    let dir = tempfile::tempdir_in("/tmp").expect("tempdir");
    let ops_log = make_ops_log(dir.path());

    let source = tempfile::tempdir_in("/tmp").expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);

    let out = run_skillfs(&["validate", source.path().to_str().unwrap()], &ops_log);
    assert!(out.status.success(), "validate should succeed");

    let records = read_records(&ops_log);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["ops_name"], "validate");
    assert_eq!(records[0]["err_reason"], "none");
}

#[test]
fn validate_appends_record_before_nonzero_exit() {
    if skip_if_telemetry_disabled() {
        return;
    }
    let dir = tempfile::tempdir_in("/tmp").expect("tempdir");
    let ops_log = make_ops_log(dir.path());

    let source = tempfile::tempdir_in("/tmp").expect("source tempdir");
    create_skill_dir(source.path(), "bad-yaml", ERROR_SKILL);

    let out = run_skillfs(&["validate", source.path().to_str().unwrap()], &ops_log);
    assert!(
        !out.status.success(),
        "validate with a bad skill must exit non-zero"
    );

    let records = read_records(&ops_log);
    assert_eq!(records.len(), 1, "record must be written before exiting");
    assert_eq!(records[0]["ops_name"], "validate");
    assert_ne!(
        records[0]["err_reason"], "none",
        "failed validation must set a non-none err_reason"
    );
}

#[test]
fn classify_dry_run_appends_ops_record_on_success() {
    if skip_if_telemetry_disabled() {
        return;
    }
    let dir = tempfile::tempdir_in("/tmp").expect("tempdir");
    let ops_log = make_ops_log(dir.path());

    let source = tempfile::tempdir_in("/tmp").expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);

    let out = run_skillfs(
        &["classify", source.path().to_str().unwrap(), "--dry-run"],
        &ops_log,
    );
    assert!(out.status.success(), "classify --dry-run should succeed");

    let records = read_records(&ops_log);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["ops_name"], "classify");
    assert_eq!(records[0]["err_reason"], "none");
}

#[test]
fn missing_ops_log_is_not_created_and_command_succeeds() {
    let dir = tempfile::tempdir_in("/tmp").expect("tempdir");
    // Do NOT create the ops log — the CLI must skip writing without creating it.
    let ops_log = dir.path().join("absent-ops.jsonl");

    let source = tempfile::tempdir_in("/tmp").expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);

    let out = run_skillfs(&["list", source.path().to_str().unwrap()], &ops_log);
    assert!(
        out.status.success(),
        "list should still succeed when the ops log is absent"
    );
    assert!(
        !ops_log.exists(),
        "CLI must not create the missing ops log file"
    );
}

// ---------------------------------------------------------------------------
// Broken-pipe regression tests (issue #1506)
//
// When a downstream reader closes early (`skillfs list | head -5` or the
// issue's own `skillfs list 2>&1 | head -5`), the CLI's `println!`/`eprintln!`
// panics on EPIPE and unwinds. The SLS ops record must still be written exactly
// once via the drop guard, with `err_reason="panic"`.
//
// Two channels are covered: stdout-only (panic in the command body, after the
// guard) and merged stdout+stderr closed before any output (panic at the first
// logging write in `main`, exercising the guard armed before logging).
// ---------------------------------------------------------------------------

#[test]
fn list_writes_one_record_when_stdout_reader_closes_early() {
    if skip_if_telemetry_disabled() {
        return;
    }
    let dir = tempfile::tempdir_in("/tmp").expect("tempdir");
    let ops_log = make_ops_log(dir.path());

    let source = tempfile::tempdir_in("/tmp").expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);

    let status = run_with_broken_stdout(&["list", source.path().to_str().unwrap()], &ops_log);
    assert!(
        !status.success(),
        "list must not exit 0 once its stdout pipe breaks"
    );

    let records = read_records(&ops_log);
    assert_eq!(
        records.len(),
        1,
        "exactly one list ops record must survive the broken pipe"
    );
    assert_eq!(records[0]["ops_name"], "list");
    assert_eq!(records[0]["err_reason"], "panic");
}

#[test]
fn classify_dry_run_writes_one_record_when_stdout_reader_closes_early() {
    if skip_if_telemetry_disabled() {
        return;
    }
    let dir = tempfile::tempdir_in("/tmp").expect("tempdir");
    let ops_log = make_ops_log(dir.path());

    let source = tempfile::tempdir_in("/tmp").expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);

    let status = run_with_broken_stdout(
        &["classify", source.path().to_str().unwrap(), "--dry-run"],
        &ops_log,
    );
    assert!(
        !status.success(),
        "classify must not exit 0 once its stdout pipe breaks"
    );

    let records = read_records(&ops_log);
    assert_eq!(
        records.len(),
        1,
        "exactly one classify ops record must survive the broken pipe"
    );
    assert_eq!(records[0]["ops_name"], "classify");
    assert_eq!(records[0]["err_reason"], "panic");
    // --dry-run must not persist a config even when it panics mid-print.
    assert!(
        !source.path().join("skillfs-views.toml").exists(),
        "classify --dry-run must not write skillfs-views.toml"
    );
}

#[test]
fn validate_writes_one_record_when_stdout_reader_closes_early() {
    if skip_if_telemetry_disabled() {
        return;
    }
    let dir = tempfile::tempdir_in("/tmp").expect("tempdir");
    let ops_log = make_ops_log(dir.path());

    let source = tempfile::tempdir_in("/tmp").expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);

    let status = run_with_broken_stdout(&["validate", source.path().to_str().unwrap()], &ops_log);
    assert!(
        !status.success(),
        "validate must not exit 0 once its stdout pipe breaks"
    );

    let records = read_records(&ops_log);
    assert_eq!(
        records.len(),
        1,
        "exactly one validate ops record must survive the broken pipe"
    );
    assert_eq!(records[0]["ops_name"], "validate");
    assert_eq!(records[0]["err_reason"], "panic");
}

#[test]
fn mount_writes_one_record_through_guard_on_fast_failure() {
    // `mount` routes through the same `SlsOpsGuard` as the other subcommands. A
    // fast configuration error returns before any FUSE mount, so the guard's
    // `finish` path emits exactly one "mount" record without a live mount. The
    // broken-pipe Drop path for `mount` is covered separately by
    // `mount_writes_one_record_when_merged_output_closes_early`.
    if skip_if_telemetry_disabled() {
        return;
    }
    let dir = tempfile::tempdir_in("/tmp").expect("tempdir");
    let ops_log = make_ops_log(dir.path());

    let source = tempfile::tempdir_in("/tmp").expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);
    let mountpoint = tempfile::tempdir_in("/tmp").expect("mountpoint tempdir");

    // An invalid --activation-mode fails inside cmd_mount before mounting.
    let out = run_skillfs(
        &[
            "mount",
            source.path().to_str().unwrap(),
            mountpoint.path().to_str().unwrap(),
            "--activation-mode",
            "bogus",
        ],
        &ops_log,
    );
    assert!(
        !out.status.success(),
        "an invalid --activation-mode must exit non-zero"
    );

    let records = read_records(&ops_log);
    assert_eq!(
        records.len(),
        1,
        "exactly one mount ops record on a fast startup failure"
    );
    assert_eq!(records[0]["ops_name"], "mount");
    assert_ne!(
        records[0]["err_reason"], "none",
        "a startup failure must set a non-none err_reason"
    );
}

// ---------------------------------------------------------------------------
// Merged stdout+stderr (`2>&1`) closed before any output.
//
// The consumer closes before the first tracing write in `main`; that write
// gets EPIPE, then tracing's internal error report panics on the closed stderr.
// Because the guard is armed before logging, exactly one record must still be
// written with `err_reason="panic"`. This is the strict form of the
// contract: logging must not depend on when the downstream closes.
// ---------------------------------------------------------------------------

#[test]
fn list_writes_one_record_when_merged_output_closes_early() {
    if skip_if_telemetry_disabled() {
        return;
    }
    let dir = tempfile::tempdir_in("/tmp").expect("tempdir");
    let ops_log = make_ops_log(dir.path());

    let source = tempfile::tempdir_in("/tmp").expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);

    let status =
        run_with_merged_output_closed(&["list", source.path().to_str().unwrap()], &ops_log);
    assert!(!status.success(), "list must not exit 0 on a broken pipe");

    let records = read_records(&ops_log);
    assert_eq!(records.len(), 1, "exactly one list ops record");
    assert_eq!(records[0]["ops_name"], "list");
    assert_eq!(records[0]["err_reason"], "panic");
}

#[test]
fn classify_dry_run_writes_one_record_when_merged_output_closes_early() {
    if skip_if_telemetry_disabled() {
        return;
    }
    let dir = tempfile::tempdir_in("/tmp").expect("tempdir");
    let ops_log = make_ops_log(dir.path());

    let source = tempfile::tempdir_in("/tmp").expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);

    let status = run_with_merged_output_closed(
        &["classify", source.path().to_str().unwrap(), "--dry-run"],
        &ops_log,
    );
    assert!(
        !status.success(),
        "classify must not exit 0 on a broken pipe"
    );

    let records = read_records(&ops_log);
    assert_eq!(records.len(), 1, "exactly one classify ops record");
    assert_eq!(records[0]["ops_name"], "classify");
    assert_eq!(records[0]["err_reason"], "panic");
    assert!(
        !source.path().join("skillfs-views.toml").exists(),
        "classify --dry-run must not write skillfs-views.toml"
    );
}

#[test]
fn validate_writes_one_record_when_merged_output_closes_early() {
    if skip_if_telemetry_disabled() {
        return;
    }
    let dir = tempfile::tempdir_in("/tmp").expect("tempdir");
    let ops_log = make_ops_log(dir.path());

    let source = tempfile::tempdir_in("/tmp").expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);

    let status =
        run_with_merged_output_closed(&["validate", source.path().to_str().unwrap()], &ops_log);
    assert!(
        !status.success(),
        "validate must not exit 0 on a broken pipe"
    );

    let records = read_records(&ops_log);
    assert_eq!(records.len(), 1, "exactly one validate ops record");
    assert_eq!(records[0]["ops_name"], "validate");
    assert_eq!(records[0]["err_reason"], "panic");
}

#[test]
fn mount_writes_one_record_when_merged_output_closes_early() {
    if skip_if_telemetry_disabled() {
        return;
    }
    // The first tracing write gets EPIPE, then tracing's internal error report
    // panics on the closed stderr. This happens before the command body and any
    // FUSE mount attempt; the guard records the panic and the test leaves no
    // mount.
    let dir = tempfile::tempdir_in("/tmp").expect("tempdir");
    let ops_log = make_ops_log(dir.path());

    let source = tempfile::tempdir_in("/tmp").expect("source tempdir");
    create_skill_dir(source.path(), "good-skill", VALID_SKILL);
    let mountpoint = tempfile::tempdir_in("/tmp").expect("mountpoint tempdir");

    let status = run_with_merged_output_closed(
        &[
            "mount",
            source.path().to_str().unwrap(),
            mountpoint.path().to_str().unwrap(),
        ],
        &ops_log,
    );
    assert!(!status.success(), "mount must not exit 0 on a broken pipe");

    let records = read_records(&ops_log);
    assert_eq!(records.len(), 1, "exactly one mount ops record");
    assert_eq!(records[0]["ops_name"], "mount");
    assert_eq!(records[0]["err_reason"], "panic");
}

#[test]
fn mount_file_validation_logs_actual_error_once() {
    if skip_if_telemetry_disabled() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("mount.toml");
    std::fs::write(&config, "mountpoint = '/mnt/skillfs'\nsources = []\n").unwrap();
    let log = make_ops_log(temp.path());
    let output = run_skillfs(&["mount", "--config", config.to_str().unwrap()], &log);
    assert!(!output.status.success());
    let records = read_records(&log);
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0]["err_reason"],
        "mount config sources must not be empty"
    );
}
