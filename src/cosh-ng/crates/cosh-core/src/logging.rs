use std::io::Write;
use std::os::unix::fs::MetadataExt;
use tracing_subscriber::EnvFilter;

/// Per-user log retention: today and the preceding two UTC dates. Approved by the user as a per-user total; the
/// decision record is not committed to this repository, so the value is stated
/// here rather than cited.
///
/// Duplicated from the Shell writer rather than shared: cosh-shell has no
/// internal crate dependencies and cosh-types forbids I/O, so there is no common
/// home for one authoritative constant. Both writers retire logs in the same
/// `~/.copilot-shell/logs`, so the two copies must be kept equal by hand.
const LOG_RETENTION_DAYS: u64 = 3;
/// Log file prefixes managed by the per-user budget. Both components' files are
/// matched so either writer can retire the other's expired logs in the shared
/// directory.
const LOG_PREFIXES: &[&str] = &["cosh-core.log.", "cosh-shell.log."];

const LOG_BUDGET_BYTES: u64 = 20 * 1024 * 1024;

pub fn init_logging(level: &str) {
    let log_dir = log_directory();

    let filter = if let Ok(cosh_log) = std::env::var("COSH_LOG") {
        EnvFilter::try_new(&cosh_log).unwrap_or_else(|_| EnvFilter::new("info"))
    } else if let Ok(rust_log) = std::env::var("RUST_LOG") {
        EnvFilter::try_new(&rust_log).unwrap_or_else(|_| EnvFilter::new("info"))
    } else {
        EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"))
    };

    if let Some(dir) = &log_dir {
        // `tracing_appender::rolling::daily` panics when it cannot open its file,
        // so probe first and degrade to stderr: logging is optional persistence
        // and must never take the process down.
        if ensure_private_log_dir(dir).is_err() || !dir_is_writable(dir) {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .with_target(true)
                .init();
            return;
        }
        let mut gate = LogBudgetGate::new(dir.clone());
        match acquire_budget_lock(dir) {
            Ok(Some(_lock)) => {
                if let Err(error) = cleanup_old_logs(dir, LOG_RETENTION_DAYS)
                    .and_then(|_| evict_to_budget(dir, LOG_BUDGET_BYTES))
                {
                    gate.suppress_io(&error);
                }
            }
            Ok(None) => {}
            Err(error) => gate.suppress_io(&error),
        }
        if open_log_file(dir, "cosh-core.log").is_err() {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .with_target(true)
                .init();
            return;
        }
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(BudgetedWriter {
                prefix: "cosh-core.log",
                gate: std::sync::Arc::new(std::sync::Mutex::new(gate)),
            })
            .with_ansi(false)
            .with_target(true)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_target(true)
            .init();
    }
}

fn log_directory() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".copilot-shell/logs"))
}

/// Write-time gate that keeps the shared per-user log directory inside budget.
///
/// Startup cleanup alone cannot honour "3 days / 20 MiB per user": a single long
/// session keeps appending to today's file, and several concurrent sessions share
/// one directory. The gate is consulted before each write, so growth is bounded
/// during the session rather than only at the next start.
pub(crate) struct LogBudgetGate {
    dir: std::path::PathBuf,
    budget_bytes: u64,
    retention_days: u64,
    suppressed: bool,
}

impl LogBudgetGate {
    pub(crate) fn new(dir: std::path::PathBuf) -> Self {
        Self::with_limits(dir, LOG_BUDGET_BYTES, LOG_RETENTION_DAYS)
    }

    fn with_limits(dir: std::path::PathBuf, budget_bytes: u64, retention_days: u64) -> Self {
        Self {
            dir,
            budget_bytes,
            retention_days,
            suppressed: false,
        }
    }

