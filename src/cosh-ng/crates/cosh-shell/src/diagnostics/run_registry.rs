//! Process-state registry for post-mortem diagnosis.
//!
//! A live session writes one single-line JSON entry under
//! `~/.copilot-shell/run/<kind>-<pid>.json` and removes it on clean shutdown.
//! Entries left behind by a crash or SIGKILL are the diagnostic evidence the
//! runtime collector and `cosh-shell doctor` report on, so every failure path
//! here degrades silently instead of disturbing the session.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Entries whose pid is dead and whose start time is older than this are
/// reaped at the next session startup; doctor only reports, never removes.
const STALE_ENTRY_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Routing facts snapshot carried by a shell entry.
///
/// Phase 1 carries only facts the host process observes itself: the effective
/// AI-enabled state, the integration mode, the assistance (routing) toggle,
/// and the latest marker generation reported by the child shell. CNF handler
/// ownership and recent route decisions live on the child-shell side and are
/// not yet propagated through `ShellEnvironmentSnapshot`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RoutingFacts {
    pub ai_enabled: bool,
    pub integration: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistance_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker_generation: Option<u64>,
}

/// One registry entry as stored on disk. Both kinds share the shape so the
/// runtime collector parses shell and core entries through one code path;
/// shell- or core-only fields stay `None` on the other kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RunEntry {
    pub kind: String,
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ppid: Option<u32>,
    pub version: String,
    pub start_ts_ms: u128,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_shell_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<RoutingFacts>,
}

/// Process-local state of this shell's own entry, so later routing/core-pid
/// changes rewrite the file instead of spawning duplicate entries.
struct ShellRegistryState {
    path: PathBuf,
    entry: RunEntry,
}

static SHELL_STATE: OnceLock<Mutex<Option<ShellRegistryState>>> = OnceLock::new();

fn shell_state() -> &'static Mutex<Option<ShellRegistryState>> {
    SHELL_STATE.get_or_init(|| Mutex::new(None))
}

/// Registry root: `~/.copilot-shell/run/`.
pub(crate) fn run_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| Path::new(&home).join(".copilot-shell/run"))
}

fn entry_path(dir: &Path, kind: &str, pid: u32) -> PathBuf {
    dir.join(format!("{kind}-{pid}.json"))
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

/// Writes the entry via a temp file + rename so a crash mid-write never leaves
/// a truncated JSON line for doctor to misread. Returns false on any failure.
fn atomic_write(path: &Path, entry: &RunEntry) -> bool {
    let Some(dir) = path.parent() else {
        return false;
    };
    if fs::create_dir_all(dir).is_err() {
        return false;
    }
    let Ok(line) = serde_json::to_string(entry) else {
        return false;
    };
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, line.as_bytes()).is_err() {
        return false;
    }
    fs::rename(&tmp, path).is_ok()
}

/// Publishes this session's entry. Called once per raw session before the
/// interactive loop; silent on failure.
pub(crate) fn record_shell(
    shell_kind: &str,
    session_id: &str,
    ai_enabled: bool,
    integration: &str,
    assistance_enabled: bool,
) {
    let Some(dir) = run_dir() else {
        return;
    };
    let pid = std::process::id();
    let entry = RunEntry {
        kind: "shell".to_string(),
        pid,
        ppid: Some(nix::unistd::getppid().as_raw() as u32),
        version: env!("CARGO_PKG_VERSION").to_string(),
        start_ts_ms: now_ms(),
        shell_kind: Some(shell_kind.to_string()),
        mode: None,
        session_id: Some(session_id.to_string()),
        core_pid: None,
        owner_shell_pid: None,
        routing: Some(RoutingFacts {
            ai_enabled,
            integration: integration.to_string(),
            assistance_enabled: Some(assistance_enabled),
            marker_generation: None,
        }),
    };
    let path = entry_path(&dir, "shell", pid);
    atomic_write(&path, &entry);
    if let Ok(mut state) = shell_state().lock() {
        *state = Some(ShellRegistryState { path, entry });
    }
}

fn rewrite(current: &ShellRegistryState) {
    atomic_write(&current.path, &current.entry);
}

fn update_routing(apply: impl FnOnce(&mut RoutingFacts)) {
    let Ok(mut state) = shell_state().lock() else {
        return;
    };
    let Some(current) = state.as_mut() else {
        return;
    };
    let facts = current
        .entry
        .routing
        .get_or_insert_with(RoutingFacts::default);
    apply(facts);
    rewrite(current);
}

/// Backfills the persistent core pid after a successful spawn, pairing the
/// shell entry with its core for the runtime collector.
pub(crate) fn update_core_pid(core_pid: u32) {
    let Ok(mut state) = shell_state().lock() else {
        return;
    };
    let Some(current) = state.as_mut() else {
        return;
    };
    if current.entry.core_pid == Some(core_pid) {
        return;
    }
    current.entry.core_pid = Some(core_pid);
    rewrite(current);
}

/// Records an assistance (routing) toggle from `/mode routing` or the
/// ESC-[Z shortcut.
pub(crate) fn update_assistance(enabled: bool) {
    update_routing(|facts| facts.assistance_enabled = Some(enabled));
}

