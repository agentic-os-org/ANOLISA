//! Host operations the control plane needs from systemd, nftables and procfs.

use std::fs::File;
use std::io::{Read as _, Seek as _, Write as _};
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};

use crate::{GatewayError, ProcessIdentity};

const OUTPUT_LIMIT: usize = 512;
const CAPTURE_LIMIT: u64 = 64 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const COMMAND_POLL: Duration = Duration::from_millis(20);
const ATTACH_WAIT: Duration = Duration::from_secs(2);

/// Account the gateway data plane runs as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatewayAccount {
    /// User id.
    pub uid: u32,
    /// Primary group id; runtime files, the secrets snapshot included, are readable by
    /// this group, so it must be dedicated to the account.
    pub gid: u32,
}

/// Privileged host operations, replaceable in tests.
pub trait Host: Send + Sync {
    /// Checks the mounted cgroup v2 hierarchy used for process routing without changing it.
    /// # Errors
    /// Rejects a missing, unreadable or non-v2 hierarchy.
    fn check_cgroup_v2(&self) -> Result<(), GatewayError>;

    /// Resolves the gateway system account; `Ok(None)` when it does not exist.
    ///
    /// # Errors
    /// Returns a failure when the account database cannot be read, or when the
    /// account's primary group is not named after it or lists other members.
    fn gateway_account(&self, user: &str) -> Result<Option<GatewayAccount>, GatewayError>;

    /// Returns the primary and supplementary group ids of the local user `uid`;
    /// `None` when no such user exists.
    fn user_groups(&self, uid: u32) -> Option<Vec<u32>>;

    /// Rejects groups that can read secrets or escalate to the privileged gateway.
    /// # Errors
    /// Returns inaccessible group databases or sensitive membership.
    fn check_sensitive_groups(&self, groups: &[u32]) -> Result<(), GatewayError> {
        for gid in groups {
            let group = nix::unistd::Group::from_gid(nix::unistd::Gid::from_raw(*gid))
                .map_err(|_| GatewayError::unavailable("cannot inspect group database"))?;
            if *gid == 0
                || group.is_some_and(|group| {
                    matches!(
                        group.name.as_str(),
                        "root"
                            | "sudo"
                            | "wheel"
                            | "docker"
                            | "adm"
                            | "disk"
                            | "shadow"
                            | "systemd-journal"
                            | "lxd"
                            | "libvirt"
                    )
                })
            {
                return Err(GatewayError::new(
                    crate::GatewayErrorKind::PermissionDenied,
                    "Scope owner or process belongs to a sensitive group",
                ));
            }
        }
        Ok(())
    }

    /// Reads boot and PID-namespace fences from the same procfs view used for attachment.
    /// # Errors
    /// Rejects unavailable process identity evidence.
    fn process_fence(&self, pid: u32) -> Result<(String, String), GatewayError> {
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map_err(|_| GatewayError::unavailable("cannot read host boot identity"))?;
        let namespace = std::fs::read_link(format!("/proc/{pid}/ns/pid")).map_err(|error| {
            if crate::processes::process_vanished(&error) {
                GatewayError::process_exited()
            } else {
                GatewayError::unavailable("cannot read process PID namespace")
            }
        })?;
        Ok((
            boot.trim().to_owned(),
            namespace.to_string_lossy().into_owned(),
        ))
    }

    /// Starts a systemd unit and waits for the start job.
    ///
    /// # Errors
    /// Returns a failure when `systemctl` fails.
    fn start_unit(&self, unit: &str) -> Result<(), GatewayError>;

    /// Stops a systemd unit and waits for the stop job.
    ///
    /// # Errors
    /// Returns a failure when `systemctl` fails.
    fn stop_unit(&self, unit: &str) -> Result<(), GatewayError>;

    /// Whether a systemd unit is active.
    fn unit_active(&self, unit: &str) -> bool;

    /// Whether gateway routing remains installed, independently of systemd and runtime files.
    /// # Errors
    /// Returns failures to inspect the host ruleset.
    fn routing_active(&self) -> Result<bool, GatewayError>;

    /// Loads one nftables transaction.
    ///
    /// # Errors
    /// Returns a failure when `nft` rejects the transaction.
    fn apply_nft(&self, ruleset: &str) -> Result<(), GatewayError>;

    /// Moves a captured process into `scope`, creating it under `slice` when absent.
    ///
    /// # Errors
    /// Returns systemd failures or a changed/exited identity before or after attachment.
    fn attach_to_scope(
        &self,
        scope: &str,
        slice: &str,
        process: &ProcessIdentity,
    ) -> Result<(), GatewayError>;