    /// Accounts an upcoming write of `bytes` and reports whether it may proceed.
    ///
    /// Returning `false` means the per-user budget is exhausted and nothing more
    /// may be evicted, so the caller drops the record instead of growing the
    /// directory. Logging is optional persistence: suppression must never block
    /// the shell, and it is latched so a long session does not rescan the
    /// directory for every remaining record.
    pub(crate) fn allow(&mut self, bytes: u64) -> bool {
        if self.suppressed {
            return false;
        }
        let admissible =
            match reclaim_for_write(&self.dir, self.retention_days, self.budget_bytes, bytes) {
                Ok(admissible) => admissible,
                Err(error) => {
                    self.suppress_io(&error);
                    return false;
                }
            };
        if admissible {
            return true;
        }
        self.suppressed = true;
        let _ = writeln!(
            std::io::stderr(),
            "cosh-core per-user log budget of {} bytes exhausted; this process stops writing \
             log records. Remove older logs and restart to resume.",
            self.budget_bytes
        );
        false
    }
    fn suppress_io(&mut self, error: &std::io::Error) {
        if !self.suppressed {
            self.suppressed = true;
            let _ = writeln!(
                std::io::stderr(),
                "cosh optional log persistence stopped: {error}; restart to resume"
            );
        }
    }
}

/// Serializes directory accounting, daily file open and record writes.
struct BudgetedWriter {
    prefix: &'static str,
    gate: std::sync::Arc<std::sync::Mutex<LogBudgetGate>>,
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for BudgetedWriter {
    type Writer = BudgetedWriterGuard<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        let mut gate = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let budget_lock = if gate.suppressed {
            None
        } else {
            match acquire_budget_lock(&gate.dir) {
                Ok(lock) => lock,
                Err(error) => {
                    gate.suppress_io(&error);
                    None
                }
            }
        };
        // Never retain a descriptor across dates or a failed daily open.
        let inner =
            budget_lock
                .as_ref()
                .and_then(|_| match open_log_file(&gate.dir, self.prefix) {
                    Ok(file) => Some(file),
                    Err(error) => {
                        gate.suppress_io(&error);
                        None
                    }
                });
        BudgetedWriterGuard {
            inner,
            gate,
            _budget_lock: budget_lock,
        }
    }
}

fn open_log_file(dir: &std::path::Path, prefix: &str) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(dir.join(format!("{prefix}.{}", today_suffix())))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "log is not a regular file",
        ));
    }
    ensure_log_owner(&metadata)?;
    Ok(file)
}

fn ensure_log_owner(metadata: &std::fs::Metadata) -> std::io::Result<()> {
    if metadata.uid() != nix::unistd::geteuid().as_raw() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "log belongs to another user",
        ));
    }
    Ok(())
}

fn acquire_budget_lock(
    dir: &std::path::Path,
) -> std::io::Result<Option<nix::fcntl::Flock<std::fs::File>>> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(".budget.lock"))?;
    match nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock) {
        Ok(lock) => Ok(Some(lock)),
        Err((_, nix::errno::Errno::EWOULDBLOCK)) => Ok(None),
        Err((_, error)) => Err(std::io::Error::from_raw_os_error(error as i32)),
    }
}

struct BudgetedWriterGuard<'a> {
    inner: Option<std::fs::File>,
    gate: std::sync::MutexGuard<'a, LogBudgetGate>,
    _budget_lock: Option<nix::fcntl::Flock<std::fs::File>>,
}

impl std::io::Write for BudgetedWriterGuard<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let Some(inner) = self.inner.as_mut() else {
            return Ok(buf.len());
        };
        if !self.gate.allow(buf.len() as u64) {
            return Ok(buf.len());
        }
        // Optional persistence must not make the application fail on ENOSPC.
        if let Err(error) = inner.write_all(buf) {
            self.gate.suppress_io(&error);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(inner) = self.inner.as_mut() {
            if let Err(error) = inner.flush() {
                self.gate.suppress_io(&error);
            }
        }
        Ok(())
    }
}

