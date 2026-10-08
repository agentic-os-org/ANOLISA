//! Regression tests for login-startup PATH probe containment.
//!
//! The login PATH probes (`bash -lic` / `zsh -lic`) execute the user's login
//! startup files. A profile that backgrounds a daemon must not leave that
//! daemon behind as an unmanaged orphan of the probe: the managed shell
//! re-sources the same files moments later, so the probe's copy is a
//! duplicate that must be reaped by the supervised helper. These tests spawn
//! the real `cosh-shell` binary as an interactive login and assert that a
//! profile-started background process does not outlive the probe.
//! Linux-only: needs a real `bash`, `setsid`, and `TIOCSCTTY`.

use std::fs;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nix::libc;
use wait_timeout::ChildExt;

use crate::support::raw_cli::{raw_cli_shared_run_guard, RawCliRunGuard};

const DEADLINE: Duration = Duration::from_secs(20);

/// A login `cosh-shell` driven over a PTY for probe-containment assertions.
struct LoginProbeSession {
    child: Child,
    master: std::fs::File,
    output: Vec<u8>,
    root: std::path::PathBuf,
    _gate: RawCliRunGuard,
}

impl LoginProbeSession {
    /// Spawn `cosh-shell` as an interactive login shell (argv[0] carries the
    /// `-` login prefix) whose `.bash_profile` starts a background daemon with
    /// all three standard descriptors redirected, the shape a real daemon
    /// (ssh-agent, gpg-agent, ...) takes.
    fn spawn_with_daemon_profile(profile: &str) -> Self {
        let gate = raw_cli_shared_run_guard();
        let root = tempfile::Builder::new()
            .prefix("cosh-login-probe-")
            .tempdir()
            .expect("login probe fixture HOME");
        let root_path = root.path().to_path_buf();
        fs::write(root_path.join(".bash_profile"), profile).unwrap();
        fs::write(root_path.join(".bashrc"), "PS1='lp$ '\n").unwrap();
        fs::create_dir(root_path.join(".copilot-shell")).unwrap();
        fs::write(
            root_path.join(".copilot-shell/config.toml"),
            "[shell]\nadapter_default = 'fake'\n",
        )
        .unwrap();

        let size = libc::winsize {
            ws_row: 24,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = nix::pty::openpty(Some(&size), None).expect("login probe PTY");
        let master = std::fs::File::from(pty.master);
        let slave = std::fs::File::from(pty.slave);
        let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );

        let mut command = Command::new(env!("CARGO_BIN_EXE_cosh-shell"));
        command
            .arg0("-cosh")
            .args(["--shell", "bash"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &root_path)
            .env("TMPDIR", &root_path)
            .env("TERM", "xterm-256color")
            .env("LANG", "C.UTF-8")
            .env("LC_ALL", "C.UTF-8")
            // The login PATH probe is the subject under test: bootstrap must
            // be on. R2 stays off so the login probe is the only child that
            // executes the profile.
            .env("COSH_SHELL_BOOTSTRAP_PATH", "1")
            .env("COSH_SHELL_LOGIN_IDENTITY", "0")
            .env("COSH_SHELL_STARTUP_BANNER", "0")
            .env("COSH_SHELL_HEALTH_SCAN", "disabled")
            .env("COSH_RECOMMENDATIONS_ENABLED", "0")
            .current_dir(&root_path)
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0
                    || libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().expect("spawn login probe session");
        // Keep the fixture alive for the whole session; the tempdir guard is
        // moved out because the assertions read files from it after `finish`.
        std::mem::forget(root);
        Self {
            child,
            master,
            output: Vec::new(),
            root: root_path,
            _gate: gate,
        }
    }

    fn pump_once(&mut self) {
        let mut buf = [0u8; 8192];
        match self.master.read(&mut buf) {
            Ok(count) if count > 0 => self.output.extend_from_slice(&buf[..count]),
            _ => std::thread::sleep(Duration::from_millis(10)),
        }
    }

    fn wait_for(&mut self, needle: &str) -> bool {
        let deadline = Instant::now() + DEADLINE;
        while Instant::now() < deadline {
            if self.text().contains(needle) {
                return true;
            }
            self.pump_once();
        }
        self.text().contains(needle)
    }

    fn send(&mut self, bytes: &[u8]) {
        let _ = self.master.write_all(bytes);
        let _ = self.master.flush();
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.output).into_owned()
    }

    /// Drain remaining output and reap the session child within the deadline.
    fn finish(&mut self) -> String {
        let deadline = Instant::now() + DEADLINE;
        loop {
            match self.child.wait_timeout(Duration::from_millis(50)) {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
        for _ in 0..20 {
            self.pump_once();
        }
        self.text()
    }
}

impl Drop for LoginProbeSession {
    fn drop(&mut self) {
        // A panicking assertion drops the session with the child still live;
        // reap it so the test binary never leaks a shell or a zombie.
        let _ = self.child.kill();
        let _ = self.child.wait();
        // The fixture was kept alive via mem::forget; clean it up now that
        // every assertion reading from it has run.
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// `kill(pid, 0)` probe: true when the process still exists.
fn process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn best_effort_kill(pid: u32) {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
}

/// Read the daemon pids the profile appended, one per line.
fn daemon_pids(root: &std::path::Path) -> Vec<u32> {
    fs::read_to_string(root.join("probe_daemons.pids"))
        .expect("profile should have recorded its daemon pid")
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .collect()
}

/// The login PATH probe and the managed shell's marker login replay each
/// source the login startup files once, so a profile-started daemon is
/// started twice per fresh login. The managed shell's copy belongs to the
/// session (the user's normal login environment); the probe's copy is a
/// duplicate that must be reaped by the supervised helper before the session
/// proceeds — otherwise every login leaks an unmanaged orphan (an extra
/// ssh-agent, and nothing reaps it when the session ends). All three standard
/// descriptors of the background job are redirected, the shape real daemons
/// take, so the probe itself completes cleanly and containment is the only
/// thing that can remove the process.
#[test]
fn login_path_probe_reaps_profile_started_daemon() {
    let profile = concat!(
        "nohup sleep 60 </dev/null >/dev/null 2>&1 &\n",
        "echo $! >> \"$HOME/probe_daemons.pids\"\n",
        "PS1='lp$ '\n",
    );
    let mut session = LoginProbeSession::spawn_with_daemon_profile(profile);

    assert!(
        session.wait_for("lp$"),
        "managed login shell never reached its prompt; got:\n{}",
        session.text()
    );
    session.send(b"exit\n");
    let transcript = session.finish();

    let pids = daemon_pids(&session.root);
    assert_eq!(
        pids.len(),
        2,
        "expected the login PATH probe and the managed shell's login replay \
         to each start the daemon once; got {}: {transcript}",
        pids.len()
    );
    let probe_pid = pids[0];
    assert!(
        !process_alive(probe_pid),
        "login PATH probe leaked its profile-started daemon (pid {probe_pid}); \
         the supervised helper must reap it before the session proceeds: {transcript}"
    );
    // Defense in depth for the red state: never leak the daemons past the test.
    for pid in pids {
        best_effort_kill(pid);
    }
    let _ = fs::remove_dir_all(&session.root);
}
