//! Real inherited streams, signal forwarding and isolated foreground-terminal sessions.
#![cfg(target_os = "linux")]

// The shared fixture also exposes byte-transport helpers used by execution.rs.
#[allow(dead_code)]
#[path = "support/fixture.rs"]
mod fixture;

use aw_exec::{run_foreground, CommandSpec};
use fixture::{execute, limits, wait_until_exists, Directory, NORMAL_TIMEOUT};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::{CommandExt, ExitStatusExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
    thread,
    time::{Duration, Instant},
};

fn helper_command(directory: &Directory, scenario: &str) -> CommandSpec {
    let python = directory.command("unused").program;
    CommandSpec {
        program: std::env::current_exe().unwrap(),
        args: vec![
            "--exact".into(),
            "foreground_helper".into(),
            "--nocapture".into(),
        ],
        cwd: directory.0.clone(),
        environment: BTreeMap::from([
            ("LC_ALL".into(), "C".into()),
            (
                "AW_FOREGROUND_FIXTURE".into(),
                directory.0.clone().into_os_string(),
            ),
            ("AW_FOREGROUND_SCENARIO".into(), scenario.into()),
            ("AW_FOREGROUND_PYTHON".into(), python.into_os_string()),
        ]),
    }
}

#[test]
fn foreground_helper() {
    let Some(directory) = std::env::var_os("AW_FOREGROUND_FIXTURE") else {
        return;
    };
    let directory = PathBuf::from(directory);
    record_process(&directory, "helper", std::process::id());
    let scenario = std::env::var("AW_FOREGROUND_SCENARIO").unwrap();
    // Each helper has a separate process and terminal session, so signal and
    // foreground changes never affect libtest's own process or other tests.
    let original = unsafe { libc::getpgrp() };
    let mut anchor = if scenario == "background" {
        let child = Command::new("/bin/sleep")
            .arg("8")
            .process_group(0)
            .spawn()
            .unwrap();
        record_process(&directory, "anchor", child.id());
        assert_eq!(unsafe { libc::tcsetpgrp(0, child.id() as i32) }, 0);
        Some(child)
    } else {
        None
    };
    let before = unsafe { libc::tcgetpgrp(0) };
    let signal = AtomicI32::new(0);
    let finished = AtomicBool::new(false);
    let command = CommandSpec {
        program: std::env::var_os("AW_FOREGROUND_PYTHON").unwrap().into(),
        args: vec![
            "-I".into(),
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/foreground.py")
                .into_os_string(),
            directory.clone().into_os_string(),
            scenario.clone().into(),
        ],
        cwd: directory.clone(),
        environment: BTreeMap::from([
            ("LC_ALL".into(), "C".into()),
            ("FOREGROUND_CONTEXT".into(), "explicit".into()),
        ]),
    };
    let started = Instant::now();
    let result = thread::scope(|scope| {
        let notifier = scope.spawn(|| {
            let deadline = Instant::now() + NORMAL_TIMEOUT;
            loop {
                if finished.load(Ordering::Acquire) {
                    return;
                }
                if (matches!(scenario.as_str(), "signal" | "stubborn")
                    && directory.join("ready").exists())
                    || (scenario == "tty-stubborn" && directory.join("child-terminal").exists())
                {
                    signal.store(libc::SIGTERM, Ordering::Release);
                    return;
                }
                if Instant::now() >= deadline {
                    signal.store(libc::SIGKILL, Ordering::Release);
                    return;
                }
                thread::sleep(Duration::from_millis(5));
            }
        });
        let result = run_foreground(&command, &signal);
        finished.store(true, Ordering::Release);
        notifier.join().unwrap();
        result
    });
    let after = unsafe { libc::tcgetpgrp(0) };
    fs::write(
        directory.join("terminal"),
        format!("{original} {before} {after}"),
    )
    .unwrap();
    fs::write(
        directory.join("elapsed"),
        started.elapsed().as_millis().to_string(),
    )
    .unwrap();
    let record = match result {
        Ok(status) => format!("code={:?} signal={:?}", status.code(), status.signal()),
        Err(error) => format!("error={error}"),
    };
    fs::write(directory.join("result"), record).unwrap();
    if let Some(child) = anchor.as_mut() {
        // The isolated helper is now a background group; restore its terminal
        // without SIGTTOU before terminating the separate foreground anchor.
        let previous = unsafe { libc::signal(libc::SIGTTOU, libc::SIG_IGN) };
        assert_eq!(unsafe { libc::tcsetpgrp(0, original) }, 0);
        unsafe {
            libc::signal(libc::SIGTTOU, previous);
        }
        child.kill().unwrap();
        child.wait().unwrap();
    }
}

