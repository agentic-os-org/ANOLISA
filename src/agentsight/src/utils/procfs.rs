// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
// Copyright (c) 2025 AgentSight Project
//
// Which procfs do pid-keyed lookups resolve through?
//
// Everything AgentSight learns about a process by path -- cmdline, comm, exe,
// maps, cgroup, and the `<pid>/root` paths uprobes attach to -- goes through the
// root handed out here. Funnelling it through one place is what lets an observer
// running inside a container be pointed at a bind-mounted host procfs
// (`/logtail_host/proc`) instead of its own `/proc`. A DaemonSet can then resolve
// processes living in *other* pods without `hostPID: true`, the same way the rest
// of a collector container reads the host through a path prefix.
//
// Three kinds of reads deliberately do *not* come from here:
//
//   * `/proc/self/...` -- questions about *ourselves* have to be answered by our
//     own procfs no matter where pid lookups point. See `probes::pidns`.
//   * `/proc/net/...`, `/proc/uptime` -- not pid-keyed. The former is
//     network-namespace scoped and follows the observer's netns; the latter is
//     identical through either root.
//   * anything backing an **action** on a process -- the enforcement and
//     containment paths (`enforcement::target`, `server::enforcement`,
//     `agentsight-enforcer`) validate a pid via `stat` and then `kill()` it.
//     Signals resolve in the *sender's* pid namespace, so validating against a
//     foreign procfs while signalling in our own could confirm one process and
//     kill a different one that happens to share the number. Those reads stay on
//     `/proc` so they remain self-consistent with the syscall that follows: they
//     fail closed instead of acting on the wrong target.
//
// The root is process-global rather than threaded through call signatures
// because it is fixed for the process lifetime and read from ~20 leaf sites that
// otherwise have no access to configuration.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Procfs mount point used when nothing overrides it.
pub const DEFAULT_PROC_ROOT: &str = "/proc";

static PROC_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Point pid-keyed lookups at `root`, e.g. `/logtail_host/proc`.
///
/// Must be called during startup, before any probe is loaded: the BPF
/// `observer_pidns_is_init` rodata flag is derived from this root (see
/// `probes::pidns`) and is baked into each skeleton between `open()` and
/// `load()`, so a later change would leave the flag disagreeing with the paths
/// we read.
///
/// Returns `false` when a root was already established, in which case the
/// existing one stays in effect and the caller should treat it as a
/// misconfiguration rather than retrying.
pub fn set_proc_root(root: impl Into<PathBuf>) -> bool {
    PROC_ROOT.set(root.into()).is_ok()
}

/// The procfs root that pid-keyed lookups resolve through.
///
/// Defaults to [`DEFAULT_PROC_ROOT`] when [`set_proc_root`] was never called,
/// which keeps every existing deployment on its own `/proc`.
pub fn proc_root() -> &'static Path {
    PROC_ROOT
        .get_or_init(|| PathBuf::from(DEFAULT_PROC_ROOT))
        .as_path()
}

/// Establishes the procfs root for this process and reports what it implies.
///
/// A wrong root degrades discovery rather than stopping the collector, so an
/// unusable one is warned about and kept: failing to start would take the
/// working half of the pipeline (already-attached uprobes, SSL events) down with
/// it. The "no pid 1" check catches the common misconfiguration -- a prefix that
/// exists but has no procfs mounted under it -- while it is still cheap to
/// diagnose from the log.
pub fn configure(root: &Path) {
    if !set_proc_root(root) {
        if proc_root() != root {
            log::warn!(
                "procfs root already set to {:?}; ignoring later request for {root:?}",
                proc_root()
            );
        }
        return;
    }
    if root == Path::new(DEFAULT_PROC_ROOT) {
        log::debug!("pid lookups resolve through {root:?} (default)");
    } else if root.join("1").is_dir() {
        log::info!("pid lookups resolve through {root:?}");
    } else {
        log::warn!(
            "procfs root {root:?} has no pid 1 entry; process discovery will find \
             nothing -- is a procfs mounted there?"
        );
    }
}

/// `<root>/<pid>`.
pub fn proc_pid(pid: impl fmt::Display) -> PathBuf {
    pid_dir(proc_root(), pid)
}

/// `<root>/<pid>/<entry>`, e.g. `proc_pid_entry(pid, "cmdline")`.
///
/// `entry` may contain separators (`"ns/pid"`) but must stay relative.
pub fn proc_pid_entry(pid: impl fmt::Display, entry: &str) -> PathBuf {
    pid_dir(proc_root(), pid).join(entry)
}

