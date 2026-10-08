use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct SystemInfo {
    pub kernel_version: String,
    pub os_distro: String,
    pub cpu_model: String,
    pub cpu_cores: usize,
    pub numa_nodes: usize,
    pub memory_total_gb: u64,
    pub disks: Vec<DiskInfo>,
    pub network: Vec<NetInfo>,
    pub sysctl: SysctlValues,
    pub processes: Vec<ProcessInfo>,
}

#[derive(Debug, Clone)]
pub struct DiskInfo {
    pub name: String,
    pub disk_type: DiskType,
    pub scheduler: String,
    pub available_schedulers: Vec<String>,
    pub nr_requests: u64,
    pub read_ahead_kb: u64,
    pub rq_affinity: u64,
}

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::upper_case_acronyms)]
pub enum DiskType {
    NVMe,
    SSD,
    HDD,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct NetInfo {
    pub name: String,
    pub speed_mbps: u64,
}

#[derive(Debug, Clone)]
pub struct SysctlValues {
    pub swappiness: u64,
    pub dirty_ratio: u64,
    pub dirty_background_ratio: u64,
    pub somaxconn: u64,
    pub tcp_fastopen: u64,
    pub thp_enabled: String,
}

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub name: String,
}

impl std::fmt::Display for DiskType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiskType::NVMe => write!(f, "NVMe"),
            DiskType::SSD => write!(f, "SSD"),
            DiskType::HDD => write!(f, "HDD"),
            DiskType::Unknown => write!(f, "Unknown"),
        }
    }
}

impl std::fmt::Display for DiskInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}({})", self.name, self.disk_type)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeEnv {
    BareHost,
    Container,
}

impl std::fmt::Display for RuntimeEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeEnv::BareHost => write!(f, "物理机/虚拟机"),
            RuntimeEnv::Container => write!(f, "容器"),
        }
    }
}

pub fn detect_runtime_env() -> RuntimeEnv {
    runtime_env_from(Path::new("/"))
}

/// Classify the runtime from the filesystem rooted at `root` (`/` in
/// production; a temp dir in tests).
fn runtime_env_from(root: &Path) -> RuntimeEnv {
    if root.join(".dockerenv").exists() || root.join("run/.containerenv").exists() {
        return RuntimeEnv::Container;
    }
    // systemd's container interface: when systemd runs as PID 1 inside a
    // container it records the manager's name here, taken from the
    // `container=` variable the manager puts in its environment. In a cgroup
    // v2 container this is the only signal that is left — /proc/1/cgroup is
    // then the namespace root `0::/` and PID 1 is a known init, so both checks
    // below report a bare host.
    //
    // `wsl` is the one value that is not a container here: WSL2's own init
    // exports it (systemd documents WSL as "categorized as a container for
    // practical purposes"), but /proc/sys in WSL2 is the WSL kernel the
    // workload itself runs on — not a host kernel shared with other machines —
    // so the bare-host verdict the scope already gets must not change.
    if let Some(manager) = read_text_lossy(&root.join("run/systemd/container")) {
        let manager = manager.trim();
        if !manager.is_empty() && manager != "wsl" {
            return RuntimeEnv::Container;
        }
    }
    // Both files are read lossily: cgroup paths and PID 1's comm (the first
    // token of /proc/1/sched) may hold non-UTF-8 bytes, which made
    // `read_to_string` skip the check and report a container as `BareHost`.
    if let Some(cgroup) = read_text_lossy(&root.join("proc/1/cgroup")) {
        if cgroup.contains("docker")
            || cgroup.contains("kubepods")
            || cgroup.contains("containerd")
            || cgroup.contains("lxc")
        {
            return RuntimeEnv::Container;
        }
    }
    // PID 1 comm fallback: only treat an UNKNOWN init as a container. Bare hosts
    // run a variety of init systems besides systemd/sysvinit (runit, s6,
    // OpenRC, ...), so whitelist those to avoid misclassifying them as
    // containers (which would wrongly mark params read-only and steer the user
    // to the host-export workflow).
    if let Some(sched) = read_text_lossy(&root.join("proc/1/sched")) {
        const KNOWN_INIT: &[&str] = &[
            "systemd",
            "init",
            "runit",
            "s6-svscan",
            "s6-linux-init",
            "openrc-init",
            "upstart",
            "busybox",
            "procd",
            "dumb-init",
        ];
        let comm = sched.split_whitespace().next().unwrap_or("");
        if !KNOWN_INIT.iter().any(|i| comm.starts_with(i)) {
            return RuntimeEnv::Container;
        }
    }
    RuntimeEnv::BareHost
}

fn read_text_lossy(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

pub fn is_param_writable(path: &str) -> bool {
    // Check write permission WITHOUT actually writing. The previous approach
    // (reading the file and writing its content back) had two flaws:
    //   1. It mutated /proc/sys during read-only `check`/`status` runs.
    //   2. For /sys scheduler & THP the read-back includes the full option
    //      list (e.g. "none [mq-deadline] kyber"), which is not a valid value,
    //      so the write always failed and these params were wrongly flagged
    //      read-only — tune/fixall then never touched them.
    // libc::access(W_OK) reflects file mode and read-only mounts (containers)
    // without side effects.
    if !Path::new(path).exists() {
        return false;
    }
    let c_path = match std::ffi::CString::new(path) {
        Ok(p) => p,
        Err(_) => return false,
    };
    // Every ktuner write path runs as root (tune/fix/rollback refuse
    // otherwise), so `writable` describes the ROOT run, not the invoking
    // user. An unprivileged `check`/`why`/`tune --dry-run` — all documented
    // as root-free — used to evaluate access(W_OK) as the caller, so a
    // non-root preview reported EVERY recommendation unwritable and answered
    // "blocked" on a host where `sudo ktuner tune` applies them all. For a
    // non-root caller decide from the facts that bound root instead: the
    // file's write bits (root owns proc sysctls) and the mount's read-only
    // flag, which statvfs reports to anyone.
    if unsafe { libc::geteuid() } == 0 {
        return unsafe { libc::access(c_path.as_ptr(), libc::W_OK) == 0 };
    }
    root_run_can_write(path)
}

/// Whether ROOT can write `path`, judged without being root: the file grants
/// a write bit (a proc sysctl is root-owned 0644, so the owner bit covers it;
/// a write-only 0200 knob also passes) and the mount is not read-only (the
/// container case, which a root access(W_OK) also reports as unwritable).
fn root_run_can_write(path: &str) -> bool {
    let mode = match fs::metadata(path) {
        Ok(metadata) => std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()),
        Err(_) => return false,
    };
    if mode & 0o222 == 0 {
        return false;
    }
    let c_path = match std::ffi::CString::new(path) {
        Ok(p) => p,
        Err(_) => return false,
    };
    let mut buf: libc::statvfs = unsafe { std::mem::zeroed() };
    // f_flag is unsigned long on Linux; ST_RDONLY is the low bit.
    unsafe { libc::statvfs(c_path.as_ptr(), &mut buf) == 0 && buf.f_flag & libc::ST_RDONLY == 0 }
}

pub fn gather_system_info() -> Result<SystemInfo> {
    let (cpu_model, cpu_cores) = read_cpu_info()?;
    Ok(SystemInfo {
        kernel_version: read_kernel_version()?,
        os_distro: read_os_distro(),
        cpu_model,
        // The same scaling input memory_total_gb already is: inside a cgroup
        // CPU limit, every cpu-scaled rule would size its recommendation for
        // processors the workload can never run on.
        cpu_cores: effective_cpu_cores(cpu_cores, read_cgroup_cpu_limit_cores()),
        numa_nodes: read_numa_nodes(),
        memory_total_gb: read_memory_total_gb()?,
        disks: read_disk_info()?,
        network: read_network_info()?,
        sysctl: read_sysctl_values()?,
        processes: read_processes()?,
    })
}

fn read_file_trimmed(path: &str) -> Result<String> {
    fs::read_to_string(path)
        .with_context(|| format!("failed to read {path}"))
        .map(|s| s.trim().to_string())
}

fn read_kernel_version() -> Result<String> {
    read_file_trimmed("/proc/sys/kernel/osrelease")
}

fn read_os_distro() -> String {
    fs::read_to_string("/etc/os-release")
        .map_or_else(|_| "Unknown".to_string(), |c| parse_os_release(&c))
}

/// Pure /etc/os-release parsing: PRETTY_NAME wins; otherwise NAME + VERSION;
/// quotes are stripped; anything else yields "Unknown" (the value shown in
/// every check output).
fn parse_os_release(content: &str) -> String {
    let mut pretty_name = None;
    let mut name = None;
    let mut version = None;
    for line in content.lines() {
        if let Some(val) = line.strip_prefix("PRETTY_NAME=") {
            pretty_name = Some(val.trim_matches('"').to_string());
        } else if let Some(val) = line.strip_prefix("NAME=") {
            name = Some(val.trim_matches('"').to_string());
        } else if let Some(val) = line.strip_prefix("VERSION=") {
            version = Some(val.trim_matches('"').to_string());
        }
    }
    if let Some(pn) = pretty_name {
        return pn;
    }
    if let (Some(n), Some(v)) = (name, version) {
        return format!("{n} {v}");
    }
    "Unknown".to_string()
}

fn read_cpu_info() -> Result<(String, usize)> {
    let cpuinfo = fs::read_to_string("/proc/cpuinfo").context("failed to read /proc/cpuinfo")?;
    let mut model = "Unknown CPU".to_string();
    let mut cores = 0usize;
    for line in cpuinfo.lines() {
        if line.starts_with("processor") {
            cores += 1;
        } else if model == "Unknown CPU" && line.starts_with("model name") {
            if let Some(val) = line.split(':').nth(1) {
                model = val.trim().to_string();
            }
        }
    }
    Ok((model, cores.max(1)))
}

fn read_numa_nodes() -> usize {
    let path = "/sys/devices/system/node";
    if let Ok(entries) = fs::read_dir(path) {
        entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .map(|n| n.starts_with("node"))
                    .unwrap_or(false)
            })
            .count()
    } else {
        1
    }
}

fn read_memory_total_gb() -> Result<u64> {
    let meminfo = fs::read_to_string("/proc/meminfo").context("failed to read /proc/meminfo")?;
    Ok(effective_memory_gb(
        parse_meminfo_total_kb(&meminfo),
        read_cgroup_memory_limit_kb(),
    ))
}

/// Pure /proc/meminfo parsing: the numeric field of the MemTotal line, 0 when
/// absent or unparseable.
fn parse_meminfo_total_kb(content: &str) -> u64 {
    for line in content.lines() {
        if line.starts_with("MemTotal:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let Some(kb_str) = parts.get(1) {
                return kb_str.parse().unwrap_or(0);
            }
        }
    }
    0
}

/// Default huge page size in kB, which is the unit of `vm.nr_hugepages`; 0
/// when /proc/meminfo is unreadable or reports no huge page support.
pub fn read_default_hugepage_kb() -> u64 {
    fs::read_to_string("/proc/meminfo")
        .map(|meminfo| parse_meminfo_hugepagesize_kb(&meminfo))
        .unwrap_or(0)
}