fn record_process(directory: &Path, name: &str, pid: u32) {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let start = stat
        .rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .nth(19)
        .unwrap();
    fs::write(
        directory.join(format!("{name}.pid")),
        format!("{pid} {start}"),
    )
    .unwrap();
}

#[test]
fn inherited_streams_context_and_native_exit_status_are_preserved() {
    let directory = Directory::new();
    let input = b"inherited-input\0\xff";
    let output = execute(
        &helper_command(&directory, "streams"),
        input,
        limits(input.len(), 4096, 4096),
    )
    .unwrap();
    assert!(output.status.success());
    assert!(output
        .stdout
        .windows(input.len())
        .any(|window| window == input));
    assert_eq!(output.stderr, b"inherited-stderr\xff");
    assert_eq!(
        fs::read_to_string(directory.0.join("result")).unwrap(),
        "code=Some(7) signal=None"
    );
    directory.assert_reaped();
}

#[test]
fn atomic_signal_is_forwarded_and_stubborn_group_has_a_bounded_grace() {
    for scenario in ["signal", "stubborn"] {
        let directory = Directory::new();
        let output = execute(
            &helper_command(&directory, scenario),
            b"",
            limits(0, 4096, 4096),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected = if scenario == "signal" {
            "code=None signal=Some(15)"
        } else {
            "code=None signal=Some(9)"
        };
        assert_eq!(
            fs::read_to_string(directory.0.join("result")).unwrap(),
            expected
        );
        let elapsed: u128 = fs::read_to_string(directory.0.join("elapsed"))
            .unwrap()
            .parse()
            .unwrap();
        assert!(elapsed < 4500);
        if scenario == "stubborn" {
            assert!(elapsed >= 2000, "termination grace ended early: {elapsed}");
            directory.assert_descendant_stopped();
        } else {
            directory.assert_reaped();
        }
    }
}

#[test]
fn naturally_exited_leader_keeps_status_and_cleans_live_descendants() {
    let directory = Directory::new();
    let output = execute(
        &helper_command(&directory, "descendant"),
        b"",
        limits(0, 4096, 4096),
    )
    .unwrap();
    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(directory.0.join("result")).unwrap(),
        "code=Some(23) signal=None"
    );
    directory.assert_descendant_stopped();
}

struct Pty {
    master: File,
    slave: File,
}
impl Pty {
    fn new() -> Self {
        let mut master = -1;
        let mut slave = -1;
        // SAFETY: openpty fills two descriptors, adopted exactly once on success.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                )
            },
            0
        );
        Self {
            master: unsafe { File::from_raw_fd(master) },
            slave: unsafe { File::from_raw_fd(slave) },
        }
    }
}

