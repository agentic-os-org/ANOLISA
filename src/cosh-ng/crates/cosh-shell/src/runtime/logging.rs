use tracing_subscriber::EnvFilter;

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
        if std::fs::create_dir_all(dir).is_err() || !dir_is_writable(dir) {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .with_target(true)
                .init();
            return;
        }
        cleanup_old_logs(dir, 7);
        let file_appender = tracing_appender::rolling::daily(dir, "cosh-shell.log");
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
    dirs_next_or_home().map(|h| h.join(".copilot-shell/logs"))
}

fn dirs_next_or_home() -> Option<std::path::PathBuf> {
    std::env::var("HOME").ok().map(std::path::PathBuf::from)
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