/// Records the latest marker generation reported by the child shell.
pub(crate) fn update_marker_generation(generation: u64) {
    update_routing(|facts| facts.marker_generation = Some(generation));
}

/// Best-effort removal on clean shutdown (main return, signal path, panic
/// hook). Uses `try_lock` so it can never block a signal handler.
pub(crate) fn remove_shell() {
    let Ok(mut state) = shell_state().try_lock() else {
        return;
    };
    if let Some(current) = state.take() {
        let _ = fs::remove_file(&current.path);
    }
}

/// Removes dead-pid entries older than a week at session startup. Doctor
/// never calls this: stale entries are evidence until a new session reaps
/// them.
pub(crate) fn cleanup_stale() {
    let Some(dir) = run_dir() else {
        return;
    };
    let Ok(read) = fs::read_dir(&dir) else {
        return;
    };
    let now = now_ms();
    for item in read.flatten() {
        let path = item.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let Ok(entry) = serde_json::from_slice::<RunEntry>(&bytes) else {
            continue;
        };
        if !pid_alive(entry.pid)
            && now.saturating_sub(entry.start_ts_ms) > STALE_ENTRY_MAX_AGE.as_millis()
        {
            let _ = fs::remove_file(&path);
        }
    }
}

/// Reads every parseable entry in the run directory (single-level scan).
pub(crate) fn read_entries() -> Vec<RunEntry> {
    let Some(dir) = run_dir() else {
        return Vec::new();
    };
    let Ok(read) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for item in read.flatten() {
        let path = item.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        if let Ok(bytes) = fs::read(&path) {
            if let Ok(entry) = serde_json::from_slice::<RunEntry>(&bytes) {
                entries.push(entry);
            }
        }
    }
    entries
}

/// Whether a pid is currently alive (a zero-signal probe; EPERM still means
/// the process exists but belongs to someone else).
pub(crate) fn pid_alive(pid: u32) -> bool {
    if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok() {
        return true;
    }
    nix::errno::Errno::last() == nix::errno::Errno::EPERM
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::test_env::env_guard;

    fn temp_home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cosh-run-registry-test-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn set_home(home: &Path) {
        std::env::set_var("HOME", home);
    }

    fn write_fixture(home: &Path, entry: &RunEntry) -> PathBuf {
        let dir = home.join(".copilot-shell/run");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}-{}.json", entry.kind, entry.pid));
        fs::write(&path, serde_json::to_string(entry).unwrap()).unwrap();
        path
    }

    fn shell_entry(pid: u32, start_ts_ms: u128) -> RunEntry {
        RunEntry {
            kind: "shell".to_string(),
            pid,
            ppid: None,
            version: "test".to_string(),
            start_ts_ms,
            shell_kind: Some("zsh".to_string()),
            mode: None,
            session_id: Some("stale".to_string()),
            core_pid: None,
            owner_shell_pid: None,
            routing: None,
        }
    }

    #[test]
    fn record_remove_and_reread_roundtrip() {
        let _guard = env_guard();
        let home = temp_home("roundtrip");
        set_home(&home);
        remove_shell();
        record_shell("zsh", "session-1", true, "enhanced", true);
        let entries = read_entries();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.kind, "shell");
        assert_eq!(entry.pid, std::process::id());
        assert_eq!(entry.session_id.as_deref(), Some("session-1"));
        assert_eq!(entry.shell_kind.as_deref(), Some("zsh"));
        let facts = entry.routing.as_ref().unwrap();
        assert!(facts.ai_enabled);
        assert_eq!(facts.assistance_enabled, Some(true));
        remove_shell();
        assert!(read_entries().is_empty());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn routing_updates_rewrite_the_entry() {
        let _guard = env_guard();
        let home = temp_home("routing");
        set_home(&home);
        remove_shell();
        record_shell("bash", "session-2", false, "native", false);
        update_core_pid(4242);
        update_assistance(true);
        update_marker_generation(7);
        let entries = read_entries();
        let entry = entries
            .iter()
            .find(|entry| entry.kind == "shell")
            .expect("shell entry present");
        assert_eq!(entry.core_pid, Some(4242));
        let facts = entry.routing.as_ref().unwrap();
        assert_eq!(facts.assistance_enabled, Some(true));
        assert_eq!(facts.marker_generation, Some(7));
        remove_shell();
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn cleanup_stale_reaps_only_dead_entries_older_than_a_week() {
        let _guard = env_guard();
        let home = temp_home("stale");
        set_home(&home);
        let fresh_dead = shell_entry(999_999, now_ms());
        let old_dead = shell_entry(
            999_998,
            now_ms().saturating_sub(STALE_ENTRY_MAX_AGE.as_millis() + 1000),
        );
        write_fixture(&home, &fresh_dead);
        write_fixture(&home, &old_dead);
        cleanup_stale();
        let remaining = read_entries();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].pid, fresh_dead.pid);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn pid_alive_detects_the_current_process() {
        assert!(pid_alive(std::process::id()));
    }
}
