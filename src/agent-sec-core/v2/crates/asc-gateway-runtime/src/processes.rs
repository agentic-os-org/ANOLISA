//! Bounded procfs snapshots with pidfd liveness and start-time identity checks.

use std::collections::BTreeSet;
use std::io::Read as _;
use std::path::Path;

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fd::OwnedFd;
#[cfg(target_os = "linux")]
use rustix::process::{Pid, PidfdFlags, pidfd_open};

use crate::{GatewayError, GatewayErrorKind};

/// Maximum processes (including the root) captured for one registration.
pub const MAX_PROCESS_TREE: usize = 4096;
const MAX_PROC_BYTES: u64 = 1024 * 1024;

/// Captured process identity; a production snapshot retains a live pidfd.
#[derive(Debug)]
pub struct ProcessIdentity {
    /// Numeric PID used by systemd's attach API.
    pub pid: u32,
    /// Parent PID when captured; used to validate procfs child enumeration.
    pub parent_pid: u32,
    /// Kernel start time in clock ticks since boot.
    pub start_time: u64,
    /// Real, effective, saved and filesystem UIDs, in procfs order.
    pub uids: [u32; 4],
    /// Primary, saved/filesystem and supplementary groups, sorted and deduplicated.
    pub groups: Vec<u32>,
    pub(crate) pidfd: Option<OwnedFd>,
}

impl ProcessIdentity {
    pub(crate) fn verify(&self) -> Result<(), GatewayError> {
        self.check_live()?;
        let current = read_identity(self.pid)?;
        if current.start_time != self.start_time
            || current.uids != self.uids
            || current.groups != self.groups
        {
            return Err(GatewayError::new(
                GatewayErrorKind::PermissionDenied,
                format!(
                    "process {} changed identity or its PID was reused",
                    self.pid
                ),
            ));
        }
        // Numeric procfs reads can race exit/reuse, even when start times match.
        self.check_live()
    }

    fn check_live(&self) -> Result<(), GatewayError> {
        let pin = self
            .pidfd
            .as_ref()
            .ok_or_else(|| GatewayError::internal(format!("process {} has no pidfd", self.pid)))?;
        let mut fds = [PollFd::new(pin, PollFlags::IN)];
        let timeout = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        if poll(&mut fds, Some(&timeout)).map_err(|error| {
            GatewayError::unavailable(format!("cannot check pidfd for {}: {error}", self.pid))
        })? != 0
        {
            return Err(GatewayError::unavailable(format!(
                "process {} exited",
                self.pid
            )));
        }
        Ok(())
    }
}

pub(crate) fn snapshot(root: u32) -> Result<Vec<ProcessIdentity>, GatewayError> {
    let mut processes = vec![capture(root)?];
    let mut seen = BTreeSet::from([root]);
    let mut thread_count = 0;
    let mut index = 0;
    while index < processes.len() {
        let process = &processes[index];
        let parent_pid = process.pid;
        process.verify()?;
        let tasks_path = format!("/proc/{}/task", process.pid);
        let tasks = std::fs::read_dir(&tasks_path).map_err(|error| {
            GatewayError::io(
                "cannot list process threads",
                Path::new(&tasks_path),
                &error,
            )
        })?;
        let mut children = BTreeSet::new();
        for task in tasks {
            thread_count += 1;
            if thread_count > MAX_PROCESS_TREE {
                return Err(capacity());
            }
            let task = task.map_err(|error| {
                GatewayError::io("cannot read process thread", Path::new(&tasks_path), &error)
            })?;
            for child in read_proc(&task.path().join("children"))?.split_whitespace() {
                let pid = child
                    .parse::<u32>()
                    .map_err(|_| GatewayError::internal("invalid procfs child PID"))?;
                children.insert(pid);
                if children.len() > MAX_PROCESS_TREE {
                    return Err(capacity());
                }
            }
        }
        process.verify()?;
        for pid in children {
            if seen.insert(pid) {
                if processes.len() == MAX_PROCESS_TREE {
                    return Err(capacity());
                }
                // A child gone after enumeration fails the snapshot, without moving any target.
                let child = capture(pid)?;
                if child.parent_pid != parent_pid {
                    return Err(GatewayError::unavailable(format!(
                        "child {pid} of {parent_pid} exited, was reparented or its PID was reused"
                    )));
                }
                processes.push(child);
            }
        }
        index += 1;
    }
    for process in &processes {
        process.verify()?;
    }
    Ok(processes)
}

