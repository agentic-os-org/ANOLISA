use super::*;
use std::path::PathBuf;

/// Log records carry command text, paths and error detail, so the directory
/// holding them must be owner-only. Measured on ALinux4: `create_dir_all`
/// plus the rolling appender leave 0755/0644, and with a shared HOME a
/// second UID reads the first user's logs successfully.
#[test]
#[cfg(unix)]
fn log_directory_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(".copilot-shell/logs");
    ensure_private_log_dir(&dir).unwrap();
    let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "new log directory must be owner-only");

    // An existing world-readable directory must be tightened, not left as
    // it is: upgrades cannot depend on the user fixing permissions first.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    ensure_private_log_dir(&dir).unwrap();
    let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "existing log directory must be tightened");
}

#[test]
#[cfg(unix)]
fn readonly_descriptor_write_failure_latches_io_suppression() {
    use std::io::Write;
    use tracing_subscriber::fmt::MakeWriter;
    let dir = tempfile::tempdir().unwrap();
    let gate = std::sync::Arc::new(std::sync::Mutex::new(LogBudgetGate::new(
        dir.path().to_path_buf(),
    )));
    let writer = BudgetedWriter {
        prefix: "cosh-shell.log",
        gate: gate.clone(),
    };
    writer.make_writer().write_all(b"healthy").unwrap();
    let today = dir
        .path()
        .join(format!("cosh-shell.log.{}", today_suffix()));
    let read_only = std::fs::File::open(&today).unwrap();
    assert_eq!(
        (&read_only).write_all(b"probe").unwrap_err().raw_os_error(),
        Some(nix::libc::EBADF)
    );
    let mut failing = BudgetedWriterGuard {
        inner: Some(read_only),
        gate: gate.lock().unwrap(),
        _budget_lock: acquire_budget_lock(dir.path()).unwrap(),
    };
    for _ in 0..2 {
        failing.write_all(b"must not persist").unwrap();
    }
    drop(failing);
    assert!(gate.lock().unwrap().suppressed);
    assert_eq!(std::fs::metadata(&today).unwrap().len(), 7);
    writer.make_writer().write_all(b"still latched").unwrap();
    assert_eq!(std::fs::metadata(&today).unwrap().len(), 7);
    let restarted = BudgetedWriter {
        prefix: "cosh-shell.log",
        gate: std::sync::Arc::new(std::sync::Mutex::new(LogBudgetGate::new(
            dir.path().to_path_buf(),
        ))),
    };
    restarted.make_writer().write_all(b"recovered").unwrap();
    assert_eq!(std::fs::metadata(&today).unwrap().len(), 16);
}

#[test]
#[cfg(unix)]
fn hard_linked_log_preserves_external_archive() {
    use std::os::unix::fs::MetadataExt;
    let home = tempfile::tempdir().unwrap();
    let logs = home.path().join("logs");
    std::fs::create_dir(&logs).unwrap();
    let expired = logs.join(format!(
        "cosh-shell.log.{}",
        date_suffix_days_ago(LOG_RETENTION_DAYS + 1)
    ));
    let archive = home.path().join("archive");
    std::fs::write(&expired, b"archived sensitive record").unwrap();
    std::fs::hard_link(&expired, &archive).unwrap();
    assert_eq!(std::fs::metadata(&expired).unwrap().nlink(), 2);
    let mut gate = LogBudgetGate::new(logs.clone());
    let allowed = gate.allow(8);
    assert_eq!(
        std::fs::read(&archive).unwrap(),
        b"archived sensitive record",
        "reclamation must preserve the external inode alias"
    );
    assert!(!allowed);
    assert!(gate.suppressed);
    assert!(!gate.allow(8));
    assert!(expired.exists());
    std::fs::remove_file(&archive).unwrap();
    let mut restarted = LogBudgetGate::new(logs);
    assert!(
        restarted.allow(8),
        "single-link reclamation must still work"
    );
    assert!(!expired.exists());
}

