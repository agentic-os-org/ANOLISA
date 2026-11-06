//! Run probe spawning separately from contract tests that close and reopen leases.

mod common;

use asc_policy_repository::BindingStateRepository;
use asc_policy_repository_sqlite::SqlitePolicyRepository;
use asc_policy_types::binding::BindingStatus;
use common::{private_dir, seed, transition};
use std::io::{BufRead, BufReader, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct WalLockProbe {
    child: Child,
    socket: BufReader<UnixStream>,
}

impl WalLockProbe {
    fn start(path: &Path) -> Self {
        let metadata = std::fs::metadata(path).unwrap();
        let (parent, child) = UnixStream::pair().unwrap();
        parent
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        parent
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "wal_lock_probe_child", "--nocapture"])
            .env("ASC_WAL_LOCK_PROBE_PATH", path)
            .stdin(Stdio::from(OwnedFd::from(child.try_clone().unwrap())))
            .stdout(Stdio::from(OwnedFd::from(child)))
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut probe = Self {
            child,
            socket: BufReader::new(parent),
        };
        assert_eq!(
            probe.response(),
            format!("ready {} {}", metadata.dev(), metadata.ino()),
            "probe must open the same SHM inode"
        );
        probe
    }

    fn held(&mut self) -> bool {
        self.socket.get_mut().write_all(b"probe\n").unwrap();
        match self.response().as_str() {
            "held" => true,
            "free" => false,
            response => panic!("unexpected WAL lock probe response: {response}"),
        }
    }

    fn response(&mut self) -> String {
        loop {
            let mut line = String::new();
            assert_ne!(
                self.socket.read_line(&mut line).unwrap(),
                0,
                "WAL lock probe exited without a response"
            );
            // The subprocess's test harness also writes to stdout.
            if let Some((_, response)) = line.split_once("WAL_PROBE ") {
                return response.trim().to_owned();
            }
        }
    }
}

impl Drop for WalLockProbe {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn wal_lock_probe_child() {
    use nix::errno::Errno;
    use nix::fcntl::{FcntlArg, fcntl};
    use nix::libc::{F_UNLCK, F_WRLCK, SEEK_SET, flock};

    let Some(path) = std::env::var_os("ASC_WAL_LOCK_PROBE_PATH") else {
        return;
    };
    // POSIX locks belong to a process: even closing a probe fd in the parent
    // would release SQLite's locks. Keep this child fd across connection close
    // so the release check still uses the same inode after SQLite unlinks SHM.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let metadata = file.metadata().unwrap();
    let mut output = std::io::stdout().lock();
    writeln!(
        output,
        "WAL_PROBE ready {} {}",
        metadata.dev(),
        metadata.ino()
    )
    .unwrap();
    output.flush().unwrap();
    for command in std::io::stdin().lock().lines() {
        assert_eq!(command.unwrap(), "probe");
        let mut lock = flock {
            l_type: F_WRLCK.try_into().unwrap(),
            l_whence: SEEK_SET.try_into().unwrap(),
            l_start: 128,
            l_len: 1,
            l_pid: 0,
        };
        let state = match fcntl(&file, FcntlArg::F_SETLK(&lock)) {
            Ok(_) => {
                lock.l_type = F_UNLCK.try_into().unwrap();
                fcntl(&file, FcntlArg::F_SETLK(&lock)).unwrap();
                "free"
            }
            Err(Errno::EACCES | Errno::EAGAIN) => "held",
            Err(error) => panic!("WAL byte 128 lock probe failed: {error}"),
        };
        writeln!(output, "WAL_PROBE {state}").unwrap();
        output.flush().unwrap();
    }
}

#[test]
fn repository_retains_sqlite_wal_locks_like_a_plain_connection() {
    let dir = private_dir();
    let plain = rusqlite::Connection::open(dir.path().join("plain.db")).unwrap();
    plain
        .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE example(value);")
        .unwrap();
    let mut plain_probe = WalLockProbe::start(&dir.path().join("plain.db-shm"));
    assert!(
        plain_probe.held(),
        "positive control must retain the WAL deadman lock"
    );
    drop(plain);
    assert!(
        !plain_probe.held(),
        "closed plain connection must release the WAL deadman lock"
    );

    let repository = SqlitePolicyRepository::open(&dir.path().join("policy-state.db")).unwrap();
    repository.check_writable().unwrap();
    let mut repository_probe = WalLockProbe::start(&dir.path().join("policy-state.db-shm"));
    // An unrelated close() must not release SQLite's cross-process protection.
    assert!(
        repository_probe.held(),
        "Repository file validation released SQLite's WAL deadman lock"
    );
    let initial = seed(&repository);
    let applying = transition(&repository, &initial, BindingStatus::Applying);
    assert_eq!(
        repository
            .get_binding_state(&initial.binding.spec.binding_id)
            .unwrap(),
        Some(applying)
    );
    assert!(
        repository_probe.held(),
        "Repository reads and writes must preserve SQLite's WAL deadman lock"
    );
    drop(repository);
    assert!(
        !repository_probe.held(),
        "closed repository must release the WAL deadman lock"
    );
}

#[test]
fn wal_lock_probe_detects_released_lock() {
    let dir = private_dir();
    let connection = rusqlite::Connection::open(dir.path().join("plain.db")).unwrap();
    connection
        .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE example(value);")
        .unwrap();
    let path = dir.path().join("plain.db-shm");
    let mut probe = WalLockProbe::start(&path);
    assert!(
        probe.held(),
        "open connection must hold the WAL deadman lock"
    );
    // Reproduce the original descriptor-validation bug while SQLite stays open.
    drop(std::fs::File::open(path).unwrap());
    assert!(
        !probe.held(),
        "probe must detect locks released by an unrelated descriptor close"
    );
}
