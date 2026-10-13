use tracing_subscriber::EnvFilter;

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
        // Fall back to stderr when the log directory cannot be used: either
        // it cannot be created, or it exists but is not writable (e.g. read-
        // only mount, 0555 permissions).  `tracing_appender::rolling::daily`
        // panics on file-open failure, so we probe writability first to keep
        // cosh-core alive when it is spawned as a cosh-shell agent backend
        // or supervised by cosh-gateway, instead of dying with only a raw
        // panic string on the child's stderr.
        if std::fs::create_dir_all(dir).is_err() || !dir_is_writable(dir) {
            eprintln!(
                "[cosh-core] log directory {} is not writable; logging to stderr",
                dir.display()
            );
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .with_target(true)
                .init();
            return;
        }
        cleanup_old_logs(dir, 7);
        let file_appender = tracing_appender::rolling::daily(dir, "cosh-core.log");
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(file_appender)
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

/// Returns the log directory path: `~/.copilot-shell/logs/`.
/// Can be overridden via the `COSH_LOG_DIR` environment variable (for testing).
fn log_directory() -> Option<std::path::PathBuf> {
    if let Ok(override_dir) = std::env::var("COSH_LOG_DIR") {
        return Some(std::path::PathBuf::from(override_dir));
    }
    dirs::home_dir().map(|h| h.join(".copilot-shell/logs"))
}

/// Probe whether `dir` accepts new file writes. `create_dir_all` succeeds on
/// an existing read-only directory, so we need an explicit writability check
/// before handing the path to `tracing_appender::rolling::daily`, which panics
/// on file-open failure.
fn dir_is_writable(dir: &std::path::Path) -> bool {
    // Unique probe path (PID) so pre-existing files do not affect the result.
    // create_new guarantees we never truncate a user file; we only remove
    // files we successfully created.
    let probe = dir.join(format!(".cosh-write-probe-{}", std::process::id()));
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

fn cleanup_old_logs(dir: &std::path::Path, keep_days: u64) {
    let cutoff =
        std::time::SystemTime::now() - std::time::Duration::from_secs(keep_days * 24 * 3600);
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e.len() != 10) {
            continue;
        }
        if let Ok(meta) = path.metadata() {
            if let Ok(modified) = meta.modified() {
                if modified < cutoff {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A regular file standing where the log directory should be. This is
    /// the deterministic shape of an "unusable log dir" that must never reach
    /// `tracing_appender::rolling::daily` (which panics on file-open
    /// failure). Unlike a chmod 0555 directory it behaves identically whether
    /// the tests run as root or not.
    fn file_blocked_log_dir(tmp: &tempfile::TempDir) -> std::path::PathBuf {
        let path = tmp.path().join("logs");
        std::fs::write(&path, b"").unwrap();
        path
    }

    #[test]
    fn init_logging_falls_back_to_stderr_when_log_dir_is_a_file() {
        let tmp = tempfile::tempdir().unwrap();
        let blocker = file_blocked_log_dir(&tmp);
        std::env::set_var("COSH_LOG_DIR", &blocker);
        // Must not panic: pre-fix, `create_dir_all`'s error was swallowed
        // and `rolling::daily` died on the unwritable path.
        init_logging("info");
        std::env::remove_var("COSH_LOG_DIR");
    }

    #[test]
    fn dir_is_writable_accepts_fresh_dir_and_leaves_no_residue() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(dir_is_writable(tmp.path()));
        // The probe file must be removed after a successful check.
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[test]
    fn dir_is_writable_rejects_regular_file_path() {
        let tmp = tempfile::tempdir().unwrap();
        let blocker = file_blocked_log_dir(&tmp);
        assert!(!dir_is_writable(&blocker));
        // The probe must never touch or truncate an existing file.
        assert_eq!(std::fs::metadata(&blocker).unwrap().len(), 0);
    }
}