/// Pure /proc/meminfo parsing: the numeric field of the Hugepagesize line, 0
/// when absent or unparseable.
fn parse_meminfo_hugepagesize_kb(content: &str) -> u64 {
    content
        .lines()
        .find_map(|line| line.strip_prefix("Hugepagesize:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kb| kb.parse().ok())
        .unwrap_or(0)
}

/// Effective memory in whole GB: the cgroup limit only when it is a real
/// limit that is smaller than the host; floored to GB; never 0 when any RAM
/// exists (the sub-1GB clamp — a 0 here would mis-scale every memory rule,
/// which is why every memory-scaled rule consumes this one number).
fn effective_memory_gb(host_kb: u64, cgroup_kb: u64) -> u64 {
    let effective_kb = if cgroup_kb > 0 && cgroup_kb < host_kb {
        cgroup_kb
    } else {
        host_kb
    };
    let gb = effective_kb / 1024 / 1024;
    if gb == 0 && effective_kb > 0 {
        1
    } else {
        gb
    }
}

fn read_cgroup_memory_limit_kb() -> u64 {
    let self_cgroup = fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    cgroup_memory_limit_kb_from(Path::new("/sys/fs/cgroup"), &self_cgroup)
}

/// The memory limit that binds a process: the smallest real limit from its
/// own cgroup up to the mount root, 0 when nothing on that chain limits
/// memory.
///
/// The root file alone is not the process's limit. Limits are inherited — a
/// child cgroup can never exceed its ancestors — so the binding limit is the
/// minimum over the chain: a systemd unit with `MemoryMax=4G` on a 64 GB
/// host, or a `--memory 2g` container run with host-shared cgroup
/// namespaces, keeps its deeper `memory.max` while the root file still reads
/// `max`. Reading only the root file made `effective_memory_gb` — the one
/// number every memory-scaled rule consumes — report the host's RAM for a
/// process that actually gets 4 GB, so dirty-page, watermark and huge-page
/// sizing all mis-scaled.
///
/// `root` is the cgroup filesystem root (`/sys/fs/cgroup`) and
/// `self_cgroup` the process's `/proc/self/cgroup` content; both are
/// parameters so the navigation is testable against a synthetic tree.
fn cgroup_memory_limit_kb_from(root: &Path, self_cgroup: &str) -> u64 {
    // cgroup v2: the "0::<path>" line. Preferred over v1, mirroring the
    // root-file order, but only when the chain actually offers memory.max —
    // a hybrid host delegates the memory controller to v1, so the v2 walk
    // finds no file at all and must not answer "no limit" from that.
    if let Some(rel) = cgroup_v2_relative_path(self_cgroup) {
        if let Some(kb) = chain_limit_kb(root, &rel, "memory.max", cgroup_v2_limit_kb) {
            return kb;
        }
    }
    // cgroup v1: the line of the memory controller, walked under the
    // controller's own mount (`/sys/fs/cgroup/memory`).
    if let Some(rel) = cgroup_v1_relative_path(self_cgroup) {
        if let Some(kb) = chain_limit_kb(
            &root.join("memory"),
            &rel,
            "memory.limit_in_bytes",
            cgroup_v1_limit_kb_from_content,
        ) {
            return kb;
        }
    }
    // No navigable /proc/self/cgroup (unreadable, or a layout the walks
    // cannot follow): the root files remain the best approximation — the
    // pre-existing behaviour.
    if let Some(kb) = chain_limit_kb(root, "/", "memory.max", cgroup_v2_limit_kb) {
        return kb;
    }
    chain_limit_kb(
        &root.join("memory"),
        "/",
        "memory.limit_in_bytes",
        cgroup_v1_limit_kb_from_content,
    )
    .unwrap_or(0)
}

/// Pure /proc/self/cgroup parsing: the relative cgroup path of the unified
/// (v2) hierarchy line `0::<path>`, when present.
fn cgroup_v2_relative_path(content: &str) -> Option<String> {
    content
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(str::to_string)
}

/// Pure /proc/self/cgroup parsing: the relative cgroup path of the v1 line
/// for the memory controller (`<hierarchy>:<controllers>:<path>`), when
/// present. The controller list decides, so the v2 line (empty controller
/// field) and other controllers' lines never match.
fn cgroup_v1_relative_path(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let mut fields = line.split(':');
        let hierarchy = fields.next()?;
        let controllers = fields.next()?;
        let path = fields.next()?;
        if !hierarchy.is_empty() && controllers.split(',').any(|c| c == "memory") {
            Some(path.to_string())
        } else {
            None
        }
    })
}

/// Walk the cgroup chain from `rel` up to `root`, reading `file` at every
/// level; the effective limit is the smallest real limit seen, because a
/// child can never exceed its ancestors. Levels with no limit (the v2 `max`
/// sentinel, the v1 unlimited constant, unparsable content) are skipped.
/// `None` when no level of the chain offers the file at all, so the caller
/// can fall back to the next hierarchy.
fn chain_limit_kb(root: &Path, rel: &str, file: &str, parse: fn(&str) -> u64) -> Option<u64> {
    let mut dir = root.join(rel.trim_start_matches('/'));
    let mut saw_limit_file = false;
    let mut best: u64 = 0;
    loop {
        if let Ok(content) = fs::read_to_string(dir.join(file)) {
            saw_limit_file = true;
            let kb = parse(&content);
            if kb > 0 && (best == 0 || kb < best) {
                best = kb;
            }
        }
        if dir == root || !dir.pop() {
            break;
        }
    }
    saw_limit_file.then_some(best)
}

/// cgroup v2 memory.max content → KB, 0 for the "max" (no-limit) sentinel or
/// unparseable content.
fn cgroup_v2_limit_kb(raw: &str) -> u64 {
    let s = raw.trim();
    if s != "max" {
        if let Ok(bytes) = s.parse::<u64>() {
            return bytes / 1024;
        }
    }
    0
}

/// cgroup v1 limit_in_bytes → KB, 0 at/above the 1<<62 "unlimited" sentinel
/// (v1 reports a huge constant rather than "max").
fn cgroup_v1_limit_kb(bytes: u64) -> u64 {
    if bytes < 1u64 << 62 {
        bytes / 1024
    } else {
        0
    }
}

/// cgroup v1 `memory.limit_in_bytes` content → KB, 0 at/above the 1<<62
/// "unlimited" sentinel (v1 reports a huge constant rather than "max") — the
/// content-reading form of [`cgroup_v1_limit_kb`].
fn cgroup_v1_limit_kb_from_content(raw: &str) -> u64 {
    raw.trim()
        .parse::<u64>()
        .map(cgroup_v1_limit_kb)
        .unwrap_or(0)
}

fn read_cgroup_cpu_limit_cores() -> u64 {
    let self_cgroup = fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    cgroup_cpu_limit_cores_from(Path::new("/sys/fs/cgroup"), &self_cgroup)
}

// CPU bandwidth is constrained by both the process's cgroup and its ancestors.
// A readable unlimited v2 chain must not fall through to an unrelated v1 tree;
// an absent v2 CPU controller on a hybrid hierarchy must allow v1 fallback.
fn cgroup_cpu_limit_cores_from(root: &Path, self_cgroup: &str) -> u64 {
    if let Some(rel) = cgroup_v2_relative_path(self_cgroup) {
        if let Some(cores) = cpu_chain_limit(root, &rel, true) {
            return cores;
        }
    }
    let cpu_rel = self_cgroup.lines().find_map(|line| {
        let mut fields = line.splitn(3, ':');
        fields.next()?;
        let controllers = fields.next()?;
        let rel = fields.next()?;
        controllers.split(',').any(|c| c == "cpu").then_some(rel)
    });
    // The common v1 layouts mount cpu alone or together with cpuacct.
    let v1_roots = [
        root.join("cpu"),
        root.join("cpu,cpuacct"),
        root.join("cpuacct,cpu"),
    ];
    if let Some(rel) = cpu_rel {
        for cpu_root in &v1_roots {
            if let Some(cores) = cpu_chain_limit(cpu_root, rel, false) {
                return cores;
            }
        }
    }
    // Preserve the root approximation when membership cannot be followed.
    if let Some(cores) = cpu_chain_limit(root, "/", true) {
        return cores;
    }
    v1_roots
        .iter()
        .find_map(|r| cpu_chain_limit(r, "/", false))
        .unwrap_or(0)
}

fn cpu_chain_limit(root: &Path, rel: &str, v2: bool) -> Option<u64> {
    // Namespace-relative parent components must never walk above this mount.
    let rel = Path::new(rel.trim_start_matches('/'));
    if rel
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }
    let mut dir = root.join(rel);
    let mut best = 0;
    let mut saw_files = false;
    loop {
        let cores = if v2 {
            fs::read_to_string(dir.join("cpu.max"))
                .ok()
                .map(|s| cgroup_v2_cpu_max_cores(&s))
        } else {
            fs::read_to_string(dir.join("cpu.cfs_quota_us"))
                .ok()
                .zip(fs::read_to_string(dir.join("cpu.cfs_period_us")).ok())
                .map(|(quota, period)| cgroup_v1_cfs_cores(&quota, &period))
        };
        if let Some(cores) = cores {
            saw_files = true;
            if cores > 0 && (best == 0 || cores < best) {
                best = cores;
            }
        }
        if dir == root || !dir.pop() {
            break;
        }
    }
    saw_files.then_some(best)
}

/// cgroup v2 cpu.max content → whole cores, 0 for the "max" (no-limit)
/// sentinel or unparseable content. A fractional quota rounds up: a
/// 150000/100000 limit is one and a half CPUs, and scaling by 1 would
/// under-size rules relative to what the workload can actually run.
fn cgroup_v2_cpu_max_cores(raw: &str) -> u64 {
    let mut fields = raw.split_whitespace();
    let quota: u64 = match fields.next() {
        Some(q) if q != "max" => match q.parse() {
            Ok(quota) => quota,
            Err(_) => return 0,
        },
        _ => return 0,
    };
    let period: u64 = fields.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    cores_from_quota_period(quota, period)
}

/// cgroup v1 cfs quota/period pair → whole cores, 0 when the quota is the -1
/// "unlimited" sentinel or either value is unparseable.
fn cgroup_v1_cfs_cores(quota_raw: &str, period_raw: &str) -> u64 {
    let quota: i64 = quota_raw.trim().parse().unwrap_or(-1);
    if quota < 0 {
        return 0;
    }
    let period: u64 = period_raw.trim().parse().unwrap_or(0);
    cores_from_quota_period(quota as u64, period)
}

/// Quota per period → whole cores (ceiling), 0 when there is no real limit.
fn cores_from_quota_period(quota: u64, period: u64) -> u64 {
    if quota == 0 || period == 0 {
        return 0;
    }
    quota.div_ceil(period)
}

/// Effective CPU cores: the cgroup limit only when it is a real limit below
/// the host count; never 0 (a 0 here would mis-scale every cpu-scaled rule,
/// which is why every cpu-scaled rule consumes this one number). Mirrors
/// [`effective_memory_gb`]: /proc/cpuinfo counts the host's processors, and a
/// container with a cpu.max / cfs_quota limit can only run a fraction of
/// them, so rules scaled by `cpu_cores` sized recommendations for processors
/// the workload can never run on.
fn effective_cpu_cores(host_cores: usize, cgroup_cores: u64) -> usize {
    if cgroup_cores > 0 && (cgroup_cores as usize) < host_cores {
        cgroup_cores as usize
    } else {
        host_cores
    }
}

fn read_disk_info() -> Result<Vec<DiskInfo>> {
    let mut disks = Vec::new();
    let block_dir = "/sys/block";

    if let Ok(entries) = fs::read_dir(block_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy().to_string();

            if name.starts_with("loop")
                || name.starts_with("ram")
                || name.starts_with("dm-")
                || name.starts_with("sr")
                || name.starts_with("zram")
                || name.starts_with("nbd")
                || name.starts_with("md")
            {
                continue;
            }

            let disk_type = detect_disk_type(&name);
            let scheduler = read_current_scheduler(&name);
            let available_schedulers = read_available_schedulers(&name);
            let nr_requests = read_nr_requests(&name);
            let read_ahead_kb = read_read_ahead_kb(&name);
            let rq_affinity = read_rq_affinity(&name);

            disks.push(DiskInfo {
                name,
                disk_type,
                scheduler,
                available_schedulers,
                nr_requests,
                read_ahead_kb,
                rq_affinity,
            });
        }
    }

    Ok(disks)
}

fn detect_disk_type(name: &str) -> DiskType {
    if name.starts_with("nvme") {
        return DiskType::NVMe;
    }

    // virtio-blk (vd*) and Xen (xvd*) cloud disks frequently report
    // rotational=1 even when backed by SSD/network storage, so the bare
    // rotational flag would mislabel them HDD and apply spinning-disk tuning.
    // Treat them as SSD-like (the SSD optimizations are safe and beneficial,
    // and a real spinning disk presented as vd* in modern clouds is vanishingly
    // rare).
    let is_virtual = name.starts_with("vd") || name.starts_with("xvd");

    let rotational_path = format!("/sys/block/{name}/queue/rotational");
    if let Ok(val) = fs::read_to_string(&rotational_path) {
        match val.trim() {
            "0" => DiskType::SSD,
            "1" => {
                if is_virtual {
                    DiskType::SSD
                } else {
                    DiskType::HDD
                }
            }
            _ => DiskType::Unknown,
        }
    } else if is_virtual {
        DiskType::SSD
    } else {
        DiskType::Unknown
    }
}

