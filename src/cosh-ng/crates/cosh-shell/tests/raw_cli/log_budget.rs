use super::*;

#[test]
fn delivered_shell_binary_enforces_budget_and_survives_notice_failure() {
    use std::process::{Command, Stdio};
    for full in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let logs = home.path().join(".copilot-shell/logs");
        std::fs::create_dir_all(&logs).unwrap();
        let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
        if full {
            std::fs::File::create(logs.join(format!("cosh-core.log.{date}")))
                .unwrap()
                .set_len(20 * 1024 * 1024)
                .unwrap();
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_cosh-shell"));
        command
            .args(["doctor"])
            .env_clear()
            .env("HOME", home.path())
            .env("PATH", std::env::var("PATH").unwrap())
            .env("COSH_LOG", "info")
            .env("COSH_SHELL_HEALTH_SCAN", "disabled")
            .env("TERM", "dumb");
        if full {
            let notice_sink = home.path().join("readonly-notice");
            fs::write(&notice_sink, b"").unwrap();
            command.stderr(Stdio::from(fs::File::open(notice_sink).unwrap()));
        }
        let output = command.output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(1),
            "doctor must report the empty fixture provider as unavailable: {output:?}"
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("doctor"),
            "{output:?}"
        );
        let total: u64 = std::fs::read_dir(&logs)
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum();
        assert!(
            total <= 20 * 1024 * 1024,
            "actual binary wrote {total} bytes"
        );
        if !full {
            assert!(
                std::fs::metadata(logs.join(format!("cosh-shell.log.{date}")))
                    .unwrap()
                    .len()
                    > 0,
                "main must connect the writer"
            );
        }
    }
}

#[test]
fn delivered_shell_binary_reports_a_budget_lock_io_failure_once() {
    let home = tempfile::tempdir().unwrap();
    let logs = home.path().join(".copilot-shell/logs");
    std::fs::create_dir_all(logs.join(".budget.lock")).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cosh-shell"))
        .args(["doctor"])
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", std::env::var("PATH").unwrap())
        .env("COSH_LOG", "info")
        .env("COSH_SHELL_HEALTH_SCAN", "disabled")
        .env("TERM", "dumb")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "doctor must report the empty fixture provider as unavailable: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches("optional log persistence stopped").count(),
        1,
        "{stderr}"
    );
}

#[test]
fn delivered_shell_binary_suppresses_contended_budget_lock_then_recovers() {
    let home = tempfile::tempdir().unwrap();
    let logs = home.path().join(".copilot-shell/logs");
    fs::create_dir_all(&logs).unwrap();
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(logs.join(".budget.lock"))
        .unwrap();
    let lock = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockSharedNonblock).unwrap();
    let run_doctor = || {
        let mut child = Command::new(env!("CARGO_BIN_EXE_cosh-shell"))
            .arg("doctor")
            .current_dir(home.path())
            .env_clear()
            .env("HOME", home.path())
            .env("PATH", std::env::var("PATH").unwrap())
            .env("COSH_LOG", "info")
            .env("COSH_SHELL_HEALTH_SCAN", "disabled")
            .env("TERM", "dumb")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let status = child.wait_timeout(Duration::from_secs(5)).unwrap();
        let Some(status) = status else {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("optional logging waited for another process's budget lock");
        };
        assert_eq!(status.code(), Some(1));
    };
    run_doctor();
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let log = logs.join(format!("cosh-shell.log.{date}"));
    assert_eq!(fs::metadata(&log).unwrap().len(), 0);
    drop(lock);
    run_doctor();
    let bytes = fs::metadata(&log).unwrap().len();
    assert!(bytes > 0, "positive control: released lock accepts logs");
    assert!(bytes <= 20 * 1024 * 1024);
}
