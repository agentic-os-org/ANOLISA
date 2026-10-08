//! Test-owned paths and process identities; cleanup also runs after assertions.

use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

pub struct Directory(pub PathBuf);

impl Directory {
    pub fn new(case: Value) -> Self {
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/sec-core-provider-tests");
        fs::create_dir_all(&root).unwrap();
        let path = root.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let directory = Self(fs::canonicalize(path).unwrap());
        fs::write(directory.cli(), include_str!("../fixtures/cli.py")).unwrap();
        fs::set_permissions(directory.cli(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(directory.0.join("case.json"), case.to_string()).unwrap();
        directory
    }

    pub fn cli(&self) -> PathBuf {
        self.0.join("scan cli")
    }

    pub fn argv(&self) -> Value {
        serde_json::from_slice(&fs::read(self.0.join("argv.json")).unwrap()).unwrap()
    }

    pub fn live_pid(&self) -> Option<i32> {
        let identity = fs::read_to_string(self.0.join("cli.pid")).ok()?;
        let (pid, started) = identity.split_once(' ').unwrap();
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let fields: Vec<_> = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .collect();
        (fields[19] == started && !matches!(fields[0], "Z" | "X")).then(|| pid.parse().unwrap())
    }

    pub fn assert_stopped(&self) {
        assert!(self.0.join("cli.pid").exists(), "CLI was never started");
        let deadline = Instant::now() + Duration::from_secs(1);
        while self.live_pid().is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(self.live_pid().is_none(), "CLI outlived its Provider");
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        if let Some(pid) = self.live_pid() {
            // SAFETY: signal only the fixture's recorded PID/start-time identity.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
            self.assert_stopped();
        }
        fs::remove_dir_all(&self.0).unwrap();
    }
}
