//! Cargo-gated regression tests for the fresh-login fail-open guardian.
//!
//! These spawn the real `cosh-shell` binary across login/TTY policy shapes,
//! inject startup failures, and assert that only an eligible armed login falls
//! open to native `bash`. They guard the wiring in
//! `runtime::controller::bootstrap::run_raw` from silent regression — the unit
//! tests cover the underlying decision function.
//! Linux-only: needs a real `bash`, `setsid`, and `TIOCSCTTY`.

use std::fs;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::{fs::PermissionsExt, process::CommandExt};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nix::libc;
use wait_timeout::ChildExt;

use crate::support::raw_cli::{raw_cli_shared_run_guard, RawCliRunGuard};

const DEADLINE: Duration = Duration::from_secs(20);
const MARKER: &str = "FAILOPEN_MARKER_OK";

/// A login `cosh-shell` driven over a PTY for fail-open assertions.
struct FailopenSession {
    child: Child,
    master: std::fs::File,
    output: Vec<u8>,
    _root: tempfile::TempDir,
    _gate: RawCliRunGuard,
}

impl FailopenSession {
    /// Spawn `cosh-shell` as an interactive login shell (argv[0] = `-bash`
    /// login prefix `-cosh`), with the given extra environment applied last so
    /// it overrides the deterministic defaults.
    fn spawn(extra_env: &[(&str, &str)]) -> Self {
        Self::spawn_with_profile(extra_env, "echo FO_LOGIN_PROFILE\nPS1='fo$ '\n")
    }

    fn spawn_non_login(extra_env: &[(&str, &str)]) -> Self {
        Self::spawn_with_argv0("cosh", extra_env, "echo FO_LOGIN_PROFILE\nPS1='fo$ '\n")
    }

    fn spawn_with_profile(extra_env: &[(&str, &str)], profile: &str) -> Self {
        Self::spawn_with_fixture(extra_env, profile, "bash", None, false)
    }

    fn spawn_with_profile_and_bash_wrapper(
        extra_env: &[(&str, &str)],
        profile: &str,
        bash_wrapper: &str,
    ) -> Self {
        Self::spawn_with_fixture(extra_env, profile, "bash", Some(bash_wrapper), false)
    }

    fn spawn_with_missing_zsh_and_bash_wrapper(
        extra_env: &[(&str, &str)],
        profile: &str,
        bash_wrapper: &str,
    ) -> Self {
        Self::spawn_with_fixture(extra_env, profile, "zsh", Some(bash_wrapper), true)
    }

    fn spawn_with_fixture(
        extra_env: &[(&str, &str)],
        profile: &str,
        shell: &str,
        bash_wrapper: Option<&str>,
        wrapper_only_path: bool,
    ) -> Self {
        Self::spawn_with_argv0_and_fixture(
            "-cosh",
            extra_env,
            profile,
            shell,
            bash_wrapper,
            wrapper_only_path,
        )
    }

    fn spawn_with_argv0(argv0: &str, extra_env: &[(&str, &str)], profile: &str) -> Self {
        Self::spawn_with_argv0_and_fixture(argv0, extra_env, profile, "bash", None, false)
    }

