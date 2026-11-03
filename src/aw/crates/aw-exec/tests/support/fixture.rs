//! Fixture ownership, process inspection, and bounded cleanup for runtime tests.

#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use aw_exec::{run, CommandSpec, Error, Limits, Output};

static NEXT: AtomicUsize = AtomicUsize::new(0);
pub const NORMAL_TIMEOUT: Duration = Duration::from_secs(5);
pub const CLEANUP_ALLOWANCE: Duration = Duration::from_secs(2);

pub struct Directory(pub PathBuf);

impl Directory {
    pub fn new() -> Self {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/executor-tests");
        fs::create_dir_all(&parent).unwrap();
        let path = parent.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    pub fn command(&self, scenario: &str) -> CommandSpec {
        let python = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|directory| directory.join("python3"))
            .find(|candidate| candidate.is_file())
            .expect("runtime tests require python3 on PATH");
        CommandSpec {
            program: fs::canonicalize(python).unwrap(),
            args: vec![
                "-I".into(),
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/child.py")
                    .into_os_string(),
                self.0.clone().into_os_string(),
                scenario.into(),
            ],
            cwd: self.0.clone(),
            // Python otherwise adds LC_CTYPE when coercing an empty locale.
            environment: BTreeMap::from([("LC_ALL".into(), "C".into())]),
        }
    }

    pub fn process(&self, name: &str) -> Process {
        let content = fs::read_to_string(self.0.join(format!("{name}.pid")))
            .unwrap_or_else(|error| panic!("fixture did not record {name}: {error}"));
        let (pid, started) = content.trim().split_once(' ').unwrap();
        Process {
            pid: pid.parse().unwrap(),
            started: started.parse().unwrap(),
        }
    }

    pub fn assert_reaped(&self) {
        assert!(
            self.process("leader").state().is_none(),
            "executor returned without reaping its child"
        );
    }

    pub fn assert_descendant_stopped(&self) {
        let process = self.process("descendant");
        assert!(
            !process.is_live(),
            "descendant {} remained live after executor returned",
            process.pid
        );
        self.assert_reaped();
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let processes: Vec<_> = fs::read_dir(&self.0)
            .unwrap()
            .map(Result::unwrap)
            .filter_map(|entry| {
                let path = entry.path();
                (path.extension().is_some_and(|extension| extension == "pid")).then(|| {
                    let name = path.file_stem().unwrap().to_str().unwrap();
                    (self.process(name), name == "leader")
                })
            })
            .collect();
        for (process, leader) in &processes {
            if process.state().is_some() {
                // Recorded start times prevent killing a process that reused a PID.
                // Each child leader owns its process group; descendants need only
                // their individual PID when the leader has already been reaped.
                unsafe {
                    if *leader {
                        libc::kill(-(process.pid as i32), libc::SIGKILL);
                    }
                    libc::kill(process.pid as i32, libc::SIGKILL);
                }
            }
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        while processes.iter().any(|(process, _)| process.is_live()) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        for (process, leader) in &processes {
            // A failed assertion may leave our direct child as a zombie. Reap
            // only recorded PIDs; grandchildren remain their parent's concern.
            if *leader && process.state().is_some() {
                unsafe {
                    libc::waitpid(process.pid as i32, std::ptr::null_mut(), libc::WNOHANG);
                }
            }
        }
        let remaining: Vec<_> = processes
            .iter()
            .filter(|(process, _)| process.is_live())
            .map(|(process, _)| process.pid)
            .collect();
        let removed = fs::remove_dir_all(&self.0);
        if thread::panicking() {
            if !remaining.is_empty() || removed.is_err() {
                eprintln!("fixture cleanup: live PIDs {remaining:?}, directory {removed:?}");
            }
        } else {
            assert!(
                remaining.is_empty(),
                "fixture cleanup left PIDs {remaining:?}"
            );
            removed.unwrap();
            assert!(!self.0.exists());
        }
    }
}

pub struct Process {
    pid: u32,
    started: u64,
}

impl Process {
    pub fn state(&self) -> Option<char> {
        let contents = match fs::read_to_string(format!("/proc/{}/stat", self.pid)) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
            Err(error) => panic!("cannot inspect owned process {}: {error}", self.pid),
        };
        let fields: Vec<_> = contents
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .collect();
        (fields[19].parse::<u64>().unwrap() == self.started)
            .then(|| fields[0].chars().next().unwrap())
    }

    pub fn is_live(&self) -> bool {
        self.state()
            .is_some_and(|state| state != 'Z' && state != 'X')
    }
}

pub fn limits(input_bytes: usize, stdout_bytes: usize, stderr_bytes: usize) -> Limits {
    Limits {
        input_bytes,
        stdout_bytes,
        stderr_bytes,
    }
}

pub fn execute(command: &CommandSpec, input: &[u8], limits: Limits) -> Result<Output, Error> {
    run(
        command,
        input,
        limits,
        Instant::now() + NORMAL_TIMEOUT,
        &AtomicBool::new(false),
    )
}

pub fn wait_until_exists(path: &Path, deadline: Instant) -> bool {
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(5));
    }
    false
}
