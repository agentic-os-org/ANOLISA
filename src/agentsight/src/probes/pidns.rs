// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
// Copyright (c) 2025 AgentSight Project
//
// Which pid namespace numbers the `/proc` we resolve pids against?
//
// Every pid that comes out of a BPF event is eventually resolved against a procfs
// -- cmdline, exe, maps, cgroup, and the `<pid>/root` paths uprobes attach to. A
// procfs numbers processes in the pid namespace it was mounted for, so BPF has to
// report the pid in *that* namespace, not the target's innermost one. The BPF side
// needs one bit to decide that (see `current_observer_pid()` in
// `src/bpf/common.h`), and this module computes it.
//
// The bit is "is that namespace the initial one". That is the only case where the
// namespace is an ancestor of every process on the box, so every task has a pid we
// can resolve -- its host pid. Otherwise the only processes we can see are the ones
// sharing the namespace, whose innermost-namespace pid is already the number the
// procfs shows, which is the historical behaviour.
//
// Note what the question is *not*: it is not "which namespace are we in". Those
// answers coincide only while we read our own `/proc`. An observer that reads a
// bind-mounted host procfs (see `utils::procfs`) sits in its own namespace while
// the paths it reads are numbered in the initial one -- asking about ourselves
// there yields exactly the mismatch this bit exists to prevent.

use std::os::unix::fs::MetadataExt;
use std::sync::OnceLock;

use crate::utils::procfs::{DEFAULT_PROC_ROOT, proc_pid_entry, proc_root};

/// Inode number of the initial pid namespace. Fixed by the kernel as
/// `PROC_PID_INIT_INO` (`include/linux/proc_ns.h`) and part of the `/proc` ABI,
/// which is what makes comparing against it a stable check.
const PROC_PID_INIT_INO: u64 = 0xEFFF_FFFC;

/// Path whose inode identifies *our own* pid namespace.
///
/// Deliberately not routed through the configured procfs root: `self` there would
/// resolve to our entry in a foreign procfs, answering a different question.
const SELF_PIDNS: &str = "/proc/self/ns/pid";

/// Whether the procfs we resolve pids against is numbered in the initial pid
/// namespace.
///
/// Cached: neither the procfs root nor our own namespace changes for our lifetime.
///
/// Probes copy this into their BPF `observer_pidns_is_init` rodata flag between
/// `open()` and `load()`.
pub fn proc_root_is_init_pidns() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| {
        // pid 1 in a procfs is the init process of the namespace that procfs is
        // numbered for, so its pid namespace *is* that namespace.
        let root_pidns = proc_pid_entry(1, "ns/pid");
        let root_ino = match std::fs::metadata(&root_pidns) {
            Ok(md) => Some(md.ino()),
            Err(e) => {
                // Reading another task's namespace link needs privilege, and
                // `hidepid=2` hides it outright.
                log::warn!(
                    "failed to stat {} ({e}); falling back to our own pid namespace",
                    root_pidns.display()
                );
                None
            }
        };
        let self_ino = std::fs::metadata(SELF_PIDNS).map(|md| md.ino()).ok();
        let root_is_default = proc_root() == std::path::Path::new(DEFAULT_PROC_ROOT);

        let is_init = decide(root_ino, root_is_default, self_ino);
        log::debug!(
            "procfs root {:?} (pid-1 pidns inode {:?}, own {:?}); event pids will be {}",
            proc_root(),
            root_ino,
            self_ino,
            if is_init {
                "host pids"
            } else {
                "innermost-namespace pids"
            }
        );
        is_init
    })
}

