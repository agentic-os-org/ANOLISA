use super::*;

#[cfg(target_os = "linux")]
mod parent_lifecycle {
    use super::*;

    use std::collections::BTreeSet;
    use std::fs::File;
    use std::os::fd::AsRawFd;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::path::{Path, PathBuf};
    use std::process::{Child, ExitStatus, Stdio};

    use nix::libc;
    use nix::pty::Winsize;
    use wait_timeout::ChildExt;

    const TERMINAL_LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(10);
    const LOGIN_PROFILE_WINSIZE: Winsize = Winsize {
        ws_row: 37,
        ws_col: 113,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    struct ParentPtySession {
        child: Option<Child>,
        master: File,
        terminal: File,
        original: libc::termios,
        original_flags: i32,
        output: Vec<u8>,
        root: PathBuf,
    }

    struct EscapedProcessCleanup(i32);

    impl Drop for EscapedProcessCleanup {
        fn drop(&mut self) {
            if process_is_running(self.0) {
                unsafe {
                    libc::kill(self.0, libc::SIGKILL);
                }
            }
        }
    }

    impl ParentPtySession {
        fn spawn(label: &str) -> Self {
            Self::spawn_with_args(label, &["raw", "fake", "--shell", "bash"], false, true)
        }

        fn spawn_passthrough_with_ignored_sigint(label: &str) -> Self {
            Self::spawn_with_args(
                label,
                &[
                    "raw",
                    "fake",
                    "-c",
                    "kill -INT $$; printf '%s\\n' __INHERITED_SIGINT_IGNORED__",
                ],
                true,
                false,
            )
        }

        fn spawn_with_args(
            label: &str,
            args: &[&str],
            ignore_sigint: bool,
            wait_for_raw_mode: bool,
        ) -> Self {
            Self::spawn_with_options(label, args, ignore_sigint, wait_for_raw_mode, None, None)
        }

        fn spawn_with_login_profile(label: &str, profile: &str) -> Self {
            Self::spawn_with_options(
                label,
                &["raw", "fake", "--shell", "bash"],
                false,
                true,
                Some(profile),
                Some(&LOGIN_PROFILE_WINSIZE),
            )
        }

        fn spawn_with_options(
            label: &str,
            args: &[&str],
            ignore_sigint: bool,
            wait_for_raw_mode: bool,
            login_profile: Option<&str>,
            winsize: Option<&Winsize>,
        ) -> Self {
            let root = std::env::temp_dir().join(format!(
                "cosh-shell-parent-termios-{label}-{}-{}",
                std::process::id(),
                unique_suffix()
            ));
            let home = root.join("home");
            let work = root.join("work");
            std::fs::create_dir_all(&home).expect("create isolated HOME");
            std::fs::create_dir_all(&work).expect("create isolated work dir");
            if let Some(profile) = login_profile {
                std::fs::write(home.join(".bash_profile"), profile).expect("write login profile");
            }

            let pty = nix::pty::openpty(winsize, None).expect("open parent PTY");
            let master = File::from(pty.master);
            let terminal = File::from(pty.slave);
            let original = read_termios(terminal.as_raw_fd());
            let original_flags = read_file_status_flags(terminal.as_raw_fd());
            let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
            assert!(flags >= 0, "read parent PTY master flags");
            assert_eq!(
                unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
                0,
                "make parent PTY master nonblocking"
            );

            let stdin = terminal.try_clone().expect("clone PTY stdin");
            let stdout = terminal.try_clone().expect("clone PTY stdout");
            let stderr = terminal.try_clone().expect("clone PTY stderr");
            let mut command = Command::new(env!("CARGO_BIN_EXE_cosh-shell"));
            command
                .args(args)
                .stdin(Stdio::from(stdin))
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(stderr))
                .current_dir(&work)
                .env("HOME", &home)
                .env(
                    "COSH_SHELL_ISOLATED",
                    if login_profile.is_some() { "0" } else { "1" },
                )
                .env("COSH_SHELL_INTEGRATION", "enhanced")
                .env("COSH_SHELL_RAW_SHELL", "bash")
                .env("COSH_SHELL_DEFAULT_SHELL", "bash")
                .env("COSH_SHELL_LANG", "en-US")
                .env(
                    "COSH_SHELL_BOOTSTRAP_PATH",
                    if login_profile.is_some() { "1" } else { "0" },
                )
                .env("COSH_SHELL_HEALTH_SCAN", "disabled")
                .env("COSH_RECOMMENDATIONS_ENABLED", "0")
                .env("TERM", "xterm-256color")
                .env("LANG", "C.UTF-8")
                .env("LC_ALL", "C.UTF-8");
            if login_profile.is_some() {
                command.env("PATH", "/usr/bin:/bin");
            }
            unsafe {
                command.pre_exec(move || {
                    if ignore_sigint {
                        libc::signal(libc::SIGINT, libc::SIG_IGN);
                    }
                    if libc::setsid() < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::tcsetpgrp(libc::STDIN_FILENO, libc::getpgrp()) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = command.spawn().expect("spawn cosh-shell on parent PTY");
            let mut session = Self {
                child: Some(child),
                master,
                terminal,
                original,
                original_flags,
                output: Vec::new(),
                root,
            };
            if wait_for_raw_mode {
                session.wait_for_raw_mode();
            }
            session
        }

        fn wrapper_pid(&self) -> i32 {
            self.child.as_ref().expect("live wrapper").id() as i32
        }

        fn wait_for_raw_mode(&mut self) {
            let deadline = Instant::now() + TERMINAL_LIFECYCLE_TIMEOUT;
            while Instant::now() < deadline {
                self.drain_output();
                let current = read_termios(self.terminal.as_raw_fd());
                let modify_other_keys_enabled = self
                    .output
                    .windows(b"\x1b[>4;1m".len())
                    .any(|window| window == b"\x1b[>4;1m");
                if current.c_lflag & (libc::ECHO | libc::ICANON) == 0 && modify_other_keys_enabled {
                    return;
                }
                if self
                    .child
                    .as_mut()
                    .expect("live wrapper")
                    .try_wait()
                    .expect("poll wrapper while waiting for raw mode")
                    .is_some()
                {
                    panic!("cosh-shell exited before activating raw mode");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("cosh-shell did not activate raw mode");
        }

        fn write(&mut self, bytes: &[u8]) {
            let deadline = Instant::now() + TERMINAL_LIFECYCLE_TIMEOUT;
            let mut written = 0;
            while written < bytes.len() {
                match self.master.write(&bytes[written..]) {
                    Ok(0) => panic!("parent PTY accepted zero input bytes"),
                    Ok(count) => written += count,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        self.drain_output();
                        if Instant::now() >= deadline {
                            panic!("timed out writing parent PTY input");
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("write parent PTY input: {error}"),
                }
            }
        }

        fn wait(self) -> ExitStatus {
            self.wait_with_output().0
        }

        fn wait_with_output(mut self) -> (ExitStatus, Vec<u8>) {
            let deadline = Instant::now() + TERMINAL_LIFECYCLE_TIMEOUT;
            let status = loop {
                self.drain_output();
                if let Some(status) = self
                    .child
                    .as_mut()
                    .expect("live wrapper")
                    .try_wait()
                    .expect("poll cosh-shell")
                {
                    break status;
                }
                if Instant::now() >= deadline {
                    panic!("cosh-shell did not exit within {TERMINAL_LIFECYCLE_TIMEOUT:?}");
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            self.drain_output();
            self.child.take();
            self.assert_restored();
            (status, std::mem::take(&mut self.output))
        }

        fn wait_for_child_shell(&mut self) -> i32 {
            let wrapper = self.wrapper_pid();
            let deadline = Instant::now() + TERMINAL_LIFECYCLE_TIMEOUT;
            while Instant::now() < deadline {
                self.drain_output();
                for pid in direct_children(wrapper) {
                    if std::fs::read_to_string(format!("/proc/{pid}/comm"))
                        .is_ok_and(|comm| comm.trim() == "bash")
                    {
                        return pid;
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("cosh-shell child bash did not appear");
        }

        fn wait_for_pid_file(&mut self, path: &Path) -> i32 {
            let deadline = Instant::now() + TERMINAL_LIFECYCLE_TIMEOUT;
            while Instant::now() < deadline {
                self.drain_output();
                if let Ok(contents) = std::fs::read_to_string(path) {
                    if let Ok(pid) = contents.trim().parse::<i32>() {
                        return pid;
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("foreground PID file was not populated");
        }

        fn drain_output(&mut self) {
            let mut buffer = [0_u8; 4096];
            loop {
                match self.master.read(&mut buffer) {
                    Ok(0) => return,
                    Ok(count) => self.output.extend_from_slice(&buffer[..count]),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                    Err(error) if error.raw_os_error() == Some(libc::EIO) => return,
                    Err(error) => panic!("drain parent PTY output: {error}"),
                }
            }
        }

        fn assert_restored(&self) {
            let restored = read_termios(self.terminal.as_raw_fd());
            assert_termios_eq(&restored, &self.original);
            assert_eq!(
                read_file_status_flags(self.terminal.as_raw_fd()),
                self.original_flags,
                "stdin file status flags changed"
            );
        }
    }

    impl Drop for ParentPtySession {
        fn drop(&mut self) {
            if let Some(child) = self.child.as_mut() {
                if child.try_wait().ok().flatten().is_none() {
                    unsafe {
                        libc::kill(-(child.id() as i32), libc::SIGKILL);
                        libc::kill(child.id() as i32, libc::SIGKILL);
                    }
                    let _ = child.wait_timeout(Duration::from_secs(2));
                }
            }
            unsafe {
                libc::tcsetattr(self.terminal.as_raw_fd(), libc::TCSANOW, &self.original);
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn read_termios(fd: i32) -> libc::termios {
        let mut termios = unsafe { std::mem::zeroed::<libc::termios>() };
        assert_eq!(unsafe { libc::tcgetattr(fd, &mut termios) }, 0);
        termios
    }

    fn read_file_status_flags(fd: i32) -> i32 {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(flags >= 0, "read file status flags");
        flags
    }

    fn assert_termios_eq(actual: &libc::termios, expected: &libc::termios) {
        assert_eq!(actual.c_iflag, expected.c_iflag, "input flags changed");
        assert_eq!(actual.c_oflag, expected.c_oflag, "output flags changed");
        assert_eq!(actual.c_cflag, expected.c_cflag, "control flags changed");
        assert_eq!(actual.c_lflag, expected.c_lflag, "local flags changed");
        assert_eq!(actual.c_line, expected.c_line, "line discipline changed");
        assert_eq!(actual.c_cc, expected.c_cc, "control characters changed");
        assert_eq!(actual.c_ispeed, expected.c_ispeed, "input speed changed");
        assert_eq!(actual.c_ospeed, expected.c_ospeed, "output speed changed");
    }

    fn process_is_running(pid: i32) -> bool {
        let is_zombie = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| {
                stat.rsplit_once(") ")
                    .map(|(_, suffix)| suffix.starts_with('Z'))
            })
            == Some(true);
        !is_zombie && unsafe { libc::kill(pid, 0) } == 0
    }

    fn assert_process_stopped(pid: i32) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if !process_is_running(pid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("profile-started daemon {pid} survived the PATH probe");
    }

    fn direct_children(pid: i32) -> Vec<i32> {
        let mut children = BTreeSet::new();
        let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
            return Vec::new();
        };
        for task in tasks.flatten() {
            let Ok(values) = std::fs::read_to_string(task.path().join("children")) else {
                continue;
            };
            children.extend(
                values
                    .split_whitespace()
                    .filter_map(|value| value.parse::<i32>().ok()),
            );
        }
        children.into_iter().collect()
    }

    fn assert_wrapper_signal_restores(signal: i32) {
        let _guard = shell_host_run_guard();
        let session = ParentPtySession::spawn(&format!("wrapper-signal-{signal}"));
        let wrapper = session.wrapper_pid();
        assert_eq!(unsafe { libc::kill(wrapper, signal) }, 0);
        let (status, output) = session.wait_with_output();
        assert_eq!(status.signal(), Some(signal), "wrapper signal status");
        assert!(
            output
                .windows(b"\x1b[>4;1m".len())
                .any(|window| window == b"\x1b[>4;1m"),
            "modifyOtherKeys was not enabled: {}",
            String::from_utf8_lossy(&output)
        );
        assert!(
            output
                .windows(b"\x1b[>4;0m".len())
                .any(|window| window == b"\x1b[>4;0m"),
            "modifyOtherKeys was not disabled: {}",
            String::from_utf8_lossy(&output)
        );
    }

    #[test]
    fn startup_path_probe_preserves_login_profile_terminal_semantics() {
        let _guard = shell_host_run_guard();
        let mut session = ParentPtySession::spawn_with_login_profile(
            "bootstrap-path-terminal",
            "[ -t 0 ] && [ -t 1 ] && [ \"$(stty size)\" = \"37 113\" ] && \
             export PATH=\"$HOME/terminal-login:$PATH\"\n",
        );
        let expected = format!("{}/home/terminal-login", session.root.display());
        session.write(b"printf '__BOOTSTRAP_PATH__=%s\\n' \"$PATH\"; exit\n");
        let (status, output) = session.wait_with_output();
        let output = String::from_utf8_lossy(&output);

        assert!(status.success(), "startup status: {status:?}; {output}");
        assert!(
            output.contains(&format!("__BOOTSTRAP_PATH__={expected}:")),
            "{output}"
        );
    }

    #[test]
    fn startup_path_probe_preserves_precedence_before_fallback_directories() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = shell_host_run_guard();
        let mut session = ParentPtySession::spawn_with_login_profile(
            "bootstrap-path-fallback-precedence",
            "export PATH=\"$HOME/login-only:/usr/local/bin:/usr/bin:/bin\"\n",
        );
        let directory = session.root.join("home/login-only");
        std::fs::create_dir(&directory).unwrap();
        let command = directory.join("ls");
        std::fs::write(
            &command,
            "#!/bin/sh\nprintf '__LOGIN_COMMAND_SELECTED__\\n'\n",
        )
        .unwrap();
        std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o755)).unwrap();
        session.write(b"command ls; exit\n");
        let (status, output) = session.wait_with_output();
        let output = String::from_utf8_lossy(&output);

        assert!(status.success(), "startup status: {status:?}; {output}");
        assert!(output.contains("__LOGIN_COMMAND_SELECTED__"), "{output}");
    }

    #[test]
    fn startup_path_probe_reaps_detached_profile_daemon() {
        if Command::new("setsid").arg("--help").output().is_err() {
            return;
        }
        let _guard = shell_host_run_guard();
        let mut session = ParentPtySession::spawn_with_login_profile(
            "bootstrap-path-daemon",
            "printf \"%s\\n\" \"$PPID\" > \"$HOME/profile-supervisor.pid\"\n\
             setsid --fork sh -c 'printf \"%s\\n\" \"$$\" > \
             \"$HOME/profile-daemon.pid\"; exec sleep 30' \
             </dev/null >/dev/null 2>&1\n\
             for ((attempt = 0; attempt < 1000; attempt++)); do \
                 [ -s \"$HOME/profile-daemon.pid\" ] && break; sleep 0.001; \
             done\n\
             export PATH=\"$HOME/daemon-login:$PATH\"\n",
        );
        let supervisor_pid_file = session.root.join("home/profile-supervisor.pid");
        let supervisor_pid = session.wait_for_pid_file(&supervisor_pid_file);
        let _supervisor_cleanup = EscapedProcessCleanup(supervisor_pid);
        let pid_file = session.root.join("home/profile-daemon.pid");
        let daemon_pid = session.wait_for_pid_file(&pid_file);
        let _daemon_cleanup = EscapedProcessCleanup(daemon_pid);
        let expected = format!("{}/home/daemon-login", session.root.display());

        assert_ne!(
            supervisor_pid,
            session.wrapper_pid(),
            "profile Bash must be owned by a dedicated supervisor process"
        );

        session.write(b"printf '__BOOTSTRAP_PATH__=%s\\n' \"$PATH\"; exit\n");
        let (status, output) = session.wait_with_output();
        let output = String::from_utf8_lossy(&output);

        assert!(status.success(), "startup status: {status:?}; {output}");
        assert!(
            output.contains(&format!("__BOOTSTRAP_PATH__={expected}:")),
            "{output}"
        );
        assert_process_stopped(supervisor_pid);
        assert_process_stopped(daemon_pid);
    }

    #[test]
    fn startup_path_probe_timeout_reaps_supervisor_descendants() {
        if Command::new("setsid").arg("--help").output().is_err() {
            return;
        }
        let _guard = shell_host_run_guard();
        let mut session = ParentPtySession::spawn_with_login_profile(
            "bootstrap-path-timeout-daemon",
            "printf \"%s\\n\" \"$PPID\" > \"$HOME/profile-supervisor.pid\"\n\
             setsid --fork sh -c 'printf \"%s\\n\" \"$$\" > \
             \"$HOME/profile-daemon.pid\"; exec sleep 30' \
             </dev/null >/dev/null 2>&1\n\
             for ((attempt = 0; attempt < 1000; attempt++)); do \
                 [ -s \"$HOME/profile-daemon.pid\" ] && break; sleep 0.001; \
             done\n\
             sleep 30\n",
        );
        let supervisor_pid_file = session.root.join("home/profile-supervisor.pid");
        let daemon_pid_file = session.root.join("home/profile-daemon.pid");
        let supervisor_pid = session.wait_for_pid_file(&supervisor_pid_file);
        let _supervisor_cleanup = EscapedProcessCleanup(supervisor_pid);
        let daemon_pid = session.wait_for_pid_file(&daemon_pid_file);
        let _daemon_cleanup = EscapedProcessCleanup(daemon_pid);

        assert_ne!(
            supervisor_pid,
            session.wrapper_pid(),
            "profile Bash must be owned by a dedicated supervisor process"
        );
        session.write(b"exit\n");
        let status = session.wait();

        assert!(status.success(), "startup status: {status:?}");
        assert_process_stopped(supervisor_pid);
        assert_process_stopped(daemon_pid);
    }

    #[test]
    fn direct_children_scans_every_wrapper_thread() {
        let (pid_tx, pid_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut child = Command::new("sleep")
                .arg("30")
                .spawn()
                .expect("spawn worker-thread child");
            pid_tx.send(child.id() as i32).expect("send child PID");
            release_rx.recv().expect("release worker-thread child");
            child.kill().expect("terminate worker-thread child");
            child.wait().expect("reap worker-thread child");
        });
        let child = pid_rx.recv().expect("receive child PID");
        let deadline = Instant::now() + TERMINAL_LIFECYCLE_TIMEOUT;
        let found = loop {
            if direct_children(std::process::id() as i32).contains(&child) {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        release_tx.send(()).expect("release worker thread");
        worker.join().expect("join worker thread");
        assert!(found, "worker-thread child {child} was not discovered");
    }

    #[test]
    fn raw_cli_restores_parent_termios_after_normal_exit() {
        let _guard = shell_host_run_guard();
        let mut session = ParentPtySession::spawn("normal-exit");
        session.write(b"exit\n");
        let status = session.wait();
        assert!(status.success(), "normal exit status: {status:?}");
    }

    #[test]
    fn raw_cli_restores_parent_termios_after_input_eof() {
        let _guard = shell_host_run_guard();
        let mut session = ParentPtySession::spawn("input-eof");
        session.write(&[0x04]);
        let _status = session.wait();
    }

    #[test]
    fn raw_cli_restores_parent_termios_after_wrapper_sigint() {
        assert_wrapper_signal_restores(libc::SIGINT);
    }

    #[test]
    fn raw_cli_restores_parent_termios_after_wrapper_sigterm() {
        assert_wrapper_signal_restores(libc::SIGTERM);
    }

    #[test]
    fn raw_cli_restores_parent_termios_after_wrapper_sighup() {
        assert_wrapper_signal_restores(libc::SIGHUP);
    }

    #[test]
    fn raw_cli_restores_parent_termios_after_wrapper_sigquit() {
        assert_wrapper_signal_restores(libc::SIGQUIT);
    }

    #[test]
    fn raw_passthrough_preserves_inherited_sigint_ignore() {
        let _guard = shell_host_run_guard();
        let session =
            ParentPtySession::spawn_passthrough_with_ignored_sigint("passthrough-sigint-ignore");
        let (status, output) = session.wait_with_output();
        let output = String::from_utf8_lossy(&output);
        assert!(status.success(), "passthrough status: {status:?}; {output}");
        assert!(output.contains("__INHERITED_SIGINT_IGNORED__"), "{output}");
    }

    #[test]
    fn raw_cli_restores_parent_termios_after_child_shell_signal() {
        let _guard = shell_host_run_guard();
        let mut session = ParentPtySession::spawn("child-shell-sighup");
        let shell = session.wait_for_child_shell();
        assert_eq!(unsafe { libc::kill(shell, libc::SIGHUP) }, 0);
        let _status = session.wait();
    }

    #[test]
    fn raw_cli_restores_parent_termios_after_foreground_group_signal() {
        let _guard = shell_host_run_guard();
        let mut session = ParentPtySession::spawn("foreground-group-sigterm");
        let pid_file = session.root.join("foreground.pid");
        session
            .write(format!("sh -c 'echo $$ > {}; exec sleep 30'\n", pid_file.display()).as_bytes());
        let foreground_pid = session.wait_for_pid_file(&pid_file);
        let child_shell = session.wait_for_child_shell();
        let foreground_group = unsafe { libc::getpgid(foreground_pid) };
        assert!(foreground_group > 0, "read foreground process group");
        assert_ne!(
            foreground_group, child_shell,
            "foreground command must own a distinct process group"
        );
        assert_eq!(unsafe { libc::kill(-foreground_group, libc::SIGTERM) }, 0);
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            session
                .child
                .as_mut()
                .expect("live wrapper")
                .try_wait()
                .expect("poll wrapper after foreground signal")
                .is_none(),
            "foreground signal must not terminate the wrapper"
        );
        session.write(b"exit\n");
        let status = session.wait();
        assert_eq!(
            status.code(),
            Some(128 + libc::SIGTERM),
            "exit preserves the foreground command signal status"
        );
    }
}

#[test]
fn scripted_shell_exit_timeout_kills_foreground_group() {
    let root = std::env::temp_dir().join(format!(
        "cosh-scripted-exit-timeout-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let work_dir = root.join("work");
    let descendant_pid_file = root.join("descendant.pid");
    std::fs::create_dir_all(&root).expect("timeout test root");
    let mut config = ShellHostConfig::new("scripted-exit-timeout", &work_dir);
    config.native_mode = false;
    let started = Instant::now();
    let override_exit = format!(
        "exit() {{ sh -c 'trap \"\" HUP TERM; printf \"%s\\n\" \"$$\" > {}; while :; do sleep 60; done'; }}",
        shell_arg(&descendant_pid_file)
    );

    let error = run_scripted_bash(&config, &[ScriptedInput::command(override_exit)])
        .expect_err("scripted shell that ignores exit must time out");

    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(8));
    let descendant_pid = std::fs::read_to_string(&descendant_pid_file)
        .expect("descendant pid")
        .trim()
        .parse::<i32>()
        .expect("numeric descendant pid");
    for _ in 0..20 {
        #[cfg(target_os = "linux")]
        let is_zombie = std::fs::read_to_string(format!("/proc/{descendant_pid}/stat"))
            .ok()
            .and_then(|stat| {
                stat.rsplit_once(") ")
                    .map(|(_, suffix)| suffix.starts_with('Z'))
            })
            == Some(true);
        #[cfg(not(target_os = "linux"))]
        let is_zombie = false;
        let result = unsafe { nix::libc::kill(descendant_pid, 0) };
        let is_gone =
            result < 0 && io::Error::last_os_error().raw_os_error() == Some(nix::libc::ESRCH);
        if is_zombie || is_gone {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    unsafe {
        nix::libc::kill(descendant_pid, nix::libc::SIGKILL);
    }
    panic!("scripted shell descendant {descendant_pid} survived timeout");
}

#[test]
fn transparent_bash_preserves_user_stty_modes() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-transparent-stty-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("transparent-stty-test", &work_dir);
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::line("stty -echo"),
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line(stty_flag_probe(
                "-echo",
                "__ECHO_OFF__",
                "__ECHO_ON__",
                "stty echo",
            )),
            RawRelayAction::line("stty -isig"),
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line(stty_flag_probe(
                "-isig",
                "__ISIG_OFF__",
                "__ISIG_ON__",
                "stty isig",
            )),
            RawRelayAction::line("stty -icanon min 1 time 0"),
            RawRelayAction::wait(Duration::from_millis(200)),
            RawRelayAction::line(stty_flag_probe(
                "-icanon",
                "__ICANON_OFF__",
                "__ICANON_ON__",
                "stty icanon",
            )),
            RawRelayAction::line("stty sane"),
        ],
        &mut rendered,
    )
    .expect("raw relay stty parity");

    let ledger = ledger_from_output(&output);
    let command_output = ledger_output_refs_text(&ledger);
    let output_lines = command_output.lines().map(str::trim).collect::<Vec<_>>();
    assert!(output_lines.contains(&"__ECHO_OFF__"), "{command_output}");
    assert!(!output_lines.contains(&"__ECHO_ON__"), "{command_output}");
    assert!(output_lines.contains(&"__ISIG_OFF__"), "{command_output}");
    assert!(!output_lines.contains(&"__ISIG_ON__"), "{command_output}");
    assert!(output_lines.contains(&"__ICANON_OFF__"), "{command_output}");
    assert!(!output_lines.contains(&"__ICANON_ON__"), "{command_output}");
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("stty sane") && block.exit_code == 0));
}

#[test]
fn transparent_ctrl_d_exits_bash_and_zsh() {
    if Command::new("bash").arg("--version").output().is_ok() {
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-shell-bash-ctrl-d-test-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let config = ShellHostConfig::new("bash-ctrl-d-test", &work_dir);
        let mut rendered = Vec::new();
        let output = run_raw_relay_bash_with_actions(
            &config,
            vec![
                RawRelayAction::wait(Duration::from_millis(200)),
                RawRelayAction::write(vec![0x04]),
                RawRelayAction::wait(Duration::from_millis(300)),
                RawRelayAction::line("echo __BASH_AFTER_CTRL_D__"),
            ],
            &mut rendered,
        )
        .expect("bash ctrl-d");

        let rendered_text = String::from_utf8_lossy(&rendered);
        assert!(
            !rendered_text.contains("__BASH_AFTER_CTRL_D__"),
            "{rendered_text}"
        );
        assert!(output
            .events
            .iter()
            .any(|event| event.kind == ShellEventKind::ShellExited));
    }

    if Command::new("zsh").arg("--version").output().is_ok() {
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-shell-zsh-ctrl-d-test-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let mut config = ShellHostConfig::new("zsh-ctrl-d-test", &work_dir);
        config.native_mode = false;
        let mut rendered = Vec::new();
        let output = run_raw_relay_zsh_with_actions(
            &config,
            vec![
                RawRelayAction::wait(Duration::from_millis(200)),
                RawRelayAction::write(vec![0x04]),
                RawRelayAction::wait(Duration::from_millis(300)),
                RawRelayAction::line("echo __ZSH_AFTER_CTRL_D__"),
            ],
            &mut rendered,
        )
        .expect("zsh ctrl-d");

        let rendered_text = String::from_utf8_lossy(&rendered);
        assert!(
            !rendered_text.contains("__ZSH_AFTER_CTRL_D__"),
            "{rendered_text}"
        );
        assert!(output
            .events
            .iter()
            .any(|event| event.kind == ShellEventKind::ShellExited));
    }
}

#[test]
fn transparent_ctrl_backslash_is_not_synthesized_from_ctrl_c() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-ctrl-backslash-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("ctrl-backslash-test", &work_dir);
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::line(
                "bash -c 'trap \"\" INT; trap \"exit 0\" QUIT; while IFS= read -r _; do :; done'",
            ),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::write(vec![0x03]),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::line("printf '%s\\n' __AFTER_CTRL_C__"),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::write(vec![0x1c]),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::line("printf '%s\\n' __AFTER_QUIT__"),
        ],
        &mut rendered,
    )
    .expect("ctrl-c ctrl-backslash parity");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(rendered_text.contains("__AFTER_QUIT__"), "{rendered_text}");
    assert_no_synthetic_terminal_restore_after_interrupt(&rendered);

    let ledger = ledger_from_output(&output);
    assert!(!ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("__AFTER_CTRL_C__")));
    assert_eq!(
        ledger
            .blocks
            .iter()
            .filter(|block| block.command.starts_with("bash -c 'trap"))
            .count(),
        1,
        "stale history must not be attributed to a later command: {ledger:#?}"
    );
    assert!(
        ledger
            .blocks
            .iter()
            .any(|block| (block.command.contains("__AFTER_QUIT__")
                || block.command == "<redacted untracked command>")
                && block.exit_code == 0),
        "{ledger:#?}"
    );
}

#[test]
fn raw_relay_action_watchdog_turns_swallowed_exit_into_timeout_error() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-watchdog-timeout-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let mut config = ShellHostConfig::new("watchdog-timeout-test", &work_dir);
    config.raw_action_watchdog = Duration::from_secs(5);
    let mut rendered = Vec::new();
    let err = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::line(
                "bash -c 'trap \"\" INT QUIT TERM; while IFS= read -r _; do :; done'",
            ),
            RawRelayAction::wait(Duration::from_millis(300)),
        ],
        &mut rendered,
    )
    .expect_err("watchdog must turn a swallowed trailing exit into an error");
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
}

#[test]
fn raw_relay_host_preserves_user_tty_mutation_after_interrupt() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-raw-tty-restore-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("raw-tty-restore-test", &work_dir);
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions(
        &config,
        vec![
            RawRelayAction::line("stty -echo; sleep 5"),
            RawRelayAction::wait(Duration::from_millis(250)),
            RawRelayAction::write(vec![0x03]),
            RawRelayAction::wait(Duration::from_millis(300)),
            RawRelayAction::line(
                "if stty -a | tr ' ;' '\\n\\n' | grep -qx -- '-echo'; then printf '%s\\n' __STATE_OFF__; stty echo; else printf '%s\\n' __STATE_ON__; fi",
            ),
            RawRelayAction::line("echo after-tty-restore"),
        ],
        &mut rendered,
    )
    .expect("raw relay host");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(rendered_text.contains("__STATE_OFF__"), "{rendered_text}");
    assert!(!rendered_text.contains("__STATE_ON__"), "{rendered_text}");
    assert!(
        rendered_text.contains("after-tty-restore"),
        "{rendered_text}"
    );
    assert!(
        !rendered_text.contains("stty echo icanon"),
        "{rendered_text}"
    );
    assert_no_osc_marker(&rendered);
    assert_no_synthetic_terminal_restore_after_interrupt(&rendered);

    let ledger = ledger_from_output(&output);
    assert!(!ledger
        .blocks
        .iter()
        .any(|block| { block.command.contains("stty echo icanon") }));
    assert!(ledger
        .blocks
        .iter()
        .any(|block| { block.command.contains("echo after-tty-restore") && block.exit_code == 0 }));
}

#[test]
fn cosh_owned_timeout_recovery_restores_pty_without_visible_command() {
    if Command::new("bash").arg("--version").output().is_err() {
        return;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "cosh-shell-cosh-owned-recovery-test-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    let config = ShellHostConfig::new("cosh-owned-recovery-test", &work_dir);
    let command = "stty -echo; sleep 5";
    let mut emitted = false;
    let mut interrupted = false;
    let mut command_started_at: Option<Instant> = None;
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions_output_control(
        &config,
        vec![
            RawRelayAction::wait(Duration::from_millis(900)),
            RawRelayAction::line(stty_flag_probe(
                "-echo",
                "__COSH_RECOVERY_ECHO_OFF__",
                "__COSH_RECOVERY_ECHO_ON__",
                "stty echo",
            )),
            RawRelayAction::line("echo after-cosh-recovery"),
        ],
        &mut rendered,
        move |events, _| {
            if !emitted {
                emitted = true;
                let request = ShellHandoffRequest::new(
                    command,
                    format!("$ {command}"),
                    "validation",
                    "policy",
                    "approval-cosh-owned-recovery",
                    "run-cosh-owned-recovery",
                    1,
                )
                .expect("handoff request");
                return Ok(RawObserverAction::EmitToPty(request));
            }
            if command_started_at.is_none()
                && events.iter().any(|event| {
                    event.kind == ShellEventKind::CommandStarted
                        && event.command.as_deref() == Some(command)
                })
            {
                command_started_at = Some(Instant::now());
            }
            if !interrupted
                && command_started_at
                    .is_some_and(|started| started.elapsed() > Duration::from_millis(250))
            {
                interrupted = true;
                return Ok(RawObserverAction::InterruptForeground);
            }
            Ok(RawObserverAction::Continue)
        },
    )
    .expect("cosh-owned recovery");

    let rendered_text = String::from_utf8_lossy(&rendered);
    assert!(
        rendered_text.contains("after-cosh-recovery"),
        "{rendered_text}"
    );
    assert_no_synthetic_terminal_restore_after_interrupt(&rendered);

    let ledger = ledger_from_output(&output);
    let command_output = ledger_output_refs_text(&ledger);
    assert!(
        command_output.contains("__COSH_RECOVERY_ECHO_ON__"),
        "{command_output}"
    );
    assert!(
        !command_output.contains("__COSH_RECOVERY_ECHO_OFF__"),
        "{command_output}"
    );
    assert!(ledger
        .blocks
        .iter()
        .any(|block| block.command.contains("echo after-cosh-recovery") && block.exit_code == 0));
}
