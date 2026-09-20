use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nix::libc;
use wait_timeout::ChildExt;

use super::raw_cli::{raw_cli_shared_run_guard, RawCliRunGuard};

const ROWS: u16 = 24;
const DEADLINE: Duration = Duration::from_secs(5);
const QUIET: Duration = Duration::from_millis(50);

pub(crate) struct TerminalSession {
    child: Child,
    master: File,
    terminal: File,
    original_termios: libc::termios,
    original_flags: i32,
    parser: vt100::Parser,
    raw: Vec<u8>,
    action_start: usize,
    last_output: Instant,
    root: tempfile::TempDir,
    _gate: RawCliRunGuard,
}

impl TerminalSession {
    pub(crate) fn spawn(integration: &str, cols: u16) -> Self {
        Self::spawn_for_shell("bash", integration, cols)
    }

    pub(crate) fn spawn_for_shell(shell: &str, integration: &str, cols: u16) -> Self {
        Self::spawn_with_startup(shell, integration, cols, &[])
    }

    pub(crate) fn spawn_with_startup(
        shell: &str,
        integration: &str,
        cols: u16,
        files: &[(&str, &str)],
    ) -> Self {
        Self::spawn_with_startup_env(shell, integration, cols, files, &[])
    }

    pub(crate) fn spawn_for_shell_with_env(
        shell: &str,
        integration: &str,
        cols: u16,
        extra_envs: &[(&str, &str)],
    ) -> Self {
        Self::spawn_with_startup_env(shell, integration, cols, &[], extra_envs)
    }