    /// Removes an empty transient scope so a later registration may recreate it with PIDs.
    ///
    /// # Errors
    /// Returns a failure when systemd cannot reset or stop the scope.
    fn remove_scope(&self, scope: &str) -> Result<(), GatewayError>;

    /// Snapshots the root first and then existing descendants, retaining identity pins.
    ///
    /// # Errors
    /// Returns exited processes, unavailable pidfds, or a snapshot over 4096 processes.
    fn process_tree(&self, root: u32) -> Result<Vec<ProcessIdentity>, GatewayError>;

    /// Snapshots existing members of a cgroup and its children, or an empty absent scope.
    ///
    /// # Errors
    /// Returns unreadable/changed processes or a snapshot over the process/directory limit.
    fn scope_processes(&self, path: &str) -> Result<Vec<ProcessIdentity>, GatewayError>;

    /// Checks start time, all UIDs/groups and live pidfd against a captured identity.
    ///
    /// # Errors
    /// Returns a changed, reused or exited process.
    fn verify_process(&self, process: &ProcessIdentity) -> Result<(), GatewayError>;

    /// Returns the cgroup v2 path of `pid`, starting with `/`.
    ///
    /// # Errors
    /// Returns a failure when the process is gone or not in a v2 hierarchy.
    fn process_cgroup(&self, pid: u32) -> Result<String, GatewayError>;

    /// Whether the cgroup at `path` (relative to the hierarchy root) exists.
    fn cgroup_exists(&self, path: &str) -> bool;
}

/// Production host backed by `systemctl`, `busctl`, `nft` and procfs.
#[derive(Debug, Default)]
pub struct SystemHost;

impl Host for SystemHost {
    #[cfg(target_os = "linux")]
    fn check_cgroup_v2(&self) -> Result<(), GatewayError> {
        let hierarchy = nix::sys::statfs::statfs("/sys/fs/cgroup").map_err(|error| {
            GatewayError::unavailable(format!("cannot inspect cgroup hierarchy: {error}"))
        })?;
        if hierarchy.filesystem_type() != nix::sys::statfs::CGROUP2_SUPER_MAGIC {
            return Err(GatewayError::unavailable(
                "Gateway requires cgroup v2 mounted at /sys/fs/cgroup",
            ));
        }
        self.process_cgroup(std::process::id())
            .map(drop)
            .map_err(|error| {
                GatewayError::unavailable(format!("cannot verify cgroup v2 hierarchy: {error}"))
            })
    }

    #[cfg(not(target_os = "linux"))]
    fn check_cgroup_v2(&self) -> Result<(), GatewayError> {
        Err(GatewayError::unavailable(
            "Gateway requires Linux cgroup v2",
        ))
    }

    fn gateway_account(&self, user: &str) -> Result<Option<GatewayAccount>, GatewayError> {
        let Some(account) = nix::unistd::User::from_name(user).map_err(|error| {
            GatewayError::internal(format!("cannot look up user {user}: {error}"))
        })?
        else {
            return Ok(None);
        };
        let group = nix::unistd::Group::from_gid(account.gid).map_err(|error| {
            GatewayError::internal(format!("cannot look up the group of {user}: {error}"))
        })?;
        if !group.is_some_and(|group| group.name == account.name && group.mem.is_empty()) {
            return Err(GatewayError::unavailable(format!(
                "the primary group of {user} must be a dedicated group named {user} without members"
            )));
        }
        Ok(Some(GatewayAccount {
            uid: account.uid.as_raw(),
            gid: account.gid.as_raw(),
        }))
    }

    #[cfg(not(target_os = "linux"))]
    fn user_groups(&self, _uid: u32) -> Option<Vec<u32>> {
        None
    }

