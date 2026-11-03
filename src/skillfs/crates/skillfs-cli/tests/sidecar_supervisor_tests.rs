//! Process-level tests for the Kubernetes sidecar mount supervisor.
//!
//! Fake worker, preflight, and probe executables keep these tests independent
//! of `/dev/fuse` while exercising the shipped Bash supervisor end to end.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const WORKER: &str = r#"#!/usr/bin/env bash
set -uo pipefail
printf 'start\n' >> "$TEST_STATE/starts"
date +%s%N >> "$TEST_STATE/start_times"
start_count="$(wc -l < "$TEST_STATE/starts")"
trap 'printf "term\n" >> "$TEST_STATE/terms"; exit 0' TERM INT
printf 'ready\n' > "$TEST_STATE/worker-ready"
if [[ "$TEST_SCENARIO" == "exit-once" && "$start_count" == "1" ]]; then
    sleep 0.15
    exit 42
fi
if [[ "$TEST_SCENARIO" == "exit-always" || "$TEST_SCENARIO" == "exit-always-healthy-probe" ]]; then
    sleep 0.08
    exit 42
fi
while :; do sleep 0.05; done
"#;

const PREFLIGHT: &str = r#"#!/usr/bin/env bash
set -uo pipefail
printf '%s\n' "${1:-full}" >> "$TEST_STATE/preflights"
[[ "$TEST_SCENARIO" == "preflight-failure" ]] && exit 1
exit 0
"#;

const PROBE: &str = r#"#!/usr/bin/env bash
set -uo pipefail
count=0
if [[ -f "$TEST_STATE/probes" ]]; then
    count="$(wc -l < "$TEST_STATE/probes")"
fi
count=$((count + 1))
printf '%s\n' "$count" >> "$TEST_STATE/probes"
case "$TEST_SCENARIO" in
healthy | sleep-race)
    for _ in {1..100}; do
        [[ -f "$TEST_STATE/worker-ready" ]] && exit 0
        sleep 0.01
    done
    exit 2
    ;;
delayed-mount)
    ((count <= 2)) && exit 2
    ;;
transient-failure)
    ((count == 2)) && exit 3
    ;;
io-failure)
    ((count == 2 || count == 3)) && exit 3
    ;;
never-healthy)
    exit 2
    ;;
exit-always)
    exit 2
    ;;
esac
exit 0
"#;

struct Harness {
    _temp: tempfile::TempDir,
    state: PathBuf,
    supervisor: PathBuf,
    worker: PathBuf,
    preflight: PathBuf,
    probe: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("test tempdir");
        let state = temp.path().join("state");
        fs::create_dir(&state).expect("state dir");

        let supervisor = temp.path().join("supervisor.sh");
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../container/supervisor.sh");
        fs::copy(source, &supervisor).expect("copy supervisor");
        make_executable(&supervisor);

        let worker = write_executable(temp.path(), "worker.sh", WORKER);
        let preflight = write_executable(temp.path(), "preflight.sh", PREFLIGHT);
        let probe = write_executable(temp.path(), "probe.sh", PROBE);

        Self {
            _temp: temp,
            state,
            supervisor,
            worker,
            preflight,
            probe,
        }
    }

    fn command(&self, scenario: &str) -> Command {
        let mut command = Command::new("bash");
        command
            .process_group(0)
            .arg(&self.supervisor)
            .arg(&self.worker)
            .env("PATH", "/usr/bin:/bin")
            .env("TEST_STATE", &self.state)
            .env("TEST_SCENARIO", scenario)
            .env("SKILLFS_SUPERVISOR_PREFLIGHT_BIN", &self.preflight)
            .env("SKILLFS_SUPERVISOR_PROBE_BIN", &self.probe)
            .env("SKILLFS_SUPERVISOR_PROBE_INTERVAL_SECONDS", "0.05")
            .env("SKILLFS_SUPERVISOR_FAILURE_THRESHOLD", "2")
            .env("SKILLFS_SUPERVISOR_STABLE_HEALTHY_PROBES", "2")
            .env("SKILLFS_SUPERVISOR_STARTUP_TIMEOUT_SECONDS", "3")
            .env("SKILLFS_SUPERVISOR_STOP_TIMEOUT_SECONDS", "1")
            .env("SKILLFS_SUPERVISOR_MAX_FAILED_ATTEMPTS", "3")
            .env("SKILLFS_SUPERVISOR_BACKOFF_INITIAL_SECONDS", "0.05")
            .env("SKILLFS_SUPERVISOR_BACKOFF_MAX_SECONDS", "0.1")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    fn spawn(&self, scenario: &str) -> SupervisorProcess {
        SupervisorProcess {
            child: Some(self.command(scenario).spawn().expect("spawn supervisor")),
        }
    }

    fn line_count(&self, name: &str) -> usize {
        fs::read_to_string(self.state.join(name))
            .unwrap_or_default()
            .lines()
            .count()
    }

    fn start_times(&self) -> Vec<u128> {
        fs::read_to_string(self.state.join("start_times"))
            .expect("start timestamps")
            .lines()
            .map(|line| line.parse().expect("nanosecond timestamp"))
            .collect()
    }

    fn wait_for_lines(&self, name: &str, minimum: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if self.line_count(name) >= minimum {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "timed out waiting for {minimum} lines in {name}; observed {}",
            self.line_count(name)
        );
    }
}

