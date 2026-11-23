//! 有界收养既有后代，并以只读终检确认 Binding 的进程覆盖。

use std::collections::{HashMap, HashSet, VecDeque};
use std::{fs, io};

use agentsight_enforcement_protocol::{Binding, BindingState};
use ebpf_ifc_engine::PinnedEngine;

const MAX_PASSES: usize = 4;
const MAX_PROCESSES: usize = 16_384;
const MAX_DETAILS: usize = 16;

/// 快照身份；重新托管只改变父进程，不改变启动时间。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Process {
    pid: i32,
    parent: i32,
    start: u64,
}

#[derive(Default)]
pub(super) struct Report {
    adopted: usize,
    gaps: usize,
    details: Vec<String>,
}

impl Report {
    fn gap(&mut self, detail: String) {
        self.gaps += 1;
        if self.details.len() < MAX_DETAILS {
            self.details.push(detail);
        }
    }

    pub(super) fn annotate(&self, binding: &mut Binding) {
        if self.gaps == 0 {
            return;
        }
        binding.state = BindingState::Degraded;
        let detail = format!(
            "pre-existing descendant coverage incomplete ({} gaps): {}{}",
            self.gaps,
            self.details.join("; "),
            if self.gaps > self.details.len() {
                "; further gaps omitted"
            } else {
                ""
            }
        );
        log::warn!("binding {}: {detail}", binding.request.binding_id);
        binding.message = Some(match binding.message.take() {
            Some(message) => format!("{message}; {detail}"),
            None => detail,
        });
    }
}

/// 隔离 procfs 与 map 操作，以便构造确定性的并发调度。
pub(super) trait Surface {
    fn validate_namespace(&self) -> io::Result<()>;
    fn tree(&self, root: i32) -> io::Result<Vec<Process>>;
    fn process(&self, pid: i32) -> io::Result<Option<Process>>;
    fn domain(&self, pid: i32) -> io::Result<Option<u32>>;
    fn adopt(&self, parent: i32, child: i32, domain: u32) -> io::Result<bool>;
    fn revoke(&self, pid: i32, domain: u32) -> io::Result<()>;
}

impl Surface for PinnedEngine {
    fn validate_namespace(&self) -> io::Result<()> {
        validate_initial_procfs()
    }
    fn tree(&self, root: i32) -> io::Result<Vec<Process>> {
        process_tree(root)
    }
    fn process(&self, pid: i32) -> io::Result<Option<Process>> {
        read_process(pid)
    }
    fn domain(&self, pid: i32) -> io::Result<Option<u32>> {
        self.domain_for_pid(pid)
    }
    fn adopt(&self, parent: i32, child: i32, domain: u32) -> io::Result<bool> {
        self.adopt_child(parent, child, domain)
    }
    fn revoke(&self, pid: i32, domain: u32) -> io::Result<()> {
        self.revoke_adopted_child(pid, domain)
    }
}