    fn spawn_with_argv0_and_fixture(
        argv0: &str,
        extra_env: &[(&str, &str)],
        profile: &str,
        shell: &str,
        bash_wrapper: Option<&str>,
        wrapper_only_path: bool,
    ) -> Self {
        let gate = raw_cli_shared_run_guard();
        let root = tempfile::Builder::new()
            .prefix("cosh-failopen-")
            .tempdir()
            .expect("failopen fixture HOME");
        // The fallback is a LOGIN bash, so it sources `.bash_profile`; use it to
        // prove the fallback is a real login shell and to give a stable prompt.
        fs::write(root.path().join(".bash_profile"), profile).unwrap();
        fs::write(root.path().join(".bashrc"), "PS1='fo$ '\n").unwrap();
        fs::create_dir(root.path().join(".copilot-shell")).unwrap();
        fs::write(
            root.path().join(".copilot-shell/config.toml"),
            "[shell]\nadapter_default = 'fake'\n",
        )
        .unwrap();
        let path = if let Some(wrapper) = bash_wrapper {
            let bin = root.path().join("bin");
            fs::create_dir(&bin).expect("create failopen fixture bin");
            let bash = bin.join("bash");
            fs::write(&bash, wrapper).expect("write failopen bash wrapper");
            fs::set_permissions(&bash, fs::Permissions::from_mode(0o755))
                .expect("make failopen bash wrapper executable");
            if wrapper_only_path {
                bin.display().to_string()
            } else {
                format!("{}:/usr/bin:/bin", bin.display())
            }
        } else {
            "/usr/bin:/bin".to_string()
        };

        let size = libc::winsize {
            ws_row: 24,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = nix::pty::openpty(Some(&size), None).expect("failopen PTY");
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
            .arg0(argv0)
            .args(["--shell", shell])
            .env_clear()
            .env("PATH", path)
            .env("HOME", root.path())
            .env("TMPDIR", root.path())
            .env("TERM", "xterm-256color")
            .env("LANG", "C.UTF-8")
            .env("LC_ALL", "C.UTF-8")
            .env("COSH_SHELL_BOOTSTRAP_PATH", "0")
            .env("COSH_SHELL_LOGIN_IDENTITY", "0")
            .env("COSH_SHELL_STARTUP_BANNER", "0")
            .env("COSH_SHELL_HEALTH_SCAN", "disabled")
            .env("COSH_RECOMMENDATIONS_ENABLED", "0")
            .current_dir(root.path())
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        for (key, value) in extra_env {
            command.env(key, value);
        }
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
        let child = command.spawn().expect("spawn failopen session");
        Self {
            child,
            master,
            output: Vec::new(),
            _root: root,
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

    /// Read until `needle` appears in the accumulated output or the deadline
    /// elapses; returns whether it appeared.
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
        // Best-effort; the child may already be exiting on the no-fallback path.
        let _ = self.master.write_all(bytes);
        let _ = self.master.flush();
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.output).into_owned()
    }

    /// Drain remaining output and reap the child within the deadline.
    fn finish(mut self) -> String {
        self.finish_output();
        self.text()
    }

    fn finish_with_profile_hits(mut self) -> (String, usize) {
        self.finish_output();
        let profile_hits = Self::read_hit_count(&self._root.path().join(".cosh_profile_hits"));
        (self.text(), profile_hits)
    }

    fn finish_with_r2_hits(mut self) -> (String, usize) {
        self.finish_output();
        let wrapper_hits = Self::read_hit_count(&self._root.path().join(".cosh_wrapper_hits"));
        (self.text(), wrapper_hits)
    }

    fn finish_with_wrapper_and_profile_hits(mut self) -> (String, usize, usize) {
        self.finish_output();
        let wrapper_hits = Self::read_hit_count(&self._root.path().join(".cosh_wrapper_hits"));
        let profile_hits = Self::read_hit_count(&self._root.path().join(".cosh_profile_hits"));
        (self.text(), wrapper_hits, profile_hits)
    }

    fn read_hit_count(path: &std::path::Path) -> usize {
        fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("read hit counter {}: {error}", path.display()))
            .lines()
            .count()
    }

    fn finish_output(&mut self) {
        Self::reap(&mut self.child);
        // Final drain.
        for _ in 0..20 {
            self.pump_once();
        }
    }

    /// Kill and reap the child so a failed assertion cannot leak a process.
    fn reap(child: &mut Child) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            match child.wait_timeout(Duration::from_millis(50)) {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
            }
        }
    }
}

impl Drop for FailopenSession {
    fn drop(&mut self) {
        // A panicking assertion drops the session with the child still live;
        // reap it so the test binary never leaks a shell or a zombie.
        Self::reap(&mut self.child);
    }
}