struct SupervisorProcess {
    child: Option<Child>,
}

impl SupervisorProcess {
    fn terminate_and_wait(&mut self) -> ExitStatus {
        self.signal_and_wait("-TERM")
    }

    fn signal_and_wait(&mut self, signal: &str) -> ExitStatus {
        let started = Instant::now();
        let child = self.child.as_mut().expect("live supervisor");
        let status = Command::new("kill")
            .args([signal, &child.id().to_string()])
            .status()
            .expect("send shutdown signal");
        assert!(status.success(), "kill must succeed");
        let status = self.wait(Duration::from_secs(5));
        assert!(
            started.elapsed() < Duration::from_millis(800),
            "graceful shutdown should reap the worker without consuming the stop timeout"
        );
        status
    }

    fn wait(&mut self, timeout: Duration) -> ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            let child = self.child.as_mut().expect("live supervisor");
            if let Some(status) = child.try_wait().expect("poll supervisor") {
                // Retain the process-group ID so Drop also cleans up a sleeper
                // left behind by a supervisor that exited prematurely.
                return status;
            }
            if Instant::now() >= deadline {
                panic!("supervisor did not exit within {timeout:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for SupervisorProcess {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = Command::new("kill")
                .args(["-TERM", "--", &format!("-{}", child.id())])
                .stderr(Stdio::null())
                .status();
            for _ in 0..20 {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = Command::new("kill")
                .args(["-KILL", "--", &format!("-{}", child.id())])
                .stderr(Stdio::null())
                .status();
            let _ = child.wait();
        }
    }
}

fn write_executable(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, contents).expect("write helper");
    make_executable(&path);
    path
}

fn make_executable(path: &Path) {
    let mut permissions = fs::metadata(path).expect("helper metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod helper");
}

#[test]
fn healthy_probe_requires_worker_readiness() {
    let harness = Harness::new();
    let mut probe = Command::new(&harness.probe);
    probe
        .env("PATH", "/usr/bin:/bin")
        .env("TEST_STATE", &harness.state)
        .env("TEST_SCENARIO", "healthy");

    assert_eq!(
        probe.status().expect("probe before readiness").code(),
        Some(2)
    );
    fs::write(harness.state.join("worker-ready"), "ready\n").expect("worker readiness");
    assert!(probe.status().expect("probe after readiness").success());
}

#[test]
fn initial_not_mounted_waits_for_health_without_remounting() {
    let harness = Harness::new();
    let mut supervisor = harness.spawn("delayed-mount");
    harness.wait_for_lines("probes", 4);

    assert_eq!(harness.line_count("starts"), 1);
    assert!(supervisor.terminate_and_wait().success());
}

#[test]
fn one_transient_failure_does_not_remount() {
    let harness = Harness::new();
    let mut supervisor = harness.spawn("transient-failure");
    harness.wait_for_lines("probes", 4);

    assert_eq!(harness.line_count("starts"), 1);
    assert!(supervisor.terminate_and_wait().success());
}

#[test]
fn consecutive_io_failures_remount() {
    let harness = Harness::new();
    let mut supervisor = harness.spawn("io-failure");
    harness.wait_for_lines("starts", 2);
    harness.wait_for_lines("probes", 4);

    assert_eq!(harness.line_count("starts"), 2);
    assert!(harness.line_count("preflights") >= 2);
    assert!(
        fs::read_to_string(harness.state.join("preflights"))
            .expect("preflight log")
            .lines()
            .any(|line| line == "--cleanup-only")
    );
    assert!(supervisor.terminate_and_wait().success());
}

