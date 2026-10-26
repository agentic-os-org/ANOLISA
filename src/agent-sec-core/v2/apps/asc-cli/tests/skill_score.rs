//! The public Rust entrypoint delegates locally without changing the assessment process contract.

mod common;

use std::ffi::{OsStr, OsString};
use std::io::Write as _;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};

use asc_cli::{Cli, InputError, Plan};
use serde_json::{Value, json};

const PRIVATE_CLI: &str = "lib/anolisa/sec-core/skill-score/cli/bin/agent-sec-cli";
static INSTALLATION_LOCK: Mutex<()> = Mutex::new(());
const FIXTURE: &str = r"#!/usr/bin/env python3
import json, os, signal, sys
mode = os.environ.get('SCORE_TEST_MODE', 'capture')
if mode == 'signal':
    os.kill(os.getpid(), signal.SIGTERM)
if mode == 'wait':
    def stop(signum, frame):
        print('cleanup-on-term', file=sys.stderr, flush=True)
        sys.exit(23)
    signal.signal(signal.SIGTERM, stop)
    signal.alarm(10)
print(json.dumps({'argv': [list(os.fsencode(a)) for a in sys.argv[1:]],
                  'pid': os.getpid(), 'pgid': os.getpgrp(),
                  'stdin': sys.stdin.read() if sys.argv[-1] == '-' else '',
                  'model_key': os.environ.get('ANTHROPIC_API_KEY')}), flush=True)
if mode == 'wait':
    while True:
        signal.pause()
print('assessment-diagnostic', file=sys.stderr)
sys.exit(int(os.environ.get('SCORE_TEST_EXIT', '0')))
";

