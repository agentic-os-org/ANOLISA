//! Identify existing processes and label the process tree during deployment.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::Path;

use asc_policy_types::process_discovery::ProcessIdentity;
use asc_policy_types::target::Failure;

use super::preflight::MappingPreflight;
use super::{LuaLsmClient, MAX_OBJECTS, local_error, rejected};

impl LuaLsmClient {
    pub(super) fn label_process(
        &self,
        controls: &Path,
        root: &ProcessIdentity,
        process: &ProcessIdentity,
    ) -> Result<(), Failure> {
        if process == root {
            // The first command marks only a candidate task. A reused PID between
            // either command and the identity check cannot publish a new root.
            self.command(controls, &format!("root_candidate/{}", root.pid))?;
            verify_process(root)?;
            self.command(controls, &format!("seed_root/{}", root.pid))
        } else {
            self.command(controls, &format!("seed/{}", process.pid))
        }
    }

    pub(super) fn label_existing_processes(
        &self,
        root: &ProcessIdentity,
        controls: &Path,
        family: &[ProcessIdentity],
    ) -> Result<(), Failure> {
        for process in family {
            if !available_to_seed(root, process)? {
                continue;
            }
            self.label_process(controls, root, process)?;
            verify_process(process)?;
        }
        Ok(())
    }

    pub(super) fn label_current_process_tree(
        &self,
        controls: &Path,
        root: &ProcessIdentity,
        mappings: &MappingPreflight,
        initial: Vec<ProcessIdentity>,
    ) -> Result<(), Failure> {
        let mut initial_only: BTreeSet<_> = initial.into_iter().collect();
        for process in process_family(root)? {
            initial_only.remove(&process);
            if !available_to_seed(root, &process)? {
                continue;
            }
            let result = mappings.check(process.pid);
            // The descendant can exit or its PID can be reused while reading maps.
            if !available_to_seed(root, &process)? {
                continue;
            }
            result?;
            self.label_process(controls, root, &process)?;
            verify_process(&process)?;
        }
        // A seeded descendant can be reparented before pattern matching is available.
        // Its mappings remain in scope for as long as this process identity survives.
        for process in initial_only {
            if !same_process(&process)? {
                continue;
            }
            let result = mappings.check(process.pid);
            if same_process(&process)? {
                result?;
            }
        }
        Ok(())
    }
}

pub(super) fn boot_id() -> io::Result<String> {
    Ok(fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .into())
}

fn identity(pid: u32, boot: &str) -> io::Result<(ProcessIdentity, u32)> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let end = stat
        .rfind(')')
        .ok_or_else(|| io::Error::other("malformed stat"))?;
    let fields: Vec<_> = stat[end + 1..].split_whitespace().collect();
    let parse = |index| {
        fields
            .get(index)
            .and_then(|v: &&str| v.parse::<u64>().ok())
            .ok_or_else(|| io::Error::other("malformed stat field"))
    };
    Ok((
        ProcessIdentity {
            boot_id: boot.to_owned(),
            pid,
            start_time: parse(19)?,
            pid_namespace: match fs::read_link(format!("/proc/{pid}/ns/pid")) {
                Ok(namespace) => namespace.to_string_lossy().into_owned(),
                Err(_) if matches!(fields.first().copied(), Some("Z" | "X")) => "<exited>".into(),
                Err(error) => return Err(error),
            },
        },
        u32::try_from(parse(1)?).map_err(io::Error::other)?,
    ))
}

pub(super) fn same_process(expected: &ProcessIdentity) -> Result<bool, Failure> {
    match identity(expected.pid, &boot_id().map_err(local_error)?) {
        Ok((actual, _)) => Ok(actual == *expected),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(local_error(error)),
    }
}

fn available_to_seed(root: &ProcessIdentity, process: &ProcessIdentity) -> Result<bool, Failure> {
    if process == root {
        verify_process(root)?;
        Ok(true)
    } else {
        same_process(process)
    }
}

