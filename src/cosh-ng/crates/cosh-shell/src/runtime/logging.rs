use std::io::Write;
use tracing_subscriber::EnvFilter;

/// Per-user log retention: today and the preceding two UTC dates. Approved by the user as a per-user total
/// covering rotated and detailed logs; the decision record is not committed to
/// this repository, so the value is stated here rather than cited.
const LOG_RETENTION_DAYS: u64 = 3;
/// Per-user log byte budget: 20 MiB across Shell+Core. Approved together with
/// [`LOG_RETENTION_DAYS`], whichever limit is reached first applies.
const LOG_BUDGET_BYTES: u64 = 20 * 1024 * 1024;
/// Log file prefixes managed by the per-user budget.
const LOG_PREFIXES: &[&str] = &["cosh-shell.log.", "cosh-core.log."];

#[cfg(debug_assertions)]
static FILE_SINK_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(debug_assertions)]
pub(crate) fn file_sink_is_ready() -> bool {
    FILE_SINK_READY.load(std::sync::atomic::Ordering::Acquire)
}

pub(crate) fn init_logging(config_log_level: &str) {
    let log_dir = log_directory();

    let filter = if let Ok(cosh_log) = std::env::var("COSH_LOG") {
        EnvFilter::try_new(&cosh_log).unwrap_or_else(|_| EnvFilter::new("info"))
    } else if let Ok(rust_log) = std::env::var("RUST_LOG") {
        EnvFilter::try_new(&rust_log).unwrap_or_else(|_| EnvFilter::new("info"))
    } else {
        EnvFilter::try_new(config_log_level).unwrap_or_else(|_| EnvFilter::new("info"))
    };

    if let Some(dir) = &log_dir {
        // Fall back to stderr when the log directory cannot be used: either
        // it cannot be created, or it exists but is not writable (e.g. read-
        // only mount, 0555 permissions).  `tracing_appender::rolling::daily`
        // panics on file-open failure, so we probe writability first to keep
        // `cosh-shell doctor` alive long enough to report the permission issue.
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
                if let Err(error) = enforce_log_budget(dir) {
                    gate.suppress_io(&error);
                }
            }
            Ok(None) => {}
            Err(error) => gate.suppress_io(&error),
        }
        if open_log_file(dir, "cosh-shell.log").is_err() {
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
                prefix: "cosh-shell.log",
                gate: std::sync::Arc::new(std::sync::Mutex::new(gate)),
            })
            .with_ansi(false)
            .with_target(true)
            .init();
        #[cfg(debug_assertions)]
        FILE_SINK_READY.store(true, std::sync::atomic::Ordering::Release);
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_target(true)
            .init();
    }
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

    #[cfg(test)]
    fn with_budget(dir: std::path::PathBuf, budget_bytes: u64) -> Self {
        Self::with_limits(dir, budget_bytes, LOG_RETENTION_DAYS)
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
            "cosh-shell per-user log budget of {} bytes exhausted; this process stops writing \
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
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "log is not a regular file",
        ));
    }
    Ok(file)
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
/// Log records carry command text, paths and error detail. The default
/// `create_dir_all` mode (0755) plus the rolling appender's 0644 files let any
/// local UID read another user's logs whenever a parent directory does not
/// already block it — measured with a shared HOME. Restricting the directory is
/// what closes that, since file modes inside an unreachable directory cannot be
/// opened by another UID.
fn ensure_private_log_dir(dir: &std::path::Path) -> std::io::Result<()> {
    // `create_dir_all` is a no-op on a symlink, and both the mode change below
    // and the budget's deletion pass would then act on whatever the link points
    // at, with the filename filter as the only thing keeping them inside. Refuse
    // the link instead; a race after this check needs write access to
    // `$HOME/.copilot-shell`, which is already game over.
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

/// Structured-logging directory, `~/.copilot-shell/logs`.
pub(crate) fn log_directory() -> Option<std::path::PathBuf> {
    dirs_next_or_home().map(|h| h.join(".copilot-shell/logs"))
}
fn dirs_next_or_home() -> Option<std::path::PathBuf> {
    std::env::var("HOME").ok().map(std::path::PathBuf::from)
}

/// Probe whether `dir` accepts new file writes. `create_dir_all` succeeds on
/// an existing read-only directory, so we need an explicit writability check
/// before handing the path to `tracing_appender::rolling::daily`, which panics
/// on file-open failure.
///
/// The probe is created with `create_new`, which never truncates an existing
/// file, and its name carries a nanosecond count as well as the pid. A probe
/// left behind by a process that died between create and remove would otherwise
/// make `create_new` report `AlreadyExists` forever, and every later process
/// reusing that pid would silently lose file logging. The residue is not managed
/// log data, so the budget neither counts nor removes it.
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

/// Returns true when `name` matches a known cosh log file pattern:
/// `cosh-shell.log.YYYY-MM-DD` or `cosh-core.log.YYYY-MM-DD`.
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

/// Enforce the per-user log budget: 3-day retention then 20 MiB byte cap.
///
/// Only touches files matching [`LOG_PREFIXES`] with a valid date suffix.
/// Unrelated files in the same directory are never deleted.
fn enforce_log_budget(dir: &std::path::Path) -> std::io::Result<()> {
    delete_expired_logs(dir, LOG_RETENTION_DAYS)?;
    evict_to_budget(dir, LOG_BUDGET_BYTES)?;
    Ok(())
}

/// Retains today and the preceding `retention_days - 1` UTC dates.
fn delete_expired_logs(dir: &std::path::Path, keep_days: u64) -> std::io::Result<()> {
    let cutoff = date_suffix_days_ago(keep_days);
    for (path, date, _, _) in managed_logs(dir)? {
        if date <= cutoff {
            reclaim_log_file(&path)?;
        }
    }
    Ok(())
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
/// Truncates before unlinking and propagates failures other than concurrent disappearance.
///
/// Truncation is the step that frees space. A session that outlives midnight
/// keeps yesterday's descriptor open until its own next write rotates, and
/// unlinking such a file succeeds while the inode — and every byte — stays alive
/// until that descriptor closes. Subtracting the size after `remove_file` alone
/// would therefore hand the budget room that does not exist on disk.
///
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
        if meta.file_type().is_file() {
            logs.push((path, date, meta.modified()?, meta.len()));
        }
    }
    Ok(logs)
}

fn log_date_suffix(path: &std::path::Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    LOG_PREFIXES
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix))
        .map(str::to_string)
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
#[path = "logging_tests.rs"]
mod tests;