pub(super) fn adopt(
    surface: &impl Surface,
    root: i32,
    start: u64,
    kernel_pid: i32,
    control_pid: i32,
    domain: u32,
) -> Report {
    let mut report = Report::default();
    if root != kernel_pid || control_pid != std::process::id() as i32 {
        report.gap(format!("PID namespace prevents descendant adoption (procfs root {root}, kernel root {kernel_pid})"));
        return report;
    }
    if let Err(error) = surface.validate_namespace() {
        report.gap(format!("PID namespace could not be validated: {error}"));
        return report;
    }
    let mut seen = HashMap::new();
    for _ in 0..MAX_PASSES {
        let tree = match surface.tree(root) {
            Ok(tree) => tree,
            Err(error) => {
                report.gap(format!("inspect process tree: {error}"));
                break;
            }
        };
        if !tree
            .iter()
            .any(|task| task.pid == root && task.start == start)
        {
            report.gap(format!("root pid {root} changed identity during adoption"));
            break;
        }
        if !remember(&mut seen, &tree) {
            report.gap("observed process identities exceed 16384 entries".into());
            break;
        }
        let mut progress = false;
        let identities: HashMap<_, _> = tree.iter().map(|task| (task.pid, *task)).collect();
        for child in tree.iter().filter(|task| task.pid != root) {
            let Some(parent) = identities.get(&child.parent) else {
                continue;
            };
            // 写入前复查父子身份；写入后只因身份失效撤销，保留 subreaper 接管的同一进程。
            let result = (|| -> io::Result<bool> {
                if surface
                    .process(parent.pid)?
                    .is_none_or(|current| current.start != parent.start)
                    || surface.process(child.pid)? != Some(*child)
                {
                    return Ok(false);
                }
                if !surface.adopt(parent.pid, child.pid, domain)? {
                    return Ok(false);
                }
                match surface.process(child.pid) {
                    Ok(Some(current)) if current.start == child.start => {
                        report.adopted += 1;
                        Ok(true)
                    }
                    current => {
                        surface.revoke(child.pid, domain)?;
                        report.gap(format!(
                            "pid {} identity could not be confirmed after adoption: {current:?}",
                            child.pid
                        ));
                        Ok(true)
                    }
                }
            })();
            match result {
                Ok(changed) => progress |= changed,
                Err(error) => report.gap(format!("adopt pid {}: {error}", child.pid)),
            }
        }
        if !progress {
            break;
        }
    }
    // 终检绝不写 map：未入域父进程仍可能 fork，不能补收养后宣称覆盖完整。
    match surface.tree(root) {
        Ok(tree) => {
            if !tree
                .iter()
                .any(|task| task.pid == root && task.start == start)
            {
                report.gap(format!("root pid {root} changed identity during adoption"));
            }
            if !remember(&mut seen, &tree) {
                report.gap("observed process identities exceed 16384 entries".into());
            }
            // 保留所有已见身份：祖先退出后接管到树外的存活后代也必须核验。
            for task in seen.into_values() {
                match surface.process(task.pid) {
                    Ok(None) if task.pid != root => continue,
                    Ok(Some(current)) if current.start == task.start => {}
                    Ok(Some(_)) if task.pid != root => continue,
                    identity => {
                        report.gap(format!(
                            "pid {} identity changed during final inspection: {identity:?}",
                            task.pid
                        ));
                        continue;
                    }
                }
                match surface.domain(task.pid) {
                    Ok(Some(actual)) if actual == domain => {}
                    Ok(actual) => report.gap(format!(
                        "pid {} is outside domain {domain} (actual {actual:?})",
                        task.pid
                    )),
                    Err(error) => report.gap(format!("inspect pid {} domain: {error}", task.pid)),
                }
            }
        }
        Err(error) => report.gap(format!("final process tree inspection: {error}")),
    }
    if report.adopted > 0 {
        log::info!("adopted {} pre-existing descendants", report.adopted);
    }
    report
}

fn remember(seen: &mut HashMap<(i32, u64), Process>, tree: &[Process]) -> bool {
    for task in tree {
        if seen.len() == MAX_PROCESSES && !seen.contains_key(&(task.pid, task.start)) {
            return false;
        }
        seen.insert((task.pid, task.start), *task);
    }
    true
}

fn validate_initial_procfs() -> io::Result<()> {
    let init = read_process(1)?.ok_or_else(|| io::Error::other("procfs init disappeared"))?;
    let identity = ebpf_ifc_engine::resolve_process_identity(1, init.start)?;
    // 解析器已核验 procfs init 与调用方 namespace 相同；kernel PID 1 才证明这是初始 namespace。
    if identity.kernel_pid != 1 {
        return Err(io::Error::other(
            "procfs does not expose the initial PID namespace",
        ));
    }
    Ok(())
}

fn read_process(pid: i32) -> io::Result<Option<Process>> {
    let bytes = match fs::read(format!("/proc/{pid}/stat")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let stat = String::from_utf8_lossy(&bytes);
    let (_, tail) = stat
        .rsplit_once(')')
        .ok_or_else(|| io::Error::other("invalid proc stat comm"))?;
    let mut fields = tail.split_whitespace();
    if matches!(fields.next(), Some("Z" | "X")) {
        return Ok(None);
    }
    let invalid = || io::Error::other(format!("invalid proc stat for pid {pid}"));
    let parent = fields
        .next()
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    let start = fields
        .nth(17)
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    Ok(Some(Process { pid, parent, start }))
}

fn process_tree(root: i32) -> io::Result<Vec<Process>> {
    let mut children: HashMap<i32, Vec<Process>> = HashMap::new();
    let root_process =
        read_process(root)?.ok_or_else(|| io::Error::other("root process disappeared"))?;
    let mut count = 0;
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        count += 1;
        if count > MAX_PROCESSES {
            return Err(io::Error::other("process snapshot exceeds 16384 entries"));
        }
        if let Some(task) = read_process(pid)? {
            children.entry(task.parent).or_default().push(task);
        }
    }
    let mut tree = vec![root_process];
    let mut visited = HashSet::from([root]);
    let mut queue = VecDeque::from([root]);
    while let Some(parent) = queue.pop_front() {
        for child in children.get(&parent).into_iter().flatten() {
            if visited.insert(child.pid) {
                tree.push(*child);
                queue.push_back(child.pid);
            }
        }
    }
    Ok(tree)
}

#[cfg(test)]
#[path = "process_adoption_tests.rs"]
mod tests;