fn read_current_scheduler(name: &str) -> String {
    let path = format!("/sys/block/{name}/queue/scheduler");
    // Current scheduler is enclosed in brackets: "none [mq-deadline] bfq"
    fs::read_to_string(&path).map_or_else(
        |_| "unknown".to_string(),
        |c| active_option(&c).unwrap_or_else(|| c.trim().to_string()),
    )
}

/// The ACTIVE option of a sysfs list file: the token in brackets, wherever it
/// appears ("none [mq-deadline] bfq" → "mq-deadline"). None when the file has
/// no bracketed token (the caller falls back to the raw trimmed content).
/// Shared by the scheduler and THP readers so the two can never drift apart —
/// that copy-paste drift is exactly how the tuner's readback handling
/// historically missed THP.
fn active_option(content: &str) -> Option<String> {
    content
        .split_whitespace()
        .find(|t| t.starts_with('[') && t.ends_with(']') && t.len() > 2)
        .map(|t| t[1..t.len() - 1].to_string())
}

fn read_available_schedulers(name: &str) -> Vec<String> {
    let path = format!("/sys/block/{name}/queue/scheduler");
    if let Ok(content) = fs::read_to_string(&path) {
        content
            .split_whitespace()
            .map(|s| s.trim_matches(|c| c == '[' || c == ']').to_string())
            .collect()
    } else {
        Vec::new()
    }
}

fn read_nr_requests(name: &str) -> u64 {
    let path = format!("/sys/block/{name}/queue/nr_requests");
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn read_read_ahead_kb(name: &str) -> u64 {
    let path = format!("/sys/block/{name}/queue/read_ahead_kb");
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn read_rq_affinity(name: &str) -> u64 {
    let path = format!("/sys/block/{name}/queue/rq_affinity");
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn read_network_info() -> Result<Vec<NetInfo>> {
    read_network_info_from(Path::new("/sys/class/net"))
}

/// Read every interface under `net_dir` together with its link speed.
///
/// The speed lookup is built from the directory entry's raw name, not from
/// its lossy display form: interface names may contain any byte the kernel's
/// naming rules allow (`dev_valid_name` accepts everything except `/`, `:`
/// and whitespace), so a non-UTF-8 name came out of `to_string_lossy` with
/// U+FFFD replacement bytes and the rebuilt `/sys/class/net/<mangled>/speed`
/// path no longer resolved — the interface was reported with speed 0.
/// `net_dir` is a parameter so tests can build a fake sysfs tree.
fn read_network_info_from(net_dir: &Path) -> Result<Vec<NetInfo>> {
    let mut nets = Vec::new();

    if let Ok(entries) = fs::read_dir(net_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "lo"
                || name.starts_with("veth")
                || name.starts_with("br-")
                || name.starts_with("virbr")
                || name == "docker0"
                || name == "bonding_masters"
            {
                continue;
            }

            let speed_path = entry.path().join("speed");
            let speed_mbps = fs::read_to_string(&speed_path)
                .ok()
                .and_then(|s| s.trim().parse::<i64>().ok())
                .map(sanitize_speed_mbps)
                .unwrap_or(0);

            nets.push(NetInfo {
                name: name.clone(),
                speed_mbps,
            });
        }
    }

    Ok(nets)
}

fn read_sysctl_values() -> Result<SysctlValues> {
    Ok(SysctlValues {
        swappiness: read_sysctl_u64("/proc/sys/vm/swappiness"),
        dirty_ratio: read_sysctl_u64("/proc/sys/vm/dirty_ratio"),
        dirty_background_ratio: read_sysctl_u64("/proc/sys/vm/dirty_background_ratio"),
        somaxconn: read_sysctl_u64("/proc/sys/net/core/somaxconn"),
        tcp_fastopen: read_sysctl_u64("/proc/sys/net/ipv4/tcp_fastopen"),
        thp_enabled: read_thp_enabled(),
    })
}

/// Normalize a raw link-speed reading. The kernel prints SPEED_UNKNOWN (-1
/// cast to %u) as 4294967295 for interfaces that are UP but have no
/// negotiated rate (common for macvlan/ipvlan); treat it — and any
/// non-positive value — as unknown (0), not as a real rate, so the
/// >= 10000 10-GbE rules do not misfire.
fn sanitize_speed_mbps(s: i64) -> u64 {
    if s > 0 && s != 4_294_967_295 {
        s as u64
    } else {
        0
    }
}

pub(crate) fn read_sysctl_u64(path: &str) -> u64 {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Signed reader for sysctls whose legal range includes -1 (e.g.
/// kernel.hung_task_warnings, kernel.perf_event_paranoid). The unsigned
/// reader maps "-1" to 0, which is a different, meaningful value.
pub(crate) fn read_sysctl_i64(path: &str) -> i64 {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn read_thp_enabled() -> String {
    let path = "/sys/kernel/mm/transparent_hugepage/enabled";
    fs::read_to_string(path).map_or_else(
        |_| "unknown".to_string(),
        |c| active_option(&c).unwrap_or_else(|| c.trim().to_string()),
    )
}

/// Read a process's `/proc/<pid>/comm` as a name, lossily.
///
/// The kernel allows almost any non-NUL bytes in comm (a process can set
/// them via `prctl(PR_SET_NAME)`, and an executable named with non-UTF-8
/// bytes gives its main thread those bytes too). `read_to_string` rejects
/// such a file outright, which used to make the process invisible to
/// service detection — its rules then did not fire even though the
/// workload was running. A lossy read keeps the ASCII prefix, which is
/// what the boundary matching in `process_name_matches` compares anyway.
fn read_comm_from(path: &str) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).trim().to_string())
}

/// Read a process's `/proc/<pid>/cmdline` as text, lossily.
///
/// The kernel stores argv as raw NUL-terminated bytes, so one non-UTF-8 byte
/// anywhere in a command line makes `read_to_string` reject the whole file.
/// The command line is where a JVM's concrete service and an exporter's full
/// name live, so a rejected read dropped that mapping even though the workload
/// was running. Lossy decoding keeps the ASCII tokens the matching below
/// compares; the ordinary path is unchanged.
fn read_cmdline_from(path: &str) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn read_processes() -> Result<Vec<ProcessInfo>> {
    let mut procs = Vec::new();
    let proc_dir = "/proc";

    if let Ok(entries) = fs::read_dir(proc_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let fname = entry.file_name().to_string_lossy().to_string();
            if fname.parse::<u32>().is_ok() {
                let comm_path = format!("/proc/{fname}/comm");
                if let Some(comm) = read_comm_from(&comm_path) {
                    // A metrics exporter shares its target's name prefix
                    // ("postgres_exporter"), so keeping it in the process list
                    // makes the database rules fire on a host that only scrapes
                    // metrics. It is not the workload, so drop it here rather
                    // than teaching every rule about exporters.
                    if is_monitoring_helper(&fname) {
                        continue;
                    }
                    // A client tool names the service it talks to, not the
                    // service: `clickhouse-client` and `kafka-topics.sh` are
                    // not a database or a broker, and only the host that runs
                    // the daemon should get the daemon's rules.
                    if is_service_tool(&fname) {
                        continue;
                    }
                    // For a JVM the name is just "java" (likewise
                    // "python"/"node"/"beam.smp"), so the actual service is
                    // invisible. Recover it from cmdline.
                    if is_generic_runtime(&comm) {
                        if let Some(svc) = detect_runtime_service(&fname) {
                            procs.push(ProcessInfo { name: svc });
                        }
                    }
                    procs.push(ProcessInfo { name: comm });
                }
            }
        }
    }

    Ok(procs)
}

/// Whether `name` names the process `pattern`, either whole or as the role
/// prefix a multi-process daemon gives its workers (`nginx: worker process`).
fn process_name_matches(name: &str, pattern: &str) -> bool {
    name == pattern
        || name
            .split(|c: char| !c.is_ascii_alphanumeric())
            .next()
            .is_some_and(|token| token == pattern)
}

/// Whether the process named `name` is a metrics collector rather than the
/// workload it monitors. Prometheus-style exporters are named after their target
/// (`postgres_exporter`, `node-exporter`, `postgres_exporter_v2`), so a service
/// match reports PostgreSQL, MySQL, Redis or Node as running on a host that only
/// scrapes metrics, and the database rules then fire.
pub(crate) fn is_monitoring_helper_name(name: &str) -> bool {
    let name = name.to_lowercase();
    name.starts_with("prometheus")
        || name
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|token| token == "exporter")
}

/// Whether `pid` is a metrics collector. The kernel truncates
/// `/proc/<pid>/comm` and the name field of `/proc/<pid>/stat` to 15 bytes,
/// which hides the distinguishing suffix: `postgres_exporter` becomes
/// `postgres_export` and is then indistinguishable from a real PostgreSQL
/// server by name alone. The command line keeps the full name the process was
/// started with, so it decides when the truncated name does not.
pub(crate) fn is_monitoring_helper(pid: &str) -> bool {
    let comm = read_comm_from(&format!("/proc/{pid}/comm")).unwrap_or_default();
    if is_monitoring_helper_name(&comm) {
        return true;
    }
    read_cmdline_from(&format!("/proc/{pid}/cmdline"))
        .map(|cmdline| cmdline_names_helper(cmdline.split('\0')))
        .unwrap_or(false)
}

/// Whether a command line names a metrics collector. Only the program being
/// run decides — argv[0], the payload of a `sh -c` wrapper, or the jar a JVM
/// runs — because a bare `exporter` token in a later argument (a config
/// path, say) does not make the process a collector.
fn cmdline_names_helper<'a>(args: impl Iterator<Item = &'a str>) -> bool {
    let args: Vec<&str> = args.collect();
    let Some(&program) = args.first() else {
        return false;
    };
    if is_monitoring_helper_name(program) {
        return true;
    }
    if is_shell_program(program) {
        // `sh -c "node_exporter --web.listen-address=..."` runs the collector as
        // the payload, not as argv[0]; the payload's first word is its command.
        return args.get(1) == Some(&"-c")
            && args
                .get(2)
                .and_then(|&payload| payload.split_whitespace().next())
                .is_some_and(is_monitoring_helper_name);
    }
    if program.rsplit('/').next().unwrap_or_default() == "java" {
        // A JVM names its program in the jar after `-jar`; argv[0] is only the
        // launcher. `java -jar jmx_prometheus_httpserver-1.2.1.jar 5556
        // config.yaml` is the Prometheus JMX exporter's documented standalone
        // form — a collector whose jar carries no "exporter" token, which is
        // how a metrics-only host kept being detected as a Java workload.
        return args
            .iter()
            .position(|a| *a == "-jar")
            .and_then(|i| args.get(i + 1))
            .is_some_and(|&jar| jvm_jar_names_helper(jar));
    }
    false
}

/// Whether `program` (an argv[0]) is a shell that a script file is run under.
fn is_shell_program(program: &str) -> bool {
    matches!(
        program.rsplit('/').next().unwrap_or_default(),
        "sh" | "bash" | "dash" | "zsh" | "ksh"
    )
}

/// ClickHouse's client-side entry points. The vendor's `clickhouse-client`
/// package ships every one of them as a symlink to the single multi-call
/// `clickhouse` binary, so the kernel truncates the client's comm to a prefix
/// the server binary's own name shares (`clickhouse-clie` / `clickhouse-serv`)
/// and the name alone cannot tell the two apart. The program name can.
const CLICKHOUSE_CLIENT_TOOLS: &[&str] = &[
    "clickhouse-benchmark",
    "clickhouse-client",
    "clickhouse-compressor",
    "clickhouse-format",
    "clickhouse-local",
    "clickhouse-obfuscator",
];