/// An invalid integration config on an interactive login must hand the user a
/// real native login bash (not lock them out), with a visible diagnostic. This
/// fails before the child shell is spawned, so the login profile is sourced
/// exactly once — by the fallback bash.
#[test]
fn integration_misconfig_falls_open_to_login_bash() {
    let mut session = FailopenSession::spawn(&[("COSH_SHELL_INTEGRATION", "bogus-not-a-mode")]);
    assert!(
        session.wait_for("falling back to bash"),
        "expected fall-open diagnostic; got:\n{}",
        session.text()
    );
    assert!(
        session.wait_for("FO_LOGIN_PROFILE"),
        "fallback did not source the login profile; got:\n{}",
        session.text()
    );
    session.send(format!("echo {MARKER}\n").as_bytes());
    assert!(
        session.wait_for(MARKER),
        "fallback bash did not run a command; got:\n{}",
        session.text()
    );
    session.send(b"exit\n");
    let transcript = session.finish();
    // No child shell was spawned, so only the fallback bash sourced the profile.
    let count = transcript.matches("FO_LOGIN_PROFILE").count();
    assert!(
        count == 1,
        "expected profile sourced once; got {count}:\n{transcript}"
    );
}

#[test]
fn non_login_pty_with_invalid_integration_does_not_fall_open() {
    let mut session =
        FailopenSession::spawn_non_login(&[("COSH_SHELL_INTEGRATION", "bogus-not-a-mode")]);
    assert!(
        session.wait_for("invalid shell integration"),
        "expected invalid-integration diagnostic; got:\n{}",
        session.text()
    );
    let transcript = session.finish();
    assert!(
        !transcript.contains("falling back to bash"),
        "non-login PTY must not fall open:\n{transcript}"
    );
    assert!(
        !transcript.contains("FO_LOGIN_PROFILE"),
        "non-login invalid integration must not source login startup:\n{transcript}"
    );
}