/// Creates the log directory owner-only, tightening an existing one.
///
/// Log records carry command text, paths and error detail, and `create_dir_all`
/// leaves the directory world-readable (0755), which exposes them to every local
/// UID whenever a parent directory does not already block access.
fn ensure_private_log_dir(dir: &std::path::Path) -> std::io::Result<()> {
    // `create_dir_all` is a no-op on a symlink, and both the mode change below
    // and the retention pass would then act on whatever the link points at, with
    // the filename filter as the only thing keeping them inside.
    if let Ok(metadata) = std::fs::symlink_metadata(dir) {
        if metadata.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "log directory is a symbolic link",
            ));
        }
    }
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Probes whether `dir` accepts new file writes. `create_dir_all` succeeds on an
/// existing read-only directory, so an explicit check is needed before handing
/// the path to the rolling appender.
///
/// The probe is created with `create_new`, which never truncates an existing
/// file, and its name carries a nanosecond count as well as the pid: a probe
/// left behind by a process that died between create and remove would otherwise
/// report `AlreadyExists` forever and silently cost every later process reusing
/// that pid its file logging.
fn dir_is_writable(dir: &std::path::Path) -> bool {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let probe = dir.join(format!(".cosh-write-probe-{}-{unique}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Returns true when `name` is a cosh daily log file (`<prefix>YYYY-MM-DD`).
fn is_managed_log_file(name: &str) -> bool {
    LOG_PREFIXES.iter().any(|prefix| {
        if !name.starts_with(prefix) {
            return false;
        }
        let date_part = &name[prefix.len()..];
        date_part.len() == 10 && is_date_suffix(date_part)
    })
}

fn is_date_suffix(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[2].is_ascii_digit()
        && b[3].is_ascii_digit()
        && b[4] == b'-'
        && b[5].is_ascii_digit()
        && b[6].is_ascii_digit()
        && b[7] == b'-'
        && b[8].is_ascii_digit()
        && b[9].is_ascii_digit()
}

fn log_date_suffix(path: &std::path::Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    LOG_PREFIXES
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix))
        .map(str::to_string)
}

/// Removes managed logs older than `keep_days`.
///
/// Only files matching [`LOG_PREFIXES`] with a valid date suffix are considered,
/// so unrelated files sharing the directory are never touched. Age comes from the
/// date the rolling appender wrote into the name, not from the mtime: an mtime is
/// rewritable by `touch`, a backup restore or a network filesystem, and both
/// writers in this directory must agree on one clock. Dates at or before
/// `today - keep_days` expire, retaining exactly `keep_days` UTC dates.
fn cleanup_old_logs(dir: &std::path::Path, keep_days: u64) -> std::io::Result<()> {
    let cutoff = date_suffix_days_ago(keep_days);
    for (path, date, _, _) in managed_logs(dir)? {
        if date <= cutoff {
            reclaim_log_file(&path)?;
        }
    }
    Ok(())
}

/// Truncates before unlinking and propagates failures other than concurrent disappearance.
///
/// Truncation is the step that frees space, and this runs in a different process
/// from the writer's: a session that outlives midnight holds yesterday's
/// descriptor until its own next write rotates, and unlinking such a file
/// succeeds while the inode — and every byte — stays alive until that descriptor
/// closes. The name would be gone, so the Shell budget gate that measures by name
/// would count those bytes as reclaimed when the disk still holds them.
///
/// Duplicated from the Shell writer for the same reason as [`LOG_RETENTION_DAYS`].
fn reclaim_log_file(path: &std::path::Path) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK);
    }
    let result = options
        .open(path)
        .and_then(|file| {
            let metadata = file.metadata()?;
            if !metadata.is_file() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "managed log is not a regular file",
                ));
            }
            ensure_log_owner(&metadata)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.nlink() != 1 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "refusing to truncate a log with multiple hard links",
                    ));
                }
            }
            file.set_len(0)
        })
        .and_then(|_| std::fs::remove_file(path));
    match result {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Evicts oldest-first until managed logs fit in `budget_bytes`, returning the
/// remaining total.
///
/// Today's file is never evicted: a concurrent session may hold it open, so
/// unlinking it reclaims nothing while breaking that writer.
fn evict_to_budget(dir: &std::path::Path, budget_bytes: u64) -> std::io::Result<u64> {
    let today = today_suffix();
    let mut logs = managed_logs(dir)?;
    let mut total: u64 = logs.iter().map(|(_, _, _, size)| *size).sum();
    if total <= budget_bytes {
        return Ok(total);
    }
    logs.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.2.cmp(&right.2)));
    for (path, date, _, size) in logs {
        if total <= budget_bytes {
            break;
        }
        if date == today {
            continue;
        }
        reclaim_log_file(&path)?;
        total = total.saturating_sub(size);
    }
    Ok(total)
}

