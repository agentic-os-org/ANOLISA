use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use cosh_shell::adapter::{CoshCoreAdapter, EcsProbeTask};
use nix::libc;
use serde_json::Value;
use tempfile::TempDir;

static PROBE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn fixture(mode: &str) -> (TempDir, CoshCoreAdapter, PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let program = home.path().join("core");
    let pid_file = home.path().join("pid");
    fs::write(
        &program,
        format!(
            r#"#!/bin/sh
trap '' TERM
read -r request
request_id=${{request#*\"request_id\":\"}}
request_id=${{request_id%%\"*}}
mkfifo {:?}
printf '%s' "$$" > {:?}
if [ {:?} != silent ]; then
    printf '{{"type":"registry_response","request_id":"%s","success":true,"data":{{"status":"ready"}}}}\n' "$request_id"
fi
if [ {:?} != ready ]; then
    read -r unused < {:?}
fi
"#,
            home.path().join("wait"), pid_file, mode, mode, home.path().join("wait")
        ),
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let adapter = CoshCoreAdapter::new(program.to_str().unwrap(), false);
    (home, adapter, pid_file)
}

fn started_pid(path: &Path) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(pid) = fs::read_to_string(path)
            .ok()
            .and_then(|value| value.parse().ok())
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "fixture never received the request"
        );
        thread::sleep(Duration::from_millis(2));
    }
}

fn finish(task: &mut EcsProbeTask) -> Result<Value, String> {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(result) = task.try_finish() {
            return result;
        }
        assert!(Instant::now() < deadline, "probe did not finish and join");
        thread::sleep(Duration::from_millis(2));
    }
}

fn assert_reaped(pid: i32) {
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "owned child still exists"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

fn sigchld_disposition() -> libc::sighandler_t {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut action) },
        0,
        "SIGCHLD disposition must be readable"
    );
    action.sa_sigaction
}

#[cfg(target_os = "linux")]
fn probe_threads() -> Vec<PathBuf> {
    fs::read_dir("/proc/self/task")
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            fs::read_to_string(path.join("comm"))
                .is_ok_and(|name| name.starts_with("cosh-auth-ecs"))
        })
        .collect()
}

#[test]
fn probe_returns_only_after_its_child_is_reaped() {
    let _guard = PROBE_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (_home, adapter, pid_file) = fixture("ready");
    let mut task = adapter.start_ecs_probe("verify").expect("start probe");
    let result = finish(&mut task).expect("ready response");
    assert_eq!(result["status"], "ready");
    assert_reaped(started_pid(&pid_file));
    assert!(task.try_finish().is_none());
}

#[test]
fn fifty_cancellations_reap_each_owned_child() {
    let _guard = PROBE_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (_home, adapter, pid_file) = fixture("silent");
    for _ in 0..50 {
        let _ = fs::remove_file(&pid_file);
        let mut task = adapter.start_ecs_probe("verify").expect("start probe");
        let pid = started_pid(&pid_file);
        #[cfg(target_os = "linux")]
        let threads = probe_threads();
        #[cfg(target_os = "linux")]
        assert_eq!(threads.len(), 1, "expected exactly one probe worker");
        let cancellation = Instant::now();
        task.cancel();
        let error = finish(&mut task).expect_err("cancelled probe must not succeed");
        assert!(error.contains("cancel"), "{error}");
        assert!(cancellation.elapsed() < Duration::from_secs(5));
        assert_reaped(pid);
        #[cfg(target_os = "linux")]
        for thread in threads {
            assert!(!thread.exists(), "cancelled probe thread still exists");
        }
    }
}

#[test]
fn dropping_a_probe_cancels_and_joins_it() {
    let _guard = PROBE_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (_home, adapter, pid_file) = fixture("silent");
    let task = adapter.start_ecs_probe("prepare").expect("start probe");
    let pid = started_pid(&pid_file);
    drop(task);
    assert_reaped(pid);
}