struct Running(Child);
impl Running {
    fn start(directory: &Directory, scenario: &str, pty: &Pty, controlling: bool) -> Self {
        let spec = helper_command(directory, scenario);
        let mut command = Command::new(spec.program);
        command
            .args(spec.args)
            .env_clear()
            .envs(spec.environment)
            .current_dir(spec.cwd)
            .stdin(pty.slave.try_clone().unwrap())
            .stdout(pty.slave.try_clone().unwrap())
            .stderr(Stdio::from(
                File::create(directory.0.join("helper.stderr")).unwrap(),
            ));
        // SAFETY: only async-signal-safe session/terminal syscalls run after fork;
        // fd 0 has already been installed by Command and belongs to this fixture.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if controlling && libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Self(command.spawn().unwrap())
    }
    fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + NORMAL_TIMEOUT + Duration::from_secs(2);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "foreground helper did not exit");
            thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[test]
fn terminal_foreground_is_transferred_for_input_and_restored_after_exit() {
    for scenario in ["tty", "tty-stubborn"] {
        let directory = Directory::new();
        let mut pty = Pty::new();
        let mut before = unsafe { std::mem::zeroed::<libc::termios>() };
        assert_eq!(
            unsafe { libc::tcgetattr(pty.slave.as_raw_fd(), &mut before) },
            0
        );
        let mut helper = Running::start(&directory, scenario, &pty, true);
        assert!(wait_until_exists(
            &directory.0.join("leader.pid"),
            Instant::now() + NORMAL_TIMEOUT
        ));
        pty.master.write_all(b"x\n").unwrap();
        assert!(
            helper.wait().success(),
            "{}",
            fs::read_to_string(directory.0.join("helper.stderr")).unwrap()
        );
        let mut after = unsafe { std::mem::zeroed::<libc::termios>() };
        assert_eq!(
            unsafe { libc::tcgetattr(pty.slave.as_raw_fd(), &mut after) },
            0
        );
        assert_eq!(
            before.c_lflag, after.c_lflag,
            "{scenario}: terminal local modes leaked"
        );
        assert_eq!(before.c_iflag, after.c_iflag);
        assert_eq!(before.c_oflag, after.c_oflag);
        assert_eq!(before.c_cflag, after.c_cflag);
        assert_eq!(before.c_cc, after.c_cc);
        let terminal = fs::read_to_string(directory.0.join("terminal")).unwrap();
        let groups: Vec<_> = terminal.split_whitespace().collect();
        assert_eq!(groups[0], groups[1]);
        assert_eq!(groups[0], groups[2]);
        assert_eq!(
            fs::read_to_string(directory.0.join("result")).unwrap(),
            if scenario == "tty" {
                "code=Some(0) signal=None"
            } else {
                "code=None signal=Some(9)"
            }
        );
        assert_eq!(
            fs::read_to_string(directory.0.join("child-terminal")).unwrap(),
            "foreground"
        );
        directory.assert_reaped();
    }
}

#[test]
fn background_caller_does_not_take_another_groups_terminal() {
    let directory = Directory::new();
    let pty = Pty::new();
    let mut helper = Running::start(&directory, "background", &pty, true);
    assert!(
        helper.wait().success(),
        "{}",
        fs::read_to_string(directory.0.join("helper.stderr")).unwrap()
    );
    let terminal = fs::read_to_string(directory.0.join("terminal")).unwrap();
    let groups: Vec<_> = terminal.split_whitespace().collect();
    assert_ne!(groups[0], groups[1]);
    assert_eq!(groups[1], groups[2]);
    assert_eq!(
        fs::read_to_string(directory.0.join("child-terminal")).unwrap(),
        "background"
    );
    assert_eq!(
        fs::read_to_string(directory.0.join("result")).unwrap(),
        "code=Some(0) signal=None"
    );
    assert!(directory.process("anchor").state().is_none());
    directory.assert_reaped();
}

#[test]
fn terminal_transfer_error_still_cleans_the_spawned_child() {
    let directory = Directory::new();
    let pty = Pty::new();
    let mut helper = Running::start(&directory, "signal", &pty, false);
    assert!(helper.wait().success());
    assert!(fs::read_to_string(directory.0.join("result"))
        .unwrap()
        .contains("terminal transfer"));
    if directory.0.join("leader.pid").exists() {
        directory.assert_reaped();
    }
}