fn installation(with_payload: bool) -> (common::Directory, MutexGuard<'static, ()>) {
    // Concurrent fork can retain another fixture's writable executable fd until exec, causing
    // ETXTBSY even though that fixture has closed its own copy. Serialize copying and child runs.
    let guard = INSTALLATION_LOCK.lock().unwrap();
    let directory = common::Directory::new();
    std::fs::create_dir(directory.0.join("bin")).unwrap();
    std::fs::copy(
        env!("CARGO_BIN_EXE_agent-sec-cli"),
        directory.0.join("bin/agent-sec-cli"),
    )
    .unwrap();
    if with_payload {
        let private = directory.0.join(PRIVATE_CLI);
        std::fs::create_dir_all(private.parent().unwrap()).unwrap();
        std::fs::write(&private, FIXTURE).unwrap();
        std::fs::set_permissions(private, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (directory, guard)
}

fn command(prefix: &Path) -> Command {
    let mut command = Command::new(prefix.join("bin/agent-sec-cli"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("AGENT_SEC_DAEMON_SOCKET", "relative-invalid-socket")
        .env("ANTHROPIC_API_KEY", "test-only-placeholder")
        .args(["skillsec", "score"]);
    command
}

fn argv_bytes(arguments: &[&OsStr]) -> Value {
    json!(
        arguments
            .iter()
            .map(|argument| argument.as_bytes().to_vec())
            .collect::<Vec<_>>()
    )
}

#[test]
fn score_is_explicitly_local_and_never_constructs_a_daemon_request() {
    let cli = Cli::parse_from(["cli", "skillsec", "score", "plan", "skill"]).unwrap();
    assert!(matches!(cli.plan(), Plan::SkillScore(_)));
    assert!(cli.socket().is_none());
    assert!(!cli.is_skill_sec());
    assert!(matches!(cli.request(), Err(InputError::LocalCommand)));
}

#[test]
fn old_and_new_skill_names_keep_the_same_daemon_contract() {
    let parse =
        |name| Cli::parse_from(["cli", "--socket", "/run/fixture.sock", name, "status"]).unwrap();
    let old = parse("skill-ledger");
    let new = parse("skillsec");
    assert!(matches!(new.plan(), Plan::Daemon { .. }));
    assert!(new.is_skill_sec());
    assert_eq!(
        serde_json::to_value(old.request().unwrap()).unwrap(),
        serde_json::to_value(new.request().unwrap()).unwrap()
    );
    assert_eq!(old.timeout(), new.timeout());
}

#[test]
fn score_rejects_daemon_and_context_options_instead_of_ignoring_them() {
    for (option, value) in [
        ("--socket", "/run/fixture.sock"),
        ("--timeout-ms", "9000"),
        ("--otel-context", "{}"),
        ("--trace-context", "{}"),
    ] {
        let error =
            Cli::parse_from(["cli", option, value, "skillsec", "score", "run", "-"]).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        assert!(error.to_string().contains(option), "{error}");
    }
    let error = Cli::parse_from([
        "cli",
        "skillsec",
        "score",
        "plan",
        "skill",
        "--timeout-ms=9000",
    ])
    .unwrap_err();
    assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn public_help_and_usage_errors_do_not_need_a_payload() {
    let (prefix, _installation_guard) = installation(false);
    for arguments in [
        vec!["--help"],
        vec!["plan", "--help"],
        vec!["run", "--help"],
    ] {
        let output = command(&prefix.0).args(arguments).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("skillsec score"));
    }
    let output = Command::new(prefix.0.join("bin/agent-sec-cli"))
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("skillsec"), "{help}");
    assert!(!help.contains("skill-ledger"), "{help}");
    for arguments in [
        vec![],
        vec!["plan"],
        vec!["run"],
        vec!["unknown"],
        vec!["run", "-", "--unknown"],
        vec!["plan", "skill", "--evidence-dir", "evidence"],
    ] {
        let output = command(&prefix.0).args(arguments).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("scoring payload"));
    }
}

#[test]
fn plan_preserves_os_paths_and_reports_the_runtime_exit_status() {
    let (prefix, _installation_guard) = installation(true);
    let skill = OsString::from_vec(b"skill with space-\xff".to_vec());
    let output_path = PathBuf::from("report with spaces-\u{4e2d}.json");
    for exit_code in [0, 1, 2, 77] {
        let output = command(&prefix.0)
            .arg("plan")
            .arg(&skill)
            .arg("--output-file")
            .arg(&output_path)
            .env("SCORE_TEST_EXIT", exit_code.to_string())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(exit_code), "{output:?}");
        let body: Value = serde_json::from_slice(&output.stdout).unwrap();
        let mut output_option = OsString::from("--output-file=");
        output_option.push(&output_path);
        assert_eq!(
            body["argv"],
            argv_bytes(&[
                OsStr::new("skill-score"),
                OsStr::new("plan"),
                &output_option,
                OsStr::new("--"),
                &skill,
            ])
        );
        assert_eq!(body["model_key"], "test-only-placeholder");
        assert_eq!(output.stderr, b"assessment-diagnostic\n");
    }
}

#[test]
fn run_preserves_inline_json_option_looking_paths_and_stdin() {
    let (prefix, _installation_guard) = installation(true);
    let inline = r#"{"skill_path":"/work/\u4e2d skill","note":"$(printf should-not-run)"}"#;
    for input in [inline, "-"] {
        let mut child = command(&prefix.0)
            .args([
                "run",
                input,
                "--output-file=-report.json",
                "--evidence-dir",
                "\u{4e2d} evidence",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if input == "-" {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(inline.as_bytes())
                .unwrap();
        }
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let body: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            body["argv"],
            argv_bytes(&[
                OsStr::new("skill-score"),
                OsStr::new("run"),
                OsStr::new("--output-file=-report.json"),
                OsStr::new("--evidence-dir=\u{4e2d} evidence"),
                OsStr::new("--"),
                OsStr::new(input),
            ])
        );
        assert_eq!(body["stdin"], if input == "-" { inline } else { "" });
    }
}

#[test]
fn a_missing_private_payload_fails_without_using_path_or_a_daemon() {
    let (prefix, _installation_guard) = installation(false);
    let output = command(&prefix.0).args(["plan", "skill"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("scoring payload"), "{error}");
    assert!(error.contains(PRIVATE_CLI), "{error}");
    assert!(!error.contains("socket"), "{error}");
}

#[test]
fn exec_preserves_signal_termination() {
    let (prefix, _installation_guard) = installation(true);
    let output = command(&prefix.0)
        .args(["plan", "skill"])
        .env("SCORE_TEST_MODE", "signal")
        .output()
        .unwrap();
    assert_eq!(output.status.signal(), Some(15));
}

#[test]
fn exec_preserves_pid_process_group_and_term_cleanup() {
    let (prefix, _installation_guard) = installation(true);
    let output = Command::new("python3")
        .arg("-c")
        .arg(
            r"
import json, os, select, signal, subprocess, sys
env = dict(os.environ, SCORE_TEST_MODE='wait', AGENT_SEC_DAEMON_SOCKET='invalid-relative')
child = subprocess.Popen([sys.argv[1], 'skillsec', 'score', 'plan', 'skill'],
                         env=env, start_new_session=True,
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE)
try:
    assert select.select([child.stdout], [], [], 5)[0], 'runtime did not become ready'
    record = json.loads(child.stdout.readline())
    assert record['pid'] == child.pid, record
    assert record['pgid'] == child.pid, record
    os.killpg(child.pid, signal.SIGTERM)
    stdout, stderr = child.communicate(timeout=5)
    assert child.returncode == 23, (child.returncode, stderr)
    assert stderr == b'cleanup-on-term\n', stderr
finally:
    if child.poll() is None:
        os.killpg(child.pid, signal.SIGKILL)
        child.wait(timeout=5)
",
        )
        .arg(prefix.0.join("bin/agent-sec-cli"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}
