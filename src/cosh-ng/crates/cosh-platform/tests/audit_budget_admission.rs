use chrono::Utc;
use cosh_platform::audit::store::{AuditDurability, AuditSegmentWriter};
use cosh_types::audit::*;
use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use uuid::Uuid;

fn event() -> AuditEventV1 {
    let now = Utc::now();
    AuditEventV1::new(
        Uuid::new_v4().to_string(),
        KnownAuditEventType::AuditDegraded.into(),
        now,
        now,
        0,
        AuditComponent {
            name: AuditComponentName::CoshCore,
            version: "0.27.0".into(),
        },
        AuditIdentity::default(),
        AuditActor {
            kind: AuditActorKind::System,
            uid: None,
            euid: None,
        },
        AuditEventOutcome {
            status: AuditOutcomeStatus::Degraded,
            code: None,
            retryable: false,
        },
        AuditSubject {
            kind: "audit".into(),
            name: None,
        },
        &AuditControlData {
            operation: Some("budget_probe".into()),
            ..Default::default()
        },
        AuditRedaction {
            policy_version: "audit-redaction-v1".into(),
            status: AuditRedactionStatus::Clean,
            fields: vec![],
        },
    )
    .unwrap()
}

fn segment_bytes(root: &Path) -> u64 {
    fn total(path: &Path) -> u64 {
        fs::read_dir(path)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    total(&entry.path())
                } else if entry.file_type().unwrap().is_file() {
                    entry.metadata().unwrap().len()
                } else {
                    0
                }
            })
            .sum()
    }
    total(&root.join("v1/segments"))
}

fn private_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn writer(root: &Path, cap: u64, component: AuditComponentName) -> AuditSegmentWriter {
    AuditSegmentWriter::create_with_settings(
        root,
        component,
        AuditSettings {
            max_disk_bytes: cap,
            ..AuditSettings::default()
        },
    )
    .unwrap()
}

fn fill(root: &Path, cap: u64) -> usize {
    let mut first = writer(root, cap, AuditComponentName::CoshCore);
    let mut second = writer(root, cap, AuditComponentName::CoshCli);
    for index in 0..100 {
        let current = if index % 2 == 0 {
            &mut first
        } else {
            &mut second
        };
        if current
            .append(&mut event(), AuditDurability::Ordinary)
            .is_err()
        {
            assert!(segment_bytes(root) <= cap);
            return index;
        }
    }
    panic!("configured capacity was never enforced");
}

#[test]
fn effective_capacity_controls_shared_writers_and_preserves_active_and_closed_history() {
    let small = private_root();
    let large = private_root();
    let small_count = fill(&small.path().canonicalize().unwrap(), 4096);
    let large_count = fill(&large.path().canonicalize().unwrap(), 8192);
    assert!(small_count > 1);
    assert!(large_count > small_count);
    let root = small.path().canonicalize().unwrap();
    let before = segment_bytes(&root);
    let sentinel = root.join("pending-execution-ledger");
    fs::write(&sentinel, b"must remain").unwrap();
    let mut incoming = writer(&root, before, AuditComponentName::CoshCore);
    assert!(incoming
        .append(&mut event(), AuditDurability::SecurityBoundary)
        .is_err());
    assert_eq!(segment_bytes(&root), before);
    assert_eq!(fs::read(sentinel).unwrap(), b"must remain");
}

#[test]
fn impossible_record_and_busy_budget_lock_fail_without_persisting_bytes() {
    use nix::fcntl::{Flock, FlockArg};
    let root = private_root();
    let root = root.path().canonicalize().unwrap();
    let mut tiny = writer(&root, 1, AuditComponentName::CoshCore);
    assert!(tiny
        .append(&mut event(), AuditDurability::Ordinary)
        .is_err());
    assert_eq!(segment_bytes(&root), 0);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("v1/write-budget.lock"))
        .unwrap();
    let lock = Flock::lock(lock, FlockArg::LockExclusiveNonblock).unwrap();
    let mut healthy = writer(&root, 4096, AuditComponentName::CoshCli);
    assert!(healthy
        .append(&mut event(), AuditDurability::SecurityBoundary)
        .is_err());
    assert_eq!(segment_bytes(&root), 0);
    drop(lock);
    healthy
        .append(&mut event(), AuditDurability::SecurityBoundary)
        .unwrap();
    assert!(segment_bytes(&root) > 0);
}

