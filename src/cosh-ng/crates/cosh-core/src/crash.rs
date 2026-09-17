//! Panic persistence for post-mortem diagnosis.
//!
//! cosh-core runs headless with its stderr piped to the shell host, so a panic
//! message may never reach a user. This module appends one JSON line per panic
//! to `~/.copilot-shell/cosh-core-crash.log` (top-level, so the shell's bundle
//! collector crashes source picks it up) for `cosh-shell doctor` and
//! `cosh-shell diagnostics export` to report.

use std::io::Write as _;
use std::panic::PanicHookInfo;

/// Upper bound for the crash log; older entries are dropped when exceeded so a
/// panic loop cannot fill the disk.
const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// Installs the crash-persisting panic hook, chaining to the previous hook.
///
/// Safe to call once at startup; every failure path in the hook is silent
/// because a panic inside the hook would abort without the default handler.
pub(crate) fn install_panic_hook() {
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        record_panic(info);
        prev_hook(info);
    }));
}

fn record_panic(info: &PanicHookInfo) {
    let message = panic_message(info);
    let location = info
        .location()
        .map(|location| format!("{}:{}", location.file(), location.line()));
    let record = serde_json::json!({
        "ts": now_ms(),
        "version": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(),
        "panic": message,
        "location": location,
        "backtrace": backtrace_summary(),
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

/// Captures at most the first few frames so the crash line stays compact while
/// still naming the originating module and function.
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
        .map(|home| std::path::Path::new(&home).join(".copilot-shell/cosh-core-crash.log"))
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
