//! Bounded subprocess support shared by CLI integration tests.

// Cargo compiles this module once per integration-test crate, each of which
// intentionally uses a different subset of the shared process helpers.
#![allow(dead_code)]

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

const PROCESS_TIMEOUT: Duration = Duration::from_secs(5);
const ANOLISA_ENV_PREFIX: &str = "ANOLISA_";
const ANOLISA_DATA_DIR_ENV: &str = "ANOLISA_DATA_DIR";

pub(crate) struct ProcessSandbox {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    data_home: PathBuf,
    config_home: PathBuf,
    state_home: PathBuf,
    cache_home: PathBuf,
    runtime_dir: PathBuf,
    fake_bin: PathBuf,
    packaged_data: PathBuf,
}

impl ProcessSandbox {
    pub(crate) fn new() -> Self {
        let tmp = tempfile::tempdir().expect("process sandbox");
        let root = tmp.path();
        let fake_bin = root.join("fake-bin");
        let packaged_data = root.join("packaged-data");
        std::fs::create_dir_all(&fake_bin).expect("fake bin");
        std::fs::create_dir_all(&packaged_data).expect("packaged data");
        Self {
            home: root.join("home"),
            data_home: root.join("xdg-data"),
            config_home: root.join("xdg-config"),
            state_home: root.join("xdg-state"),
            cache_home: root.join("xdg-cache"),
            runtime_dir: root.join("xdg-runtime"),
            fake_bin,
            packaged_data,
            _tmp: tmp,
        }
    }

    pub(crate) fn run(&self, arguments: &[&str]) -> Output {
        self.run_with_stdout(arguments, Stdio::piped())
    }

    pub(crate) fn run_with_stdout(&self, arguments: &[&str], stdout: Stdio) -> Output {
        let mut command = self.command(arguments);
        run_command(&mut command, stdout)
    }

    pub(crate) fn run_with_path_env(&self, arguments: &[&str], env: &[(&str, &Path)]) -> Output {
        let mut command = self.command(arguments);
        for (key, value) in env {
            command.env(key, value);
        }
        run_command(&mut command, Stdio::piped())
    }

    fn command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_anolisa"));
        command.args(arguments);
        for (key, _) in std::env::vars_os() {
            if is_anolisa_env_key(&key) {
                command.env_remove(key);
            }
        }
        command
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", &self.data_home)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("XDG_STATE_HOME", &self.state_home)
            .env("XDG_CACHE_HOME", &self.cache_home)
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .env(ANOLISA_DATA_DIR_ENV, &self.packaged_data);

        let mut path_entries = vec![self.fake_bin.clone()];
        if let Some(path) = std::env::var_os("PATH") {
            path_entries.extend(std::env::split_paths(&path));
        }
        let path = std::env::join_paths(path_entries).expect("isolated PATH");
        command.env("PATH", path);
        command
    }
}

fn is_anolisa_env_key(key: &OsStr) -> bool {
    key.to_string_lossy().starts_with(ANOLISA_ENV_PREFIX)
}

struct ChildGuard(Child);

impl ChildGuard {
    fn wait_bounded(&mut self, deadline: Instant) -> ExitStatus {
        loop {
            match self.0.try_wait() {
                Ok(Some(status)) => return status,
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => panic!("CLI subprocess exceeded {PROCESS_TIMEOUT:?}"),
                Err(error) => panic!("failed to wait for CLI subprocess: {error}"),
            }
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn drain<R: io::Read + Send + 'static>(mut pipe: Option<R>) -> Receiver<io::Result<Vec<u8>>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = match pipe.as_mut() {
            Some(pipe) => pipe.read_to_end(&mut bytes).map(|_| bytes),
            None => Ok(bytes),
        };
        let _ = sender.send(result);
    });
    receiver
}

fn receive_bounded(receiver: Receiver<io::Result<Vec<u8>>>, deadline: Instant) -> Vec<u8> {
    receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .expect("CLI output drain exceeded process timeout")
        .expect("CLI output must be readable")
}

pub(crate) fn run(arguments: &[&str]) -> Output {
    ProcessSandbox::new().run(arguments)
}

pub(crate) fn run_with_stdout(arguments: &[&str], stdout: Stdio) -> Output {
    ProcessSandbox::new().run_with_stdout(arguments, stdout)
}

pub(crate) fn run_with_path_env(arguments: &[&str], env: &[(&str, &Path)]) -> Output {
    ProcessSandbox::new().run_with_path_env(arguments, env)
}

fn run_command(command: &mut Command, stdout: Stdio) -> Output {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    let mut child = ChildGuard(
        command
            .stdout(stdout)
            .stderr(Stdio::piped())
            .spawn()
            .expect("CLI subprocess must start"),
    );
    let stdout = drain::<ChildStdout>(child.0.stdout.take());
    let stderr = drain::<ChildStderr>(child.0.stderr.take());
    let status = child.wait_bounded(deadline);

    Output {
        status,
        stdout: receive_bounded(stdout, deadline),
        stderr: receive_bounded(stderr, deadline),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anolisa_env_prefix_matches_future_controls_without_overmatching() {
        assert!(is_anolisa_env_key(OsStr::new("ANOLISA_DATA_DIR")));
        assert!(is_anolisa_env_key(OsStr::new("ANOLISA_FUTURE_SETTING")));
        assert!(!is_anolisa_env_key(OsStr::new("NOT_ANOLISA_SETTING")));
    }
}
