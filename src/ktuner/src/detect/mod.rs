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
    if Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists() {
        return RuntimeEnv::Container;
    }
    if let Ok(cgroup) = fs::read_to_string("/proc/1/cgroup") {
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
    if let Ok(sched) = fs::read_to_string("/proc/1/sched") {
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
    unsafe { libc::access(c_path.as_ptr(), libc::W_OK) == 0 }
}

pub fn gather_system_info() -> Result<SystemInfo> {
    let (cpu_model, cpu_cores) = read_cpu_info()?;
    Ok(SystemInfo {
        kernel_version: read_kernel_version()?,
        os_distro: read_os_distro(),
        cpu_model,
        cpu_cores,
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
    if let Ok(content) = fs::read_to_string("/etc/os-release") {
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
    let mut host_kb: u64 = 0;
    for line in meminfo.lines() {
        if line.starts_with("MemTotal:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let Some(kb_str) = parts.get(1) {
                host_kb = kb_str.parse().unwrap_or(0);
                break;
            }
        }
    }

    let cgroup_kb = read_cgroup_memory_limit_kb();
    let effective_kb = if cgroup_kb > 0 && cgroup_kb < host_kb {
        cgroup_kb
    } else {
        host_kb
    };

    // Floor to GB (unchanged), but never report 0 when the machine has any RAM:
    // a sub-1GB host floored to 0 GB would make every memory-scaled rule
    // misbehave.
    let gb = effective_kb / 1024 / 1024;
    Ok(if gb == 0 && effective_kb > 0 { 1 } else { gb })
}

fn read_cgroup_memory_limit_kb() -> u64 {
    // cgroup v2
    if let Ok(s) = fs::read_to_string("/sys/fs/cgroup/memory.max") {
        let s = s.trim();
        if s != "max" {
            if let Ok(bytes) = s.parse::<u64>() {
                return bytes / 1024;
            }
        }
        return 0;
    }
    // cgroup v1
    if let Ok(s) = fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes") {
        if let Ok(bytes) = s.trim().parse::<u64>() {
            if bytes < 1u64 << 62 {
                return bytes / 1024;
            }
        }
    }
    0
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
    if let Ok(content) = fs::read_to_string(&path) {
        // Current scheduler is enclosed in brackets: "none [mq-deadline] bfq"
        for part in content.split_whitespace() {
            if part.starts_with('[') && part.ends_with(']') {
                return part[1..part.len() - 1].to_string();
            }
        }
        content.trim().to_string()
    } else {
        "unknown".to_string()
    }
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
    let mut nets = Vec::new();
    let net_dir = "/sys/class/net";

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

            let speed_path = format!("/sys/class/net/{name}/speed");
            let speed_mbps = fs::read_to_string(&speed_path)
                .ok()
                .and_then(|s| s.trim().parse::<i64>().ok())
                .map(|s| if s > 0 { s as u64 } else { 0 })
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

pub(crate) fn read_sysctl_u64(path: &str) -> u64 {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn read_thp_enabled() -> String {
    let path = "/sys/kernel/mm/transparent_hugepage/enabled";
    if let Ok(content) = fs::read_to_string(path) {
        for part in content.split_whitespace() {
            if part.starts_with('[') && part.ends_with(']') {
                return part[1..part.len() - 1].to_string();
            }
        }
        content.trim().to_string()
    } else {
        "unknown".to_string()
    }
}

fn read_processes() -> Result<Vec<ProcessInfo>> {
    let mut procs = Vec::new();
    let proc_dir = "/proc";

    if let Ok(entries) = fs::read_dir(proc_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let fname = entry.file_name().to_string_lossy().to_string();
            if fname.parse::<u32>().is_ok() {
                let comm_path = format!("/proc/{fname}/comm");
                if let Ok(comm) = fs::read_to_string(&comm_path) {
                    let comm = comm.trim().to_string();
                    // /proc/<pid>/comm is truncated to 15 chars and for a JVM is
                    // just "java" (likewise "python"/"node"/"beam.smp"), so the
                    // actual service is invisible. Recover it from cmdline.
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
    let cmdline = fs::read_to_string(format!("/proc/{pid}/cmdline")).ok()?;
    // cmdline args are NUL-separated.
    let cmd = cmdline.replace('\0', " ").to_lowercase();
    // Order matters: more specific markers first.
    const MARKERS: &[(&str, &str)] = &[
        ("org.elasticsearch", "elasticsearch"),
        ("elasticsearch", "elasticsearch"),
        ("org.opensearch", "opensearch"),
        ("opensearch", "opensearch"),
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
        ("solr", "solr"),
        ("logstash", "logstash"),
        ("pulsar", "pulsar"),
        ("catalina", "tomcat"),
        ("tomcat", "tomcat"),
        ("jenkins", "jenkins"),
    ];
    for (marker, svc) in MARKERS {
        if cmd.contains(marker) {
            return Some(svc.to_string());
        }
    }
    None
}

impl SystemInfo {
    pub fn has_process(&self, pattern: &str) -> bool {
        self.processes.iter().any(|p| p.name.contains(pattern))
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
    for path in &["/proc/net/tcp", "/proc/net/tcp6"] {
        if let Ok(content) = fs::read_to_string(path) {
            for line in content.lines().skip(1) {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if let Some(state) = fields.get(3) {
                    if *state == "0A" {
                        return true;
                    }
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gather_system_info() {
        let info = gather_system_info().unwrap();
        assert!(!info.kernel_version.is_empty());
        assert!(info.cpu_cores > 0);
        assert!(info.memory_total_gb > 0);
    }

    #[test]
    fn test_detect_disk_type() {
        assert_eq!(detect_disk_type("nvme0n1"), DiskType::NVMe);
        assert_eq!(detect_disk_type("nvme1n1"), DiskType::NVMe);
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
}