fn reclaim_for_write(
    dir: &std::path::Path,
    retention_days: u64,
    budget_bytes: u64,
    bytes: u64,
) -> std::io::Result<bool> {
    if bytes > budget_bytes {
        return Ok(false);
    }
    let target = budget_bytes - bytes;
    let today = today_suffix();
    let cutoff = date_suffix_days_ago(retention_days);
    let mut logs = managed_logs(dir)?;
    logs.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.2.cmp(&right.2)));
    let mut remaining = logs
        .iter()
        .fold(0_u64, |total, log| total.saturating_add(log.3));
    let mut reclaim = Vec::new();
    for (path, date, _, size) in logs {
        if date != today && (date <= cutoff || remaining > target) {
            reclaim.push(path);
            remaining = remaining.saturating_sub(size);
        }
    }
    if remaining > target {
        return Ok(false);
    }
    for path in reclaim {
        reclaim_log_file(&path)?;
    }
    Ok(true)
}
/// Sums managed log bytes so a caller can decide whether a write still fits.
#[cfg(test)]
fn managed_log_bytes(dir: &std::path::Path) -> u64 {
    managed_logs(dir)
        .expect("read fixture logs")
        .iter()
        .map(|(_, _, _, size)| *size)
        .sum()
}

/// Lists managed log files with the date carried in their name, their mtime and
/// their size.
///
/// The name date is the authoritative age: the rolling appender writes it, so
/// unlike an mtime it cannot be rewritten by `touch`, a backup restore, or a
/// network filesystem returning an epoch timestamp — any of which would
/// otherwise make a large log both immortal and permanently "newest", wedge the
/// byte cap, and latch suppression for every later session. mtime is kept only
/// to order two files that share a date.
///
/// Uses `symlink_metadata`, so a symlink wearing a log-file name is neither
/// counted nor removed: deleting the link reclaims none of the target's space,
/// and the target may live outside the log directory.
fn managed_logs(
    dir: &std::path::Path,
) -> std::io::Result<Vec<(std::path::PathBuf, String, std::time::SystemTime, u64)>> {
    let mut logs = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !is_managed_log_file(name) {
            continue;
        }
        let Some(date) = log_date_suffix(&path) else {
            continue;
        };
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.file_type().is_file() && meta.uid() == nix::unistd::geteuid().as_raw() {
            logs.push((path, date, meta.modified()?, meta.len()));
        }
    }
    Ok(logs)
}