#[test]
fn rejected_record_preserves_unexpired_history() {
    for bytes in [400, 2048] {
        let dir = tempfile::tempdir().unwrap();
        let today = dir.path().join(format!("cosh-core.log.{}", today_suffix()));
        let yesterday = dir
            .path()
            .join(format!("cosh-shell.log.{}", date_suffix_days_ago(1)));
        std::fs::write(&today, vec![b't'; 700]).unwrap();
        std::fs::write(&yesterday, vec![b'h'; 300]).unwrap();
        let mut gate =
            LogBudgetGate::with_limits(dir.path().to_path_buf(), 1024, LOG_RETENTION_DAYS);
        assert!(!gate.allow(bytes));
        assert!(gate.suppressed);
        assert_eq!(
            std::fs::read(&yesterday).unwrap(),
            vec![b'h'; 300],
            "a rejected record must not destroy useful history"
        );
        assert_eq!(std::fs::read(&today).unwrap(), vec![b't'; 700]);
    }
}

#[test]
fn admissible_record_reclaims_only_the_needed_history() {
    use std::io::Write;
    use tracing_subscriber::fmt::MakeWriter;
    let dir = tempfile::tempdir().unwrap();
    let today = dir.path().join(format!("cosh-core.log.{}", today_suffix()));
    let yesterday = dir
        .path()
        .join(format!("cosh-shell.log.{}", date_suffix_days_ago(1)));
    let older = dir
        .path()
        .join(format!("cosh-core.log.{}", date_suffix_days_ago(2)));
    std::fs::write(&today, vec![b't'; 400]).unwrap();
    std::fs::write(&yesterday, vec![b'y'; 300]).unwrap();
    std::fs::write(&older, vec![b'o'; 300]).unwrap();
    let gate = std::sync::Arc::new(std::sync::Mutex::new(LogBudgetGate::with_limits(
        dir.path().to_path_buf(),
        1024,
        LOG_RETENTION_DAYS,
    )));
    let writer = BudgetedWriter {
        prefix: "cosh-core.log",
        gate: gate.clone(),
    };
    writer.make_writer().write_all(&[b'r'; 300]).unwrap();
    assert!(!gate.lock().unwrap().suppressed);
    assert!(!older.exists(), "oldest reclaimable file makes room");
    assert_eq!(std::fs::read(&yesterday).unwrap(), vec![b'y'; 300]);
    assert_eq!(
        std::fs::read(&today).unwrap(),
        [vec![b't'; 400], vec![b'r'; 300]].concat()
    );
}

#[test]
fn reclaiming_a_disappeared_log_is_successful() {
    let dir = tempfile::tempdir().unwrap();
    reclaim_log_file(&dir.path().join("gone.log")).unwrap();
}

#[test]
fn retention_excludes_exactly_three_days_ago() {
    let dir = tempfile::tempdir().unwrap();
    let boundary = dir.path().join(format!(
        "cosh-shell.log.{}",
        date_suffix_days_ago(LOG_RETENTION_DAYS)
    ));
    let kept = dir.path().join(format!(
        "cosh-shell.log.{}",
        date_suffix_days_ago(LOG_RETENTION_DAYS - 1)
    ));
    std::fs::write(&boundary, b"expired").unwrap();
    std::fs::write(&kept, b"kept").unwrap();
    let mut gate = LogBudgetGate::new(dir.path().to_path_buf());
    assert!(gate.allow(8));
    assert!(
        !boundary.exists(),
        "keep only today and the preceding two UTC dates"
    );
    assert!(kept.exists());
}

#[test]
fn managed_log_file_matches_known_prefixes() {
    assert!(is_managed_log_file("cosh-shell.log.2026-10-04"));
    assert!(is_managed_log_file("cosh-core.log.2026-01-15"));
}

#[test]
fn managed_log_file_rejects_unrelated_names() {
    assert!(!is_managed_log_file("notes.2026-09-08"));
    assert!(!is_managed_log_file("keep.abcdefghij"));
    assert!(!is_managed_log_file("README"));
    assert!(!is_managed_log_file("cosh-shell.log"));
    assert!(!is_managed_log_file("cosh-shell.log.2026-10-4"));
    assert!(!is_managed_log_file("cosh-shell.log.2026-10-040"));
    assert!(!is_managed_log_file("other.log.2026-10-04"));
}

