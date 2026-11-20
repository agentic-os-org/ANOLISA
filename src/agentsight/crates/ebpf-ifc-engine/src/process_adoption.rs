//! 既有后代的 map 控制；不覆盖并发 fork 已建立的绑定或进程状态。

use super::*;

#[repr(C)]
#[derive(Clone, Copy)]
struct PidLabelKey {
    pid: i32,
    domain_id: u32,
    label: u64,
}
// C te_prov 的 168 字节按原样复制，来源 PID/启动时间不可重写为孩子身份。
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Origin([u64; 21]);
unsafe impl aya::Pod for PidLabelKey {}
unsafe impl aya::Pod for Origin {}
const _: () = assert!(std::mem::size_of::<PidLabelKey>() == 16);

#[derive(Clone, Copy)]
enum Entry {
    Global(i32, ProcState),
    Domain(i32, u32, ProcState),
    Root(i32, i32),
    Provenance(i32, u32, u64, Origin),
    Task(i32, u32),
}

trait Maps {
    fn task(&self, pid: i32) -> io::Result<Option<u32>>;
    fn inherited(&self, parent: i32, child: i32, domain: u32) -> io::Result<Vec<Entry>>;
    fn insert(&mut self, entry: Entry) -> io::Result<()>;
    fn matches(&self, entry: Entry) -> io::Result<bool>;
    fn remove(&mut self, entry: Entry) -> io::Result<()>;
}

struct PinnedMaps {
    tasks: HashMap<MapData, i32, u32>,
    global: HashMap<MapData, i32, ProcState>,
    proc: HashMap<MapData, PidDomainKey, ProcState>,
    roots: HashMap<MapData, i32, i32>,
    origins: HashMap<MapData, PidLabelKey, Origin>,
}

impl PinnedMaps {
    fn open(paths: &PinnedEnginePaths) -> io::Result<Self> {
        Ok(Self {
            tasks: pinned_hash_map(paths, "cap_task")?,
            global: pinned_hash_map(paths, "ts_proc")?,
            proc: pinned_hash_map(paths, "ts_proc_domains")?,
            roots: pinned_hash_map(paths, "ts_root")?,
            origins: pinned_hash_map(paths, "ts_proc_prov")?,
        })
    }
}

impl Maps for PinnedMaps {
    fn task(&self, pid: i32) -> io::Result<Option<u32>> {
        map_get_optional(&self.tasks, &pid, "lookup cap_task")
    }
    fn inherited(&self, parent: i32, child: i32, domain: u32) -> io::Result<Vec<Entry>> {
        let mut entries = Vec::new();
        let global = map_get_optional(&self.global, &parent, "lookup parent ts_proc")?;
        let state = map_get(
            &self.proc,
            &PidDomainKey {
                pid: parent,
                domain_id: domain,
            },
            "lookup parent ts_proc_domains",
        )?;
        if let Some(state) = global {
            entries.push(Entry::Global(child, state));
        }
        entries.push(Entry::Domain(child, domain, state));
        entries.push(Entry::Root(
            child,
            map_get(&self.roots, &parent, "lookup parent ts_root")?,
        ));
        // 自然 te_fork 同时复制全局域与活动域中每一个 label bit 的来源。
        for (domain_id, labels) in [
            (0, global.map_or(0, |state| state.labels)),
            (domain, state.labels),
        ] {
            for index in 0..64 {
                let bit = 1u64 << index;
                if labels & bit == 0 {
                    continue;
                }
                let key = PidLabelKey {
                    pid: parent,
                    domain_id,
                    label: bit,
                };
                if let Some(origin) =
                    map_get_optional(&self.origins, &key, "lookup parent provenance")?
                {
                    entries.push(Entry::Provenance(child, domain_id, bit, origin));
                }
            }
        }
        entries.push(Entry::Task(child, domain));
        Ok(entries)
    }
    fn insert(&mut self, entry: Entry) -> io::Result<()> {
        let result = match entry {
            Entry::Global(pid, value) => self.global.insert(pid, value, BPF_NOEXIST),
            Entry::Domain(pid, domain_id, value) => {
                self.proc
                    .insert(PidDomainKey { pid, domain_id }, value, BPF_NOEXIST)
            }
            Entry::Root(pid, value) => self.roots.insert(pid, value, BPF_NOEXIST),
            Entry::Provenance(pid, domain_id, label, value) => self.origins.insert(
                PidLabelKey {
                    pid,
                    domain_id,
                    label,
                },
                value,
                BPF_NOEXIST,
            ),
            Entry::Task(pid, value) => self.tasks.insert(pid, value, BPF_NOEXIST),
        };
        result.map_err(|error| err(format!("adopt {}: {error}", entry.name())))
    }
    fn matches(&self, entry: Entry) -> io::Result<bool> {
        let same_state = |actual: Option<ProcState>, expected: ProcState| {
            actual.is_some_and(|actual| {
                actual.labels == expected.labels && actual.lin_gates == expected.lin_gates
            })
        };
        Ok(match entry {
            Entry::Global(pid, value) => same_state(
                map_get_optional(&self.global, &pid, "read existing global state")?,
                value,
            ),
            Entry::Domain(pid, domain_id, value) => same_state(
                map_get_optional(
                    &self.proc,
                    &PidDomainKey { pid, domain_id },
                    "read existing domain state",
                )?,
                value,
            ),
            Entry::Root(pid, value) => {
                map_get_optional(&self.roots, &pid, "read existing root")? == Some(value)
            }
            Entry::Provenance(pid, domain_id, label, value) => {
                map_get_optional(
                    &self.origins,
                    &PidLabelKey {
                        pid,
                        domain_id,
                        label,
                    },
                    "read existing provenance",
                )? == Some(value)
            }
            Entry::Task(pid, value) => self.task(pid)? == Some(value),
        })
    }
    fn remove(&mut self, entry: Entry) -> io::Result<()> {
        let result = match entry {
            Entry::Global(pid, _) => self.global.remove(&pid),
            Entry::Domain(pid, domain_id, _) => self.proc.remove(&PidDomainKey { pid, domain_id }),
            Entry::Root(pid, _) => self.roots.remove(&pid),
            Entry::Provenance(pid, domain_id, label, _) => self.origins.remove(&PidLabelKey {
                pid,
                domain_id,
                label,
            }),
            Entry::Task(pid, _) => self.tasks.remove(&pid),
        };
        ignore_missing_remove(result, entry.name())
    }
}

