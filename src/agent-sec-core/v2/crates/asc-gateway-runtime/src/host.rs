//! Host operations the control plane needs from systemd, nftables and procfs.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::{GatewayError, ProcessIdentity};

const OUTPUT_LIMIT: usize = 512;
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
            if error.kind() == std::io::ErrorKind::NotFound {
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
            if error.kind() == std::io::ErrorKind::NotFound
                && std::path::Path::new("/proc/self/cgroup").is_file()
            {
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
    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LC_ALL", "C")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| GatewayError::unavailable(format!("cannot run {program}: {error}")))?;
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(input.as_bytes())
            .map_err(|error| GatewayError::internal(format!("cannot feed {program}: {error}")))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| GatewayError::internal(format!("cannot wait for {program}: {error}")))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let mut detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if detail.len() > OUTPUT_LIMIT {
        let mut end = OUTPUT_LIMIT;
        while !detail.is_char_boundary(end) {
            end -= 1;
        }
        detail.truncate(end);
    }
    Err(GatewayError::unavailable(format!(
        "{program} {} failed ({}): {detail}",
        args.first().copied().unwrap_or_default(),
        output.status
    )))
}