#[test]
fn date_suffix_validation() {
    assert!(is_date_suffix("2026-10-04"));
    assert!(is_date_suffix("0000-00-00"));
    assert!(!is_date_suffix("2026-10-4"));
    assert!(!is_date_suffix("abcdefghij"));
    assert!(!is_date_suffix("2026/10/04"));
}

#[test]
fn enforce_budget_preserves_unrelated_and_recent_logs() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();

    std::fs::write(p.join("README"), b"keep me").unwrap();
    std::fs::write(p.join("notes.2026-09-08"), b"unrelated").unwrap();
    std::fs::write(p.join("keep.abcdefghij"), b"unrelated").unwrap();
    // Inside the retention window, and small enough to stay under the cap.
    let recent = seed_log(p, "cosh-shell", 1, 6);

    enforce_log_budget(p).unwrap();

    assert!(p.join("README").exists());
    assert!(p.join("notes.2026-09-08").exists());
    assert!(p.join("keep.abcdefghij").exists());
    assert!(recent.exists(), "a log inside the window must be kept");
}

/// The approved retention is 3 days, so a log four days old is reclaimed and
/// one two days old is kept. Driven through `enforce_log_budget`, which uses
/// the production constant, so raising it turns this red.
#[test]
fn enforce_budget_applies_the_approved_three_day_retention() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let expired = seed_log(p, "cosh-shell", 4, 64);
    let kept = seed_log(p, "cosh-core", 2, 64);

    enforce_log_budget(p).unwrap();

    assert!(!expired.exists(), "a log past 3 days must be reclaimed");
    assert!(kept.exists(), "a log inside 3 days must be kept");
}

#[test]
fn enforce_budget_evicts_oldest_when_over_cap() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();

    // 24 MiB together, over the 20 MiB production cap; the older name date
    // is the eviction victim.
    let oldest = seed_log(p, "cosh-shell", 2, 12 * 1024 * 1024);
    let newer = seed_log(p, "cosh-core", 1, 12 * 1024 * 1024);

    enforce_log_budget(p).unwrap();

    assert!(!oldest.exists(), "the oldest closed log must be evicted");
    assert!(newer.exists(), "eviction stops once the cap is met");
}

/// A session that outlives midnight keeps yesterday's descriptor open until its
/// own next write rotates the file. Eviction still has to reclaim those bytes
/// for real: an unlink alone succeeds, and the accounting that follows it would
/// then hand the budget room the disk does not have.
#[test]
#[cfg(unix)]
fn evicting_a_log_still_held_open_reclaims_its_bytes() {
    use std::os::unix::fs::MetadataExt;

    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let held = seed_log(p, "cosh-shell", 1, 900);
    let todays = seed_log(p, "cosh-core", 0, 100);
    // The rolling appender opens append-only, so this is the descriptor a
    // long-lived session is actually holding across the rotation.
    let writer = std::fs::OpenOptions::new()
        .append(true)
        .open(&held)
        .unwrap();

    let remaining = evict_to_budget(p, 500).unwrap();

    assert!(!held.exists(), "the evicted name must be gone");
    assert!(
        todays.exists(),
        "today's file is never an eviction candidate"
    );
    assert!(
        remaining <= 500,
        "eviction must report the budget met, got {remaining}"
    );
    let inode = writer.metadata().unwrap();
    assert_eq!(inode.len(), 0, "the held inode must be truncated");
    assert_eq!(
        inode.blocks(),
        0,
        "a truncated inode must hold no data blocks: an unlink alone leaves \
         them alive until the stale descriptor closes"
    );
}

/// Retention runs in a different process from the writer's, so it meets the same
/// open-descriptor case and has to reclaim just as really.
#[test]
#[cfg(unix)]
fn retention_cleanup_reclaims_bytes_of_a_log_still_held_open() {
    use std::os::unix::fs::MetadataExt;

    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let held = seed_log(p, "cosh-core", LOG_RETENTION_DAYS + 2, 4096);
    let writer = std::fs::OpenOptions::new()
        .append(true)
        .open(&held)
        .unwrap();

    delete_expired_logs(p, LOG_RETENTION_DAYS).unwrap();

    assert!(!held.exists(), "an expired name must be reclaimed");
    let inode = writer.metadata().unwrap();
    assert_eq!(inode.len(), 0, "the held inode must be truncated");
    assert_eq!(
        inode.blocks(),
        0,
        "a truncated inode must hold no data blocks"
    );
}