/// The decision itself, split out from the IO so every branch is testable.
///
/// `root_ino` is the pid namespace inode of pid 1 in the configured procfs, when
/// it could be read. The fallbacks matter more than the happy path:
///
/// * Reading our own `/proc` -- our namespace is the one that numbers it, so the
///   historical self-comparison is exactly right.
/// * Reading a configured foreign procfs we cannot probe -- assume the operator
///   pointed us at a host procfs deliberately. Guessing "not init" instead would
///   resolve host-pid-numbered paths with namespace pids, i.e. silently attach to
///   whatever unrelated process happens to own that number; guessing "init" costs
///   at worst a failed lookup, because a root we cannot read yields nothing.
fn decide(root_ino: Option<u64>, root_is_default: bool, self_ino: Option<u64>) -> bool {
    match root_ino {
        Some(ino) => is_init_pidns_ino(ino),
        None if root_is_default => self_ino.is_some_and(is_init_pidns_ino),
        None => true,
    }
}

/// Whether a pid-namespace inode number is the initial namespace's.
///
/// Split out from [`proc_root_is_init_pidns`] so the comparison is testable
/// without depending on which namespace the test runner happens to be in.
fn is_init_pidns_ino(ino: u64) -> bool {
    ino == PROC_PID_INIT_INO
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_pidns_inode_is_recognized() {
        assert!(is_init_pidns_ino(PROC_PID_INIT_INO));
        // The documented value, spelled out so a typo in the constant fails here.
        assert!(is_init_pidns_ino(4_026_531_836));
    }

    #[test]
    fn other_pidns_inodes_are_rejected() {
        // Namespaces created after boot get ordinary nsfs inode numbers well
        // below the initial-namespace magic value.
        assert!(!is_init_pidns_ino(4_026_532_000));
        assert!(!is_init_pidns_ino(PROC_PID_INIT_INO - 1));
        assert!(!is_init_pidns_ino(PROC_PID_INIT_INO + 1));
        assert!(!is_init_pidns_ino(0));
    }

    #[test]
    fn pid_1_namespace_decides_when_readable() {
        // The procfs root's own numbering wins regardless of where we sit: this
        // is the case an observer reading a bind-mounted host procfs relies on.
        assert!(decide(Some(PROC_PID_INIT_INO), false, Some(4_026_532_000)));
        assert!(!decide(Some(4_026_532_000), true, Some(PROC_PID_INIT_INO)));
    }

    #[test]
    fn unreadable_default_root_keeps_the_historical_answer() {
        // Equivalent to the old self-only check, including its None => false.
        assert!(decide(None, true, Some(PROC_PID_INIT_INO)));
        assert!(!decide(None, true, Some(4_026_532_000)));
        assert!(!decide(None, true, None));
    }

    #[test]
    fn unreadable_configured_root_assumes_host_pids() {
        // Fail towards "nothing resolves" rather than "resolves to the wrong
        // process": a configured root we cannot read produces no lookups anyway.
        assert!(decide(None, false, Some(4_026_532_000)));
        assert!(decide(None, false, None));
    }

    #[test]
    fn observer_lookup_is_consistent_with_its_inputs() {
        // Whichever namespace and root the test runner has, the cached lookup
        // must agree with what the pure decision says about the same inputs.
        let root_ino = std::fs::metadata(proc_pid_entry(1, "ns/pid"))
            .map(|md| md.ino())
            .ok();
        let self_ino = std::fs::metadata(SELF_PIDNS).map(|md| md.ino()).ok();
        let root_is_default = proc_root() == std::path::Path::new(DEFAULT_PROC_ROOT);
        assert_eq!(
            proc_root_is_init_pidns(),
            decide(root_ino, root_is_default, self_ino)
        );
    }

    #[test]
    fn self_pidns_inode_matches_the_symlink_target() {
        // Guards the assumption that stat()ing the magic symlink yields the
        // nsfs inode, i.e. the number inside "pid:[...]" -- that identity is
        // what makes the init comparison meaningful.
        let link = std::fs::read_link(SELF_PIDNS).expect("read_link /proc/self/ns/pid");
        let text = link.to_string_lossy();
        let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
        let from_link: u64 = digits.parse().expect("inode digits in pid:[...]");
        let from_stat = std::fs::metadata(SELF_PIDNS).expect("stat").ino();
        assert_eq!(from_link, from_stat);
    }
}
