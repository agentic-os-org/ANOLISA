//! Seccomp-bpf syscall filtering for the ws-ckpt daemon.
//!
//! Applies a **deny-list** filter: all syscalls are allowed by default,
//! and a curated list of dangerous / irrelevant syscalls is blocked with EPERM.
//!
//! This approach is chosen because the daemon shells out to external commands
//! (btrfs, mount, rsync, systemctl) whose full syscall set is impractical to
//! enumerate in an allow-list.

use std::collections::BTreeMap;

use seccompiler::{
    apply_filter, BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition,
    SeccompFilter, SeccompRule, TargetArch,
};

/// Match the build target_arch to seccompiler's TargetArch so the BPF
/// audit_arch check passes at runtime. Returns Err on unsupported arches
/// (e.g. loongarch64) so the daemon refuses to install a no-op filter.
fn target_arch() -> anyhow::Result<TargetArch> {
    #[cfg(target_arch = "x86_64")]
    return Ok(TargetArch::x86_64);
    #[cfg(target_arch = "aarch64")]
    return Ok(TargetArch::aarch64);
    #[cfg(target_arch = "riscv64")]
    return Ok(TargetArch::riscv64);
    #[cfg(not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64"
    )))]
    anyhow::bail!(
        "seccomp filter not supported on target arch '{}'; refusing to install no-op filter",
        std::env::consts::ARCH
    );
}

/// Blocked syscalls — operations the daemon should never perform.
///
/// Excluded on purpose: new mount API (fsopen/fsmount/fsconfig/move_mount/
/// open_tree/mount_setattr), io_uring, and landlock. mount(8) and various
/// libraries probe these and only fall back on ENOSYS; returning EPERM would
/// break the daemon's own mount path or future tokio io_uring opt-in.
///
/// Categories:
/// - Kernel/module manipulation
/// - System state changes (reboot, hostname, clock)
/// - Debugging / tracing
/// - Swap management
/// - Accounting / profiling
/// - Key management
/// - Virtualization / namespaces
/// - pidfd / cross-process memory (daemon never inspects other processes)
fn blocked_syscalls() -> Vec<i64> {
    vec![
        // ── Kernel module loading ──
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_delete_module,
        // ── System state ──
        libc::SYS_reboot,
        libc::SYS_kexec_load,
        libc::SYS_kexec_file_load,
        libc::SYS_sethostname,
        libc::SYS_setdomainname,
        libc::SYS_settimeofday,
        libc::SYS_adjtimex,
        libc::SYS_clock_adjtime,
        libc::SYS_pivot_root,
        // ── Debugging / tracing ──
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_kcmp,
        // ── Swap management ──
        libc::SYS_swapon,
        libc::SYS_swapoff,
        // ── Accounting / profiling ──
        libc::SYS_acct,
        libc::SYS_perf_event_open,
        // ── Key management ──
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_keyctl,
        // ── Virtualization / namespaces (daemon must not create new namespaces) ──
        libc::SYS_unshare,
        libc::SYS_setns,
        // ── Misc dangerous ──
        libc::SYS_bpf, // prevent loading arbitrary BPF programs
        libc::SYS_userfaultfd,
        libc::SYS_move_pages,
        // ── pidfd / cross-process memory (5.x) ──
        libc::SYS_pidfd_open,
        libc::SYS_pidfd_send_signal,
        libc::SYS_pidfd_getfd,
        libc::SYS_process_madvise,
    ]
}

/// Create a rule that matches unconditionally (arg0 as u64 >= 0 is always true).
fn unconditional_rule() -> Result<SeccompRule, seccompiler::Error> {
    Ok(SeccompRule::new(vec![SeccompCondition::new(
        0,
        SeccompCmpArgLen::Qword,
        SeccompCmpOp::Ge,
        0,
    )?])?)
}

/// Apply the seccomp deny-list filter to the current thread (and future children).
///
/// Must be called **after** bootstrap (image creation, loop mount) completes,
/// but **before** the listener loop starts.
pub fn apply_seccomp_filter() -> anyhow::Result<()> {
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();

    for syscall in blocked_syscalls() {
        rules.insert(syscall, vec![unconditional_rule()?]);
    }

    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,                     // default: allow everything
        SeccompAction::Errno(libc::EPERM as u32), // blocked: return EPERM
        target_arch()?,
    )?;

    let bpf: BpfProgram = filter.try_into()?;
    apply_filter(&bpf)?;

    tracing::info!(
        "Seccomp filter applied: {} dangerous syscalls blocked",
        blocked_syscalls().len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_syscalls_not_empty() {
        assert!(!blocked_syscalls().is_empty());
    }

    #[test]
    fn blocked_syscalls_no_duplicates() {
        let list = blocked_syscalls();
        let mut sorted = list.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(list.len(), sorted.len(), "duplicate syscall numbers found");
    }

    #[test]
    fn filter_builds_for_target_arch() {
        let arch = target_arch().expect("test host arch should be supported");
        let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
        for syscall in blocked_syscalls() {
            rules.insert(syscall, vec![unconditional_rule().unwrap()]);
        }
        let filter = SeccompFilter::new(
            rules,
            SeccompAction::Allow,
            SeccompAction::Errno(libc::EPERM as u32),
            arch,
        );
        assert!(filter.is_ok(), "seccomp filter should build without errors");
    }
}