fn today_suffix() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// The `YYYY-MM-DD` suffix `days` before today, for comparing against the dates
/// the rolling appender writes into managed log names.
fn date_suffix_days_ago(days: u64) -> String {
    (chrono::Utc::now() - chrono::Duration::days(days as i64))
        .format("%Y-%m-%d")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn core_only_writer_respects_the_twenty_mib_budget() {
        if std::env::var_os("COSH_TEST_CORE_LOG_CHILD").is_some() {
            init_logging("info");
            let record = "x".repeat(4096);
            for _ in 0..6000 {
                tracing::info!("{record}");
            }
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "logging::tests::core_only_writer_respects_the_twenty_mib_budget",
            ])
            .arg("--nocapture")
            .env("HOME", home.path())
            .env("COSH_TEST_CORE_LOG_CHILD", "1")
            .env_remove("COSH_LOG")
            .env_remove("RUST_LOG");
        #[cfg(target_os = "linux")]
        command.stderr(std::process::Stdio::from(
            std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .unwrap(),
        ));
        let status = command.status().unwrap();
        assert!(status.success());
        let total = managed_log_bytes(&home.path().join(".copilot-shell/logs"));
        assert!(total > 0, "positive control: Core must write logs");
        assert!(total <= 20 * 1024 * 1024, "Core wrote {total} bytes");
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
    #[cfg(unix)]
    fn readonly_descriptor_write_failure_latches_io_suppression() {
        use std::io::Write;
        use tracing_subscriber::fmt::MakeWriter;
        let dir = tempfile::tempdir().unwrap();
        let gate = std::sync::Arc::new(std::sync::Mutex::new(LogBudgetGate::new(
            dir.path().to_path_buf(),
        )));
        let writer = BudgetedWriter {
            prefix: "cosh-core.log",
            gate: gate.clone(),
        };
        writer.make_writer().write_all(b"healthy").unwrap();
        let today = dir.path().join(format!("cosh-core.log.{}", today_suffix()));
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
            prefix: "cosh-core.log",
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
            "cosh-core.log.{}",
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
            "cosh-core.log.{}",
            date_suffix_days_ago(LOG_RETENTION_DAYS)
        ));
        let kept = dir.path().join(format!(
            "cosh-core.log.{}",
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
    fn date_suffix(days_ago: u64) -> String {
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(days_ago * 86400);
        chrono::DateTime::<chrono::Utc>::from(when)
            .format("%Y-%m-%d")
            .to_string()
    }

    /// Creates a file whose name carries `days_ago`'s date and whose mtime
    /// matches.
    ///
    /// The mtime is set through `File::set_modified` rather than by spawning
    /// `touch`: the spawned form discarded its exit status, so on an image
    /// without `touch` every seed silently got a current mtime and the eviction
    /// order became `read_dir` order.
    fn seed(dir: &Path, name: &str, days_ago: u64) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"x").unwrap();
        set_mtime(&path, days_ago);
        path
    }

    fn set_mtime(path: &Path, days_ago: u64) {
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(days_ago * 86400);
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    /// The approved per-user log retention is 3 days, so a 4-day-old Core log
    /// must go even though the old 7-day rule kept it.
    #[test]
    fn cleanup_applies_the_approved_three_day_retention() {
        let dir = tempfile::tempdir().unwrap();
        let old = seed(dir.path(), &format!("cosh-core.log.{}", date_suffix(4)), 4);
        let fresh = seed(dir.path(), &format!("cosh-core.log.{}", date_suffix(1)), 1);

        cleanup_old_logs(dir.path(), LOG_RETENTION_DAYS).unwrap();

        assert!(!old.exists(), "4-day-old log must exceed the 3-day budget");
        assert!(fresh.exists(), "1-day-old log must be kept");
    }

    /// Cleanup matches by suffix length only, so any unrelated file whose
    /// extension happens to be 10 characters is deleted. That is user data loss
    /// in a directory the user may share with other tooling.
    #[test]
    fn cleanup_preserves_unrelated_files() {
        let dir = tempfile::tempdir().unwrap();
        let notes = seed(dir.path(), &format!("notes.{}", date_suffix(30)), 30);
        let other = seed(dir.path(), "keep.abcdefghij", 30);
        let readme = dir.path().join("README");
        std::fs::write(&readme, b"keep").unwrap();

        cleanup_old_logs(dir.path(), LOG_RETENTION_DAYS).unwrap();

        assert!(notes.exists(), "unrelated dated file must survive");
        assert!(other.exists(), "unrelated 10-char suffix must survive");
        assert!(readme.exists(), "extensionless file must survive");
    }

    /// Today's file may be held open by a live writer; unlinking it reclaims
    /// nothing and breaks that process, so an old mtime on a today-dated file
    /// must not cause deletion.
    #[test]
    fn cleanup_never_removes_todays_active_log() {
        let dir = tempfile::tempdir().unwrap();
        let today = seed(dir.path(), &format!("cosh-core.log.{}", date_suffix(0)), 9);

        cleanup_old_logs(dir.path(), LOG_RETENTION_DAYS).unwrap();

        assert!(today.exists(), "today's active log must never be evicted");
    }

    /// Retention here runs in a different process from the writer's, and a Shell
    /// session that outlives midnight still holds yesterday's descriptor. An
    /// unlink alone would drop the name while the bytes stay alive, so the Shell
    /// budget gate — which measures by name — would count space as reclaimed that
    /// the disk is still holding.
    #[test]
    #[cfg(unix)]
    fn cleanup_reclaims_bytes_of_a_log_another_process_holds_open() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().unwrap();
        let days = LOG_RETENTION_DAYS + 2;
        let held = seed(
            dir.path(),
            &format!("cosh-shell.log.{}", date_suffix(days)),
            days,
        );
        std::fs::write(&held, vec![b'x'; 4096]).unwrap();
        // The rolling appender opens append-only, so this is the descriptor a
        // long-lived Shell session is actually holding across the rotation.
        let writer = std::fs::OpenOptions::new()
            .append(true)
            .open(&held)
            .unwrap();

        cleanup_old_logs(dir.path(), LOG_RETENTION_DAYS).unwrap();

        assert!(!held.exists(), "an expired name must be reclaimed");
        let inode = writer.metadata().unwrap();
        assert_eq!(inode.len(), 0, "the held inode must be truncated");
        assert_eq!(
            inode.blocks(),
            0,
            "a truncated inode must hold no data blocks"
        );
    }

    /// Log records carry command text and paths, so the directory must be
    /// owner-only; `create_dir_all` leaves it world-readable.
    #[test]
    #[cfg(unix)]
    fn log_directory_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".copilot-shell/logs");
        ensure_private_log_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "new log directory must be owner-only");

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_log_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "existing log directory must be tightened");
    }

    /// An unwritable log directory must degrade to stderr, not panic: the
    /// rolling appender panics on file-open failure.
    #[test]
    #[cfg(unix)]
    fn unwritable_log_directory_is_detected() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("logs");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            dir_is_writable(&dir),
            "a usable directory must be reported usable"
        );

        // The case that carries the assertion: an ancestor that is a regular
        // file makes opening inside it fail with ENOTDIR for every uid, root
        // included. A `chmod 0500` denial is a DAC check that root bypasses, so
        // under root — which is how CI and this project's containers run — a
        // mode-only test would also pass for a probe hardcoded to return true.
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, b"not a directory").unwrap();
        assert!(
            !dir_is_writable(&blocker.join("logs")),
            "a directory that cannot hold files must be detected"
        );

        // The DAC case is still pinned where it is observable.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        if !nix::unistd::Uid::effective().is_root() {
            assert!(!dir_is_writable(&dir), "read-only dir must be detected");
        }
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
        let prefix = "cosh-core.log";
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
            "cosh-shell.log.{}",
            date_suffix_days_ago(LOG_RETENTION_DAYS + 2)
        ));
        std::fs::write(&own, b"owned reclaim positive").unwrap();
        assert_eq!(std::fs::metadata(&own).unwrap().uid(), uid);
        cleanup_old_logs(&dir, LOG_RETENTION_DAYS).unwrap();
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
        let prefix = "cosh-core.log";
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
}