pub(super) fn verify_process(expected: &ProcessIdentity) -> Result<(), Failure> {
    if same_process(expected)? {
        Ok(())
    } else {
        Err(rejected("LUA_LSM_PROCESS_IDENTITY_MISMATCH"))
    }
}

/// Walk each thread's children: a non-leader thread can fork a process of its own.
/// The caller can fall back to a procfs snapshot on churn or kernels without children files.
pub(super) fn subtree(
    root: &ProcessIdentity,
    include_threads: bool,
) -> io::Result<Vec<ProcessIdentity>> {
    let boot = boot_id()?;
    let mut processes = vec![root.clone()];
    let mut seen = HashSet::from([root.pid]);
    let mut threads = Vec::new();
    let mut index = 0;
    while index < processes.len() {
        let pid = processes[index].pid;
        for entry in fs::read_dir(format!("/proc/{pid}/task"))? {
            let entry = entry?;
            if include_threads
                && let Some(tid) = proc_id(&entry)
                && tid != pid
            {
                threads.push(identity(tid, &boot)?.0);
            }
            for child in fs::read_to_string(entry.path().join("children"))?.split_whitespace() {
                let child = child.parse::<u32>().map_err(io::Error::other)?;
                if seen.insert(child) {
                    let (process, parent) = identity(child, &boot)?;
                    if parent != pid {
                        return Err(io::Error::other("child changed parent during enumeration"));
                    }
                    processes.push(process);
                }
            }
            if include_threads && processes.len() + threads.len() > MAX_OBJECTS {
                return Err(io::Error::other("task limit exceeded"));
            }
        }
        // Discard edges read through a numeric PID if the parent was replaced.
        if identity(pid, &boot)?.0 != processes[index] {
            return Err(io::Error::other(
                "parent identity changed during enumeration",
            ));
        }
        index += 1;
    }
    processes.extend(threads);
    Ok(processes)
}

fn proc_id(entry: &fs::DirEntry) -> Option<u32> {
    entry.file_name().to_str()?.parse().ok()
}

pub(super) fn process_family(root: &ProcessIdentity) -> Result<Vec<ProcessIdentity>, Failure> {
    if let Ok(family) = subtree(root, false) {
        return Ok(family);
    }
    family_by_parent(root)
}

fn family_by_parent(root: &ProcessIdentity) -> Result<Vec<ProcessIdentity>, Failure> {
    verify_process(root)?;
    let boot = boot_id().map_err(local_error)?;
    let mut children = HashMap::<u32, Vec<ProcessIdentity>>::new();
    for entry in fs::read_dir("/proc").map_err(local_error)? {
        if let Some(pid) = proc_id(&entry.map_err(local_error)?)
            && pid != root.pid
            && let Ok((process, parent)) = identity(pid, &boot)
        {
            children.entry(parent).or_default().push(process);
        }
    }
    let mut family = vec![root.clone()];
    let mut index = 0;
    while index < family.len() {
        if let Some(direct) = children.remove(&family[index].pid) {
            family.extend(direct);
        }
        index += 1;
    }
    verify_process(root)?;
    Ok(family)
}

pub(super) fn process_tasks() -> Result<Vec<ProcessIdentity>, Failure> {
    let boot = boot_id().map_err(local_error)?;
    let mut tasks = BTreeMap::new();
    for entry in fs::read_dir("/proc").map_err(local_error)? {
        let entry = entry.map_err(local_error)?;
        let Some(pid) = proc_id(&entry) else {
            continue;
        };
        let Ok(threads) = fs::read_dir(format!("/proc/{pid}/task")) else {
            continue;
        };
        for thread in threads {
            let thread = thread.map_err(local_error)?;
            if let Some(tid) = proc_id(&thread)
                && let Ok((process, _)) = identity(tid, &boot)
            {
                tasks.insert(tid, process);
            }
            if tasks.len() > MAX_OBJECTS {
                return Err(rejected("LUA_LSM_TASK_LIMIT_EXCEEDED"));
            }
        }
    }
    Ok(tasks.into_values().collect())
}

#[cfg(test)]
mod tests;
