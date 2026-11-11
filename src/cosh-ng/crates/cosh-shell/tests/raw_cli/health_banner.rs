use std::fs;
use std::io::Read;
use std::os::unix::{fs::PermissionsExt, process::CommandExt};
use std::process::{Command, Stdio};
use std::time::Duration;

use wait_timeout::ChildExt;

#[test]
fn actual_health_banner_accepts_only_a_valid_automatic_deadline_with_captured_stdio() {
    for mode in ["manual", "expired", "valid"] {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().canonicalize().unwrap();
        let runtime = home.join("runtime");
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_cosh-shell"));
        command
            .arg0("cosh")
            .args(["health", "banner", "--budget-ms", "500"])
            .env_clear()
            .env("HOME", &home)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("PATH", "/usr/bin:/bin")
            .env("COSH_SHELL_LANG", "en-US")
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if mode != "manual" {
            let mut now: nix::libc::timespec = unsafe { std::mem::zeroed() };
            assert_eq!(
                unsafe { nix::libc::clock_gettime(nix::libc::CLOCK_BOOTTIME, &mut now) },
                0
            );
            let deadline = if mode == "valid" {
                now.tv_sec as u64 * 1_000_000 + now.tv_nsec as u64 / 1_000 + 450_000
            } else {
                1
            };
            command.args(["--bash-deadline-boottime-us", &deadline.to_string()]);
        }
        let mut child = command.spawn().unwrap();
        let status = match child.wait_timeout(Duration::from_secs(3)).unwrap() {
            Some(status) => status,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{mode}: actual health CLI exceeded test deadline");
            }
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_end(&mut stdout)
            .unwrap();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_end(&mut stderr)
            .unwrap();
        assert!(
            status.success(),
            "{mode}: {status:?}: {}",
            String::from_utf8_lossy(&stderr)
        );
        assert_eq!(
            home.join(".copilot-shell/cosh/guide").is_dir(),
            mode == "valid",
            "{mode}"
        );
        if mode != "valid" {
            assert!(
                stdout.is_empty(),
                "{mode}: {}",
                String::from_utf8_lossy(&stdout)
            );
            assert!(
                stderr.is_empty(),
                "{mode}: {}",
                String::from_utf8_lossy(&stderr)
            );
        }
        assert!(!home.join(".copilot-shell/config.toml").exists(), "{mode}");
        assert!(!home.join(".copilot-shell/logs").exists(), "{mode}");
    }
}

#[test]
fn unwritable_health_tty_does_not_consume_an_invitation() {
    use std::os::fd::AsRawFd;

    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().canonicalize().unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    let runtime = home.join("runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let pty = nix::pty::openpty(None, None).unwrap();
    let readonly = fs::File::open(format!("/proc/self/fd/{}", pty.slave.as_raw_fd())).unwrap();
    let original_flags = unsafe { nix::libc::fcntl(readonly.as_raw_fd(), nix::libc::F_GETFL) };
    let mut child = Command::new(env!("CARGO_BIN_EXE_cosh-shell"))
        .arg0("cosh")
        .args(["health", "banner"])
        .env_clear()
        .env("HOME", &home)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("PATH", "/usr/bin:/bin")
        .env("COSH_SHELL_LANG", "en-US")
        .env("COSH_SHELL_AI", "on")
        .env("COSH_SHELL_HEALTH_SCAN", "fixture:linux-warning")
        .env("TERM", "xterm-256color")
        .stdin(Stdio::null())
        .stdout(Stdio::from(readonly.try_clone().unwrap()))
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let status = match child.wait_timeout(Duration::from_secs(3)).unwrap() {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("health CLI did not return");
        }
    };
    assert!(
        !status.success(),
        "unwritable output must remain an I/O failure"
    );
    assert_eq!(
        unsafe { nix::libc::fcntl(readonly.as_raw_fd(), nix::libc::F_GETFL) },
        original_flags
    );
    assert!(
        !home.join(".copilot-shell/cosh/guide").exists(),
        "unshown invitation must not create or consume guide state"
    );
}