    #[cfg(target_os = "linux")]
    fn user_groups(&self, uid: u32) -> Option<Vec<u32>> {
        let user = nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid)).ok()??;
        let name = std::ffi::CString::new(user.name).ok()?;
        let groups = nix::unistd::getgrouplist(&name, user.gid).ok()?;
        Some(groups.into_iter().map(nix::unistd::Gid::as_raw).collect())
    }

    fn start_unit(&self, unit: &str) -> Result<(), GatewayError> {
        run("systemctl", &["start", unit], None).map(drop)
    }

    fn stop_unit(&self, unit: &str) -> Result<(), GatewayError> {
        run("systemctl", &["stop", unit], None).map(drop)
    }

    fn unit_active(&self, unit: &str) -> bool {
        run("systemctl", &["is-active", "--quiet", unit], None).is_ok()
    }

    fn routing_active(&self) -> Result<bool, GatewayError> {
        run("nft", &["list", "tables"], None).map(|tables| {
            tables
                .lines()
                .any(|line| line.trim() == "table inet agent_sec_gateway")
        })
    }

    fn apply_nft(&self, ruleset: &str) -> Result<(), GatewayError> {
        run("nft", &["-f", "-"], Some(ruleset)).map(drop)
    }

    fn attach_to_scope(
        &self,
        scope: &str,
        slice: &str,
        process: &ProcessIdentity,
    ) -> Result<(), GatewayError> {
        self.verify_process(process)?;
        let pid = process.pid;
        let pid_text = pid.to_string();
        // systemd accepts numeric PIDs, not pidfds: the pin and checks detect races
        // but cannot make the D-Bus move atomic with process identity validation.
        let created = run(
            "busctl",
            &[
                "call",
                "--system",
                "org.freedesktop.systemd1",
                "/org/freedesktop/systemd1",
                "org.freedesktop.systemd1.Manager",
                "StartTransientUnit",
                "ssa(sv)a(sa(sv))",
                scope,
                "fail",
                "4",
                "PIDs",
                "au",
                "1",
                &pid_text,
                "Slice",
                "s",
                slice,
                "CollectMode",
                "s",
                "inactive-or-failed",
                "Delegate",
                "b",
                "true",
                "0",
            ],
            None,
        );
        match created {
            Ok(_) => {}
            Err(error)
                if error.to_string().contains("UnitExists")
                    || error.to_string().contains("already exists")
                    || error.to_string().contains("already loaded") =>
            {
                self.verify_process(process)?;
                run(
                    "busctl",
                    &[
                        "call",
                        "--system",
                        "org.freedesktop.systemd1",
                        "/org/freedesktop/systemd1",
                        "org.freedesktop.systemd1.Manager",
                        "AttachProcessesToUnit",
                        "ssau",
                        scope,
                        "/",
                        "1",
                        &pid_text,
                    ],
                    None,
                )?;
            }
            Err(error) => return Err(error),
        }
        // The start job moves the process asynchronously.
        let expected = format!("/{slice}/{scope}");
        let deadline = Instant::now() + ATTACH_WAIT;
        loop {
            self.verify_process(process)?;
            if self.process_cgroup(pid)? == expected {
                self.verify_process(process)?;
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(GatewayError::unavailable(format!(
                    "process {pid} did not move into {expected}"
                )));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn process_cgroup(&self, pid: u32) -> Result<String, GatewayError> {
        let path = PathBuf::from(format!("/proc/{pid}/cgroup"));
        let text = std::fs::read_to_string(&path).map_err(|error| {
            if crate::processes::process_vanished(&error) {
                GatewayError::process_exited()
            } else {
                GatewayError::io("cannot read", &path, &error)
            }
        })?;
        text.lines()
            .find_map(|line| line.strip_prefix("0::"))
            .map(str::to_owned)
            .ok_or_else(|| GatewayError::unavailable("the host does not use a cgroup v2 hierarchy"))
    }

    fn remove_scope(&self, scope: &str) -> Result<(), GatewayError> {
        let reset = run("systemctl", &["reset-failed", scope], None);
        match reset {
            Ok(_) => Ok(()),
            Err(error)
                if error.to_string().contains("not loaded")
                    || error.to_string().contains("not found") =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn process_tree(&self, root: u32) -> Result<Vec<ProcessIdentity>, GatewayError> {
        crate::processes::snapshot(root)
    }

    fn scope_processes(&self, path: &str) -> Result<Vec<ProcessIdentity>, GatewayError> {
        crate::processes::scope_snapshot(path)
    }

    fn verify_process(&self, process: &ProcessIdentity) -> Result<(), GatewayError> {
        process.verify()
    }

    fn cgroup_exists(&self, path: &str) -> bool {
        std::fs::symlink_metadata(format!("/sys/fs/cgroup/{path}")).is_ok_and(|meta| meta.is_dir())
    }
}

fn run(program: &str, args: &[&str], stdin: Option<&str>) -> Result<String, GatewayError> {
    run_with_timeout(program, args, stdin, COMMAND_TIMEOUT)
}

fn run_with_timeout(
    program: &str,
    args: &[&str],
    stdin: Option<&str>,
    timeout: Duration,
) -> Result<String, GatewayError> {
    let start = Instant::now();
    let (input, mut stdout, mut stderr) = (|| -> std::io::Result<_> {
        let input = if let Some(text) = stdin {
            let mut file = tempfile::tempfile()?;
            file.write_all(text.as_bytes())?;
            file.rewind()?;
            Stdio::from(file)
        } else {
            Stdio::null()
        };
        Ok((input, tempfile::tempfile()?, tempfile::tempfile()?))
    })()
    .map_err(|error| GatewayError::internal(format!("cannot prepare {program} I/O: {error}")))?;
    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LC_ALL", "C")
        .process_group(0)
        .stdin(input)
        .stdout(stdout.try_clone().map_err(|error| {
            GatewayError::internal(format!("cannot prepare {program} stdout: {error}"))
        })?)
        .stderr(stderr.try_clone().map_err(|error| {
            GatewayError::internal(format!("cannot prepare {program} stderr: {error}"))
        })?)
        .spawn()
        .map_err(|error| GatewayError::unavailable(format!("cannot run {program}: {error}")))?;
    let group = Pid::from_child(&child);
    let outcome = wait_for_exit(program, group, &stdout, &stderr, start, timeout);
    let cleanup = kill_process_group(group, Signal::KILL).or_else(|error| {
        if error == rustix::io::Errno::SRCH {
            Ok(())
        } else {
            Err(error)
        }
    });
    if cleanup.is_err() {
        let _ = child.kill();
    }
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => std::thread::sleep(COMMAND_POLL),
            Err(error) => break Err(error),
        }
    }
    .map_err(|error| GatewayError::internal(format!("cannot reap {program}: {error}")))?;
    cleanup.map_err(|error| {
        GatewayError::internal(format!("cannot kill {program} process group: {error}"))
    })?;
    outcome?;
    check_output_size(program, &stdout, &stderr)?;
    let (stream, file) = if status.success() {
        ("stdout", &mut stdout)
    } else {
        ("stderr", &mut stderr)
    };
    let mut bytes = Vec::new();
    file.rewind()
        .and_then(|()| file.take(CAPTURE_LIMIT + 1).read_to_end(&mut bytes))
        .map_err(|error| {
            GatewayError::internal(format!("cannot read {program} {stream}: {error}"))
        })?;
    if bytes.len() as u64 > CAPTURE_LIMIT {
        return Err(GatewayError::unavailable(format!(
            "{program} {stream} exceeded {CAPTURE_LIMIT} bytes"
        )));
    }
    if status.success() {
        return Ok(String::from_utf8_lossy(&bytes).into_owned());
    }
    let detail: String = String::from_utf8_lossy(&bytes)
        .trim()
        .chars()
        .take(OUTPUT_LIMIT)
        .collect();
    Err(GatewayError::unavailable(format!(
        "{program} {} failed ({status}): {detail}",
        args.first().copied().unwrap_or_default(),
    )))
}

fn check_output_size(program: &str, stdout: &File, stderr: &File) -> Result<(), GatewayError> {
    for (stream, file) in [("stdout", stdout), ("stderr", stderr)] {
        let size = file.metadata().map_err(|error| {
            GatewayError::internal(format!("cannot inspect {program} {stream}: {error}"))
        })?;
        if size.len() > CAPTURE_LIMIT {
            return Err(GatewayError::unavailable(format!(
                "{program} {stream} exceeded {CAPTURE_LIMIT} bytes"
            )));
        }
    }
    Ok(())
}

fn wait_for_exit(
    program: &str,
    group: Pid,
    stdout: &File,
    stderr: &File,
    start: Instant,
    timeout: Duration,
) -> Result<(), GatewayError> {
    loop {
        check_output_size(program, stdout, stderr)?;
        if start.elapsed() >= timeout {
            return Err(GatewayError::unavailable(format!(
                "{program} timed out after {timeout:?}"
            )));
        }
        // Keep the leader waitable until group cleanup to prevent PID reuse.
        if waitid(
            WaitId::Pid(group),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )
        .map_err(|error| GatewayError::internal(format!("cannot inspect {program} exit: {error}")))?
        .is_some()
        {
            return Ok(());
        }
        std::thread::sleep(COMMAND_POLL.min(timeout.saturating_sub(start.elapsed())));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use rustix::process::{WaitOptions, waitpid};

    fn process_disappeared(error: &std::io::Error) -> bool {
        // procfs can return ESRCH when the task disappears after opening stat.
        error.kind() == std::io::ErrorKind::NotFound
            || error.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error())
    }

    #[test]
    fn process_disappearance_errors() {
        use rustix::io::Errno;
        use std::io::{Error, ErrorKind};

        for (error, disappeared) in [
            (Error::from(ErrorKind::NotFound), true),
            (Error::from_raw_os_error(Errno::NOENT.raw_os_error()), true),
            (Error::from_raw_os_error(Errno::SRCH.raw_os_error()), true),
            (
                Error::from_raw_os_error(Errno::ACCESS.raw_os_error()),
                false,
            ),
            (Error::from_raw_os_error(Errno::IO.raw_os_error()), false),
            (Error::from(ErrorKind::Other), false),
        ] {
            assert_eq!(process_disappeared(&error), disappeared, "{error:?}");
        }
    }

    fn assert_dead_and_reaped(marker: &std::path::Path) {
        let pids: Vec<Pid> = std::fs::read_to_string(marker)
            .unwrap()
            .split_whitespace()
            .map(|text| Pid::from_raw(text.parse().unwrap()).unwrap())
            .collect();
        assert_eq!(pids.len(), 2);
        assert_eq!(
            waitpid(Some(pids[0]), WaitOptions::NOHANG).unwrap_err(),
            rustix::io::Errno::CHILD
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        for pid in pids {
            let path = format!("/proc/{pid}/stat");
            loop {
                match std::fs::read_to_string(&path) {
                    Err(error) if process_disappeared(&error) => break,
                    Ok(stat)
                        if stat.rsplit_once(") ").is_some_and(|(_, rest)| {
                            matches!(rest.as_bytes().first(), Some(b'Z' | b'X'))
                        }) =>
                    {
                        break;
                    }
                    Err(error) => panic!("cannot inspect {path}: {error}"),
                    Ok(_) => {}
                }
                assert!(Instant::now() < deadline, "process {pid} survived cleanup");
                std::thread::sleep(COMMAND_POLL);
            }
        }
    }

    #[test]
    fn timeout_reaps() {
        let input = "x".repeat(2 * 1024 * 1024);
        for stdin in [None, Some(input.as_str())] {
            let directory = tempfile::tempdir().unwrap();
            let marker = directory.path().join("pids");
            let start = Instant::now();
            let error = run_with_timeout(
                "/bin/sh",
                &[
                    "-c",
                    "sleep 30 & printf '%s %s' \"$$\" \"$!\" > \"$1\"; wait",
                    "timeout-test",
                    marker.to_str().unwrap(),
                ],
                stdin,
                Duration::from_millis(500),
            )
            .unwrap_err();
            assert!(start.elapsed() < Duration::from_secs(5));
            assert_eq!(error.kind(), crate::GatewayErrorKind::Unavailable);
            assert!(error.to_string().contains("timed out"));
            assert_dead_and_reaped(&marker);
        }
    }

    #[test]
    fn output_limit() {
        for (stream, finish) in [("stdout", "wait"), ("stderr", "wait"), ("stdout", "exit")] {
            let directory = tempfile::tempdir().unwrap();
            let marker = directory.path().join("pids");
            let error = run_with_timeout(
                "/bin/sh",
                &[
                    "-c",
                    "sleep 30 & printf '%s %s' \"$$\" \"$!\" > \"$1\"; \
                     if [ \"$2\" = stdout ]; then \
                       dd if=/dev/zero bs=1024 count=65 2>/dev/null; \
                     else dd if=/dev/zero bs=1024 count=65 >&2 2>/dev/null; fi; \
                     if [ \"$3\" = wait ]; then wait; fi",
                    "output-test",
                    marker.to_str().unwrap(),
                    stream,
                    finish,
                ],
                None,
                Duration::from_secs(3),
            )
            .unwrap_err();
            assert_eq!(error.kind(), crate::GatewayErrorKind::Unavailable);
            assert!(error.to_string().contains(&format!("{stream} exceeded")));
            assert_dead_and_reaped(&marker);
        }
    }

    #[test]
    fn exit_cleanup() {
        let detail = "é".repeat(600);
        for exit_code in ["0", "7"] {
            let directory = tempfile::tempdir().unwrap();
            let marker = directory.path().join("pids");
            let start = Instant::now();
            let result = run_with_timeout(
                "/bin/sh",
                &[
                    "-c",
                    "cat; sleep 30 & printf '%s %s' \"$$\" \"$!\" > \"$1\"; \
                     printf 'done'; printf '%s' \"$2\" >&2; exit \"$3\"",
                    "exit-test",
                    marker.to_str().unwrap(),
                    &detail,
                    exit_code,
                ],
                Some("input\n"),
                Duration::from_secs(3),
            );
            assert!(start.elapsed() < Duration::from_secs(5));
            if exit_code == "0" {
                assert_eq!(result.unwrap(), "input\ndone");
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.kind(), crate::GatewayErrorKind::Unavailable);
                assert_eq!(
                    error.to_string(),
                    format!("/bin/sh -c failed (exit status: 7): {}", "é".repeat(512))
                );
            }
            assert_dead_and_reaped(&marker);
        }
    }
}
