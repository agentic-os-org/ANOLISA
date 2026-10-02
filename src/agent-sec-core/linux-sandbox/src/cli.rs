//! CLI entry point for the Linux sandbox helper.
//!
//! The sequence is:
//! 1. When needed, wrap the command with bubblewrap to construct the
//!    filesystem view.
//! 2. Apply in-process restrictions (no_new_privs + seccomp).
//! 3. `execvp` into the final command.

use clap::Parser;
use std::ffi::CString;
use std::fs::File;
use std::io::Read;
use std::os::fd::FromRawFd;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use crate::bwrap_args::BwrapNetworkMode;
use crate::bwrap_args::BwrapOptions;
use crate::bwrap_args::create_bwrap_command_args;
use crate::bwrap_args::exec_bwrap;
use crate::bwrap_args::run_bwrap_in_child;
use crate::policy::FileSystemSandboxPolicy;
use crate::policy::NetworkSandboxPolicy;
use crate::proxy::activate_proxy_routes_in_netns;
use crate::proxy::prepare_host_proxy_route_spec;
use crate::seccomp::apply_sandbox_policy_to_current_thread;

#[derive(Debug, Parser)]
/// CLI surface for the Linux sandbox helper.
pub struct SandboxCommand {
    /// The cwd used in the context of the sandbox policy,
    /// which may be different from the cwd of the process to spawn.
    #[arg(long = "sandbox-policy-cwd")]
    pub sandbox_policy_cwd: PathBuf,

    /// Filesystem sandbox policy (JSON format).
    #[arg(long = "file-system-sandbox-policy")]
    pub file_system_sandbox_policy: FileSystemSandboxPolicy,

    /// Network sandbox policy (JSON format).
    #[arg(long = "network-sandbox-policy")]
    pub network_sandbox_policy: NetworkSandboxPolicy,

    /// Internal: apply seccomp and `no_new_privs` in the already-sandboxed
    /// process, then exec the user command.
    ///
    /// This exists so we can run bubblewrap first (which may rely on setuid)
    /// and only tighten with seccomp after the filesystem view is established.
    #[arg(long = "apply-seccomp-then-exec", hide = true, default_value_t = false)]
    pub apply_seccomp_then_exec: bool,

    /// Internal compatibility flag.
    ///
    /// By default, restricted-network sandboxing uses isolated networking.
    /// If set, sandbox setup switches to proxy-only network mode with
    /// managed routing bridges.
    #[arg(long = "allow-network-for-proxy", hide = true, default_value_t = false)]
    pub allow_network_for_proxy: bool,

    /// Internal route spec used for managed proxy routing in bwrap mode.
    #[arg(long = "proxy-route-spec", hide = true)]
    pub proxy_route_spec: Option<String>,

    /// When set, skip mounting a fresh `/proc` even though PID isolation is
    /// still enabled. This is primarily intended for restrictive container
    /// environments that deny `--proc /proc`.
    #[arg(long = "no-proc", default_value_t = false)]
    pub no_proc: bool,

    /// Full command args to run under the Linux sandbox helper.
    #[arg(trailing_var_arg = true)]
    pub command: Vec<String>,
}