#[test]
fn response_without_process_exit_does_not_block_reaping() {
    let _guard = PROBE_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (_home, adapter, pid_file) = fixture("respond_hang");
    let mut task = adapter.start_ecs_probe("verify").expect("start probe");
    assert_eq!(
        finish(&mut task).expect("ready response")["status"],
        "ready"
    );
    assert_reaped(started_pid(&pid_file));
}

#[test]
fn configure_response_does_not_wait_indefinitely_for_process_exit() {
    let _guard = PROBE_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (_home, adapter, pid_file) = fixture("respond_hang");
    let (done, finished) = std::sync::mpsc::channel();
    let watchdog_pid = pid_file.clone();
    let watchdog = thread::spawn(move || {
        let pid = started_pid(&watchdog_pid);
        if finished.recv_timeout(Duration::from_secs(2)).is_err() {
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
            true
        } else {
            false
        }
    });
    let result = adapter.registry_query("auth", "configure", serde_json::json!({}));
    let _ = done.send(());
    let forced_cleanup = watchdog.join().unwrap();
    assert_reaped(started_pid(&pid_file));
    assert!(
        !forced_cleanup,
        "configure waited for the fixture watchdog to kill its child"
    );
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn probe_cleanup_with_ignored_sigchld() {
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    use wait_timeout::ChildExt;

    const CHILD_ENV: &str = "COSH_TEST_ECS_PROBE_IGNORED_SIGCHLD";
    if std::env::var_os(CHILD_ENV).is_some() {
        assert_eq!(
            sigchld_disposition(),
            libc::SIG_IGN,
            "this process must start from an inherited SIGCHLD ignore"
        );
        for mode in ["ready", "silent"] {
            let (_home, adapter, pid_file) = fixture(mode);
            let mut task = adapter.start_ecs_probe("verify").unwrap();
            // Signalling the recorded PID/PGID is only safe while the kernel
            // keeps the zombie for this process to wait for.
            assert_ne!(
                sigchld_disposition(),
                libc::SIG_IGN,
                "starting a probe must stop the kernel from reaping owned children"
            );
            let pid = started_pid(&pid_file);
            if mode == "silent" {
                task.cancel();
            }
            let result = finish(&mut task);
            if mode == "ready" {
                assert_eq!(result.unwrap()["status"], "ready");
            } else {
                assert!(result.unwrap_err().contains("cancel"));
            }
            assert_reaped(pid);
            assert!(task.try_finish().is_none());
        }

        // A caught disposition already retains zombies, so it must survive.
        extern "C" fn count_child_signal(_signal: libc::c_int) {}
        let caught = count_child_signal as *const () as libc::sighandler_t;
        assert_ne!(
            unsafe { libc::signal(libc::SIGCHLD, caught) },
            libc::SIG_ERR
        );
        let (_home, adapter, pid_file) = fixture("ready");
        let mut task = adapter.start_ecs_probe("verify").unwrap();
        let pid = started_pid(&pid_file);
        assert_eq!(finish(&mut task).unwrap()["status"], "ready");
        assert_reaped(pid);
        assert_eq!(
            sigchld_disposition(),
            caught,
            "a probe must not replace a handler installed by the host process"
        );
        return;
    }

    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "ecs_probe::probe_cleanup_with_ignored_sigchld",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1");
    // Signal disposition must not leak into the other protocol tests.
    unsafe {
        command.pre_exec(|| {
            if libc::signal(libc::SIGCHLD, libc::SIG_IGN) == libc::SIG_ERR {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    let status = child.wait_timeout(Duration::from_secs(12)).unwrap();
    if status.is_none() {
        child.kill().unwrap();
        child.wait().unwrap();
    }
    assert!(
        status.is_some_and(|status| status.success()),
        "probe cleanup must finish after the kernel has already reaped its child"
    );
}

#[test]
fn silent_probe_times_out_and_reaps_its_child() {
    let _guard = PROBE_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (_home, adapter, pid_file) = fixture("silent");
    let mut task = adapter.start_ecs_probe("prepare").expect("start probe");
    let pid = started_pid(&pid_file);
    let error = finish(&mut task).expect_err("silent probe must time out");
    assert!(error.contains("timed out"), "{error}");
    assert_reaped(pid);
}
