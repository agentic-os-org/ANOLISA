//! In-process Linux sandbox primitives: `no_new_privs` and seccomp.
//!
//! Filesystem restrictions are enforced by bubblewrap in `linux_run_main`.
use std::collections::BTreeMap;

use crate::error::Result;
use crate::error::SandboxError;
use crate::policy::NetworkSandboxPolicy;

use seccompiler::BpfProgram;
use seccompiler::SeccompAction;
use seccompiler::SeccompCmpArgLen;
use seccompiler::SeccompCmpOp;
use seccompiler::SeccompCondition;
use seccompiler::SeccompFilter;
use seccompiler::SeccompRule;
use seccompiler::TargetArch;
use seccompiler::apply_filter;

/// Apply sandbox policies inside this thread so only the child inherits
/// them, not the entire CLI process.
///
/// This function is responsible for:
/// - enabling `PR_SET_NO_NEW_PRIVS` when restrictions apply, and
/// - installing the network seccomp filter when network access is disabled.
///
/// Filesystem restrictions are intentionally handled by bubblewrap.
pub(crate) fn apply_sandbox_policy_to_current_thread(
    network_sandbox_policy: NetworkSandboxPolicy,
    allow_network_for_proxy: bool,
    proxy_routed_network: bool,
) -> Result<()> {
    let network_seccomp_mode = network_seccomp_mode(
        network_sandbox_policy,
        allow_network_for_proxy,
        proxy_routed_network,
    );

    // `PR_SET_NO_NEW_PRIVS` is required for seccomp.
    if network_seccomp_mode.is_some() {
        set_no_new_privs()?;
    }

    if let Some(mode) = network_seccomp_mode {
        install_network_seccomp_filter_on_current_thread(mode)?;
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetworkSeccompMode {
    Restricted,
    ProxyRouted,
}

fn should_install_network_seccomp(
    network_sandbox_policy: NetworkSandboxPolicy,
    allow_network_for_proxy: bool,
) -> bool {
    // Managed-network sessions should remain fail-closed even for policies that
    // would normally grant full network access (for example, DangerFullAccess).
    !network_sandbox_policy.is_enabled() || allow_network_for_proxy
}

fn network_seccomp_mode(
    network_sandbox_policy: NetworkSandboxPolicy,
    allow_network_for_proxy: bool,
    proxy_routed_network: bool,
) -> Option<NetworkSeccompMode> {
    if !should_install_network_seccomp(network_sandbox_policy, allow_network_for_proxy) {
        None
    } else if proxy_routed_network {
        Some(NetworkSeccompMode::ProxyRouted)
    } else {
        Some(NetworkSeccompMode::Restricted)
    }
}

/// Enable `PR_SET_NO_NEW_PRIVS` so seccomp can be applied safely.
fn set_no_new_privs() -> Result<()> {
    let result = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

/// Installs a seccomp filter for Linux network sandboxing.
///
/// The filter is applied to the current thread so only the sandboxed child
/// inherits it.
fn install_network_seccomp_filter_on_current_thread(
    mode: NetworkSeccompMode,
) -> std::result::Result<(), SandboxError> {
    fn deny_syscall(rules: &mut BTreeMap<i64, Vec<SeccompRule>>, nr: i64) {
        rules.insert(nr, vec![]); // empty rule vec = unconditional match
    }

    // Build rule map.
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();

    deny_syscall(&mut rules, libc::SYS_ptrace);
    deny_syscall(&mut rules, libc::SYS_io_uring_setup);
    deny_syscall(&mut rules, libc::SYS_io_uring_enter);
    deny_syscall(&mut rules, libc::SYS_io_uring_register);

    match mode {
        NetworkSeccompMode::Restricted => {
            deny_syscall(&mut rules, libc::SYS_connect);
            deny_syscall(&mut rules, libc::SYS_accept);
            deny_syscall(&mut rules, libc::SYS_accept4);
            deny_syscall(&mut rules, libc::SYS_bind);
            deny_syscall(&mut rules, libc::SYS_listen);
            deny_syscall(&mut rules, libc::SYS_getpeername);
            deny_syscall(&mut rules, libc::SYS_getsockname);
            deny_syscall(&mut rules, libc::SYS_shutdown);
            deny_syscall(&mut rules, libc::SYS_sendto);
            deny_syscall(&mut rules, libc::SYS_sendmmsg);
            // NOTE: allowing recvfrom allows some tools like: `cargo clippy`
            // to run with their socketpair + child processes for sub-proc
            // management.
            // deny_syscall(&mut rules, libc::SYS_recvfrom);
            deny_syscall(&mut rules, libc::SYS_recvmmsg);
            deny_syscall(&mut rules, libc::SYS_getsockopt);
            deny_syscall(&mut rules, libc::SYS_setsockopt);

            // For `socket` we allow AF_UNIX (arg0 == AF_UNIX) and deny
            // everything else.
            let unix_only_rule = SeccompRule::new(vec![SeccompCondition::new(
                0, // first argument (domain)
                SeccompCmpArgLen::Dword,
                SeccompCmpOp::Ne,
                libc::AF_UNIX as u64,
            )?])?;

            rules.insert(libc::SYS_socket, vec![unix_only_rule.clone()]);
            rules.insert(libc::SYS_socketpair, vec![unix_only_rule]);
        }
        NetworkSeccompMode::ProxyRouted => {
            // In proxy-routed mode we allow IP sockets in the isolated
            // namespace (used to reach the local TCP bridge) but deny all
            // other socket families, including AF_UNIX. This prevents
            // bypassing the routed bridge via new Unix sockets and narrows the
            // socket surface in proxy-only mode.
            let deny_non_ip_socket = SeccompRule::new(vec![
                SeccompCondition::new(
                    0,
                    SeccompCmpArgLen::Dword,
                    SeccompCmpOp::Ne,
                    libc::AF_INET as u64,
                )?,
                SeccompCondition::new(
                    0,
                    SeccompCmpArgLen::Dword,
                    SeccompCmpOp::Ne,
                    libc::AF_INET6 as u64,
                )?,
            ])?;
            let deny_unix_socketpair = SeccompRule::new(vec![SeccompCondition::new(
                0,
                SeccompCmpArgLen::Dword,
                SeccompCmpOp::Eq,
                libc::AF_UNIX as u64,
            )?])?;
            rules.insert(libc::SYS_socket, vec![deny_non_ip_socket]);
            rules.insert(libc::SYS_socketpair, vec![deny_unix_socketpair]);
        }
    }

    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,                     // default – allow
        SeccompAction::Errno(libc::EPERM as u32), // when rule matches – return EPERM
        if cfg!(target_arch = "x86_64") {
            TargetArch::x86_64
        } else if cfg!(target_arch = "aarch64") {
            TargetArch::aarch64
        } else {
            unimplemented!("unsupported architecture for seccomp filter");
        },
    )?;

    let prog: BpfProgram = filter.try_into()?;

    apply_filter(&prog)?;

    Ok(())
}

/// Install the strict entrypoint's baseline without weakening full-network mode.
///
/// Unix socket pairs remain usable for child-process IPC, but connecting to host
/// Unix sockets, creating namespaces, or changing mounts is never permitted.
pub(crate) fn apply_strict_restrictions(network_enabled: bool) -> Result<()> {
    set_no_new_privs()?;
    let mut rules = BTreeMap::new();
    for number in [
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_chroot,
        libc::SYS_setns,
        libc::SYS_unshare,
        libc::SYS_open_tree,
        libc::SYS_move_mount,
        libc::SYS_mount_setattr,
        libc::SYS_fsopen,
        libc::SYS_fsconfig,
        libc::SYS_fsmount,
        libc::SYS_fspick,
        libc::SYS_bpf,
        libc::SYS_perf_event_open,
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_linkat,
    ] {
        rules.insert(number, vec![]);
    }
    #[cfg(target_arch = "x86_64")]
    rules.insert(libc::SYS_link, vec![]);

    let namespace_flags = [
        libc::CLONE_NEWUSER,
        libc::CLONE_NEWNS,
        libc::CLONE_NEWNET,
        libc::CLONE_NEWPID,
        libc::CLONE_NEWIPC,
        libc::CLONE_NEWUTS,
        libc::CLONE_NEWCGROUP,
    ];
    let clone_rules = namespace_flags
        .into_iter()
        .map(|flag| {
            SeccompRule::new(vec![SeccompCondition::new(
                0,
                SeccompCmpArgLen::Dword,
                SeccompCmpOp::MaskedEq(flag as u64),
                flag as u64,
            )?])
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    rules.insert(libc::SYS_clone, clone_rules);
    let socket_conditions = if network_enabled {
        vec![
            SeccompCondition::new(
                0,
                SeccompCmpArgLen::Dword,
                SeccompCmpOp::Ne,
                libc::AF_INET as u64,
            )?,
            SeccompCondition::new(
                0,
                SeccompCmpArgLen::Dword,
                SeccompCmpOp::Ne,
                libc::AF_INET6 as u64,
            )?,
        ]
    } else {
        // A socketpair is sufficient for sandbox-local IPC; a socket could
        // reach a host abstract Unix endpoint when the host netns is shared.
        Vec::new()
    };
    rules.insert(
        libc::SYS_socket,
        if socket_conditions.is_empty() {
            vec![]
        } else {
            vec![SeccompRule::new(socket_conditions)?]
        },
    );
    // A datagram socketpair can sendmsg to a host pathname Unix socket.
    // Connected stream/seqpacket pairs cannot select a different peer.
    // Rust uses seqpacket pairs to report errors during Command::spawn.
    let connected_types = [
        libc::SOCK_STREAM,
        libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
        libc::SOCK_STREAM | libc::SOCK_NONBLOCK,
        libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
        libc::SOCK_SEQPACKET,
        libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
        libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK,
        libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
    ];
    let disallowed_type = connected_types
        .into_iter()
        .map(|kind| {
            SeccompCondition::new(1, SeccompCmpArgLen::Dword, SeccompCmpOp::Ne, kind as u64)
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    rules.insert(
        libc::SYS_socketpair,
        vec![
            SeccompRule::new(vec![SeccompCondition::new(
                0,
                SeccompCmpArgLen::Dword,
                SeccompCmpOp::Ne,
                libc::AF_UNIX as u64,
            )?])?,
            SeccompRule::new(disallowed_type)?,
        ],
    );
    if !network_enabled {
        for number in [
            libc::SYS_connect,
            libc::SYS_bind,
            libc::SYS_listen,
            libc::SYS_accept,
            libc::SYS_accept4,
        ] {
            rules.insert(number, vec![]);
        }
    }
    rules.insert(
        libc::SYS_ioctl,
        [libc::TIOCSTI, 0x541c]
            .into_iter()
            .map(|request| {
                SeccompRule::new(vec![SeccompCondition::new(
                    1,
                    SeccompCmpArgLen::Dword,
                    SeccompCmpOp::Eq,
                    request,
                )?])
            })
            .collect::<std::result::Result<Vec<_>, _>>()?,
    );
    install_strict_filter(rules, libc::EPERM)?;
    // libc falls back to clone when clone3 is unavailable; its pointer-based
    // flags cannot be inspected safely by seccomp.
    install_strict_filter(BTreeMap::from([(libc::SYS_clone3, vec![])]), libc::ENOSYS)?;
    #[cfg(target_arch = "x86_64")]
    block_x32_syscalls()?;
    Ok(())
}

#[cfg(target_arch = "x86_64")]
fn block_x32_syscalls() -> Result<()> {
    // x32 shares AUDIT_ARCH_X86_64 but adds this bit to syscall numbers. The
    // native deny table must not default-allow that alternate ABI.
    let program = [
        seccompiler::sock_filter {
            code: 0x20,
            jt: 0,
            jf: 0,
            k: 0,
        }, // LD nr
        seccompiler::sock_filter {
            code: 0x45,
            jt: 0,
            jf: 1,
            k: 0x4000_0000,
        }, // JSET x32
        seccompiler::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ERRNO | libc::EPERM as u32,
        },
        seccompiler::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ALLOW,
        },
    ];
    apply_filter(&program)?;
    Ok(())
}

fn install_strict_filter(rules: BTreeMap<i64, Vec<SeccompRule>>, errno: i32) -> Result<()> {
    let arch = if cfg!(target_arch = "x86_64") {
        TargetArch::x86_64
    } else if cfg!(target_arch = "aarch64") {
        TargetArch::aarch64
    } else {
        return Err(SandboxError::UnsupportedOperation(
            "strict seccomp architecture".into(),
        ));
    };
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(errno as u32),
        arch,
    )?;
    let program: BpfProgram = filter.try_into()?;
    apply_filter(&program)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::NetworkSeccompMode;
    use super::network_seccomp_mode;
    use super::should_install_network_seccomp;
    use crate::policy::NetworkSandboxPolicy;
    use pretty_assertions::assert_eq;

    #[test]
    fn managed_network_enforces_seccomp_even_for_full_network_policy() {
        assert_eq!(
            should_install_network_seccomp(NetworkSandboxPolicy::Enabled, true),
            true
        );
    }

    #[test]
    fn full_network_policy_without_managed_network_skips_seccomp() {
        assert_eq!(
            should_install_network_seccomp(NetworkSandboxPolicy::Enabled, false),
            false
        );
    }

    #[test]
    fn restricted_network_policy_always_installs_seccomp() {
        assert!(should_install_network_seccomp(
            NetworkSandboxPolicy::Restricted,
            false
        ));
        assert!(should_install_network_seccomp(
            NetworkSandboxPolicy::Restricted,
            true
        ));
    }

    #[test]
    fn managed_proxy_routes_use_proxy_routed_seccomp_mode() {
        assert_eq!(
            network_seccomp_mode(NetworkSandboxPolicy::Enabled, true, true),
            Some(NetworkSeccompMode::ProxyRouted)
        );
    }

    #[test]
    fn restricted_network_without_proxy_routing_uses_restricted_mode() {
        assert_eq!(
            network_seccomp_mode(NetworkSandboxPolicy::Restricted, false, false),
            Some(NetworkSeccompMode::Restricted)
        );
    }

    #[test]
    fn full_network_without_managed_proxy_skips_network_seccomp_mode() {
        assert_eq!(
            network_seccomp_mode(NetworkSandboxPolicy::Enabled, false, false),
            None
        );
    }
}