#[test]
fn non_tty_explicit_raw_login_with_invalid_integration_exits_two() {
    let root = tempfile::Builder::new()
        .prefix("cosh-failopen-non-tty-")
        .tempdir()
        .expect("non-TTY fixture HOME");
    let profile_hit = root.path().join("profile-hit");
    fs::write(
        root.path().join(".bash_profile"),
        format!("printf hit > '{}'\n", profile_hit.display()),
    )
    .expect("write non-TTY profile");
    fs::create_dir(root.path().join(".copilot-shell")).expect("create config directory");
    fs::write(
        root.path().join(".copilot-shell/config.toml"),
        "[shell]\nadapter_default = 'fake'\n",
    )
    .expect("write config");

    let output = Command::new(env!("CARGO_BIN_EXE_cosh-shell"))
        .args(["raw", "fake", "--login", "--shell", "bash"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.path())
        .env("TMPDIR", root.path())
        .env("TERM", "xterm-256color")
        .env("COSH_SHELL_INTEGRATION", "bogus-not-a-mode")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run explicit raw login without TTYs");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(2), "stderr:\n{stderr}");
    assert!(stderr.contains("invalid shell integration"), "{stderr}");
    assert!(!stderr.contains("falling back to bash"), "{stderr}");
    assert!(!profile_hit.exists(), "non-TTY raw login ran startup files");
}

/// A relay `Err` before the child shell is spawned (here: an unwritable TMPDIR
/// makes the session work_dir uncreatable) must fall open to a native login
/// bash. Covers the `run_raw_interactive_*` `Err` fall-open site.
#[test]
fn pre_spawn_error_falls_open_to_login_bash() {
    let mut session = FailopenSession::spawn(&[("TMPDIR", "/proc/1/cosh-unwritable")]);
    assert!(
        session.wait_for("runtime unavailable") && session.wait_for("falling back to bash"),
        "expected runtime-unavailable fall-open diagnostic; got:\n{}",
        session.text()
    );
    assert!(
        session.wait_for("FO_LOGIN_PROFILE"),
        "fallback did not source the login profile; got:\n{}",
        session.text()
    );
    session.send(format!("echo {MARKER}\n").as_bytes());
    assert!(
        session.wait_for(MARKER),
        "fallback bash did not run a command; got:\n{}",
        session.text()
    );
    session.send(b"exit\n");
    session.finish();
}

#[test]
fn path_probe_side_effect_prevents_relay_error_fallback() {
    let mut session = FailopenSession::spawn_with_profile(
        &[
            ("COSH_SHELL_BOOTSTRAP_PATH", "1"),
            ("TMPDIR", "/proc/1/cosh-unwritable"),
        ],
        "printf 'hit\\n' >> \"$HOME/.cosh_profile_hits\"\n",
    );
    assert!(
        session.wait_for("raw shell failed"),
        "expected relay error diagnostic; got:\n{}",
        session.text()
    );
    let (transcript, profile_hits) = session.finish_with_profile_hits();
    assert!(
        !transcript.contains("falling back to bash"),
        "PATH producer disarmed fallback before relay Err:\n{transcript}"
    );
    assert_eq!(
        profile_hits, 1,
        "PATH probe profile must run exactly once; got {profile_hits}:\n{transcript}"
    );
}

/// A panic BEFORE the child shell is spawned must be caught and fall open to a
/// native login bash. Covers the `catch_unwind` pre-spawn site (debug-only
/// fault injection is compiled into this test build).
#[test]
fn pre_spawn_panic_falls_open_to_login_bash() {
    let mut session = FailopenSession::spawn(&[("COSH_FAILOPEN_FORCE_PANIC", "pre-spawn")]);
    assert!(
        session.wait_for("panicked before shell start; falling back to bash"),
        "expected pre-spawn fall-open diagnostic; got:\n{}",
        session.text()
    );
    assert!(
        session.wait_for("FO_LOGIN_PROFILE"),
        "fallback did not source the login profile; got:\n{}",
        session.text()
    );
    session.send(format!("echo {MARKER}\n").as_bytes());
    assert!(
        session.wait_for(MARKER),
        "fallback bash did not run a command; got:\n{}",
        session.text()
    );
    session.send(b"exit\n");
    let transcript = session.finish();
    // The panic fires before the child spawns, so only the fallback bash
    // sourced the profile — no double login side effect.
    let count = transcript.matches("FO_LOGIN_PROFILE").count();
    assert!(
        count == 1,
        "expected profile sourced once; got {count}:\n{transcript}"
    );
}

/// The default login PATH probe already executes `.bash_profile`; a later
/// pre-spawn failure must not launch a second login shell and repeat it.
#[test]
fn path_probe_side_effect_prevents_pre_spawn_fallback() {
    let mut session = FailopenSession::spawn_with_profile(
        &[
            ("COSH_SHELL_BOOTSTRAP_PATH", "1"),
            ("COSH_FAILOPEN_FORCE_PANIC", "pre-spawn"),
        ],
        "printf 'hit\\n' >> \"$HOME/.cosh_profile_hits\"\n",
    );
    assert!(
        session.wait_for("panicked before shell start"),
        "expected pre-spawn panic diagnostic; got:\n{}",
        session.text()
    );
    session.send(b"exit\n");
    let (transcript, profile_hits) = session.finish_with_profile_hits();
    assert!(
        !transcript.contains("falling back to bash"),
        "PATH probe already ran the profile; must not start a second login shell:\n{transcript}"
    );
    assert_eq!(
        profile_hits, 1,
        "PATH probe profile must run exactly once; got {profile_hits}:\n{transcript}"
    );
}

/// A failed PATH probe may still have run the login profile, so its attempted
/// state must suppress fallback just like a successful probe.
#[test]
fn failed_path_probe_side_effect_prevents_pre_spawn_fallback() {
    let mut session = FailopenSession::spawn_with_profile(
        &[
            ("COSH_SHELL_BOOTSTRAP_PATH", "1"),
            ("COSH_FAILOPEN_FORCE_PANIC", "pre-spawn"),
        ],
        "printf 'hit\\n' >> \"$HOME/.cosh_profile_hits\"\nexit 23\n",
    );
    assert!(
        session.wait_for("failed to discover PATH")
            && session.wait_for("panicked before shell start"),
        "expected failed-probe and pre-spawn diagnostics; got:\n{}",
        session.text()
    );
    session.send(b"exit\n");
    let (transcript, profile_hits) = session.finish_with_profile_hits();
    assert!(
        !transcript.contains("falling back to bash"),
        "failed PATH probe may have run the profile; must not fallback:\n{transcript}"
    );
    assert_eq!(
        profile_hits, 1,
        "failed PATH probe profile must run once; got {profile_hits}:\n{transcript}"
    );
}

/// With PATH bootstrap disabled, the R2 capability probe alone can execute a
/// PATH-selected wrapper. A later pre-spawn panic must not replay that wrapper.
#[test]
fn r2_probe_side_effect_prevents_pre_spawn_fallback() {
    let mut session = FailopenSession::spawn_with_profile_and_bash_wrapper(
        &[
            ("COSH_SHELL_BOOTSTRAP_PATH", "0"),
            ("COSH_SHELL_LOGIN_IDENTITY", "1"),
            ("COSH_FAILOPEN_FORCE_PANIC", "pre-spawn"),
        ],
        "PS1='fo$ '\n",
        "#!/bin/bash\nprintf 'wrapper-hit\\n' >> \"$HOME/.cosh_wrapper_hits\"\nexec -a -bash /bin/bash \"$@\"\n",
    );
    assert!(
        session.wait_for("panicked before shell start"),
        "expected pre-spawn panic diagnostic; got:\n{}",
        session.text()
    );
    session.send(b"exit\n");
    let (transcript, wrapper_hits) = session.finish_with_r2_hits();
    assert!(
        transcript.contains("raw shell failed: runtime panicked before shell start"),
        "R2-only failure must remain in the before-shell-start phase:\n{transcript}"
    );
    assert!(
        !transcript.contains("falling back to bash"),
        "R2 probe already ran login startup; must not fallback:\n{transcript}"
    );
    assert_eq!(
        wrapper_hits, 1,
        "only the R2 probe may invoke the frozen wrapper; a managed child must not start: \
         {transcript}"
    );
}

#[test]
fn missing_zsh_probe_and_spawn_fall_open_to_preserving_bash() {
    let mut session = FailopenSession::spawn_with_missing_zsh_and_bash_wrapper(
        &[
            ("COSH_SHELL_BOOTSTRAP_PATH", "1"),
            ("COSH_SHELL_LOGIN_IDENTITY", "0"),
        ],
        "printf 'profile-hit\\n' >> \"$HOME/.cosh_profile_hits\"\nprintf 'FO_LOGIN_PROFILE\\n'\nPS1='fo$ '\n",
        "#!/bin/bash\nprintf 'wrapper-hit\\n' >> \"$HOME/.cosh_wrapper_hits\"\nexec -a -bash /bin/bash \"$@\"\n",
    );
    assert!(
        session.wait_for("falling back to bash") && session.wait_for("FO_LOGIN_PROFILE"),
        "missing zsh did not fall open through the preserving bash wrapper:\n{}",
        session.text()
    );
    session.send(format!("echo {MARKER}\n").as_bytes());
    assert!(session.wait_for(MARKER), "fallback bash is not interactive");
    session.send(b"exit\n");
    let (transcript, wrapper_hits, profile_hits) = session.finish_with_wrapper_and_profile_hits();
    assert!(
        transcript.contains("failed to discover PATH from Zsh interactive login startup")
            && transcript.contains("runtime unavailable"),
        "both missing-zsh failures must be visible:\n{transcript}"
    );
    assert_eq!(wrapper_hits, 1, "fallback wrapper ran more than once");
    assert_eq!(profile_hits, 1, "fallback profile ran more than once");
}

/// Isolated login must keep its fail-loud contract for every pre-spawn failure:
/// fallback to a login bash would execute startup files that isolation forbids.
#[test]
fn isolated_pre_spawn_failures_do_not_run_login_profile() {
    let cases: &[(&str, &[(&str, &str)], &str)] = &[
        (
            "integration-misconfig",
            &[
                ("COSH_SHELL_ISOLATED", "1"),
                ("COSH_SHELL_INTEGRATION", "bogus-not-a-mode"),
            ],
            "invalid shell integration",
        ),
        (
            "relay-error",
            &[
                ("COSH_SHELL_ISOLATED", "1"),
                ("TMPDIR", "/proc/1/cosh-unwritable"),
            ],
            "raw shell failed",
        ),
        (
            "relay-panic",
            &[
                ("COSH_SHELL_ISOLATED", "1"),
                ("COSH_FAILOPEN_FORCE_PANIC", "pre-spawn"),
            ],
            "panicked before shell start",
        ),
    ];

    for (label, environment, diagnostic) in cases {
        let mut session = FailopenSession::spawn(environment);
        assert!(
            session.wait_for(diagnostic),
            "{label}: expected failure diagnostic; got:\n{}",
            session.text()
        );
        session.send(b"exit\n");
        let transcript = session.finish();
        assert!(
            !transcript.contains("falling back to bash"),
            "{label}: isolated login must not fall open; got:\n{transcript}"
        );
        assert!(
            !transcript.contains("FO_LOGIN_PROFILE"),
            "{label}: isolated login must not source the profile; got:\n{transcript}"
        );
    }
}

/// A panic AFTER the child shell is spawned must NOT fall open: the child has
/// already sourced the login files and may have dispatched an effect, and it
/// has been dropped during unwind. The session ends without a second bash.
#[test]
fn post_spawn_panic_does_not_fall_open() {
    let mut session = FailopenSession::spawn(&[("COSH_FAILOPEN_FORCE_PANIC", "post-spawn")]);
    assert!(
        session.wait_for("FO_LOGIN_PROFILE"),
        "managed login shell did not expose its startup marker; got:\n{}",
        session.text()
    );
    session.send(b"echo should-not-fall-open\n");
    let transcript = session.finish();
    assert!(
        transcript.contains("panicked after shell start"),
        "expected post-spawn panic diagnostic; got:\n{transcript}"
    );
    assert!(
        !transcript.contains("falling back to bash"),
        "post-spawn panic must NOT fall open; got:\n{transcript}"
    );
    // The spawned child login shell sources `.bash_profile` once (marker login
    // replay, COSH_LOGIN_SHELL=1). A fallback login bash would source it a
    // second time, so `== 1` proves no fallback shell started after the panic.
    let login_profile_count = transcript.matches("FO_LOGIN_PROFILE").count();
    assert!(
        login_profile_count == 1,
        "post-spawn panic must not start a fallback login bash \
         (login profile sourced {login_profile_count}×, expected 1); got:\n{transcript}"
    );
}

#[test]
fn native_post_spawn_panic_does_not_fall_open() {
    let mut session = FailopenSession::spawn(&[
        ("COSH_SHELL_INTEGRATION", "native"),
        ("COSH_FAILOPEN_FORCE_PANIC", "post-spawn"),
    ]);
    assert!(
        session.wait_for("panicked after shell start"),
        "Native managed-shell spawn did not reach the guarded post-spawn panic:\n{}",
        session.text()
    );
    let transcript = session.finish();
    assert!(
        !transcript.contains("falling back to bash"),
        "Native post-spawn panic must not fall open:\n{transcript}"
    );
}