/// `<root>/<pid>/root<path>` -- the target's own view of an absolute `path`.
///
/// The kernel resolves this through the target process's mount namespace, which
/// is what makes a library inside another container reachable, and it stays
/// correct when the observer reads a bind-mounted host procfs.
pub fn proc_pid_rooted(pid: impl fmt::Display, path: &str) -> PathBuf {
    rooted(proc_root(), pid, path)
}

/// Split out from the public helpers so composition is testable against an
/// arbitrary root without touching the process-global one.
fn pid_dir(root: &Path, pid: impl fmt::Display) -> PathBuf {
    root.join(pid.to_string())
}

/// Appends `/root` and then `path` textually.
///
/// `Path::join` would be wrong here: joining an absolute `path` discards
/// everything to its left, turning `<root>/<pid>/root` + `/usr/lib/libssl.so`
/// into plain `/usr/lib/libssl.so` -- the observer's own copy of the library
/// rather than the target's.
fn rooted(root: &Path, pid: impl fmt::Display, path: &str) -> PathBuf {
    let mut joined = pid_dir(root, pid).into_os_string();
    joined.push("/root");
    joined.push(path);
    PathBuf::from(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_root_is_proc() {
        // The default has to stay `/proc`: every deployment that does not
        // configure a root keeps reading its own procfs.
        assert_eq!(Path::new(DEFAULT_PROC_ROOT), Path::new("/proc"));
        assert_eq!(
            pid_dir(Path::new(DEFAULT_PROC_ROOT), 42u32),
            PathBuf::from("/proc/42")
        );
    }

    #[test]
    fn pid_entries_compose_under_any_root() {
        let root = Path::new("/logtail_host/proc");
        assert_eq!(pid_dir(root, 7u32), PathBuf::from("/logtail_host/proc/7"));
        assert_eq!(
            pid_dir(root, 7u32).join("cmdline"),
            PathBuf::from("/logtail_host/proc/7/cmdline")
        );
        // Multi-segment entries are used for `ns/pid`.
        assert_eq!(
            pid_dir(root, 1u32).join("ns/pid"),
            PathBuf::from("/logtail_host/proc/1/ns/pid")
        );
    }

    #[test]
    fn negative_and_wide_pids_render_verbatim() {
        // Call sites carry pids as both `i32` and `u32`; formatting must not
        // silently reinterpret them.
        assert_eq!(
            pid_dir(Path::new("/proc"), -1i32),
            PathBuf::from("/proc/-1")
        );
        assert_eq!(
            pid_dir(Path::new("/proc"), 4_194_304u32),
            PathBuf::from("/proc/4194304")
        );
    }

    // The final assertion deliberately feeds join() an absolute right-hand side to
    // *demonstrate* the footgun that rooted() exists to avoid -- the lint exists to
    // stop accidental uses, which is precisely not what this line is.
    #[allow(clippy::join_absolute_paths)]
    #[test]
    fn rooted_keeps_the_pid_prefix_for_absolute_paths() {
        // The regression this guards: `Path::join` with an absolute path drops
        // the left-hand side, which would attach a uprobe to the observer's own
        // library instead of the target's.
        assert_eq!(
            rooted(Path::new("/proc"), 123i32, "/usr/lib64/libssl.so.1.1.1k"),
            PathBuf::from("/proc/123/root/usr/lib64/libssl.so.1.1.1k")
        );
        assert_eq!(
            rooted(
                Path::new("/logtail_host/proc"),
                123i32,
                "/usr/lib/libssl.so.3"
            ),
            PathBuf::from("/logtail_host/proc/123/root/usr/lib/libssl.so.3")
        );
        // Kept message-free on purpose: assert messages are only evaluated on
        // failure, which would leave the line permanently uncovered.
        assert_eq!(
            Path::new("/proc/123/root").join("/usr/lib/libssl.so"),
            PathBuf::from("/usr/lib/libssl.so")
        );
    }

    #[test]
    fn public_accessors_follow_the_current_root() {
        // Read-only against the process-global root, whatever it is in this
        // test binary: the accessors must compose under it.
        let root = proc_root();
        assert_eq!(proc_pid(42u32), root.join("42"));
        assert_eq!(proc_pid_entry(42u32, "cmdline"), root.join("42/cmdline"));
        assert_eq!(proc_pid_entry(42u32, "ns/pid"), root.join("42/ns/pid"));

        let mut expected = root.join("42").into_os_string();
        expected.push("/root/usr/lib/libssl.so");
        assert_eq!(
            proc_pid_rooted(42u32, "/usr/lib/libssl.so"),
            PathBuf::from(expected)
        );
    }

    #[test]
    fn proc_root_falls_back_to_the_default() {
        // Whatever ran first in this test binary, the accessor must yield a
        // usable root rather than panicking or returning an empty path.
        let root = proc_root();
        assert!(root.is_absolute());
        assert!(!root.as_os_str().is_empty());
    }
}