#[test]
fn unexpected_worker_exit_remounts() {
    let harness = Harness::new();
    let started = Instant::now();
    let mut supervisor = harness.spawn("exit-once");
    harness.wait_for_lines("starts", 2);

    assert_eq!(harness.line_count("starts"), 2);
    assert!(
        started.elapsed() < Duration::from_millis(800),
        "an exited worker should be reaped and remounted without consuming the stop timeout"
    );
    assert!(supervisor.terminate_and_wait().success());
}

#[test]
fn sigterm_exits_cleanly_without_respawn() {
    let harness = Harness::new();
    let mut supervisor = SupervisorProcess {
        child: Some(
            harness
                .command("healthy")
                .env("SKILLFS_SUPERVISOR_PROBE_INTERVAL_SECONDS", "5")
                .spawn()
                .expect("spawn supervisor"),
        ),
    };
    harness.wait_for_lines("probes", 1);
    harness.wait_for_lines("worker-ready", 1);

    assert!(supervisor.terminate_and_wait().success());
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(harness.line_count("starts"), 1);
    assert_eq!(harness.line_count("terms"), 1);
}

fn shutdown_during_long_sleep(scenario: &str, inject_race: bool) {
    let harness = Harness::new();
    // DEBUG runs before PID assignment, making the launch/registration race
    // deterministic without changing the shipped supervisor script.
    let bash_env = write_executable(
        harness._temp.path(),
        "sleep-hook.sh",
        r#"set -T
trap 'if [[ "$BASH_COMMAND" == '\''sleep_pid=$!'\'' && "$duration" == 30 ]]; then
    printf "%s\n" "$!" >> "$TEST_STATE/sleepers"
    trap - DEBUG
    if [[ "$TEST_INJECT_SLEEP_RACE" == 1 ]]; then
        kill -TERM "$$"
    fi
fi' DEBUG
"#,
    );
    let mut supervisor = SupervisorProcess {
        child: Some(
            harness
                .command(scenario)
                .env("BASH_ENV", bash_env)
                .env(
                    "TEST_INJECT_SLEEP_RACE",
                    if inject_race { "1" } else { "0" },
                )
                .env("SKILLFS_SUPERVISOR_PROBE_INTERVAL_SECONDS", "30")
                .env("SKILLFS_SUPERVISOR_BACKOFF_INITIAL_SECONDS", "30")
                .env("SKILLFS_SUPERVISOR_BACKOFF_MAX_SECONDS", "30")
                .spawn()
                .expect("spawn supervisor"),
        ),
    };
    harness.wait_for_lines("sleepers", 1);

    let status = if inject_race {
        supervisor.wait(Duration::from_millis(800))
    } else {
        supervisor.signal_and_wait("-INT")
    };
    assert!(status.success());
    let expected_workers = usize::from(scenario != "preflight-failure");
    assert_eq!(harness.line_count("starts"), expected_workers);
    assert_eq!(harness.line_count("terms"), expected_workers);
    assert!(
        fs::read_to_string(harness.state.join("preflights"))
            .expect("preflight log")
            .lines()
            .any(|line| line == "--cleanup-only")
    );
    for pid in fs::read_to_string(harness.state.join("sleepers"))
        .expect("sleeper PIDs")
        .lines()
    {
        assert!(
            !Path::new("/proc").join(pid).exists(),
            "sleeper {pid} must be terminated and reaped"
        );
    }
}

#[test]
fn sigint_interrupts_probe_interval_without_respawn() {
    shutdown_during_long_sleep("healthy", false);
}

#[test]
fn sigint_interrupts_recovery_backoff_without_respawn() {
    shutdown_during_long_sleep("preflight-failure", false);
}

#[test]
fn sigterm_between_sleep_launch_and_pid_assignment_reaps_sleeper() {
    shutdown_during_long_sleep("sleep-race", true);
}

