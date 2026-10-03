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
        // panics on file-open failure, so we probe writability first — the
        // same guard cosh-shell has had since d7cc87af; without it every
        // cosh-core invocation (including agent backends spawned by
        // cosh-shell) dies before dispatch.
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

fn log_directory() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".copilot-shell/logs"))
}

/// Probe whether `dir` accepts new file writes. `create_dir_all` succeeds on
/// an existing read-only directory, so we need an explicit writability check
/// before handing the path to `tracing_appender::rolling::daily`, which panics
/// on file-open failure (mirrors the cosh-shell guard from d7cc87af).
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

    #[test]
    fn dir_is_writable_accepts_a_writable_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(dir_is_writable(dir.path()));
        // The probe must clean up after itself.
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .flatten()
            .collect();
        assert!(entries.is_empty(), "probe file left behind: {entries:?}");
    }

    #[test]
    fn dir_is_writable_rejects_a_regular_file_path() {
        // A regular file where the directory should be fails the probe with
        // ENOTDIR regardless of privileges (a chmod-0555 directory would stay
        // writable for root, so this is the privilege-independent check).
        let file = tempfile::NamedTempFile::new().expect("tempfile");
        assert!(!dir_is_writable(file.path()));
    }
}
