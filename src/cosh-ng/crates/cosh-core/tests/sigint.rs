#![cfg(target_os = "linux")]

use std::fs;
use std::ops::{Deref, DerefMut};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn binary_path() -> std::path::PathBuf {
    let mut path = std::env::current_exe()
        .expect("current test executable")
        .parent()
        .expect("test binary directory")
        .parent()
        .expect("target directory")
        .to_path_buf();
    path.push("cosh-core");
    path
}

struct ChildGuard(Child);

impl Deref for ChildGuard {
    type Target = Child;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn signal_mask(pid: u32, field: &str) -> u64 {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).expect("read process status");
    let value = status
        .lines()
        .find_map(|line| line.strip_prefix(field))
        .expect("signal status")
        .trim();
    u64::from_str_radix(value, 16).expect("parse signal mask")
}

fn send_signal(pid: u32, signal: &str) {
    let status = Command::new("kill")
        .args([format!("-{signal}"), pid.to_string()])
        .status()
        .expect("run kill");
    assert!(status.success(), "send SIG{signal}");
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("poll child") {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn sigint_exits_when_inherited_as_ignored() {
    let home = tempfile::tempdir().expect("temp home");
    let binary = binary_path();
    let mut child = ChildGuard(
        Command::new("sh")
            .args(["-c", "trap '' INT; exec \"$1\" --headless", "sh"])
            .arg(binary)
            .env("HOME", home.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn cosh-core"),
    );
    let pid = child.id();

    let deadline = Instant::now() + Duration::from_secs(2);
    while signal_mask(pid, "SigCgt:\t") & 0b10 == 0 {
        assert!(
            Instant::now() < deadline,
            "SIGINT handler was not installed"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        child.try_wait().expect("poll child").is_none(),
        "cosh-core exited before receiving SIGINT"
    );

    send_signal(pid, "INT");
    let status = wait_for_exit(&mut child, Duration::from_secs(2))
        .expect("cosh-core did not exit after SIGINT");
    assert!(
        status.success(),
        "cosh-core exited unsuccessfully: {status}"
    );
}
