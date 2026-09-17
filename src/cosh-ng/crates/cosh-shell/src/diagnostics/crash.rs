//! Panic persistence for post-mortem diagnosis.
//!
//! The default panic hook prints to stderr, which the TUI owns; once the
//! terminal is restored the message is lost. This module appends one JSON line
//! per panic to `~/.copilot-shell/cosh-shell-crash.log` (top-level, so the
//! bundle collector's crashes source picks it up) so `cosh-shell doctor` and
//! `cosh-shell diagnostics export` can report the crash even when the session
//! dies without an error on screen.

use std::io::Write as _;
use std::panic::PanicHookInfo;

/// Upper bound for the crash log; older entries are dropped when exceeded so a
/// panic loop cannot fill the disk.
const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// Appends one JSON line describing `info` to the crash log.
///
/// Best-effort by design: every failure path is silent because the panic hook
/// must never panic itself (a hook panic aborts without running the default
/// handler).
pub(crate) fn record_panic(info: &PanicHookInfo) {
    let message = panic_message(info);
    let location = info
        .location()
        .map(|location| format!("{}:{}", location.file(), location.line()));
    let backtrace = backtrace_summary();
    let record = serde_json::json!({
        "ts": now_ms(),
        "version": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(),
        "panic": message,
        "location": location,
        "backtrace": backtrace,
    });
    let Some(path) = crash_log_path() else {
        return;
    };
    let mut line = serde_json::to_string(&record).unwrap_or_default();
    line.push('\n');
    let _ = append_bounded(&path, line.as_bytes());
}

/// Extracts the panic payload as a short human-readable string.
fn panic_message(info: &PanicHookInfo) -> String {
    let payload = info.payload();
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "non-string panic payload".to_string()
}

/// Captures at most the first few frames of the backtrace, so the crash line
/// stays compact while still naming the originating module and function.
fn backtrace_summary() -> Vec<String> {
    std::backtrace::Backtrace::force_capture()
        .to_string()
        .lines()
        .take(8)
        .map(str::to_string)
        .collect()
}

fn crash_log_path() -> Option<std::path::PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|home| std::path::Path::new(&home).join(".copilot-shell/cosh-shell-crash.log"))
}

/// Appends `bytes` to `path`, truncating the oldest content when the file
/// would exceed `MAX_LOG_BYTES`. Creates the parent directory if missing.
fn append_bounded(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let current_len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    if current_len + bytes.len() as u64 > MAX_LOG_BYTES {
        // Rewrite from scratch: keep only the trailing portion of the old
        // content so the newest crashes survive a panic loop.
        let mut rewritten = String::new();
        if current_len > 0 {
            if let Ok(existing) = std::fs::read_to_string(path) {
                let keep_start = existing.len().saturating_sub((MAX_LOG_BYTES as usize) / 2);
                // Align to a line boundary so retained records stay parseable.
                let boundary = existing[keep_start..]
                    .find('\n')
                    .map(|offset| keep_start + offset + 1)
                    .unwrap_or(existing.len());
                rewritten.push_str(&existing[boundary..]);
            }
        }
        file.set_len(0)?;
        file.write_all(rewritten.as_bytes())?;
    }
    file.write_all(bytes)
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{append_bounded, MAX_LOG_BYTES};
    use std::io::Read as _;

    #[test]
    fn append_bounded_truncates_oldest_content_after_overflow() {
        let path = std::env::temp_dir().join(format!("cosh-crash-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // Seed at the exact limit so the appended line triggers truncation.
        let seed = "x".repeat(MAX_LOG_BYTES as usize);
        let _ = std::fs::write(&path, seed.as_bytes());

        append_bounded(&path, b"new\n").expect("append");

        let mut content = String::new();
        std::fs::File::open(&path)
            .expect("open")
            .read_to_string(&mut content)
            .expect("read");
        assert!(content.len() < MAX_LOG_BYTES as usize, "{}", content.len());
        assert!(content.ends_with("new\n"), "{content}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn append_bounded_creates_parent_directory() {
        let root = std::env::temp_dir().join(format!("cosh-crash-dir-test-{}", std::process::id()));
        let path = root.join("sub").join("crash.log");
        let _ = std::fs::remove_dir_all(&root);

        append_bounded(&path, b"line\n").expect("append");
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