#[test]
fn unsafe_managed_entry_is_not_ignored_or_modified() {
    use std::os::unix::fs::symlink;
    let root = private_root();
    let path = root.path().canonicalize().unwrap();
    let mut current = writer(&path, 4096, AuditComponentName::CoshCore);
    current
        .append(&mut event(), AuditDurability::Ordinary)
        .unwrap();
    let date = path
        .join("v1/segments")
        .join(Utc::now().format("%Y-%m-%d").to_string());
    let sentinel = path.join("outside-sentinel");
    fs::write(&sentinel, b"do not touch").unwrap();
    symlink(&sentinel, date.join("foreign.jsonl")).unwrap();
    assert!(current
        .append(&mut event(), AuditDurability::Ordinary)
        .is_err());
    assert_eq!(fs::read(&sentinel).unwrap(), b"do not touch");
    fs::remove_file(date.join("foreign.jsonl")).unwrap();
    current
        .append(&mut event(), AuditDurability::Ordinary)
        .unwrap();
}

#[test]
fn bounded_discovery_fails_closed() {
    let root = private_root();
    let path = root.path().canonicalize().unwrap();
    let mut current = writer(&path, 4096, AuditComponentName::CoshCore);
    current
        .append(&mut event(), AuditDurability::Ordinary)
        .unwrap();
    let before = segment_bytes(&path);
    let date = path
        .join("v1/segments")
        .join(Utc::now().format("%Y-%m-%d").to_string());
    for index in 0..4096 {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(date.join(format!("sentinel-{index}")))
            .unwrap();
    }
    assert!(current
        .append(&mut event(), AuditDurability::Ordinary)
        .is_err());
    assert_eq!(segment_bytes(&path), before);
}

fn wait_for_child(child: &mut std::process::Child) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status.success();
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        std::thread::yield_now();
    }
}

fn audit_budget_child_process() {
    let root = std::path::PathBuf::from(std::env::var_os("AUDIT_BUDGET_TEST_ROOT").unwrap());
    let barrier = std::path::PathBuf::from(std::env::var_os("AUDIT_BUDGET_TEST_BARRIER").unwrap());
    let mut current = writer(&root, 16384, AuditComponentName::CoshCli);
    fs::write(
        barrier.with_extension(format!("ready-{}", std::process::id())),
        b"ready",
    )
    .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !barrier.exists() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    for _ in 0..200 {
        let _ = current.append(&mut event(), AuditDurability::Ordinary);
        std::thread::yield_now();
    }
}

#[test]
fn independent_processes_share_one_atomic_capacity() {
    use std::process::Command;
    if std::env::var_os("COSH_AUDIT_BUDGET_TEST_CHILD").is_some() {
        audit_budget_child_process();
        return;
    }
    let root = private_root();
    let path = root.path().canonicalize().unwrap();
    let mut positive = writer(&path, 16384, AuditComponentName::CoshCore);
    positive
        .append(&mut event(), AuditDurability::Ordinary)
        .unwrap();
    let before = segment_bytes(&path);
    let barrier = root.path().join("go");
    let mut children: Vec<_> = (0..4)
        .map(|_| {
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "independent_processes_share_one_atomic_capacity",
                    "--test-threads=1",
                ])
                .env("COSH_AUDIT_BUDGET_TEST_CHILD", "1")
                .env("AUDIT_BUDGET_TEST_ROOT", &path)
                .env("AUDIT_BUDGET_TEST_BARRIER", &barrier)
                .spawn()
                .unwrap()
        })
        .collect();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while children.iter().any(|child| {
        !barrier
            .with_extension(format!("ready-{}", child.id()))
            .exists()
    }) {
        if std::time::Instant::now() >= deadline {
            for child in &mut children {
                let _ = child.kill();
                let _ = child.wait();
            }
            panic!("budget children did not become ready");
        }
        std::thread::yield_now();
    }
    fs::write(&barrier, b"go").unwrap();
    let mut succeeded = true;
    for child in &mut children {
        succeeded &= wait_for_child(child);
    }
    assert!(succeeded, "independent budget children failed or timed out");
    assert!(segment_bytes(&path) > before);
    assert!(segment_bytes(&path) <= 16384);
}