/// Entry point for the Linux sandbox helper.
///
/// The sequence is:
/// 1. When needed, wrap the command with bubblewrap to construct the
///    filesystem view.
/// 2. Apply in-process restrictions (no_new_privs + seccomp).
/// 3. `execvp` into the final command.
pub fn run_main() -> ! {
    let SandboxCommand {
        sandbox_policy_cwd,
        file_system_sandbox_policy,
        network_sandbox_policy,
        apply_seccomp_then_exec,
        allow_network_for_proxy,
        proxy_route_spec,
        no_proc,
        command,
    } = SandboxCommand::parse();

    if command.is_empty() {
        panic!("No command specified to execute.");
    }

    // Inner stage: apply seccomp/no_new_privs after bubblewrap has already
    // established the filesystem view.
    if apply_seccomp_then_exec {
        if allow_network_for_proxy {
            let spec = proxy_route_spec
                .as_deref()
                .unwrap_or_else(|| panic!("managed proxy mode requires --proxy-route-spec"));
            if let Err(err) = activate_proxy_routes_in_netns(spec) {
                panic!("error activating Linux proxy routing bridge: {err}");
            }
        }
        let proxy_routing_active = allow_network_for_proxy;
        if let Err(e) = apply_sandbox_policy_to_current_thread(
            network_sandbox_policy,
            allow_network_for_proxy,
            proxy_routing_active,
        ) {
            panic!("error applying Linux sandbox restrictions: {e:?}");
        }
        exec_or_panic(command);
    }

    if file_system_sandbox_policy.has_full_disk_write_access() && !allow_network_for_proxy {
        if let Err(e) = apply_sandbox_policy_to_current_thread(
            network_sandbox_policy,
            allow_network_for_proxy,
            false,
        ) {
            panic!("error applying Linux sandbox restrictions: {e:?}");
        }
        exec_or_panic(command);
    }

    // Bubblewrap path: wrap the command with bubblewrap to construct the
    // filesystem view, then re-enter this binary to apply seccomp.
    let proxy_route_spec = if allow_network_for_proxy {
        Some(
            prepare_host_proxy_route_spec()
                .unwrap_or_else(|err| panic!("failed to prepare host proxy routing bridge: {err}")),
        )
    } else {
        None
    };
    let inner = build_inner_seccomp_command(InnerSeccompCommandArgs {
        sandbox_policy_cwd: &sandbox_policy_cwd,
        file_system_sandbox_policy: &file_system_sandbox_policy,
        network_sandbox_policy,
        allow_network_for_proxy,
        proxy_route_spec,
        command,
    });
    run_bwrap_with_proc_fallback(
        &sandbox_policy_cwd,
        &file_system_sandbox_policy,
        network_sandbox_policy,
        inner,
        !no_proc,
        allow_network_for_proxy,
    );
}

fn run_bwrap_with_proc_fallback(
    sandbox_policy_cwd: &Path,
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
    network_sandbox_policy: NetworkSandboxPolicy,
    inner: Vec<String>,
    mount_proc: bool,
    allow_network_for_proxy: bool,
) -> ! {
    let network_mode = bwrap_network_mode(network_sandbox_policy, allow_network_for_proxy);
    let mut mount_proc = mount_proc;
    // Detect once; share across preflight and real invocation.
    let supports_argv0 = bwrap_supports_argv0();

    if mount_proc
        && !preflight_proc_mount_support(
            sandbox_policy_cwd,
            file_system_sandbox_policy,
            network_mode,
            supports_argv0,
        )
    {
        // Keep the retry silent so sandbox-internal diagnostics do not leak into the
        // child process stderr stream.
        mount_proc = false;
    }

    let options = BwrapOptions {
        mount_proc,
        network_mode,
    };
    let bwrap_args = build_bwrap_argv(
        inner,
        file_system_sandbox_policy,
        sandbox_policy_cwd,
        options,
        supports_argv0,
    );
    exec_bwrap(bwrap_args.args, bwrap_args.preserved_files);
}

fn bwrap_network_mode(
    network_sandbox_policy: NetworkSandboxPolicy,
    allow_network_for_proxy: bool,
) -> BwrapNetworkMode {
    if allow_network_for_proxy {
        BwrapNetworkMode::ProxyOnly
    } else if network_sandbox_policy.is_enabled() {
        BwrapNetworkMode::FullAccess
    } else {
        BwrapNetworkMode::Isolated
    }
}