pub(crate) fn scope_snapshot(path: &str) -> Result<Vec<ProcessIdentity>, GatewayError> {
    if !crate::nft::is_safe_cgroup_path(path) {
        return Err(GatewayError::internal("unsafe scope snapshot path"));
    }
    let root = std::path::PathBuf::from(format!("/sys/fs/cgroup/{path}"));
    match std::fs::symlink_metadata(&root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(GatewayError::io("cannot inspect scope", &root, &error)),
        Ok(metadata) if !metadata.is_dir() => {
            return Err(GatewayError::internal("scope is not a cgroup directory"));
        }
        Ok(_) => {}
    }
    let mut directories = vec![root];
    let mut pids = BTreeSet::new();
    let mut index = 0;
    while index < directories.len() {
        let directory = &directories[index];
        for pid in read_proc(&directory.join("cgroup.procs"))?.split_whitespace() {
            pids.insert(
                pid.parse::<u32>()
                    .map_err(|_| GatewayError::internal("invalid cgroup PID"))?,
            );
            if pids.len() > MAX_PROCESS_TREE {
                return Err(capacity());
            }
        }
        let entries = std::fs::read_dir(directory)
            .map_err(|error| GatewayError::io("cannot list scope", directory, &error))?;
        let mut children = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| {
                GatewayError::io("cannot read scope directory", directory, &error)
            })?;
            if entry
                .file_type()
                .map_err(|error| {
                    GatewayError::io("cannot inspect scope child", &entry.path(), &error)
                })?
                .is_dir()
            {
                if directories.len() + children.len() >= MAX_PROCESS_TREE {
                    return Err(capacity());
                }
                children.push(entry.path());
            }
        }
        children.sort();
        directories.extend(children);
        index += 1;
    }
    pids.into_iter().map(capture).collect()
}

#[cfg(not(target_os = "linux"))]
fn capture(_pid: u32) -> Result<ProcessIdentity, GatewayError> {
    Err(GatewayError::unavailable(
        "process registration requires Linux pidfds",
    ))
}

#[cfg(target_os = "linux")]
fn capture(pid: u32) -> Result<ProcessIdentity, GatewayError> {
    let raw = i32::try_from(pid).map_err(|_| GatewayError::invalid("PID is out of range"))?;
    let target = Pid::from_raw(raw).ok_or_else(|| GatewayError::invalid("PID must be positive"))?;
    let pin = pidfd_open(target, PidfdFlags::empty()).map_err(|error| {
        GatewayError::unavailable(format!("cannot pin process {pid} with pidfd: {error}"))
    })?;
    let mut identity = read_identity(pid)?;
    identity.pidfd = Some(pin);
    identity.verify()?;
    Ok(identity)
}

fn read_identity(pid: u32) -> Result<ProcessIdentity, GatewayError> {
    let stat = read_proc(Path::new(&format!("/proc/{pid}/stat")))?;
    let fields: Vec<&str> = stat
        .rsplit_once(')')
        .ok_or_else(|| GatewayError::internal("invalid procfs stat"))?
        .1
        .split_whitespace()
        .collect();
    if fields
        .first()
        .is_some_and(|state| matches!(*state, "Z" | "X" | "x"))
    {
        return Err(GatewayError::unavailable(format!("process {pid} exited")));
    }
    let parent_pid = fields
        .get(1)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| GatewayError::internal("invalid procfs parent PID"))?;
    let start_time = fields
        .get(19)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| GatewayError::internal("invalid procfs start time"))?;
    let status = read_proc(Path::new(&format!("/proc/{pid}/status")))?;
    let ids = |label: &str| -> Result<Vec<u32>, GatewayError> {
        status
            .lines()
            .find_map(|line| line.strip_prefix(label))
            .ok_or_else(|| GatewayError::internal(format!("process {pid} lacks {label}")))?
            .split_whitespace()
            .map(|value| {
                value.parse::<u32>().map_err(|_| {
                    GatewayError::internal(format!("invalid {label} for process {pid}"))
                })
            })
            .collect()
    };
    let uids = ids("Uid:")?
        .try_into()
        .map_err(|_| GatewayError::internal("invalid procfs UID count"))?;
    let gids = ids("Gid:")?;
    if gids.len() != 4 {
        return Err(GatewayError::internal("invalid procfs GID count"));
    }
    let mut groups = ids("Groups:")?;
    groups.extend(gids);
    groups.sort_unstable();
    groups.dedup();
    Ok(ProcessIdentity {
        pid,
        parent_pid,
        start_time,
        uids,
        groups,
        pidfd: None,
    })
}

fn read_proc(path: &Path) -> Result<String, GatewayError> {
    let mut text = String::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(MAX_PROC_BYTES + 1).read_to_string(&mut text))
        .map_err(|error| GatewayError::io("cannot read process snapshot", path, &error))?;
    if text.len() as u64 > MAX_PROC_BYTES {
        return Err(capacity());
    }
    Ok(text)
}

fn capacity() -> GatewayError {
    GatewayError::new(
        GatewayErrorKind::ResourceExhausted,
        "process snapshot exceeds 4096 processes/threads or the procfs read limit",
    )
}