#[test]
fn replacing_the_segment_tree_cannot_hide_a_live_writer() {
    use std::os::unix::fs::symlink;
    let root = private_root();
    let path = root.path().canonicalize().unwrap();
    let mut current = writer(&path, 4096, AuditComponentName::CoshCore);
    current
        .append(&mut event(), AuditDurability::Ordinary)
        .unwrap();
    let before = segment_bytes(&path);
    fs::rename(path.join("v1/segments"), path.join("hidden")).unwrap();
    let empty = path.join("empty");
    fs::create_dir(&empty).unwrap();
    fs::set_permissions(&empty, fs::Permissions::from_mode(0o700)).unwrap();
    symlink(&empty, path.join("v1/segments")).unwrap();
    assert!(current
        .append(&mut event(), AuditDurability::Ordinary)
        .is_err());
    fs::remove_file(path.join("v1/segments")).unwrap();
    fs::rename(path.join("hidden"), path.join("v1/segments")).unwrap();
    assert_eq!(segment_bytes(&path), before);
    current
        .append(&mut event(), AuditDurability::Ordinary)
        .unwrap();
}

#[cfg(target_os = "linux")]
fn audit_partial_flush_child_process() {
    let root = private_root();
    let path = root.path().canonicalize().unwrap();
    let mut current = writer(&path, 4096, AuditComponentName::CoshCore);
    let mut other = writer(&path, 4096, AuditComponentName::CoshCli);
    let mut original = nix::libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // This test is dispatched alone in a disposable child, never the main test process.
    unsafe {
        assert_eq!(
            nix::libc::getrlimit(nix::libc::RLIMIT_FSIZE, &mut original),
            0
        );
        nix::libc::signal(nix::libc::SIGXFSZ, nix::libc::SIG_IGN);
        let limited = nix::libc::rlimit {
            rlim_cur: 32,
            ..original
        };
        assert_eq!(nix::libc::setrlimit(nix::libc::RLIMIT_FSIZE, &limited), 0);
    }
    let failed = current.append(&mut event(), AuditDurability::Ordinary);
    assert!(failed.is_err());
    assert_eq!(segment_bytes(&path), 32);
    // Remove EFBIG so the peer must be rejected by the held reservation, not the kernel limit.
    unsafe {
        assert_eq!(nix::libc::setrlimit(nix::libc::RLIMIT_FSIZE, &original), 0);
    }
    assert!(other
        .append(&mut event(), AuditDurability::Ordinary)
        .is_err());
    assert_eq!(segment_bytes(&path), 32);
    current
        .append(&mut event(), AuditDurability::SecurityBoundary)
        .unwrap();
    other
        .append(&mut event(), AuditDurability::Ordinary)
        .unwrap();
    assert!(segment_bytes(&path) > 32);
    assert!(segment_bytes(&path) <= 4096);
}

#[cfg(target_os = "linux")]
#[test]
fn partial_kernel_write_remains_reserved_until_flush_recovers() {
    if std::env::var_os("COSH_AUDIT_PARTIAL_FLUSH_TEST_CHILD").is_some() {
        audit_partial_flush_child_process();
        return;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "partial_kernel_write_remains_reserved_until_flush_recovers",
            "--test-threads=1",
        ])
        .env("COSH_AUDIT_PARTIAL_FLUSH_TEST_CHILD", "1")
        .spawn()
        .unwrap();
    assert!(
        wait_for_child(&mut child),
        "partial-write child failed or timed out"
    );
}

#[test]
fn rejected_writers_do_not_publish_empty_closed_segments() {
    let root = private_root();
    let path = root.path().canonicalize().unwrap();
    for _ in 0..8 {
        let mut rejected = writer(&path, 1, AuditComponentName::CoshCore);
        assert!(rejected
            .append(&mut event(), AuditDurability::Ordinary)
            .is_err());
    }
    let directory = path
        .join("v1/segments")
        .join(Utc::now().format("%Y-%m-%d").to_string());
    assert_eq!(
        fs::read_dir(&directory).unwrap().count(),
        0,
        "rejected writers must not accumulate empty segments"
    );
    let mut healthy = writer(&path, 4096, AuditComponentName::CoshCore);
    healthy
        .append(&mut event(), AuditDurability::SecurityBoundary)
        .unwrap();
    healthy.close().unwrap();
    let files = fs::read_dir(directory)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(files.len(), 1);
    assert!(files[0]
        .path()
        .extension()
        .is_some_and(|extension| extension == "jsonl"));
    assert!(files[0].metadata().unwrap().len() > 0);
}