impl Entry {
    fn name(self) -> &'static str {
        match self {
            Self::Global(..) => "ts_proc",
            Self::Domain(..) => "ts_proc_domains",
            Self::Root(..) => "ts_root",
            Self::Provenance(..) => "ts_proc_prov",
            Self::Task(..) => "cap_task",
        }
    }
}

fn adopt_maps(maps: &mut impl Maps, parent: i32, child: i32, domain: u32) -> io::Result<bool> {
    if maps.task(child)?.is_some() || maps.task(parent)? != Some(domain) {
        return Ok(false);
    }
    let entries = maps.inherited(parent, child, domain)?;
    for entry in entries {
        // 不回滚 PID key：检查 cap_task 后再删除无法与 BPF fork 原子同步。
        // 失败留下未激活状态；相同值可在下一次收养重用，冲突值不覆盖也不删除。
        if maps.matches(entry)? {
            continue;
        }
        if let Err(error) = maps.insert(entry) {
            if !maps.matches(entry)? {
                return Err(err(format!("{error}; partial state preserved for retry")));
            }
        }
    }
    Ok(true)
}

impl PinnedEngine {
    /// 查询 init namespace TGID 当前绑定的策略域，供只读覆盖核验使用。
    ///
    /// # Errors
    /// map 打开或读取失败时返回错误，不能将其视为未绑定。
    pub fn domain_for_pid(&self, pid: i32) -> io::Result<Option<u32>> {
        let tasks: HashMap<_, i32, u32> = pinned_hash_map(&self.paths, "cap_task")?;
        map_get_optional(&tasks, &pid, "lookup task domain")
    }

    /// 为已绑定父进程的既有孩子复制状态，再激活策略域。
    ///
    /// 调用方必须在操作前后验证进程身份。已绑定的孩子和不属于目标域的
    /// 父进程返回 `Ok(false)`，不会修改 map。激活始终最后执行。
    ///
    /// # Errors
    /// map 操作失败时保留部分状态，避免破坏并发 fork；重试只复用完全匹配的值。
    /// 持续冲突时调用方需报告覆盖缺口，不能宣称已经受保护。
    pub fn adopt_child(&self, parent_pid: i32, child_pid: i32, domain_id: u32) -> io::Result<bool> {
        if parent_pid <= 0 || child_pid <= 0 || domain_id == 0 {
            return Err(err("parent pid, child pid and domain id must all be set"));
        }
        adopt_maps(
            &mut PinnedMaps::open(&self.paths)?,
            parent_pid,
            child_pid,
            domain_id,
        )
    }

    /// 撤销身份复查失败的收养及全局状态，但不删除其他策略域的绑定。
    ///
    /// # Errors
    /// map 无法读取或删除时返回错误，调用方必须保留覆盖缺口告警。
    pub fn revoke_adopted_child(&self, pid: i32, domain_id: u32) -> io::Result<()> {
        let mut maps = PinnedMaps::open(&self.paths)?;
        if maps.task(pid)?.is_some_and(|actual| actual != domain_id) {
            return Ok(());
        }
        let zero = ProcState {
            labels: 0,
            lin_gates: 0,
        };
        let mut entries = vec![
            Entry::Global(pid, zero),
            Entry::Domain(pid, domain_id, zero),
            Entry::Root(pid, 0),
        ];
        for domain in [0, domain_id] {
            for index in 0..64 {
                entries.push(Entry::Provenance(
                    pid,
                    domain,
                    1u64 << index,
                    Origin([0; 21]),
                ));
            }
        }
        entries.push(Entry::Task(pid, domain_id));
        let mut errors = Vec::new();
        for entry in entries.into_iter().rev() {
            if let Err(error) = maps.remove(entry) {
                errors.push(format!("revoke {}: {error}", entry.name()));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(err(errors.join("; ")))
        }
    }
}

#[cfg(test)]
#[path = "process_adoption_tests.rs"]
mod tests;