/// Pins the two approved production constants. The behavioural cases above
/// run through them, so this is the drift guard that names the values.
#[test]
fn log_budget_defaults_match_the_approved_per_user_limits() {
    assert_eq!(LOG_RETENTION_DAYS, 3);
    assert_eq!(LOG_BUDGET_BYTES, 20 * 1024 * 1024);
}

/// Writes a managed log file of `bytes` whose name date is `days_ago` old.
///
/// Retention and eviction both order by the name date, so no mtime has to be
/// set up: the fixture is deterministic without spawning `touch`, whose exit
/// status this suite used to discard.
fn seed_log(dir: &std::path::Path, component: &str, days_ago: u64, bytes: usize) -> PathBuf {
    let path = dir.join(format!(
        "{component}.log.{}",
        date_suffix_days_ago(days_ago)
    ));
    std::fs::write(&path, vec![b'x'; bytes]).unwrap();
    path
}

/// Rewrites a file's mtime without spawning `touch`, whose exit status this
/// suite used to discard.
fn set_mtime(path: &std::path::Path, days_ago: u64) {
    let when = std::time::SystemTime::now() - std::time::Duration::from_secs(days_ago * 86400);
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

/// Retention and eviction order read the date the rolling appender wrote into
/// the filename, not the mtime. An mtime is rewritable by `touch`, a backup
/// restore or a network filesystem returning an epoch timestamp, and any of
/// those would otherwise make a large log immortal and permanently "newest",
/// wedging the byte cap and latching suppression for every later session.
#[test]
fn log_retention_uses_the_name_date_not_the_mtime() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    // Named today, mtime pushed far past the window: must survive.
    let fresh_name = seed_log(p, "cosh-shell", 0, 64);
    set_mtime(&fresh_name, 30);
    // Named past the window, mtime refreshed to now: must be reclaimed.
    let old_name = seed_log(p, "cosh-core", 5, 64);
    set_mtime(&old_name, 0);

    enforce_log_budget(p).unwrap();

    assert!(
        fresh_name.exists(),
        "today's name must be kept whatever its mtime says"
    );
    assert!(
        !old_name.exists(),
        "an expired name must be reclaimed whatever its mtime says"
    );
}

#[test]
fn gate_allows_writes_while_under_budget() {
    let dir = tempfile::tempdir().unwrap();
    seed_log(dir.path(), "cosh-shell", 0, 100);
    let mut gate = LogBudgetGate::with_budget(dir.path().to_path_buf(), 1024);
    assert!(gate.allow(200));
    assert!(gate.allow(200));
}

/// The point of the gate: growth is bounded *during* a session, so a write
/// that would exceed the budget evicts the oldest closed log instead of
/// waiting for the next process start.
#[test]
fn gate_evicts_oldest_closed_log_during_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let older = seed_log(dir.path(), "cosh-shell", 2, 600);
    let newer = seed_log(dir.path(), "cosh-core", 1, 600);
    let mut gate = LogBudgetGate::with_budget(dir.path().to_path_buf(), 1024);

    assert!(gate.allow(300), "write must proceed after making room");
    assert!(!older.exists(), "oldest closed log must be evicted");
    assert!(newer.exists(), "newer log must survive");
}

/// Today's file may be open in another session, so unlinking it reclaims
/// nothing and breaks that writer: it is never evicted. The directory is over
/// budget and today's file is the only thing left that could be evicted, so
/// the gate suppresses instead — and stays suppressed, so a long session does
/// not rescan on every remaining record.
#[test]
fn gate_suppresses_without_evicting_todays_file() {
    let dir = tempfile::tempdir().unwrap();
    let today = seed_log(dir.path(), "cosh-shell", 0, 1500);
    let yesterday = seed_log(dir.path(), "cosh-core", 1, 500);
    let mut gate = LogBudgetGate::with_budget(dir.path().to_path_buf(), 1024);

    assert!(
        !gate.allow(100),
        "must suppress when nothing more can be evicted"
    );
    assert_eq!(
        std::fs::read(&yesterday).unwrap(),
        vec![b'x'; 500],
        "rejecting a record must not reclaim useful history"
    );
    assert!(today.exists(), "today's log must never be evicted");
    assert!(!gate.allow(1), "suppression must persist");
    assert!(today.exists());
}