#[test]
fn repeated_failed_starts_exhaust_with_bounded_backoff() {
    let harness = Harness::new();
    let started = Instant::now();
    // Separate retry intervals enough that worker/probe scheduling jitter
    // cannot dominate the backoff increase being measured.
    let mut supervisor = SupervisorProcess {
        child: Some(
            harness
                .command("exit-always")
                .env("SKILLFS_SUPERVISOR_BACKOFF_INITIAL_SECONDS", "0.2")
                .env("SKILLFS_SUPERVISOR_BACKOFF_MAX_SECONDS", "0.4")
                .spawn()
                .expect("spawn supervisor"),
        ),
    };
    let status = supervisor.wait(Duration::from_secs(5));

    assert!(!status.success());
    assert_eq!(harness.line_count("starts"), 3);
    let start_times = harness.start_times();
    let first_gap = start_times[1] - start_times[0];
    let second_gap = start_times[2] - start_times[1];
    assert!(
        started.elapsed() >= Duration::from_millis(120),
        "failed attempts should be separated by backoff"
    );
    assert!(
        second_gap >= first_gap + 30_000_000,
        "exponential backoff should increase retry spacing: first={first_gap}ns second={second_gap}ns"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn rapid_post_start_flapping_exhausts_recovery_budget() {
    let harness = Harness::new();
    let started = Instant::now();
    let mut supervisor = harness.spawn("exit-always-healthy-probe");
    let status = supervisor.wait(Duration::from_secs(5));

    assert!(!status.success());
    assert_eq!(harness.line_count("starts"), 3);
    assert!(started.elapsed() >= Duration::from_millis(120));
}

#[test]
fn invalid_numeric_configuration_fails_before_starting_worker() {
    let harness = Harness::new();
    let status = harness
        .command("healthy")
        .env("SKILLFS_SUPERVISOR_PROBE_INTERVAL_SECONDS", "NaN")
        .status()
        .expect("run supervisor");

    assert_eq!(status.code(), Some(64));
    assert_eq!(harness.line_count("starts"), 0);
    assert_eq!(harness.line_count("preflights"), 0);
}

#[test]
fn cleanup_only_preflight_does_not_require_fuse_device() {
    let temp = tempfile::tempdir().expect("test tempdir");
    let source = temp.path().join("source");
    let mountpoint = temp.path().join("mount");
    fs::create_dir(&source).expect("source dir");
    fs::create_dir(&mountpoint).expect("mountpoint dir");
    let preflight = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../container/preflight.sh");

    let status = Command::new("bash")
        .arg(preflight)
        .arg("--cleanup-only")
        .env("PATH", "/usr/bin:/bin")
        .env("SKILLFS_SOURCE", source)
        .env("SKILLFS_MOUNTPOINT", mountpoint)
        .env("SKILLFS_FUSE_DEVICE", temp.path().join("missing-fuse"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run cleanup-only preflight");

    assert!(status.success());
}

#[test]
fn entrypoint_passes_exact_foreground_mount_argv_to_supervisor() {
    let temp = tempfile::tempdir().expect("test tempdir");
    let args_file = temp.path().join("args");
    let skillfs = write_executable(
        temp.path(),
        "skillfs",
        "#!/usr/bin/env bash\n[[ \"${1:-}\" == --version ]] && echo 'skillfs test'\n",
    );
    let supervisor = write_executable(
        temp.path(),
        "supervisor",
        "#!/usr/bin/env bash\nprintf '%s\\n' \"$@\" > \"$TEST_ARGS_FILE\"\n",
    );
    let entrypoint = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../container/entrypoint.sh");

    let status = Command::new("bash")
        .arg(entrypoint)
        .env("PATH", "/usr/bin:/bin")
        .env("TEST_ARGS_FILE", &args_file)
        .env("SKILLFS_BIN", &skillfs)
        .env("SKILLFS_SUPERVISOR_BIN", &supervisor)
        .env("SKILLFS_SOURCE", "/source")
        .env("SKILLFS_MOUNTPOINT", "/mount")
        .env("SKILLFS_DISCOVER_ROOT", "/discover")
        .env("SKILLFS_EXTRA_ARGS", "--security --decision-command helper")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run entrypoint");

    assert!(status.success());
    let args: Vec<_> = fs::read_to_string(args_file)
        .expect("captured argv")
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        args,
        [
            skillfs.to_string_lossy().into_owned(),
            "mount".to_owned(),
            "/source".to_owned(),
            "/mount".to_owned(),
            "--foreground".to_owned(),
            "--allow-other".to_owned(),
            "--skill-discover-root".to_owned(),
            "/discover".to_owned(),
            "--security".to_owned(),
            "--decision-command".to_owned(),
            "helper".to_owned(),
        ]
    );
}