/// Whether the program `program` is a *tool* of a service rather than the
/// service itself.
///
/// A service ktuner identifies through its runtime — the JVM/Erlang daemons in
/// [`RUNTIME_SERVICE_MARKERS`] — is never the process named after it: the
/// daemon runs as `java` (or the Erlang VM) and is decided from the runtime's
/// own command line, so an executable called `kafka-topics.sh`, `spark-submit`
/// or `elasticsearch-keystore` can only be one of its tools. ClickHouse has the
/// same shape through its multi-call binary. Both share the service's name
/// prefix with a `-` boundary, which is what `has_process` matches, so a host
/// that only talks to a service running elsewhere used to get that service's
/// rules and workload classification.
fn program_names_service_tool(program: &str) -> bool {
    let basename = program.rsplit('/').next().unwrap_or(program);
    if CLICKHOUSE_CLIENT_TOOLS.contains(&basename) {
        return true;
    }
    RUNTIME_SERVICE_MARKERS.iter().any(|(_, service)| {
        basename
            .strip_prefix(service)
            .is_some_and(|rest| rest.starts_with('-'))
    })
}

/// Whether `pid` runs a service tool instead of the workload.
///
/// Like [`is_monitoring_helper`], the program decides: argv[0] for a binary
/// (a multi-call tool is started under its own name), and — because the kernel
/// runs a script under its interpreter — the script path in the argument after
/// argv[0] when that is a shell. `/proc/<pid>/comm` keeps the script's own
/// name, which is exactly the name that must not satisfy a service match.
fn is_service_tool(pid: &str) -> bool {
    let Some(cmdline) = read_cmdline_from(&format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let mut args = cmdline.split('\0').filter(|arg| !arg.is_empty());
    let Some(program) = args.next() else {
        return false;
    };
    if program_names_service_tool(program) {
        return true;
    }
    is_shell_program(program) && args.next().is_some_and(program_names_service_tool)
}

/// Whether the jar a JVM runs names a metrics collector. The Prometheus JMX
/// exporter's standalone jars are `jmx_prometheus_httpserver-<v>.jar` (the
/// name carries no "exporter" token), while other JVM collectors carry it
/// (`cassandra_exporter-<v>.jar`).
fn jvm_jar_names_helper(jar: &str) -> bool {
    let basename = jar.rsplit('/').next().unwrap_or(jar);
    is_monitoring_helper_name(basename) || basename.starts_with("jmx_prometheus")
}

fn is_generic_runtime(comm: &str) -> bool {
    matches!(
        comm,
        "java" | "python" | "python3" | "node" | "nodejs" | "ruby" | "beam.smp" | "erlang"
    )
}

/// Map a JVM/interpreter process to the concrete service it runs by scanning
/// its cmdline (main class / jar / script). Returns a canonical service name
/// that matches the has_process() checks used by rules and classification.
fn detect_runtime_service(pid: &str) -> Option<String> {
    let cmdline = read_cmdline_from(&format!("/proc/{pid}/cmdline"))?;
    runtime_service_from_cmdline(&cmdline).map(str::to_string)
}

/// Markers mapped to canonical service names, most specific first.
const RUNTIME_SERVICE_MARKERS: &[(&str, &str)] = &[
    ("org.elasticsearch", "elasticsearch"),
    ("elasticsearch", "elasticsearch"),
    ("org.opensearch", "opensearch"),
    ("opensearch", "opensearch"),
    // Kafka's daemon main classes all live under the connect package (the
    // Connect workers and MirrorMaker 2); the bare org.apache.kafka root is
    // deliberately NOT mapped — clients and tools (org.apache.kafka.clients,
    // .tools) are consumers of the service, not the service itself.
    ("org.apache.kafka.connect", "kafka"),
    ("kafka.kafka", "kafka"),
    ("kafka", "kafka"),
    ("org.apache.zookeeper", "zookeeper"),
    ("zookeeper", "zookeeper"),
    ("org.apache.flink", "flink"),
    ("flink", "flink"),
    ("org.apache.spark", "spark"),
    ("spark", "spark"),
    ("org.apache.cassandra", "cassandra"),
    ("cassandra", "cassandra"),
    ("org.apache.hadoop", "hadoop"),
    ("hadoop", "hadoop"),
    ("hbase", "hbase"),
    ("org.apache.solr", "solr"),
    ("solr", "solr"),
    ("logstash", "logstash"),
    ("org.apache.pulsar", "pulsar"),
    ("pulsar", "pulsar"),
    ("org.apache.catalina", "tomcat"),
    ("catalina", "tomcat"),
    ("tomcat", "tomcat"),
    ("jenkins", "jenkins"),
];

/// Decide the service a generic-runtime process (java/python/node/...) runs
/// from its raw NUL-separated cmdline.
///
/// Only identity-bearing arguments decide: the jar after `-jar`, and the
/// non-option arguments (the main class, a script path). JVM options and
/// their values — a `-D` property, a log path, a classpath entry after
/// `-cp`/`-classpath`/`--class-path` — merely *mention* services, and the
/// old whole-cmdline substring scan matched them:
/// `java -Dlog4j.configurationFile=/opt/spark/conf/... com.example.Main`
/// classified as Spark and fired the streaming rules on an unrelated
/// workload. This is the same class of false positive the name-boundary fix
/// for `has_process` removed (etcdctl vs etcd).
///
/// Each candidate argument is matched by its file-name component only (a
/// directory named after a service is not the program), and a marker matches
/// only as a whole consecutive run of name tokens, so `sparklesh` or
/// `etcdbackup` no longer satisfy `spark`/`etcd`.
fn runtime_service_from_cmdline(cmdline: &str) -> Option<&'static str> {
    // Collect identity-bearing args: everything that is not an option, plus
    // the jar path that follows `-jar` (that one *is* an option's value but
    // names the program).
    let mut candidates: Vec<&str> = Vec::new();
    let mut args = cmdline.split('\0').filter(|a| !a.is_empty());
    while let Some(arg) = args.next() {
        if arg == "-jar" {
            if let Some(jar) = args.next() {
                candidates.push(jar);
            }
        } else if arg == "-cp" || arg == "-classpath" || arg == "--class-path" {
            // The classpath is an option value, not the program: it names
            // libraries (often service jars), not the entrypoint.
            let _ = args.next();
        } else if !arg.starts_with('-') {
            candidates.push(arg);
        }
    }

    let marker_tokens: Vec<Vec<String>> = RUNTIME_SERVICE_MARKERS
        .iter()
        .map(|(marker, _)| name_tokens(marker))
        .collect();
    let names: Vec<CandidateName> = candidates
        .iter()
        .map(|c| {
            let basename = c.rsplit('/').next().unwrap_or(c);
            CandidateName {
                // The class name is the last dotted component: a main class is
                // the program, its package only says where the program lives.
                class_tokens: name_tokens(basename.rsplit('.').next().unwrap_or(basename)),
                tokens: name_tokens(basename),
                is_class: is_class_name(basename),
            }
        })
        .collect();

    for ((_, svc), needle) in RUNTIME_SERVICE_MARKERS.iter().zip(marker_tokens.iter()) {
        if names.iter().any(|name| name.matches(needle)) {
            return Some(svc);
        }
    }
    None
}

/// One candidate argument, tokenized for marker matching.
struct CandidateName {
    /// Tokens of the whole (basename) argument.
    tokens: Vec<String>,
    /// Tokens of its last dotted component, i.e. the class name.
    class_tokens: Vec<String>,
    /// Whether the argument is a dotted identifier chain rather than a file.
    is_class: bool,
}

impl CandidateName {
    /// Whether `needle` identifies this candidate.
    ///
    /// A file name (`kafka_2.13-3.7.0.jar`, `sparklesh-report.py`) is matched
    /// anywhere in its tokens, as before. A class is decided by its own name or
    /// by a package run it owns from the root: `kafka.Kafka` and
    /// `org.apache.zookeeper.server.quorum.QuorumPeerMain` are the services,
    /// while `com.example.kafka.ConsumerApp` merely lives under a package that
    /// mentions kafka and is a client of it, not a broker.
    fn matches(&self, needle: &[String]) -> bool {
        if !self.is_class {
            return token_run_contains(&self.tokens, needle);
        }
        token_run_starts_at(&self.tokens, needle) || token_run_contains(&self.class_tokens, needle)
    }
}

/// Whether an argument reads as a Java class (`com.example.Main`) rather than
/// a file name: every dot-separated component is an identifier.
fn is_class_name(candidate: &str) -> bool {
    candidate.contains('.')
        && candidate.split('.').all(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        })
}

/// Whether `haystack` begins with `needle` as a run of tokens.
fn token_run_starts_at(haystack: &[String], needle: &[String]) -> bool {
    !needle.is_empty() && haystack.len() >= needle.len() && haystack[..needle.len()] == *needle
}