#[test]
fn gate_retires_expired_logs_before_a_quiet_write() {
    let dir = tempfile::tempdir().unwrap();
    let expired = seed_log(dir.path(), "cosh-shell", 5, 64);
    let mut gate = LogBudgetGate::with_budget(dir.path().to_path_buf(), LOG_BUDGET_BYTES);
    assert!(gate.allow(1));
    assert!(
        !expired.exists(),
        "retention applies even before a small record"
    );
}

#[test]
#[cfg(unix)]
fn gate_ignores_unrelated_files_and_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    // Unrelated file: not a log name, so never counted or removed.
    std::fs::write(dir.path().join("notes.2020-01-01"), vec![b'y'; 4096]).unwrap();
    let precious = dir.path().join("precious.dat");
    std::fs::write(&precious, vec![b'z'; 4096]).unwrap();
    // A symlink wearing a managed log name with an evictable (non-today)
    // date. Following it would count 4096 bytes and eviction would unlink it.
    let link = dir
        .path()
        .join(format!("cosh-core.log.{}", date_suffix_days_ago(1)));
    std::os::unix::fs::symlink(&precious, &link).unwrap();

    let mut gate = LogBudgetGate::with_budget(dir.path().to_path_buf(), 1024);
    // Not load-bearing on its own: following the symlink would also free
    // room by unlinking it, so this stays true either way. The two assertions
    // below are what pin the symlink and unrelated-file handling.
    assert!(gate.allow(100));
    assert!(dir.path().join("notes.2020-01-01").exists());
    assert!(precious.exists(), "symlink target must not be deleted");
    assert!(
        link.symlink_metadata().unwrap().file_type().is_symlink(),
        "a symlink wearing a log name must not be evicted"
    );
}

#[test]
fn quiet_gate_observes_another_process_growth_before_next_record() {
    let dir = tempfile::tempdir().unwrap();
    let mut gate = LogBudgetGate::with_budget(dir.path().to_path_buf(), 1024);
    seed_log(dir.path(), "cosh-core", 0, 1020);
    assert_eq!(managed_log_bytes(dir.path()), 1020);
    assert!(
        !gate.allow(8),
        "a quiet Shell must observe Core growth immediately"
    );
}

#[test]
fn failed_daily_open_never_writes_to_a_reclaimed_inode() {
    use std::io::Write;
    use tracing_subscriber::fmt::MakeWriter;

    let dir = tempfile::tempdir().unwrap();
    let stale = dir
        .path()
        .join(format!("cosh-shell.log.{}", date_suffix_days_ago(1)));
    std::fs::write(&stale, vec![b'x'; 4096]).unwrap();
    let held = std::fs::OpenOptions::new()
        .append(true)
        .open(&stale)
        .unwrap();
    reclaim_log_file(&stale).unwrap();
    let today = dir
        .path()
        .join(format!("cosh-shell.log.{}", today_suffix()));
    std::fs::create_dir(&today).unwrap();
    let writer = BudgetedWriter {
        prefix: "cosh-shell.log",
        gate: std::sync::Arc::new(std::sync::Mutex::new(LogBudgetGate::new(
            dir.path().to_path_buf(),
        ))),
    };
    for _ in 0..16 {
        writer.make_writer().write_all(&[b'x'; 4096]).unwrap();
    }
    assert_eq!(
        held.metadata().unwrap().len(),
        0,
        "failed daily open cannot grow an invisible inode"
    );
    std::fs::remove_dir(&today).unwrap();
    writer.make_writer().write_all(b"still suppressed").unwrap();
    assert!(!today.exists(), "I/O suppression persists until restart");
    let writer = BudgetedWriter {
        prefix: "cosh-shell.log",
        gate: std::sync::Arc::new(std::sync::Mutex::new(LogBudgetGate::new(
            dir.path().to_path_buf(),
        ))),
    };
    writer.make_writer().write_all(b"healthy").unwrap();
    assert_eq!(
        std::fs::metadata(&today).unwrap().len(),
        7,
        "a usable daily file must accept records"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn enospc_from_optional_log_write_does_not_fail_the_application() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let gate = std::sync::Mutex::new(LogBudgetGate::new(dir.path().to_path_buf()));
    let mut writer = BudgetedWriterGuard {
        inner: Some(
            std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .unwrap(),
        ),
        gate: gate.lock().unwrap(),
        _budget_lock: acquire_budget_lock(dir.path()).unwrap(),
    };
    assert_eq!(writer.write(b"record").unwrap(), 6);
    assert!(
        writer.gate.suppressed,
        "ENOSPC must stop further persistence attempts"
    );
    writer.flush().unwrap();
}

#[test]
fn budget_scan_error_suppresses_optional_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("not-a-directory");
    std::fs::write(&blocker, b"blocker").unwrap();
    let mut gate = LogBudgetGate::new(blocker);
    assert!(
        !gate.allow(8),
        "a scan error cannot be treated as an empty directory"
    );
    assert!(gate.suppressed);
}

