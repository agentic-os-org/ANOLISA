//! Run-registry entry for persistent cosh-core processes.
//!
//! One single-line JSON entry under `~/.copilot-shell/run/core-<pid>.json`,
//! written once the session is initialized and removed on clean shutdown.
//! Non-zero exits intentionally skip the removal: the surviving entry is the
//! evidence `cosh-shell doctor` reports for an abnormal exit. One-shot
//! invocations (--registry, --compact, single-turn) never reach this path.

use std::fs;
use std::path::PathBuf;

/// Registry root: `~/.copilot-shell/run/`.
fn run_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(".copilot-shell/run"))
}

fn entry_path() -> Option<PathBuf> {
    run_dir().map(|dir| dir.join(format!("core-{}.json", std::process::id())))
}

/// Writes the entry for this persistent process (atomic tmp+rename, silent on
/// failure). Called after `SessionRuntime::initialize` so `session_id` is
/// known; the owning shell passes its pid via `COSH_SHELL_PID`.
pub(crate) fn write_entry(mode: &str, session_id: &str) {
    let Some(path) = entry_path() else {
        return;
    };
    let Some(dir) = path.parent() else {
        return;
    };
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let entry = serde_json::json!({
        "kind": "core",
        "pid": std::process::id(),
        "version": env!("CARGO_PKG_VERSION"),
        "start_ts_ms": now_ms(),
        "mode": mode,
        "session_id": session_id,
        "owner_shell_pid": std::env::var("COSH_SHELL_PID")
            .ok()
            .and_then(|value| value.parse::<u32>().ok()),
    });
    let Ok(line) = serde_json::to_string(&entry) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, line.as_bytes()).is_err() {
        return;
    }
    let _ = fs::rename(&tmp, &path);
}

/// Removes the entry on clean shutdown (idempotent; best-effort).
pub(crate) fn remove_entry() {
    if let Some(path) = entry_path() {
        let _ = fs::remove_file(path);
    }
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}