/// Split a name into its runs of ASCII alphanumeric characters (lowercased).
fn name_tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Whether `haystack` contains `needle` as a consecutive run of tokens.
fn token_run_contains(haystack: &[String], needle: &[String]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

impl SystemInfo {
    /// Whether the sampled process list contains `pattern` as a whole process
    /// name or as the role prefix of one.
    ///
    /// A plain substring test conflates helpers that merely mention a service
    /// with the service itself: `etcdctl` matched "etcd" and a daemon's private
    /// helper matched its parent's name. Daemons also name their workers
    /// `nginx: worker process` or `postgres: writer`, so the name is compared
    /// both whole and as its first run of name characters — that keeps
    /// `nginx: worker process` matching while `etcdctl` does not.
    pub fn has_process(&self, pattern: &str) -> bool {
        self.processes
            .iter()
            .any(|p| !is_monitoring_helper_name(&p.name) && process_name_matches(&p.name, pattern))
    }

    /// Exact process-name match. Use this for short names that are substrings of
    /// unrelated processes (e.g. "node" vs the ubiquitous "node_exporter").
    pub fn has_process_exact(&self, name: &str) -> bool {
        self.processes.iter().any(|p| p.name == name)
    }

    pub fn max_net_speed(&self) -> u64 {
        self.network.iter().map(|n| n.speed_mbps).max().unwrap_or(0)
    }

    pub fn param_exists(&self, path: &str) -> bool {
        Path::new(path).exists()
    }

    pub fn has_listen_sockets(&self) -> bool {
        has_tcp_listen_sockets()
    }

    pub fn has_conntrack(&self) -> bool {
        std::path::Path::new("/proc/sys/net/netfilter/nf_conntrack_max").exists()
            || std::path::Path::new("/proc/sys/net/nf_conntrack_max").exists()
    }
}

fn has_tcp_listen_sockets() -> bool {
    [" /proc/net/tcp", "/proc/net/tcp6"]
        .iter()
        .map(|p| p.trim())
        .any(|path| {
            fs::read_to_string(path)
                .map(|content| has_listen_socket(&content))
                .unwrap_or(false)
        })
}

/// Pure /proc/net/tcp (or tcp6) table parse: true when any row is in LISTEN
/// (state "0A", 4th whitespace field). The header line is skipped; any other
/// state (01 established, 06 time-wait, ...) is not a listener. This gates
/// the somaxconn and syn-backlog rules, so a misparse either recommends a
/// needless change or skips a needed one.
fn has_listen_socket(content: &str) -> bool {
    content
        .lines()
        .skip(1)
        .any(|line| line.split_whitespace().nth(3) == Some("0A"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info_with_processes(names: &[&str]) -> SystemInfo {
        SystemInfo {
            kernel_version: String::new(),
            os_distro: String::new(),
            cpu_model: String::new(),
            cpu_cores: 1,
            numa_nodes: 1,
            memory_total_gb: 1,
            disks: vec![],
            network: vec![],
            sysctl: SysctlValues {
                swappiness: 60,
                dirty_ratio: 20,
                dirty_background_ratio: 10,
                somaxconn: 128,
                tcp_fastopen: 0,
                thp_enabled: "always".to_string(),
            },
            processes: names
                .iter()
                .map(|name| ProcessInfo {
                    name: name.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn test_gather_system_info() {
        let info = gather_system_info().unwrap();
        assert!(!info.kernel_version.is_empty());
        assert!(info.cpu_cores > 0);
        assert!(info.memory_total_gb > 0);
    }

    #[test]
    fn parse_os_release_prefers_pretty_name() {
        // PRETTY_NAME wins even when NAME/VERSION appear first; surrounding
        // quotes are stripped.
        let content =
            "NAME=\"Alinux\"\nVERSION=\"3 (Hupo)\"\nPRETTY_NAME=\"Alinux 3 (Hupo Dragon)\"\n";
        assert_eq!(parse_os_release(content), "Alinux 3 (Hupo Dragon)");
        // Last PRETTY_NAME wins (os-release keys are unique, but be explicit).
        assert_eq!(
            parse_os_release("PRETTY_NAME=\"A\"\nPRETTY_NAME=\"B\"\n"),
            "B"
        );
    }

    #[test]
    fn parse_os_release_falls_back_to_name_version() {
        assert_eq!(
            parse_os_release("NAME=\"Alinux\"\nVERSION=\"3\"\n"),
            "Alinux 3"
        );
        // NAME alone (no VERSION) is not enough.
        assert_eq!(parse_os_release("NAME=\"Alinux\"\n"), "Unknown");
        // Empty or unrelated content.
        assert_eq!(parse_os_release(""), "Unknown");
        assert_eq!(parse_os_release("ID=alinux\nHOME_URL=\"x\"\n"), "Unknown");
    }

    #[test]
    fn parse_meminfo_total_kb_reads_the_memtotal_line() {
        let typical = "MemTotal:       16384000 kB\nMemFree:         8000000 kB\n";
        assert_eq!(parse_meminfo_total_kb(typical), 16384000);
        // MemTotal not first line is still found.
        let later = "MemFree: 1 kB\nMemTotal:       2048 kB\n";
        assert_eq!(parse_meminfo_total_kb(later), 2048);
        // Absent -> 0.
        assert_eq!(parse_meminfo_total_kb("MemFree: 1 kB\n"), 0);
        // Garbage number -> 0, not a panic.
        assert_eq!(parse_meminfo_total_kb("MemTotal: not-a-number kB\n"), 0);
    }

    #[test]
    fn parse_meminfo_hugepagesize_kb_reads_the_default_size() {
        let x86 = "MemTotal: 16384000 kB\nHugePages_Total: 0\nHugepagesize:       2048 kB\n";
        assert_eq!(parse_meminfo_hugepagesize_kb(x86), 2048);
        // aarch64 with 64K base pages defaults to 512 MiB huge pages.
        assert_eq!(
            parse_meminfo_hugepagesize_kb("Hugepagesize:     524288 kB\n"),
            524288
        );
        // No huge page support, or garbage -> 0, not a panic.
        assert_eq!(parse_meminfo_hugepagesize_kb("MemTotal: 1 kB\n"), 0);
        assert_eq!(parse_meminfo_hugepagesize_kb("Hugepagesize: big kB\n"), 0);
    }

    #[test]
    fn effective_memory_gb_prefers_smaller_cgroup_limit() {
        // 16 GB host, 2 GB cgroup limit -> 2.
        assert_eq!(effective_memory_gb(16 * 1024 * 1024, 2 * 1024 * 1024), 2);
        // No cgroup limit (0) -> host value.
        assert_eq!(effective_memory_gb(16 * 1024 * 1024, 0), 16);
        // Cgroup limit at/above host -> host.
        assert_eq!(effective_memory_gb(1024, 2048), 1);
    }

    #[test]
    fn effective_memory_gb_clamps_sub_gb_hosts() {
        // 512 MB must not floor to 0 — the documented past bug class.
        assert_eq!(effective_memory_gb(512 * 1024, 0), 1);
        // Exactly 1 GB stays 1.
        assert_eq!(effective_memory_gb(1024 * 1024, 0), 1);
        // 1.5 GB floors to 1.
        assert_eq!(effective_memory_gb((1.5 * 1024.0 * 1024.0) as u64, 0), 1);
        // No RAM at all stays 0.
        assert_eq!(effective_memory_gb(0, 0), 0);
        // The clamp applies to the cgroup limit too (a 512 MB container).
        assert_eq!(effective_memory_gb(16 * 1024 * 1024, 512 * 1024), 1);
    }

    #[test]
    fn cgroup_v2_limit_kb_handles_max_sentinel() {
        // "max" is the no-limit sentinel, not a value.
        assert_eq!(cgroup_v2_limit_kb("max\n"), 0);
        assert_eq!(cgroup_v2_limit_kb("2147483648\n"), 2 * 1024 * 1024);
        // Garbage / empty -> 0.
        assert_eq!(cgroup_v2_limit_kb("garbage"), 0);
        assert_eq!(cgroup_v2_limit_kb(""), 0);
    }

    #[test]
    fn cgroup_v1_limit_kb_rejects_unlimited_sentinel() {
        // A real 2 GB limit converts.
        assert_eq!(cgroup_v1_limit_kb(2 * 1024 * 1024 * 1024), 2 * 1024 * 1024);
        // v1's unlimited sentinel and anything at/above it -> 0.
        assert_eq!(cgroup_v1_limit_kb(1u64 << 62), 0);
        assert_eq!(cgroup_v1_limit_kb(1u64 << 63), 0);
        // Just below the sentinel is still a real limit.
        assert_eq!(
            cgroup_v1_limit_kb((1u64 << 62) - 1024),
            (1u64 << 62) / 1024 - 1
        );
    }

    #[test]
    fn effective_cpu_cores_clamps_to_the_cgroup_limit() {
        // A 2-CPU quota on a 128-core host scales by 2, not 128.
        assert_eq!(effective_cpu_cores(128, 2), 2);
        // A single-core quota never floors to 0 — every cpu-scaled rule
        // consumes this number, and a 0 would mis-scale all of them.
        assert_eq!(effective_cpu_cores(128, 1), 1);
        // No limit (0) and a limit at/above the host count keep the host.
        assert_eq!(effective_cpu_cores(8, 0), 8);
        assert_eq!(effective_cpu_cores(2, 64), 2);
    }

    #[test]
    fn cgroup_v2_cpu_max_handles_max_sentinel_and_ceil() {
        // "max" is the no-limit sentinel, not a value.
        assert_eq!(cgroup_v2_cpu_max_cores("max 100000\n"), 0);
        // 200000 per 100000 period is exactly 2 CPUs.
        assert_eq!(cgroup_v2_cpu_max_cores("200000 100000\n"), 2);
        // 150000 per 100000 is one and a half CPUs, rounded up to 2.
        assert_eq!(cgroup_v2_cpu_max_cores("150000 100000\n"), 2);
        // A missing period is not a parseable limit.
        assert_eq!(cgroup_v2_cpu_max_cores("200000\n"), 0);
        assert_eq!(cgroup_v2_cpu_max_cores("garbage"), 0);
        assert_eq!(cgroup_v2_cpu_max_cores(""), 0);
    }

    #[test]
    fn cgroup_v1_cfs_cores_rejects_unlimited_sentinel() {
        // -1 is v1's no-limit sentinel for the quota.
        assert_eq!(cgroup_v1_cfs_cores("-1\n", "100000\n"), 0);
        // A real 200000/100000 quota converts to 2 cores.
        assert_eq!(cgroup_v1_cfs_cores("200000\n", "100000\n"), 2);
        // Garbage on either side is not a limit.
        assert_eq!(cgroup_v1_cfs_cores("garbage", "100000\n"), 0);
        assert_eq!(cgroup_v1_cfs_cores("200000\n", "garbage"), 0);
    }

    #[test]
    fn cgroup_v2_relative_path_reads_the_unified_line() {
        // The v2 line may sit among v1 lines; the path after "0::" is the
        // process's own cgroup, relative to the mount root.
        assert_eq!(
            cgroup_v2_relative_path("11:blkio:/init.scope\n0::/system.slice/ktuner.service\n")
                .as_deref(),
            Some("/system.slice/ktuner.service")
        );
        // A container with a private cgroup namespace sits at the root.
        assert_eq!(cgroup_v2_relative_path("0::/\n").as_deref(), Some("/"));
        // No unified line -> None.
        assert_eq!(cgroup_v2_relative_path("11:memory:/init.scope\n"), None);
        assert_eq!(cgroup_v2_relative_path(""), None);
    }

    #[test]
    fn cgroup_v1_relative_path_reads_the_memory_controller_line() {
        let content =
            "10:cpu,cpuacct:/user.slice\n11:memory:/system.slice/db.service\n0::/init.scope\n";
        assert_eq!(
            cgroup_v1_relative_path(content).as_deref(),
            Some("/system.slice/db.service")
        );
        // The controller list decides: other controllers' lines and the v2
        // line (empty controller field) are not the memory line.
        assert_eq!(
            cgroup_v1_relative_path("10:cpu,cpuacct:/user.slice\n"),
            None
        );
        assert_eq!(cgroup_v1_relative_path("0::/init.scope\n"), None);
        assert_eq!(cgroup_v1_relative_path(""), None);
    }

    /// A synthetic cgroup filesystem: one `memory.max` per relative path.
    fn cgroup_tree(entries: &[(&str, &str)]) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ktuner_cgroup_tree_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        for (rel, content) in entries {
            let dir = root.join(rel.trim_start_matches('/'));
            fs::create_dir_all(&dir).expect("create cgroup dir");
            fs::write(dir.join("memory.max"), content).expect("write memory.max");
        }
        root
    }

    #[test]
    fn cgroup_limit_honours_a_nested_limit() {
        // A systemd unit with MemoryMax=4G on a host whose root and slice
        // set no limit: the root file reads "max", and only the unit's own
        // cgroup names the 4 GB that actually binds the process.
        let root = cgroup_tree(&[
            ("", "max\n"),
            ("system.slice", "max\n"),
            ("system.slice/ktuner.service", "4294967296\n"),
        ]);
        assert_eq!(
            cgroup_memory_limit_kb_from(&root, "0::/system.slice/ktuner.service\n"),
            4 * 1024 * 1024,
            "the unit's own 4 GB limit must bind, not the root's max"
        );
        // A container with a private cgroup namespace sits at the root and
        // keeps seeing exactly the root's value (no limit here).
        assert_eq!(cgroup_memory_limit_kb_from(&root, "0::/\n"), 0);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn cgroup_limit_takes_the_smallest_ancestor_limit() {
        // Limits inherit: an ancestor capped at 2 GB binds a child capped at
        // 4 GB, and a pod's 4 GB sandbox binds deeper cgroups.
        let root = cgroup_tree(&[
            ("", "max\n"),
            ("system.slice", "2147483648\n"),
            ("system.slice/app.service", "4294967296\n"),
            ("kubepods", "8589934592\n"),
            ("kubepods/pod123", "4294967296\n"),
            ("kubepods/pod123/abc", "max\n"),
        ]);
        assert_eq!(
            cgroup_memory_limit_kb_from(&root, "0::/system.slice/app.service\n"),
            2 * 1024 * 1024
        );
        // Deeper than the sandbox limit, with no own limit: the walk stops at
        // the first real limit on the way up.
        assert_eq!(
            cgroup_memory_limit_kb_from(&root, "0::/kubepods/pod123/abc\n"),
            4 * 1024 * 1024
        );
        // A cgroup path deeper than anything in the tree still walks up.
        assert_eq!(
            cgroup_memory_limit_kb_from(&root, "0::/system.slice/app.service/child\n"),
            2 * 1024 * 1024
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn cgroup_limit_falls_back_to_v1_and_then_root_files() {
        let root = std::env::temp_dir().join(format!(
            "ktuner_cgroup_v1_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);

        // Pure v1 layout: the memory controller is mounted at root/memory
        // and there is no unified-hierarchy memory.max anywhere.
        let v1_leaf = root.join("memory/system.slice/db.service");
        fs::create_dir_all(&v1_leaf).expect("create v1 cgroup dir");
        fs::write(v1_leaf.join("memory.limit_in_bytes"), b"2147483648\n").expect("write limit");
        fs::write(
            root.join("memory/memory.limit_in_bytes"),
            b"9223372036854775807\n", // v1's unlimited sentinel at the root
        )
        .expect("write root limit");
        assert_eq!(
            cgroup_memory_limit_kb_from(&root, "11:memory:/system.slice/db.service\n"),
            2 * 1024 * 1024,
            "the v1 memory line must navigate to its own cgroup's limit"
        );

        // No /proc/self/cgroup information at all: the root files decide,
        // exactly as before the chain walk existed.
        fs::write(root.join("memory.max"), b"4294967296\n").expect("write v2 root limit");
        assert_eq!(cgroup_memory_limit_kb_from(&root, ""), 4 * 1024 * 1024);

        // Nothing readable anywhere -> 0 (no limit known).
        let empty = cgroup_tree(&[]);
        assert_eq!(cgroup_memory_limit_kb_from(&empty, "0::/deep/chain\n"), 0);
        assert_eq!(cgroup_memory_limit_kb_from(&empty, ""), 0);
        fs::remove_dir_all(&root).ok();
        fs::remove_dir_all(&empty).ok();
    }

    #[test]
    fn active_option_finds_bracketed_token_anywhere() {
        // The active option is the bracketed token, wherever it appears.
        assert_eq!(
            active_option("none [mq-deadline] bfq").as_deref(),
            Some("mq-deadline")
        );
        assert_eq!(
            active_option("always madvise [never]").as_deref(),
            Some("never")
        );
        assert_eq!(active_option("[none]").as_deref(), Some("none"));
        // No brackets -> None (caller falls back to trimmed content).
        assert_eq!(active_option("none bfq"), None);
        assert_eq!(active_option(""), None);
        // A bare "[]" is not an option.
        assert_eq!(active_option("[] none"), None);
    }

    #[test]
    fn readers_agree_on_active_option() {
        // An unreadable file yields "unknown" from BOTH bracket-scan readers
        // — the shared fallback pins that the deduplication did not change
        // semantics.
        assert_eq!(read_current_scheduler("ktuner_no_such_disk"), "unknown");
        // When the THP file exists (any Linux host), the reader must return
        // exactly the bracketed active option of its real content, proving
        // both readers route through the same active_option helper.
        if let Ok(content) = fs::read_to_string("/sys/kernel/mm/transparent_hugepage/enabled") {
            let expected = active_option(&content).unwrap_or_else(|| content.trim().to_string());
            assert_eq!(read_thp_enabled(), expected);
        } else {
            assert_eq!(read_thp_enabled(), "unknown");
        }
    }

    #[test]
    fn has_listen_socket_detects_only_listen_state() {
        let header = "  sl  local_address rem_address        st tx_queue\n";
        // LISTEN (0A) row -> true.
        assert!(has_listen_socket(&format!(
            "{header}   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000\n"
        )));
        // ESTABLISHED (01) and TIME_WAIT (06) rows are not listeners.
        assert!(!has_listen_socket(&format!(
            "{header}   0: 0100007F:1F90 0100007F:E1D8 01 00000000:00000000\n"
        )));
        assert!(!has_listen_socket(&format!(
            "{header}   0: 0100007F:1F90 0100007F:E1D8 06 00000000:00000000\n"
        )));
        // Header only -> false.
        assert!(!has_listen_socket(header));
        // Short/malformed row -> false (no panic, no match).
        assert!(!has_listen_socket(&format!("{header}garbage\n")));
        assert!(!has_listen_socket(&format!(
            "{header}   0: only two fields\n"
        )));
    }

    #[test]
    fn has_listen_socket_skips_header_line() {
        // The header's 4th field is "st", never "0A", but a malformed header
        // whose 4th field IS 0A-looking must not count as a listener.
        assert!(!has_listen_socket("a b c 0A\n"));
        // A LISTEN row in tcp6-shaped content still counts.
        let tcp6 = "  sl  local_address                         remote_address                        st tx_queue\n";
        assert!(has_listen_socket(
            &format!("{tcp6}   0: 00000000000000000000000000000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000\n")
        ));
    }

    #[test]
    fn test_detect_disk_type() {
        assert_eq!(detect_disk_type("nvme0n1"), DiskType::NVMe);
        assert_eq!(detect_disk_type("nvme1n1"), DiskType::NVMe);
    }

    #[test]
    fn test_sanitize_speed_mbps() {
        // SPEED_UNKNOWN arrives as -1 (and the kernel prints it as 4294967295
        // via %u); both must read as unknown, not as a 4 Gbps link that the
        // >= 10000 rules would treat as sub-10G.
        assert_eq!(sanitize_speed_mbps(-1), 0);
        assert_eq!(sanitize_speed_mbps(0), 0);
        assert_eq!(sanitize_speed_mbps(4_294_967_295), 0);
        // Real rates pass through unchanged.
        assert_eq!(sanitize_speed_mbps(1000), 1000);
        assert_eq!(sanitize_speed_mbps(10000), 10000);
        assert_eq!(sanitize_speed_mbps(25000), 25000);
    }

    #[test]
    fn root_run_can_write_follows_the_mode_bits() {
        // The unprivileged branch's judgement is caller-independent: a file
        // with a write bit on a read-write mount is writable by root (proc
        // sysctls are root-owned 0644, write-only knobs 0200), a file with
        // no write bit is not, and a missing file never is.
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "ktuner_root_write_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        for (name, mode) in [
            ("writable", 0o644),
            ("write_only", 0o200),
            ("sealed", 0o444),
        ] {
            let file = dir.join(name);
            fs::write(&file, b"0").unwrap();
            fs::set_permissions(&file, fs::Permissions::from_mode(mode)).unwrap();
        }
        assert!(root_run_can_write(dir.join("writable").to_str().unwrap()));
        assert!(root_run_can_write(dir.join("write_only").to_str().unwrap()));
        assert!(!root_run_can_write(dir.join("sealed").to_str().unwrap()));
        assert!(!root_run_can_write(dir.join("missing").to_str().unwrap()));
        fs::remove_dir_all(&dir).ok();
    }

    /// `writable` describes the root run, not the invoking user: every write
    /// path requires root, so an unprivileged `check` / `why` /
    /// `tune --dry-run` (all documented root-free) must not report a
    /// root-owned writable knob as unwritable — the preview would answer
    /// "blocked" on a host where `sudo ktuner tune` applies everything.
    /// The parent (root) prepares root-owned fixtures; the assertions run in
    /// an unprivileged child so the caller-relative access(W_OK) is what
    /// breaks, not the fixtures.
    #[test]
    #[ignore = "requires root to prepare root-owned fixtures; only fixture files are written"]
    fn writability_describes_the_root_run_for_an_unprivileged_caller() {
        use std::os::unix::fs::PermissionsExt;
        const CHILD: &str = "KTUNER_WRITABILITY_CHILD";
        if std::env::var_os(CHILD).is_none() {
            assert_eq!(unsafe { libc::geteuid() }, 0, "requires root");
            let dir = std::env::temp_dir().join(format!(
                "ktuner_writable_{}_{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            fs::remove_dir_all(&dir).ok();
            fs::create_dir_all(&dir).unwrap();
            for (name, mode) in [("writable", 0o644), ("sealed", 0o444)] {
                let file = dir.join(name);
                fs::write(&file, b"0").unwrap();
                fs::set_permissions(&file, fs::Permissions::from_mode(mode)).unwrap();
            }
            let output = std::process::Command::new("setpriv")
                .args(["--reuid=65534", "--regid=65534", "--clear-groups"])
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "detect::tests::writability_describes_the_root_run_for_an_unprivileged_caller",
                    "--ignored",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("KTUNER_WRITABILITY_DIR", &dir)
                .output()
                .unwrap();
            fs::remove_dir_all(&dir).ok();
            assert!(
                output.status.success(),
                "unprivileged child failed: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let dir = std::path::PathBuf::from(
            std::env::var_os("KTUNER_WRITABILITY_DIR").expect("fixture dir"),
        );
        assert_eq!(
            unsafe { libc::geteuid() },
            65534,
            "child must run unprivileged"
        );
        // A root-owned 0644 knob (the shape of every proc sysctl): the value
        // an unprivileged caller sees must match what the root run can do.
        assert!(
            is_param_writable(dir.join("writable").to_str().unwrap()),
            "the preview describes the root run, not the invoking user"
        );
        // No write bit at all: unwritable for the root run too.
        assert!(!is_param_writable(dir.join("sealed").to_str().unwrap()));
    }

    #[test]
    fn read_sysctl_i64_parses_negative_values() {
        // "-1" is how the kernel encodes "unlimited" for several sysctls; the
        // unsigned reader maps it to 0 (a different, meaningful value).
        let path = std::env::temp_dir().join(format!(
            "ktuner_read_sysctl_i64_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, b"-1\n").unwrap();
        assert_eq!(read_sysctl_i64(path.to_str().unwrap()), -1);
        std::fs::write(&path, b"2\n").unwrap();
        assert_eq!(read_sysctl_i64(path.to_str().unwrap()), 2);
        std::fs::remove_file(&path).ok();
    }

    /// The kernel's `dev_valid_name` accepts any interface name without `/`,
    /// `:` or whitespace, so a name can be non-UTF-8. The speed lookup must be
    /// built from the directory entry's raw name: rebuilding the path from the
    /// lossy display form (`to_string_lossy` → U+FFFD) made the `speed` file
    /// unresolvable and reported the interface as having no known speed.
    #[test]
    fn network_info_reads_speed_for_a_non_utf8_interface_name() {
        use std::os::unix::ffi::OsStrExt;

        let dir = std::env::temp_dir().join(format!(
            "ktuner_net_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&dir).ok();

        // One interface whose name carries a raw 0xFF byte, one ordinary
        // interface, and the loopback shorthand that must stay skipped.
        let raw = dir.join(std::ffi::OsStr::from_bytes(b"eth\xff0"));
        fs::create_dir_all(&raw).expect("create fake interface dir");
        fs::write(raw.join("speed"), b"10000\n").expect("write speed");
        let plain = dir.join("ens5");
        fs::create_dir_all(&plain).expect("create plain interface dir");
        fs::write(plain.join("speed"), b"1000\n").expect("write speed");
        let lo = dir.join("lo");
        fs::create_dir_all(&lo).expect("create loopback dir");
        fs::write(lo.join("speed"), b"1000\n").expect("write speed");

        let nets = read_network_info_from(&dir).expect("read fake sysfs tree");
        assert_eq!(nets.len(), 2, "lo is skipped: {nets:?}");

        let non_utf8 = nets
            .iter()
            .find(|n| n.speed_mbps == 10000)
            .expect("the non-UTF-8 interface must report its speed");
        // The stored display name is lossy, but the lookup used the raw name.
        assert_eq!(non_utf8.name, "eth\u{fffd}0");
        assert_eq!(
            nets.iter().find(|n| n.name == "ens5").unwrap().speed_mbps,
            1000
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_has_process() {
        let info = SystemInfo {
            kernel_version: String::new(),
            os_distro: String::new(),
            cpu_model: String::new(),
            cpu_cores: 1,
            numa_nodes: 1,
            memory_total_gb: 1,
            disks: vec![],
            network: vec![],
            sysctl: SysctlValues {
                swappiness: 60,
                dirty_ratio: 20,
                dirty_background_ratio: 10,
                somaxconn: 128,
                tcp_fastopen: 0,
                thp_enabled: "always".to_string(),
            },
            processes: vec![
                ProcessInfo {
                    name: "postgres".to_string(),
                },
                ProcessInfo {
                    name: "nginx".to_string(),
                },
            ],
        };

        assert!(info.has_process("postgres"));
        assert!(info.has_process("nginx"));
        assert!(!info.has_process("redis"));
    }

    #[test]
    fn test_has_process_respects_name_boundaries() {
        // A client or dump tool starts with its server's name but is not the
        // server; a server's own truncated name still is.
        let info = info_with_processes(&["etcdctl", "mongodump", "postgres", "postgres_export"]);

        assert!(
            !info.has_process("etcd"),
            "etcdctl is a client, not the etcd server"
        );
        assert!(
            !info.has_process("mongod"),
            "mongodump is a tool, not the mongod server"
        );
        assert!(info.has_process("postgres"));
        // The 15-byte comm of a long postgres* process name is still postgres.
        assert!(info.has_process("postgres_export"));
    }

    #[test]
    fn test_has_process_matches_truncated_comm_and_runtime_names() {
        // "nginx: worker p" is the 15-byte /proc/<pid>/comm of an nginx worker;
        // "elasticsearch" is the canonical name recovered from a JVM cmdline.
        let info = info_with_processes(&["nginx: worker p", "elasticsearch", "redis-server"]);

        assert!(info.has_process("nginx"));
        assert!(info.has_process("elasticsearch"));
        assert!(info.has_process("redis-server"));
    }

    /// The kernel allows non-UTF-8 bytes in comm (set via prctl, or inherited
    /// from an executable named with raw bytes); `read_to_string` rejects
    /// such a file, which made the process invisible to service detection —
    /// its rules then did not fire even though the workload was running.
    /// The lossy read keeps the ASCII prefix the boundary matching compares.
    #[test]
    fn read_comm_survives_non_utf8_bytes() {
        let dir = std::env::temp_dir().join(format!("ktuner_comm_{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let comm_path = dir.join("comm");
        // "postgres" followed by an invalid UTF-8 continuation byte and the
        // newline the kernel appends.
        fs::write(&comm_path, b"postgres\xa0\n").expect("write non-UTF-8 comm");

        let comm = read_comm_from(comm_path.to_str().unwrap()).expect("comm must be readable");
        assert!(
            comm.starts_with("postgres"),
            "the ASCII prefix must survive a lossy read: {comm:?}"
        );
        // The recovered name still identifies the service.
        assert!(
            process_name_matches(&comm, "postgres"),
            "a lossily-read comm must still match its service name: {comm:?}"
        );

        // Guard: the ordinary path is unchanged.
        fs::write(&comm_path, b"nginx: worker process\n").expect("write ordinary comm");
        assert_eq!(
            read_comm_from(comm_path.to_str().unwrap()).as_deref(),
            Some("nginx: worker process")
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// `/proc/1/sched` starts with PID 1's comm, which may hold non-UTF-8
    /// bytes (set via prctl), and a cgroup path may too; `read_to_string`
    /// rejected such a file, so a container fell through to `BareHost`.
    #[test]
    fn runtime_env_survives_non_utf8_pid1_files() {
        let root = std::env::temp_dir().join(format!("ktuner_runtime_{}", std::process::id()));
        let proc1 = root.join("proc/1");
        fs::create_dir_all(&proc1).expect("create temp proc dir");

        // Unknown init whose comm carries a raw byte, plain cgroup v2 path.
        fs::write(proc1.join("cgroup"), b"0::/\n").expect("write cgroup");
        fs::write(proc1.join("sched"), b"app\xff (1, #threads: 1)\n").expect("write sched");
        assert_eq!(runtime_env_from(&root), RuntimeEnv::Container);

        // Known init, but the container marker sits in a non-UTF-8 cgroup path.
        fs::write(proc1.join("cgroup"), b"0::/kubepods/pod\xff\n").expect("write cgroup");
        fs::write(proc1.join("sched"), b"systemd (1, #threads: 1)\n").expect("write sched");
        assert_eq!(runtime_env_from(&root), RuntimeEnv::Container);

        // Guard: a known init with a plain cgroup is still a bare host.
        fs::write(proc1.join("cgroup"), b"0::/init.scope\n").expect("write cgroup");
        assert_eq!(runtime_env_from(&root), RuntimeEnv::BareHost);

        fs::remove_dir_all(&root).ok();
    }

    /// systemd's container interface: when systemd runs as PID 1 inside a
    /// container it records the manager's name in `/run/systemd/container`
    /// (from the `container=` variable the manager puts in its environment).
    /// In a cgroup-v2 container that is the only signal left — `/proc/1/cgroup`
    /// is the namespace root `0::/` and PID 1 is a known init, so every other
    /// check reports a bare host.
    #[test]
    fn runtime_env_reads_the_systemd_container_marker() {
        let root =
            std::env::temp_dir().join(format!("ktuner_container_marker_{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let proc1 = root.join("proc/1");
        fs::create_dir_all(&proc1).expect("create temp proc dir");
        // An LXC container with systemd as PID 1 and a private cgroup namespace.
        fs::write(proc1.join("cgroup"), b"0::/\n").expect("write cgroup");
        fs::write(proc1.join("sched"), b"systemd (1, #threads: 1)\n").expect("write sched");
        let marker = root.join("run/systemd/container");
        fs::create_dir_all(marker.parent().expect("marker parent")).expect("create run/systemd");

        fs::write(&marker, b"lxc\n").expect("write container marker");
        assert_eq!(runtime_env_from(&root), RuntimeEnv::Container);

        // A manager systemd does not know is still a container.
        fs::write(&marker, b"some-unknown-manager\n").expect("write container marker");
        assert_eq!(runtime_env_from(&root), RuntimeEnv::Container);

        // `wsl` is not a container manager here: WSL2's init exports it, but
        // /proc/sys in WSL2 is the WSL kernel the workload runs on, so the
        // bare-host verdict it already reports must not change.
        fs::write(&marker, b"wsl\n").expect("write container marker");
        assert_eq!(runtime_env_from(&root), RuntimeEnv::BareHost);

        // An empty marker file names no manager.
        fs::write(&marker, b"\n").expect("write container marker");
        assert_eq!(runtime_env_from(&root), RuntimeEnv::BareHost);

        // Guard: no marker file at all is still a bare host.
        fs::remove_file(&marker).ok();
        assert_eq!(runtime_env_from(&root), RuntimeEnv::BareHost);

        fs::remove_dir_all(&root).ok();
    }

    /// Wait until a freshly spawned child's `/proc/<pid>/cmdline` carries
    /// `needle`.
    ///
    /// `Command::spawn` can return before the kernel publishes the new argv —
    /// an immediate read is usually empty — so the live-process probes below
    /// wait for the raw bytes instead of racing the exec.
    fn wait_for_cmdline(pid: &str, needle: &[u8]) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let found = fs::read(format!("/proc/{pid}/cmdline"))
                .is_ok_and(|bytes| bytes.windows(needle.len()).any(|window| window == needle));
            if found {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "/proc/{pid}/cmdline never contained {needle:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// The kernel stores argv as raw NUL-terminated bytes, so one non-UTF-8
    /// byte anywhere in a command line makes `read_to_string` reject the whole
    /// file. The service a generic runtime (java/node/...) runs then disappears
    /// from detection even though the workload is running.
    #[test]
    fn detect_runtime_service_reads_non_utf8_cmdline() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::process::CommandExt;
        // The raw byte rides in argv[0]; /bin/sleep ignores argv[0] and reads
        // only the "30" operand, so the process stays alive for the probe.
        let mut child = std::process::Command::new("/bin/sleep")
            .arg0(std::ffi::OsStr::from_bytes(b"/opt/apps/elasticsearch\xff"))
            .arg("30")
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn a process with a non-UTF-8 cmdline");
        let pid = child.id().to_string();
        wait_for_cmdline(&pid, b"elasticsearch\xff");

        let service = detect_runtime_service(&pid);

        child.kill().ok();
        child.wait().ok();

        assert_eq!(
            service.as_deref(),
            Some("elasticsearch"),
            "a raw argv byte must not hide the service the runtime runs"
        );
    }

    /// An exporter's distinguishing suffix survives only in the command line
    /// (comm truncates at 15 bytes), and that command line is raw argv bytes
    /// too. A stray byte in the program path must not stop the collector from
    /// being filtered out of the process list.
    #[test]
    fn monitoring_helper_detection_reads_non_utf8_cmdline() {
        use std::os::unix::ffi::OsStrExt;
        let root = std::env::temp_dir().join(format!("ktuner_stray_byte_{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let dir = root.join(std::ffi::OsStr::from_bytes(b"apps\xff"));
        fs::create_dir_all(&dir).expect("create temp dir");
        let exporter_path = dir.join("postgres_exporter");
        fs::copy("/bin/sleep", &exporter_path).expect("copy sleep to exporter name");

        let mut exporter = std::process::Command::new(&exporter_path)
            .arg("30")
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn exporter-named process");
        let pid = exporter.id().to_string();
        wait_for_cmdline(&pid, b"postgres_exporter");

        let helper = is_monitoring_helper(&pid);

        exporter.kill().ok();
        exporter.wait().ok();
        fs::remove_dir_all(&root).ok();

        assert!(
            helper,
            "a non-UTF-8 program path must not hide the collector"
        );
    }

    /// A client tool of a service names the service it talks to, not the
    /// service: `has_process("clickhouse")` was satisfied by
    /// `clickhouse-client` (ClickHouse ships its client as another name of the
    /// same multi-call binary, and /proc/<pid>/comm truncates both the client
    /// and the server to `clickhouse-clie` / `clickhouse-serv`, so the name
    /// cannot separate them) — the same false-positive class #4100 removed for
    /// the collectors and `etcdctl`, one step further.
    #[test]
    fn service_client_tool_is_not_the_service() {
        let root = std::env::temp_dir().join(format!("ktuner_client_tool_{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(&root).expect("create temp dir");
        let client_path = root.join("clickhouse-client");
        fs::copy("/bin/sleep", &client_path).expect("copy sleep to the client's name");

        let mut client = std::process::Command::new(&client_path)
            .arg("30")
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn a client-named process");
        let pid = client.id().to_string();
        wait_for_cmdline(&pid, b"clickhouse-client");

        // The kernel gives it the truncated server prefix as comm...
        let comm = fs::read_to_string(format!("/proc/{pid}/comm"))
            .expect("client comm")
            .trim()
            .to_string();
        assert_eq!(comm, "clickhouse-clie");
        // The process list must decide from the program, not that prefix.
        let info = gather_system_info().expect("gather system info");
        let listed_as_clickhouse = info.has_process("clickhouse");

        client.kill().ok();
        client.wait().ok();
        fs::remove_dir_all(&root).ok();

        assert!(
            !listed_as_clickhouse,
            "a host running only the ClickHouse client must not look like a database host"
        );
    }

    /// The same for a script: the kernel keeps the script's name in
    /// /proc/<pid>/comm (`kafka-topics.sh`) while argv[0] is the interpreter
    /// that runs it, so a tool script satisfied `has_process("kafka")` — and a
    /// host that only administers a broker elsewhere then ran the streaming
    /// rules and classified as a broker.
    #[test]
    fn service_tool_script_is_not_the_service() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("ktuner_tool_script_{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(&root).expect("create temp dir");
        // A Kafka client tool: the daemon itself runs as a JVM, so an
        // executable named after the service is a tool of it.
        let script = root.join("kafka-topics.sh");
        fs::write(&script, b"#!/bin/sh\nsleep 30\n").expect("write tool script");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("make it runnable");

        let mut tool = std::process::Command::new(&script)
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn a tool-named script");
        let pid = tool.id().to_string();
        wait_for_cmdline(&pid, b"kafka-topics.sh");

        let comm = fs::read_to_string(format!("/proc/{pid}/comm"))
            .expect("tool comm")
            .trim()
            .to_string();
        let info = gather_system_info().expect("gather system info");
        let listed_as_kafka = info.has_process("kafka");

        tool.kill().ok();
        tool.wait().ok();
        fs::remove_dir_all(&root).ok();

        assert_eq!(comm, "kafka-topics.sh");
        assert!(
            !listed_as_kafka,
            "kafka-topics.sh is a client tool, not the broker"
        );
    }

    /// The program names the tool filter must catch and the daemon names it
    /// must keep. ClickHouse is the case a name can never decide: its client,
    /// server and the multi-call binary all share the `clickhouse` prefix, so
    /// the filter is a list of the client-side names — the server name and the
    /// bare multi-call binary (which runs the server) stay services.
    #[test]
    fn service_tool_names_exclude_the_daemons() {
        for tool in [
            // The vendor's clickhouse-client package ships each of these as a
            // symlink to the multi-call binary.
            "clickhouse-benchmark",
            "clickhouse-client",
            "clickhouse-compressor",
            "clickhouse-format",
            "clickhouse-local",
            "clickhouse-obfuscator",
            "/usr/bin/clickhouse-client",
            // Scripts of a JVM-hosted service: the daemon itself is the JVM.
            "kafka-topics.sh",
            "kafka-server-start.sh",
            "/opt/kafka/bin/kafka-consumer-groups.sh",
            "spark-submit",
            "hadoop-daemon.sh",
            "elasticsearch-keystore",
        ] {
            assert!(
                program_names_service_tool(tool),
                "{tool} is a tool, not the service"
            );
        }
        for daemon in [
            "clickhouse",
            "clickhouse-server",
            "/usr/bin/clickhouse",
            "kafka",
            "spark",
            "hadoop",
            "elasticsearch",
            "java",
            "postgres",
            "mysqld",
            "memcached",
            "nginx",
        ] {
            assert!(
                !program_names_service_tool(daemon),
                "{daemon} is the service, not a tool"
            );
        }
    }

    #[test]
    fn test_runtime_service_ignores_option_values_and_directories() {
        // A JVM option's value (a config path) merely mentions a service; the
        // main class decides. Before the boundary fix the whole cmdline was
        // scanned as one string, so this classified as Spark and the streaming
        // rules fired on an unrelated workload.
        let cmdline =
            "java\0-Dlog4j.configurationFile=/opt/spark/conf/log4j2.properties\0com.example.Main";
        assert_eq!(runtime_service_from_cmdline(cmdline), None);

        // A jar's directory names a service the jar is not.
        let cmdline = "java\0-jar\0/opt/hadoop-tools/printer.jar";
        assert_eq!(runtime_service_from_cmdline(cmdline), None);
        // A space-separated classpath value is an option value, not the
        // program: service jars on the classpath must not classify the JVM.
        let cmdline = "java\0-cp\0/opt/spark/jars/spark-core_2.12-3.5.jar\0com.example.Main";
        assert_eq!(runtime_service_from_cmdline(cmdline), None);

        // The long classpath spellings skip their value the same way.
        let cmdline = "java\0--class-path\0/opt/kafka/libs/kafka_2.13-3.7.0.jar\0com.example.Main";
        assert_eq!(runtime_service_from_cmdline(cmdline), None);

        // Markers must match whole name tokens: `sparklesh` is not `spark`.
        let cmdline = "python3\0/opt/app/sparklesh-report.py";
        assert_eq!(runtime_service_from_cmdline(cmdline), None);
    }

    #[test]
    fn test_runtime_service_ignores_package_mentions_in_a_main_class() {
        // A main class whose *package* mentions a service is a client of it,
        // not the service: `com.example.kafka.ConsumerApp` consumes Kafka, it
        // does not run a broker, and classifying it fired the streaming rules
        // on an unrelated workload. Only the class's own name, or a package
        // run the service owns from the root, decides.
        let cmdline =
            "java\u{0}-cp\u{0}/opt/kafka/libs/kafka_2.13-3.7.0.jar\u{0}com.example.kafka.ConsumerApp";
        assert_eq!(runtime_service_from_cmdline(cmdline), None);

        let cmdline = "java\u{0}com.example.spark.JobLauncher";
        assert_eq!(runtime_service_from_cmdline(cmdline), None);

        // Kafka's own broker class still decides, through its class name.
        let cmdline = "java\u{0}kafka.Kafka";
        assert_eq!(runtime_service_from_cmdline(cmdline), Some("kafka"));
    }

    #[test]
    fn test_runtime_service_matches_kafka_daemon_main_classes() {
        // Kafka Connect workers and MirrorMaker 2 are daemons whose main
        // class lives under the org.apache.kafka.connect package: the
        // package run owns the program from the root, exactly like
        // org.apache.zookeeper for QuorumPeerMain. With no kafka root
        // mapped, the boundary rules left every one of them unclassified
        // even though the pre-boundary scan reported Some("kafka").
        for class in [
            "org.apache.kafka.connect.runtime.ConnectDistributed",
            "org.apache.kafka.connect.cli.ConnectStandalone",
            "org.apache.kafka.connect.mirror.MirrorMaker",
        ] {
            let cmdline = format!("java\u{0}-Xmx1g\u{0}{class}");
            assert_eq!(
                runtime_service_from_cmdline(&cmdline),
                Some("kafka"),
                "{class}"
            );
        }

        // Client and tool packages under org.apache.kafka are consumers of
        // the service, not the service itself — the same contract that keeps
        // com.example.kafka.ConsumerApp unclassified.
        for class in [
            "org.apache.kafka.clients.consumer.KafkaConsumer",
            "org.apache.kafka.tools.consumer.ConsoleConsumer",
            "org.apache.kafka.tools.ProducerPerformance",
        ] {
            let cmdline = format!("java\u{0}{class}");
            assert_eq!(runtime_service_from_cmdline(&cmdline), None, "{class}");
        }

        // The legacy MirrorMaker main class and the broker still decide via
        // their own names, and neighbouring services are untouched.
        let cmdline = "java\0kafka.tools.MirrorMaker";
        assert_eq!(runtime_service_from_cmdline(cmdline), Some("kafka"));
        let cmdline = "java\0org.apache.zookeeper.server.quorum.QuorumPeerMain";
        assert_eq!(runtime_service_from_cmdline(cmdline), Some("zookeeper"));
        let cmdline = "java\0org.apache.catalina.startup.Bootstrap\0start";
        assert_eq!(runtime_service_from_cmdline(cmdline), Some("tomcat"));
        let cmdline =
            "java\u{0}-cp\u{0}/opt/kafka/libs/kafka_2.13-3.7.0.jar\u{0}com.example.kafka.ConsumerApp";
        assert_eq!(runtime_service_from_cmdline(cmdline), None);
    }

    #[test]
    fn test_runtime_service_matches_main_classes_and_jars() {
        // Zookeeper's main class.
        let cmdline = "java\0-Xmx1g\0org.apache.zookeeper.server.quorum.QuorumPeerMain";
        assert_eq!(runtime_service_from_cmdline(cmdline), Some("zookeeper"));

        // Kafka: the -jar value names the program.
        let cmdline = "java\0-jar\0/opt/kafka/libs/kafka_2.13-3.7.0.jar\0kafka.Kafka";
        assert_eq!(runtime_service_from_cmdline(cmdline), Some("kafka"));

        // Tomcat's bootstrap class names neither tomcat nor catalina, so the
        // `org.apache.catalina` package run decides.
        let cmdline = "java\0org.apache.catalina.startup.Bootstrap\0start";
        assert_eq!(runtime_service_from_cmdline(cmdline), Some("tomcat"));

        // Solr and Pulsar main classes likewise carry the service only in
        // their package run, not in the class name.
        let cmdline = "java\0org.apache.solr.servlet.SolrDispatchFilter";
        assert_eq!(runtime_service_from_cmdline(cmdline), Some("solr"));
        let cmdline = "java\0org.apache.pulsar.PulsarBrokerStarter\0--broker-conf";
        assert_eq!(runtime_service_from_cmdline(cmdline), Some("pulsar"));

        // Elasticsearch, including a versioned jar basename.
        let cmdline = "java\0-jar\0/usr/share/elasticsearch-8.12.0/lib/elasticsearch-8.12.0.jar";
        assert_eq!(runtime_service_from_cmdline(cmdline), Some("elasticsearch"));
    }

    #[test]
    fn test_monitoring_helper_is_detected_from_a_live_process() {
        // The kernel truncates /proc/<pid>/comm (and the name field of
        // /proc/<pid>/stat) to 15 bytes, so a collector's distinguishing suffix
        // only survives in the command line. Drive the real /proc reader
        // against live processes instead of trusting a name string.
        let dir = std::env::temp_dir().join(format!("ktuner_exporter_{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let exporter_path = dir.join("postgres_exporter");
        fs::copy("/bin/sleep", &exporter_path).expect("copy sleep to exporter name");

        let mut exporter = std::process::Command::new(&exporter_path)
            .arg("30")
            .spawn()
            .expect("spawn exporter-named process");
        let mut service = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");

        let exporter_pid = exporter.id().to_string();
        let service_pid = service.id().to_string();
        let comm = fs::read_to_string(format!("/proc/{exporter_pid}/comm"))
            .expect("exporter comm")
            .trim()
            .to_string();

        let exporter_is_helper = is_monitoring_helper(&exporter_pid);
        let service_is_helper = is_monitoring_helper(&service_pid);
        let comm_alone = is_monitoring_helper_name(&comm);

        exporter.kill().ok();
        service.kill().ok();
        exporter.wait().ok();
        service.wait().ok();
        fs::remove_dir_all(&dir).ok();

        assert_eq!(
            comm.len(),
            15,
            "the kernel truncates the collector name to {comm}"
        );
        assert!(
            !comm_alone,
            "the truncated name cannot identify the collector"
        );
        assert!(
            exporter_is_helper,
            "the command line still names the collector"
        );
        assert!(!service_is_helper, "a plain sleep is not a collector");
    }

    #[test]
    fn test_is_monitoring_helper_name() {
        for helper in [
            "postgres_exporter",
            "mysqld_exporter",
            "redis_exporter",
            "blackbox_exporter",
            "node-exporter",
            "postgres_exporter_v2",
            "exporter",
            "prometheus",
            "prometheus-node-exporter",
            "/usr/local/bin/postgres_exporter",
        ] {
            assert!(is_monitoring_helper_name(helper), "{helper} is a collector");
        }
        for service in [
            "postgres",
            "mysqld",
            "redis-server",
            "nginx",
            "node",
            "etcd",
            "mongod",
            "httpd",
            "/usr/lib/postgresql/16/bin/postgres",
            "/usr/sbin/nginx",
        ] {
            assert!(
                !is_monitoring_helper_name(service),
                "{service} is a service, not a collector"
            );
        }
    }

    #[test]
    fn test_cmdline_names_helper_only_program_names_decide() {
        // argv[0] names the collector whatever the later arguments say...
        assert!(cmdline_names_helper(
            "/usr/local/bin/postgres_exporter\0--config=/etc/agent.conf\0serve".split('\0')
        ));
        assert!(cmdline_names_helper(
            "node-exporter\0--web.listen-address=:9100".split('\0')
        ));
        // ...and a bare "exporter" token in a later argument (a config path,
        // say) does not make the process a collector.
        assert!(!cmdline_names_helper(
            "postgres\0--plugin=exporter\0--config=/opt/exporter.conf".split('\0')
        ));
        assert!(!cmdline_names_helper("sleep\x0030".split('\0')));
        // A `sh -c` wrapper runs the collector as its payload.
        assert!(cmdline_names_helper(
            "sh\0-c\0node_exporter --web.listen-address=:9100".split('\0')
        ));
        assert!(cmdline_names_helper(
            "/bin/bash\0-c\0postgres_exporter --help".split('\0')
        ));
        // The payload's command decides, not its arguments.
        assert!(!cmdline_names_helper("sh\0-c\0echo exporter".split('\0')));
        assert!(!cmdline_names_helper(
            "sh\0-c\0cat /etc/exporter.conf".split('\0')
        ));
        assert!(!cmdline_names_helper("".split('\0')));
    }

    #[test]
    fn cmdline_names_helper_recognizes_a_jvm_collector() {
        // The Prometheus JMX exporter's documented standalone form: the jar
        // after -jar names the program, and its name carries no "exporter"
        // token. Before the JVM form was known, argv[0] "java" hid the
        // collector and a metrics-only host was detected as a Java workload.
        assert!(cmdline_names_helper(
            "java\0-jar\0jmx_prometheus_httpserver-1.2.1.jar\x005556\0config.yaml".split('\0')
        ));
        // A path to the jar resolves by its basename.
        assert!(cmdline_names_helper(
            "/usr/bin/java\0-jar\0/opt/exporters/jmx_prometheus_httpserver.jar".split('\0')
        ));
        // Other JVM collectors carry the token.
        assert!(cmdline_names_helper(
            "java\0-jar\0/opt/cassandra_exporter-2.3.8.jar".split('\0')
        ));
    }

    #[test]
    fn cmdline_names_helper_keeps_jvm_workloads_visible() {
        // The -javaagent form attaches the exporter TO the monitored
        // application: that process is the workload, and only the -jar value
        // may decide.
        assert!(!cmdline_names_helper(
            "java\0-javaagent:jmx_prometheus_javaagent-1.2.1.jar\0-jar\0cassandra.jar".split('\0')
        ));
        // A workload jar with no collector token stays a workload...
        assert!(!cmdline_names_helper(
            "java\0-jar\0/opt/cassandra/apache-cassandra-4.1.0.jar".split('\0')
        ));
        // ...and the arguments around -jar never decide.
        assert!(!cmdline_names_helper(
            "java\0-jar\0app.jar\0--config=/etc/exporter.conf".split('\0')
        ));
        assert!(!cmdline_names_helper(
            "java\0-Xmx1g\0com.example.Main".split('\0')
        ));
        // A dangling -jar decides nothing.
        assert!(!cmdline_names_helper("java\0-jar".split('\0')));
    }
}

#[cfg(test)]
#[path = "cpu_hierarchy_tests.rs"]
mod cpu_hierarchy_tests;