    fn spawn_with_startup_env(
        shell: &str,
        integration: &str,
        cols: u16,
        files: &[(&str, &str)],
        extra_envs: &[(&str, &str)],
    ) -> Self {
        let gate = raw_cli_shared_run_guard();
        let root = tempfile::Builder::new()
            .prefix("cosh-screen-")
            .tempdir()
            .expect("screen fixture HOME");
        fs::write(root.path().join(".bashrc"), "PS1='screen$ '\n").unwrap();
        fs::write(
            root.path().join(".zshrc"),
            "PROMPT='screen$ '\nRPROMPT=''\nbindkey -e\n",
        )
        .unwrap();
        fs::write(
            root.path().join("inputrc"),
            "set editing-mode emacs\nset enable-bracketed-paste on\n",
        )
        .unwrap();
        fs::create_dir(root.path().join(".copilot-shell")).unwrap();
        fs::write(
            root.path().join(".copilot-shell/config.toml"),
            "[shell]\nadapter_default = 'fake'\n",
        )
        .unwrap();
        for (name, contents) in files {
            fs::write(root.path().join(name), contents).expect("write startup fixture");
        }
        let size = libc::winsize {
            ws_row: ROWS,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = nix::pty::openpty(Some(&size), None).expect("screen PTY");
        let master = File::from(pty.master);
        let slave = File::from(pty.slave);
        let original_termios = read_termios(&slave);
        let original_flags = unsafe { libc::fcntl(slave.as_raw_fd(), libc::F_GETFL) };
        assert!(original_flags >= 0, "read parent terminal flags");
        let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        let mut command = Command::new(env!("CARGO_BIN_EXE_cosh-shell"));
        command
            .arg0("cosh")
            .args(["--shell", shell])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", root.path())
            .env("TMPDIR", root.path())
            .env("INPUTRC", root.path().join("inputrc"))
            .env("HISTFILE", "/dev/null")
            .env("TERM", "xterm-256color")
            .env("LANG", "C.UTF-8")
            .env("LC_ALL", "C.UTF-8")
            .env("COSH_SHELL_INTEGRATION", integration)
            .env("COSH_SHELL_BOOTSTRAP_PATH", "0")
            .env("COSH_SHELL_STARTUP_BANNER", "0")
            .env("COSH_SHELL_HEALTH_SCAN", "disabled")
            .env("COSH_RECOMMENDATIONS_ENABLED", "0")
            .envs(extra_envs.iter().map(|(key, value)| (*key, *value)))
            .current_dir(root.path())
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()));
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0
                    || libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) < 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().expect("spawn screen session");
        let mut session = Self {
            child,
            master,
            terminal: slave,
            original_termios,
            original_flags,
            parser: vt100::Parser::new(ROWS, cols, 0),
            raw: Vec::new(),
            action_start: 0,
            last_output: Instant::now(),
            root,
            _gate: gate,
        };
        let mut expected = vt100::Parser::new(ROWS, cols, 0);
        expected.process(session.published_prompt().as_bytes());
        session.wait_screen("initial prompt", |screen| {
            screen
                .contents()
                .trim_end()
                .ends_with(expected.screen().contents().trim_end())
        });
        session
    }

    pub(crate) fn home(&self) -> &Path {
        self.root.path()
    }

    pub(crate) fn prompt(&self) -> String {
        "screen$ ".into()
    }

    pub(crate) fn published_prompt(&self) -> String {
        "screen$ ".into()
    }

    pub(crate) fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    pub(crate) fn send(&mut self, bytes: &[u8]) {
        self.action_start = self.raw.len();
        let deadline = Instant::now() + DEADLINE;
        let mut written = 0;
        while written < bytes.len() {
            assert!(Instant::now() < deadline, "PTY write timeout");
            match self.master.write(&bytes[written..]) {
                Ok(0) => panic!("PTY write closed"),
                Ok(count) => written += count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => self.read_output(),
                Err(error) => panic!("PTY write: {error}"),
            }
        }
    }

    pub(crate) fn resize(&mut self, cols: u16) {
        self.action_start = self.raw.len();
        // Keep the same parser, changing its geometry before consuming the
        // shell's response to SIGWINCH. Old output is never replayed at a new size.
        self.parser.screen_mut().set_size(ROWS, cols);
        let size = libc::winsize {
            ws_row: ROWS,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ as _, &size) },
            0,
            "resize parent PTY"
        );
    }

    pub(crate) fn wait_screen(&mut self, label: &str, predicate: impl Fn(&vt100::Screen) -> bool) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            self.read_output();
            if self.raw.len() > self.action_start
                && self.last_output.elapsed() >= QUIET
                && predicate(self.screen())
            {
                eprintln!(
                    "{label}: cursor={:?}, screen={:?}",
                    self.screen().cursor_position(),
                    self.screen().contents()
                );
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{label}: screen timeout; cursor={:?}; screen={:?}; raw={:?}",
                self.screen().cursor_position(),
                self.screen().contents(),
                String::from_utf8_lossy(&self.raw)
            );
        }
    }

    fn read_output(&mut self) {
        let mut poll = libc::pollfd {
            fd: self.master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut poll, 1, 10) };
        assert!(
            result >= 0,
            "poll screen PTY: {}",
            io::Error::last_os_error()
        );
        if result == 0 {
            return;
        }
        let mut bytes = [0; 8192];
        match self.master.read(&mut bytes) {
            Ok(0) => panic!("screen PTY closed unexpectedly"),
            Ok(count) => {
                self.raw.extend_from_slice(&bytes[..count]);
                self.parser.process(&bytes[..count]);
                self.last_output = Instant::now();
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("screen PTY read: {error}"),
        }
    }

    pub(crate) fn finish(self) {
        self.finish_with_input(b"exit\n", 0);
    }

    pub(crate) fn finish_with_input(mut self, input: &[u8], expected_code: i32) {
        self.send(input);
        let status = self
            .child
            .wait_timeout(DEADLINE)
            .expect("wait screen shell")
            .expect("screen shell exit timeout");
        assert_eq!(status.code(), Some(expected_code), "screen shell exit");
        let restored = read_termios(&self.terminal);
        let original = &self.original_termios;
        assert_eq!(restored.c_iflag, original.c_iflag, "input flags");
        assert_eq!(restored.c_oflag, original.c_oflag, "output flags");
        assert_eq!(restored.c_cflag, original.c_cflag, "control flags");
        assert_eq!(restored.c_lflag, original.c_lflag, "local flags");
        assert_eq!(restored.c_line, original.c_line, "line discipline");
        assert_eq!(restored.c_cc, original.c_cc, "control characters");
        assert_eq!(restored.c_ispeed, original.c_ispeed, "input speed");
        assert_eq!(restored.c_ospeed, original.c_ospeed, "output speed");
        assert_eq!(
            unsafe { libc::fcntl(self.terminal.as_raw_fd(), libc::F_GETFL) },
            self.original_flags,
            "parent terminal file flags"
        );
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            unsafe {
                libc::kill(self.child.id() as i32, libc::SIGTERM);
            }
            if self
                .child
                .wait_timeout(Duration::from_secs(1))
                .ok()
                .flatten()
                .is_none()
            {
                unsafe {
                    libc::kill(-(self.child.id() as i32), libc::SIGKILL);
                }
                let _ = self.child.wait_timeout(Duration::from_secs(1));
            }
        }
    }
}

fn read_termios(terminal: &File) -> libc::termios {
    let mut value = unsafe { std::mem::zeroed::<libc::termios>() };
    assert_eq!(
        unsafe { libc::tcgetattr(terminal.as_raw_fd(), &mut value) },
        0
    );
    value
}