#[test]
fn budget_lock_open_error_suppresses_optional_persistence() {
    use std::io::Write;
    use tracing_subscriber::fmt::MakeWriter;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".budget.lock")).unwrap();
    let gate = std::sync::Arc::new(std::sync::Mutex::new(LogBudgetGate::new(
        dir.path().to_path_buf(),
    )));
    let writer = BudgetedWriter {
        prefix: "cosh-shell.log",
        gate: gate.clone(),
    };
    writer.make_writer().write_all(b"record").unwrap();
    assert!(
        gate.lock().unwrap().suppressed,
        "lock IO error must be visible and latched"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn writer_guard_holds_the_lock_until_the_record_is_written() {
    use std::io::Write;
    use tracing_subscriber::fmt::MakeWriter;

    let dir = tempfile::tempdir().unwrap();
    seed_log(dir.path(), "cosh-core", 0, 1000);
    let writer = BudgetedWriter {
        prefix: "cosh-shell.log",
        gate: std::sync::Arc::new(std::sync::Mutex::new(LogBudgetGate::with_budget(
            dir.path().to_path_buf(),
            1024,
        ))),
    };
    let mut guard = writer.make_writer();
    assert!(acquire_budget_lock(dir.path()).unwrap().is_none());
    guard.write_all(b"12345678").unwrap();
    assert_eq!(managed_log_bytes(dir.path()), 1008);
    drop(guard);
    let lock = acquire_budget_lock(dir.path()).unwrap();
    assert!(lock.is_some(), "dropping the writer releases its lock");
    drop(lock);
    writer.make_writer().write_all(b"12345678").unwrap();
    assert_eq!(managed_log_bytes(dir.path()), 1016);
}

#[test]
fn local_subscriber_enforces_a_small_log_budget() {
    let dir = tempfile::tempdir().unwrap();
    let gate = std::sync::Arc::new(std::sync::Mutex::new(LogBudgetGate::with_budget(
        dir.path().to_path_buf(),
        1024,
    )));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .without_time()
        .with_ansi(false)
        .with_writer(BudgetedWriter {
            prefix: "cosh-shell.log",
            gate: gate.clone(),
        })
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let record = "x".repeat(128);
        for _ in 0..16 {
            tracing::info!("{record}");
        }
    });
    let total = managed_log_bytes(dir.path());
    assert!(total > 0, "positive control: subscriber writes records");
    assert!(
        total <= 1024,
        "writer exceeded the injected budget: {total}"
    );
    assert!(gate.lock().unwrap().suppressed);
}