/// Detect whether the installed `bwrap` binary supports the `--argv0` option.
///
/// `--argv0` was introduced in bubblewrap v0.9.0. On older distributions the
/// binary may be as old as v0.4.x and will error with "Unknown option --argv0"
/// if the flag is passed. We query `bwrap --version` at startup and skip the
/// flag when the version is below 0.9.0.
fn bwrap_supports_argv0() -> bool {
    let output = match Command::new("bwrap").arg("--version").output() {
        Ok(output) => output,
        Err(_) => return false,
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_bwrap_version_supports_argv0(&stdout)
}

/// Parse `bwrap --version` output and return `true` when the version is >=
/// 0.9.0 (the release that introduced `--argv0`).
///
/// Expected format: `"bubblewrap <MAJOR>.<MINOR>.<PATCH>"`.
/// Returns `false` on any parse failure so we degrade gracefully.
fn parse_bwrap_version_supports_argv0(version_output: &str) -> bool {
    fn parse(s: &str) -> Option<bool> {
        // e.g. "bubblewrap 0.9.0"
        let version_str = s.split_whitespace().nth(1)?;
        let mut parts = version_str.split('.');
        let major: u32 = parts.next()?.parse().ok()?;
        let minor: u32 = parts.next()?.parse().ok()?;
        // supports --argv0 when major > 0, or major == 0 and minor >= 9
        Some(major > 0 || minor >= 9)
    }
    parse(version_output).unwrap_or(false)
}

fn build_bwrap_argv(
    inner: Vec<String>,
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
    sandbox_policy_cwd: &Path,
    options: BwrapOptions,
    supports_argv0: bool,
) -> crate::bwrap_args::BwrapArgs {
    let mut bwrap_args = create_bwrap_command_args(
        inner,
        file_system_sandbox_policy,
        sandbox_policy_cwd,
        options,
    )
    .unwrap_or_else(|err| panic!("error building bubblewrap command: {err:?}"));

    if supports_argv0 {
        let command_separator_index = bwrap_args
            .args
            .iter()
            .position(|arg| arg == "--")
            .unwrap_or_else(|| panic!("bubblewrap argv is missing command separator '--'"));
        bwrap_args.args.splice(
            command_separator_index..command_separator_index,
            ["--argv0".to_string(), "linux-sandbox".to_string()],
        );
    }

    let mut argv = vec!["bwrap".to_string()];
    argv.extend(bwrap_args.args);
    crate::bwrap_args::BwrapArgs {
        args: argv,
        preserved_files: bwrap_args.preserved_files,
    }
}

fn preflight_proc_mount_support(
    sandbox_policy_cwd: &Path,
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
    network_mode: BwrapNetworkMode,
    supports_argv0: bool,
) -> bool {
    let preflight_argv = build_preflight_bwrap_argv(
        sandbox_policy_cwd,
        file_system_sandbox_policy,
        network_mode,
        supports_argv0,
    );
    let stderr = run_bwrap_in_child_capture_stderr(preflight_argv);
    !is_proc_mount_failure(stderr.as_str())
}

fn build_preflight_bwrap_argv(
    sandbox_policy_cwd: &Path,
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
    network_mode: BwrapNetworkMode,
    supports_argv0: bool,
) -> crate::bwrap_args::BwrapArgs {
    let preflight_command = vec![resolve_true_command()];
    build_bwrap_argv(
        preflight_command,
        file_system_sandbox_policy,
        sandbox_policy_cwd,
        BwrapOptions {
            mount_proc: true,
            network_mode,
        },
        supports_argv0,
    )
}

fn resolve_true_command() -> String {
    for candidate in ["/usr/bin/true", "/bin/true"] {
        if Path::new(candidate).exists() {
            return candidate.to_string();
        }
    }
    "true".to_string()
}

/// Run a short-lived bubblewrap preflight in a child process and capture stderr.
///
/// Strategy:
/// - This is used only by `preflight_proc_mount_support`, which runs `/bin/true`
///   under bubblewrap with `--proc /proc`.
/// - The goal is to detect environments where mounting `/proc` fails (for
///   example, restricted containers), so we can retry the real run with
///   `--no-proc`.
/// - We capture stderr from that preflight to match known mount-failure text.
///   We do not stream it because this is a one-shot probe with a trivial
///   command, and reads are bounded to a fixed max size.
fn run_bwrap_in_child_capture_stderr(bwrap_args: crate::bwrap_args::BwrapArgs) -> String {
    const MAX_PREFLIGHT_STDERR_BYTES: u64 = 64 * 1024;

    let mut pipe_fds = [0; 2];
    let pipe_res = unsafe { libc::pipe2(pipe_fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if pipe_res < 0 {
        let err = std::io::Error::last_os_error();
        panic!("failed to create stderr pipe for bubblewrap: {err}");
    }
    let read_fd = pipe_fds[0];
    let write_fd = pipe_fds[1];

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        let err = std::io::Error::last_os_error();
        panic!("failed to fork for bubblewrap: {err}");
    }

    if pid == 0 {
        // Child: redirect stderr to the pipe, then run bubblewrap.
        unsafe {
            close_fd_or_panic(read_fd, "close read end in bubblewrap child");
            if libc::dup2(write_fd, libc::STDERR_FILENO) < 0 {
                let err = std::io::Error::last_os_error();
                panic!("failed to redirect stderr for bubblewrap: {err}");
            }
            close_fd_or_panic(write_fd, "close write end in bubblewrap child");
        }

        let exit_code = run_bwrap_in_child(&bwrap_args.args, &bwrap_args.preserved_files);
        std::process::exit(exit_code);
    }

    // Parent: close the write end and read stderr while the child runs.
    close_fd_or_panic(write_fd, "close write end in bubblewrap parent");

    // SAFETY: `read_fd` is a valid owned fd in the parent.
    let mut read_file = unsafe { File::from_raw_fd(read_fd) };
    let mut stderr_bytes = Vec::new();
    let mut limited_reader = (&mut read_file).take(MAX_PREFLIGHT_STDERR_BYTES);
    if let Err(err) = limited_reader.read_to_end(&mut stderr_bytes) {
        panic!("failed to read bubblewrap stderr: {err}");
    }

    let mut status: libc::c_int = 0;
    let wait_res = unsafe { libc::waitpid(pid, &mut status as *mut libc::c_int, 0) };
    if wait_res < 0 {
        let err = std::io::Error::last_os_error();
        panic!("waitpid failed for bubblewrap child: {err}");
    }

    String::from_utf8_lossy(&stderr_bytes).into_owned()
}

/// Close an owned file descriptor and panic with context on failure.
///
/// We use explicit close() checks here (instead of ignoring return codes)
/// because this code runs in low-level sandbox setup paths where fd leaks or
/// close errors can mask the root cause of later failures.
fn close_fd_or_panic(fd: libc::c_int, context: &str) {
    let close_res = unsafe { libc::close(fd) };
    if close_res < 0 {
        let err = std::io::Error::last_os_error();
        panic!("{context}: {err}");
    }
}

fn is_proc_mount_failure(stderr: &str) -> bool {
    stderr.contains("Can't mount proc")
        && stderr.contains("/newroot/proc")
        && (stderr.contains("Invalid argument")
            || stderr.contains("Operation not permitted")
            || stderr.contains("Permission denied"))
}

struct InnerSeccompCommandArgs<'a> {
    sandbox_policy_cwd: &'a Path,
    file_system_sandbox_policy: &'a FileSystemSandboxPolicy,
    network_sandbox_policy: NetworkSandboxPolicy,
    allow_network_for_proxy: bool,
    proxy_route_spec: Option<String>,
    command: Vec<String>,
}

/// Build the inner command that applies seccomp after bubblewrap.
fn build_inner_seccomp_command(args: InnerSeccompCommandArgs<'_>) -> Vec<String> {
    let InnerSeccompCommandArgs {
        sandbox_policy_cwd,
        file_system_sandbox_policy,
        network_sandbox_policy,
        allow_network_for_proxy,
        proxy_route_spec,
        command,
    } = args;
    let current_exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => panic!("failed to resolve current executable path: {err}"),
    };
    let file_system_policy_json = match serde_json::to_string(file_system_sandbox_policy) {
        Ok(json) => json,
        Err(err) => panic!("failed to serialize filesystem sandbox policy: {err}"),
    };
    let network_policy_json = match serde_json::to_string(&network_sandbox_policy) {
        Ok(json) => json,
        Err(err) => panic!("failed to serialize network sandbox policy: {err}"),
    };

    let mut inner = vec![
        current_exe.to_string_lossy().to_string(),
        "--sandbox-policy-cwd".to_string(),
        sandbox_policy_cwd.to_string_lossy().to_string(),
        "--file-system-sandbox-policy".to_string(),
        file_system_policy_json,
        "--network-sandbox-policy".to_string(),
        network_policy_json,
        "--apply-seccomp-then-exec".to_string(),
    ];
    if allow_network_for_proxy {
        inner.push("--allow-network-for-proxy".to_string());
        let proxy_route_spec = proxy_route_spec
            .unwrap_or_else(|| panic!("managed proxy mode requires a proxy route spec"));
        inner.push("--proxy-route-spec".to_string());
        inner.push(proxy_route_spec);
    }
    inner.push("--".to_string());
    inner.extend(command);
    inner
}