#[cfg(target_os = "linux")]
fn foreign_uid_snapshot(path: &std::path::Path) -> (Vec<u8>, u32, u32, u32, u64) {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).unwrap();
    (
        std::fs::read(path).unwrap(),
        metadata.uid(),
        metadata.gid(),
        metadata.mode(),
        metadata.ino(),
    )
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires an explicitly provisioned writable foreign-UID fixture"]
fn foreign_uid_expired_log_preserves_content() {
    use std::os::unix::fs::MetadataExt;
    let dir = std::path::PathBuf::from(
        std::env::var_os("COSH_TEST_FOREIGN_LOG_DIR").expect("dual-UID fixture directory"),
    );
    let prefix = "cosh-shell.log";
    let foreign = dir.join(format!(
        "{prefix}.{}",
        date_suffix_days_ago(LOG_RETENTION_DAYS + 1)
    ));
    std::fs::rename(dir.join("foreign"), &foreign).unwrap();
    let before = foreign_uid_snapshot(&foreign);
    let uid = nix::unistd::geteuid().as_raw();
    assert_ne!(uid, 0, "DUT must be nonroot");
    assert_ne!(before.1, uid, "sentinel must belong to another UID");
    std::fs::OpenOptions::new()
        .write(true)
        .open(&foreign)
        .expect("DAC must permit the actor to write foreign content");
    let own = dir.join(format!(
        "cosh-core.log.{}",
        date_suffix_days_ago(LOG_RETENTION_DAYS + 2)
    ));
    std::fs::write(&own, b"owned reclaim positive").unwrap();
    assert_eq!(std::fs::metadata(&own).unwrap().uid(), uid);
    delete_expired_logs(&dir, LOG_RETENTION_DAYS).unwrap();
    assert!(foreign.exists(), "foreign UID expired log was deleted");
    assert_eq!(
        foreign_uid_snapshot(&foreign),
        before,
        "foreign content or metadata changed"
    );
    assert!(!own.exists(), "own expired log must still be reclaimed");
    assert_eq!(
        reclaim_log_file(&foreign).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(foreign_uid_snapshot(&foreign), before);
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires an explicitly provisioned writable foreign-UID fixture"]
fn foreign_uid_current_log_refuses_append() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use tracing_subscriber::fmt::MakeWriter;
    let dir = std::path::PathBuf::from(
        std::env::var_os("COSH_TEST_FOREIGN_LOG_DIR").expect("dual-UID fixture directory"),
    );
    let prefix = "cosh-shell.log";
    let foreign = dir.join(format!("{prefix}.{}", today_suffix()));
    std::fs::rename(dir.join("foreign"), &foreign).unwrap();
    let before = foreign_uid_snapshot(&foreign);
    let uid = nix::unistd::geteuid().as_raw();
    assert_ne!(uid, 0, "DUT must be nonroot");
    assert_ne!(std::fs::metadata(&foreign).unwrap().uid(), uid);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&foreign)
        .expect("DAC must permit the actor to append foreign content");
    let gate = std::sync::Arc::new(std::sync::Mutex::new(LogBudgetGate::new(dir.clone())));
    let writer = BudgetedWriter {
        prefix,
        gate: gate.clone(),
    };
    writer
        .make_writer()
        .write_all(b"must not append foreign content")
        .unwrap();
    assert_eq!(
        foreign_uid_snapshot(&foreign),
        before,
        "foreign current log was appended or metadata changed"
    );
    assert!(
        gate.lock().unwrap().suppressed,
        "foreign write target must latch optional persistence IO failure"
    );
    assert_eq!(
        open_log_file(&dir, prefix).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    let owned_dir = dir.join("owned-positive");
    std::fs::create_dir(&owned_dir).unwrap();
    std::fs::set_permissions(&owned_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let owned_writer = BudgetedWriter {
        prefix,
        gate: std::sync::Arc::new(std::sync::Mutex::new(LogBudgetGate::new(owned_dir.clone()))),
    };
    owned_writer
        .make_writer()
        .write_all(b"owned append positive")
        .unwrap();
    assert_eq!(
        std::fs::read(owned_dir.join(format!("{prefix}.{}", today_suffix()))).unwrap(),
        b"owned append positive"
    );
    let linked_dir = dir.join("symlink-negative");
    std::fs::create_dir(&linked_dir).unwrap();
    std::os::unix::fs::symlink(
        &foreign,
        linked_dir.join(format!("{prefix}.{}", today_suffix())),
    )
    .unwrap();
    let linked_gate = std::sync::Arc::new(std::sync::Mutex::new(LogBudgetGate::new(
        linked_dir.clone(),
    )));
    let linked_writer = BudgetedWriter {
        prefix,
        gate: linked_gate.clone(),
    };
    linked_writer
        .make_writer()
        .write_all(b"must not follow symlink")
        .unwrap();
    assert!(linked_gate.lock().unwrap().suppressed);
    assert_eq!(foreign_uid_snapshot(&foreign), before);
}