/// Exec the provided argv, panicking with context if it fails.
fn exec_or_panic(command: Vec<String>) -> ! {
    #[expect(clippy::expect_used)]
    let c_command =
        CString::new(command[0].as_str()).expect("Failed to convert command to CString");
    #[expect(clippy::expect_used)]
    let c_args: Vec<CString> = command
        .iter()
        .map(|arg| CString::new(arg.as_str()).expect("Failed to convert arg to CString"))
        .collect();

    let mut c_args_ptrs: Vec<*const libc::c_char> = c_args.iter().map(|arg| arg.as_ptr()).collect();
    c_args_ptrs.push(std::ptr::null());

    unsafe {
        libc::execvp(c_command.as_ptr(), c_args_ptrs.as_ptr());
    }

    // If execvp returns, there was an error.
    let err = std::io::Error::last_os_error();
    panic!("Failed to execvp {}: {err}", command[0].as_str());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::FileSystemSandboxPolicy;
    use crate::policy::NetworkSandboxPolicy;
    use pretty_assertions::assert_eq;

    /// Helper to create a read-only filesystem policy.
    fn read_only_fs_policy() -> FileSystemSandboxPolicy {
        FileSystemSandboxPolicy::default()
    }

    #[test]
    fn detects_proc_mount_invalid_argument_failure() {
        let stderr = "bwrap: Can't mount proc on /newroot/proc: Invalid argument";
        assert!(is_proc_mount_failure(stderr));
    }

    #[test]
    fn detects_proc_mount_operation_not_permitted_failure() {
        let stderr = "bwrap: Can't mount proc on /newroot/proc: Operation not permitted";
        assert!(is_proc_mount_failure(stderr));
    }

    #[test]
    fn detects_proc_mount_permission_denied_failure() {
        let stderr = "bwrap: Can't mount proc on /newroot/proc: Permission denied";
        assert!(is_proc_mount_failure(stderr));
    }

    #[test]
    fn ignores_non_proc_mount_errors() {
        let stderr = "bwrap: Can't bind mount /dev/null: Operation not permitted";
        assert!(!is_proc_mount_failure(stderr));
    }

    #[test]
    fn inserts_bwrap_argv0_before_command_separator() {
        let argv = build_bwrap_argv(
            vec!["/bin/true".to_string()],
            &read_only_fs_policy(),
            Path::new("/"),
            BwrapOptions {
                mount_proc: true,
                network_mode: BwrapNetworkMode::FullAccess,
            },
            true,
        )
        .args;
        assert_eq!(
            argv,
            vec![
                "bwrap".to_string(),
                "--new-session".to_string(),
                "--die-with-parent".to_string(),
                "--ro-bind".to_string(),
                "/".to_string(),
                "/".to_string(),
                "--dev".to_string(),
                "/dev".to_string(),
                "--unshare-user".to_string(),
                "--unshare-pid".to_string(),
                "--proc".to_string(),
                "/proc".to_string(),
                "--argv0".to_string(),
                "linux-sandbox".to_string(),
                "--".to_string(),
                "/bin/true".to_string(),
            ]
        );
    }

    #[test]
    fn omits_argv0_when_not_supported() {
        let argv = build_bwrap_argv(
            vec!["/bin/true".to_string()],
            &read_only_fs_policy(),
            Path::new("/"),
            BwrapOptions {
                mount_proc: true,
                network_mode: BwrapNetworkMode::FullAccess,
            },
            false,
        )
        .args;
        assert!(!argv.contains(&"--argv0".to_string()));
        assert!(!argv.contains(&"linux-sandbox".to_string()));
        // "--" separator must still be present
        assert!(argv.contains(&"--".to_string()));
    }

    #[test]
    fn parse_version_detects_old_bwrap() {
        assert!(!parse_bwrap_version_supports_argv0("bubblewrap 0.4.0\n"));
        assert!(!parse_bwrap_version_supports_argv0("bubblewrap 0.8.99\n"));
    }

    #[test]
    fn parse_version_detects_new_bwrap() {
        assert!(parse_bwrap_version_supports_argv0("bubblewrap 0.9.0\n"));
        assert!(parse_bwrap_version_supports_argv0("bubblewrap 0.10.0\n"));
        assert!(parse_bwrap_version_supports_argv0("bubblewrap 1.0.0\n"));
    }

    #[test]
    fn parse_version_returns_false_on_garbage_input() {
        assert!(!parse_bwrap_version_supports_argv0(""));
        assert!(!parse_bwrap_version_supports_argv0("unknown"));
    }

    #[test]
    fn inserts_unshare_net_when_network_isolation_requested() {
        let argv = build_bwrap_argv(
            vec!["/bin/true".to_string()],
            &read_only_fs_policy(),
            Path::new("/"),
            BwrapOptions {
                mount_proc: true,
                network_mode: BwrapNetworkMode::Isolated,
            },
            false,
        )
        .args;
        assert!(argv.contains(&"--unshare-net".to_string()));
    }

    #[test]
    fn inserts_unshare_net_when_proxy_only_network_mode_requested() {
        let argv = build_bwrap_argv(
            vec!["/bin/true".to_string()],
            &read_only_fs_policy(),
            Path::new("/"),
            BwrapOptions {
                mount_proc: true,
                network_mode: BwrapNetworkMode::ProxyOnly,
            },
            false,
        )
        .args;
        assert!(argv.contains(&"--unshare-net".to_string()));
    }

    #[test]
    fn proxy_only_mode_takes_precedence_over_full_network_policy() {
        let mode = bwrap_network_mode(NetworkSandboxPolicy::Enabled, true);
        assert_eq!(mode, BwrapNetworkMode::ProxyOnly);
    }

    #[test]
    fn managed_proxy_preflight_argv_is_wrapped_for_full_access_policy() {
        let mode = bwrap_network_mode(NetworkSandboxPolicy::Enabled, true);
        let argv = build_preflight_bwrap_argv(
            Path::new("/"),
            &FileSystemSandboxPolicy::unrestricted(),
            mode,
            false,
        )
        .args;
        assert!(argv.iter().any(|arg| arg == "--"));
    }

    #[test]
    fn managed_proxy_inner_command_includes_route_spec() {
        let args = build_inner_seccomp_command(InnerSeccompCommandArgs {
            sandbox_policy_cwd: Path::new("/tmp"),
            file_system_sandbox_policy: &read_only_fs_policy(),
            network_sandbox_policy: NetworkSandboxPolicy::Restricted,
            allow_network_for_proxy: true,
            proxy_route_spec: Some("{\"routes\":[]}".to_string()),
            command: vec!["/bin/true".to_string()],
        });

        assert!(args.iter().any(|arg| arg == "--proxy-route-spec"));
        assert!(args.iter().any(|arg| arg == "{\"routes\":[]}"));
    }

    #[test]
    fn inner_command_includes_split_policy_flags() {
        let args = build_inner_seccomp_command(InnerSeccompCommandArgs {
            sandbox_policy_cwd: Path::new("/tmp"),
            file_system_sandbox_policy: &read_only_fs_policy(),
            network_sandbox_policy: NetworkSandboxPolicy::Restricted,
            allow_network_for_proxy: false,
            proxy_route_spec: None,
            command: vec!["/bin/true".to_string()],
        });

        assert!(args.iter().any(|arg| arg == "--file-system-sandbox-policy"));
        assert!(args.iter().any(|arg| arg == "--network-sandbox-policy"));
    }

    #[test]
    fn non_managed_inner_command_omits_route_spec() {
        let args = build_inner_seccomp_command(InnerSeccompCommandArgs {
            sandbox_policy_cwd: Path::new("/tmp"),
            file_system_sandbox_policy: &read_only_fs_policy(),
            network_sandbox_policy: NetworkSandboxPolicy::Restricted,
            allow_network_for_proxy: false,
            proxy_route_spec: None,
            command: vec!["/bin/true".to_string()],
        });

        assert!(!args.iter().any(|arg| arg == "--proxy-route-spec"));
    }

    #[test]
    fn managed_proxy_inner_command_requires_route_spec() {
        let result = std::panic::catch_unwind(|| {
            build_inner_seccomp_command(InnerSeccompCommandArgs {
                sandbox_policy_cwd: Path::new("/tmp"),
                file_system_sandbox_policy: &FileSystemSandboxPolicy::default(),
                network_sandbox_policy: NetworkSandboxPolicy::Restricted,
                allow_network_for_proxy: true,
                proxy_route_spec: None,
                command: vec!["/bin/true".to_string()],
            })
        });
        assert!(result.is_err());
    }
}
