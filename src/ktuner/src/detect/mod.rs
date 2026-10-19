use anyhow::{Context, Result};
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

#[cfg(test)]
mod java_application_argument_regressions {
    use super::*;

    #[test]
    fn java_application_arguments_do_not_name_a_service() {
        for cmdline in [
            "java\0-jar\0/opt/client/report.jar\0kafka",
            "java\0-cp\0/opt/client/lib\0com.example.Report\0elasticsearch",
            "java\0com.example.Report\0/opt/data/pulsar.json",
            "java\0com.example.Report\0@config\0kafka",
            "java\0-jar\0/opt/client/report.jar\0@config\0kafka",
        ] {
            assert_eq!(runtime_service_from_cmdline(cmdline), None, "{cmdline:?}");
        }
    }

    #[test]
    fn java_daemon_identity_is_not_overridden_by_application_arguments() {
        assert_eq!(
            runtime_service_from_cmdline("java\0kafka.Kafka\0elasticsearch"),
            Some("kafka")
        );
        assert_eq!(
            runtime_service_from_cmdline("java\0-jar\0kafka_2.13-3.7.0.jar\0pulsar"),
            Some("kafka")
        );
        for cmdline in [
            "java\0-m\0app/kafka.Kafka\0elasticsearch",
            "java\0--module\0app/kafka.Kafka\0elasticsearch",
            "java\0--module=app/kafka.Kafka\0elasticsearch",
        ] {
            assert_eq!(runtime_service_from_cmdline(cmdline), Some("kafka"));
        }
    }

    #[test]
    fn non_java_runtime_matching_keeps_its_existing_behavior() {
        for (cmdline, expected) in [
            ("python3\0/opt/app/spark.py", Some("spark")),
            ("node\0/opt/app/server.js\0kafka", Some("kafka")),
            ("python3\0/opt/app/sparklesh-report.py", None),
        ] {
            assert_eq!(runtime_service_from_cmdline(cmdline), expected);
        }
    }

    #[test]
    fn java_launcher_option_values_keep_the_daemon_identity() {
        for cmdline in [
            "java\0--module-path\0/opt/client/lib\0kafka.Kafka",
            "java\0--add-opens\0java.base/java.lang=ALL-UNNAMED\0kafka.Kafka",
            "java\0--source\x0021\0Report.java\0kafka",
        ] {
            let expected = if cmdline.contains("Report.java") {
                None
            } else {
                Some("kafka")
            };
            assert_eq!(
                runtime_service_from_cmdline(cmdline),
                expected,
                "{cmdline:?}"
            );
        }
    }

    #[test]
    fn java_options_argument_file_does_not_hide_the_entrypoint() {
        for cmdline in [
            "java\0@/tmp/jvm-options.txt\0kafka.Kafka",
            "java\0@/tmp/jvm-options.txt\0-jar\0kafka_2.13-3.7.0.jar",
        ] {
            assert_eq!(runtime_service_from_cmdline(cmdline), Some("kafka"));
        }
    }

    #[test]
    fn java_literal_at_source_name_keeps_its_identity() {
        for cmdline in [
            "java\0--disable-@files\0@spark.java",
            "java\0@@spark.java",
            "java\0--disable-@files\0--source\x0021\0@spark.java",
            "java\0spark.java",
        ] {
            assert_eq!(runtime_service_from_cmdline(cmdline), Some("spark"));
        }
    }

    #[test]
    fn unresolved_java_argument_files_preserve_legacy_matching() {
        for cmdline in [
            "java\0@options.txt\0/opt/client/lib\0kafka.Kafka",
            "java\0-cp\0@classpath.txt\0/opt/client/lib\0kafka.Kafka",
            "java\0-jar\0@jar-arguments.txt\0/opt/client/report.jar\0kafka.Kafka",
            "java\0--module\0@module-arguments.txt\0app/kafka.Kafka",
        ] {
            assert_eq!(runtime_service_from_cmdline(cmdline), Some("kafka"));
        }
    }

    #[test]
    fn a_java_client_argument_must_not_trigger_streaming_read_ahead() {
        let mut processes = vec![ProcessInfo {
            name: "java".into(),
        }];
        if let Some(service) =
            runtime_service_from_cmdline("java\0-jar\0/opt/client/report.jar\0kafka")
        {
            processes.push(ProcessInfo {
                name: service.into(),
            });
        }
        let info = SystemInfo {
            kernel_version: "6.6.0".into(),
            os_distro: String::new(),
            cpu_model: String::new(),
            cpu_cores: 8,
            cpu_cores_host: 8,
            numa_nodes: 1,
            memory_total_gb: 16,
            memory_gb_host: 16,
            disks: vec![DiskInfo {
                name: "fixture-java-client".into(),
                disk_type: DiskType::HDD,
                scheduler: "mq-deadline".into(),
                available_schedulers: vec!["mq-deadline".into()],
                nr_requests: Some(128),
                read_ahead_kb: Some(128),
                rq_affinity: Some(1),
                hidden: false,
                holders: vec![],
            }],
            network: vec![],
            sysctl: SysctlValues {
                swappiness: 60,
                dirty_ratio: 20,
                dirty_background_ratio: 10,
                somaxconn: Some(128),
                thp_enabled: "always".into(),
            },
            processes,
        };
        let workload = crate::profile::classify(&info);
        let evaluation = crate::rules::evaluate_with_workload(&info, &workload).unwrap();
        let disk_recommendations: Vec<_> = evaluation
            .recommendations
            .iter()
            .filter(|recommendation| {
                recommendation.param == "block/fixture-java-client/read_ahead_kb"
            })
            .collect();
        assert!(
            disk_recommendations.is_empty(),
            "a client request must not change the disk's 128-KiB readahead: {disk_recommendations:?}"
        );
        assert_eq!(workload, crate::profile::WorkloadType::Mixed);
        assert!(!crate::services::detect_services(&info).contains(&"Kafka"));
    }
}

#[derive(Debug, Clone)]
pub struct SystemInfo {
    pub kernel_version: String,
    pub os_distro: String,
    pub cpu_model: String,
    pub cpu_cores: usize,
    /// The host's own processors, before the cgroup CPU limit: what
    /// /proc/cpuinfo counts. Equal to `cpu_cores` unless a cgroup limit binds
    /// below it; the rules scale by `cpu_cores`.
    pub cpu_cores_host: usize,
    pub numa_nodes: usize,
    pub memory_total_gb: u64,
    /// The host's MemTotal in whole GB, before the cgroup memory limit: the
    /// same floor-and-clamp `effective_memory_gb` applies, with no limit to
    /// apply. Equal to `memory_total_gb` unless a cgroup limit binds below
    /// it; the rules scale by `memory_total_gb`.
    pub memory_gb_host: u64,
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
    /// `None` when `queue/nr_requests` could not be read or parsed, so the
    /// rules never size a queue from a value the host did not report.
    pub nr_requests: Option<u64>,
    /// `None` when `queue/read_ahead_kb` could not be read or parsed, so the
    /// rules never size a window from a value the host did not report.
    pub read_ahead_kb: Option<u64>,
    /// `None` when `queue/rq_affinity` could not be read or parsed, so the
    /// rules never judge completion steering from a value the host did not
    /// report.
    pub rq_affinity: Option<u64>,
    /// Whether the kernel hides the gendisk (`GENHD_FL_HIDDEN`), from the
    /// disk's `hidden` attribute. A hidden disk is registered without a device
    /// number, so nothing can be mounted on it.
    pub hidden: bool,
    /// The block devices holding this one, from the disk's `holders`
    /// directory (a dm/md/bcache/drbd member). A held disk is claimed
    /// exclusively, so it cannot be the device a filesystem is mounted on.
    pub holders: Vec<String>,
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
    /// `None` when `net.core.somaxconn` could not be read or parsed, so the
    /// rules never size the listen backlog from a value the host did not
    /// report.
    pub somaxconn: Option<u64>,
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
    // Both files are read lossily: cgroup paths and PID 1's comm (the
    // content of /proc/1/comm, the first token of /proc/1/sched) may hold
    // non-UTF-8 bytes, which made `read_to_string` skip the check and report
    // a container as `BareHost`.
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
    //
    // `dumb-init` is deliberately absent: it is "designed to run as PID 1
    // inside minimal container environments" (its own README), and outside
    // containers it is a child of a supervisor rather than PID 1, so its name
    // at PID 1 identifies a container — the same shim class as `tini`, which
    // this list has never included. It was the remaining case of a cgroup-v2
    // container (whose /proc/1/cgroup is the namespace root `0::/`) still being
    // reported as a bare host, now that systemd's own containers carry the
    // /run/systemd/container marker.
    //
    // The name is read from /proc/1/comm first: that file is registered
    // unconditionally (v6.6 `fs/proc/base.c`:3264, :3609), while the sched
    // dump that carries the same name as its first line — `"%s (%d,
    // #threads: %d)"` of `p->comm` (v6.6 `kernel/sched/debug.c`) — is
    // registered only when the kernel is built with CONFIG_SCHED_DEBUG (v6.6
    // `fs/proc/base.c`:3255-3257 for the group leader, :3606-3608 for a
    // thread; upstream removed the gate in v6.15). A kernel built without
    // that option has no sched file at all, so this fallback used to be
    // skipped and a container whose PID 1 has an unknown init was reported
    // as a bare host. The sched file stays as the second source, so a tree
    // that carries only it still decides the same way.
    let pid1_name = read_text_lossy(&root.join("proc/1/comm"))
        .or_else(|| read_text_lossy(&root.join("proc/1/sched")));
    if let Some(name) = pid1_name {
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
        ];
        let comm = name.split_whitespace().next().unwrap_or("");
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
    let (cpu_model, host_cores) = read_cpu_info()?;
    // The effective count is the scaling input every cpu-scaled rule consumes
    // — inside a cgroup CPU limit those rules would otherwise size their
    // recommendation for processors the workload can never run on — and the
    // host count travels with it, so the report says what was clamped.
    let (memory_gb_host, memory_total_gb) = read_memory_total_gb()?;
    Ok(SystemInfo {
        kernel_version: read_kernel_version()?,
        os_distro: read_os_distro(),
        cpu_model,
        cpu_cores: effective_cpu_cores(host_cores, read_cgroup_cpu_limit_cores()),
        cpu_cores_host: host_cores,
        numa_nodes: read_numa_nodes(),
        memory_total_gb,
        memory_gb_host,
        disks: read_disk_info()?,
        network: read_network_info()?,
        sysctl: read_sysctl_values()?,
        processes: read_processes()?,
    })
}

fn read_kernel_version() -> Result<String> {
    read_kernel_version_from("/proc/sys/kernel/osrelease")
}

/// Path-injectable form of [`read_kernel_version`] (the `eval_*_at` idiom) so
/// the raw-byte handling is assertable against a fixture instead of the live
/// `/proc`.
///
/// Read as bytes and decoded lossily: the file is the byte array
/// `init_uts_ns.name.release`, handed to the reader verbatim (v6.6
/// `kernel/utsname_sysctl.c`:93-99), and that array is composed from build
/// inputs — `KERNELRELEASE` is `VERSION.PATCHLEVEL.SUBLEVEL` plus
/// `EXTRAVERSION`/`LOCALVERSION`, written into `include/config/kernel.release`
/// (v6.6 `Makefile`:368-370, :1173-1180) and only length-checked, a `wc -c`
/// against 64, before it becomes `UTS_RELEASE` (v6.6 `Makefile`:1232-1239) —
/// so nothing checks its encoding. A strict read rejected the whole file on one
/// such byte, which made [`gather_system_info`] fail and took every command
/// with it, although the version is reported and compared as a `major.minor`
/// prefix that a replacement character cannot impersonate.
///
/// The single reader for this file: the rules that name a release directory
/// (the `/lib/modules` tree) decode it exactly as this one does.
pub(crate) fn read_kernel_version_from(path: &str) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {path}"))?;
    Ok(String::from_utf8_lossy(&bytes).trim().to_string())
}

fn read_os_distro() -> String {
    read_os_distro_from(&["/etc/os-release", "/usr/lib/os-release"])
}

/// The distro from the first os-release file that exists, in os-release(5)
/// order: `/etc/os-release` takes precedence and is used exclusively when it
/// exists; `/usr/lib/os-release`, the vendor tree's copy, is read only when the
/// former is missing. A file that exists but cannot be read is not a reason to
/// read the other one, so it answers "Unknown".
fn read_os_distro_from(paths: &[&str]) -> String {
    for path in paths {
        match fs::read_to_string(path) {
            Ok(content) => return parse_os_release(&content),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => break,
        }
    }
    "Unknown".to_string()
}

/// Pure /etc/os-release parsing: PRETTY_NAME wins; otherwise NAME + VERSION;
/// values are unquoted the way [`os_release_value`] reads them; anything else
/// yields "Unknown" (the value shown in every check output).
fn parse_os_release(content: &str) -> String {
    let mut pretty_name = None;
    let mut name = None;
    let mut version = None;
    for line in content.lines() {
        if let Some(val) = line.strip_prefix("PRETTY_NAME=") {
            pretty_name = Some(os_release_value(val));
        } else if let Some(val) = line.strip_prefix("NAME=") {
            name = Some(os_release_value(val));
        } else if let Some(val) = line.strip_prefix("VERSION=") {
            version = Some(os_release_value(val));
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

/// One os-release value, read the way os-release(5) writes it: a value may be
/// enclosed in double or single quotes, and the shell special characters
/// (`$`, quotes, backslash, backtick) are escaped with a backslash, "following
/// shell style". Inside double quotes a backslash escapes only those four
/// characters and is literal before anything else; single quotes take every
/// byte literally; an unquoted value drops the backslash of every escape.
fn os_release_value(raw: &str) -> String {
    let raw = raw.trim();
    if let Some(inner) = raw
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
    {
        return inner.to_string();
    }
    let (body, quoted) = match raw
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        Some(inner) => (inner, true),
        None => (raw, false),
    };
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some(next) if !quoted || matches!(next, '$' | '"' | '\\' | '`') => out.push(next),
            Some(next) => {
                out.push('\\');
                out.push(next);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn read_cpu_info() -> Result<(String, usize)> {
    read_cpu_info_from("/proc/cpuinfo")
}

/// Path-injectable form of [`read_cpu_info`] (the `eval_*_at` idiom) so the
/// per-line parse is assertable against a synthetic /proc/cpuinfo.
///
/// Read lossily: the model-name line is the kernel's verbatim copy of the CPU
/// brand string — `get_model_name()` memcpy's 48 bytes out of the CPUID leaves
/// and only trims spaces, with no encoding check (v6.6
/// `arch/x86/kernel/cpu/common.c:788-792`, 7.3.0-rc6:835-839) — and under a
/// hypervisor those bytes are whatever VMM userspace put in the guest's CPUID
/// (`KVM_SET_CPUID2`, `Documentation/virt/kvm/api.rst`). A strict read rejected
/// the whole file on one byte outside UTF-8, which made
/// [`gather_system_info`] fail and took every command with it, although the
/// model string is never reported and the processor count from the same file
/// is what the engine consumes. The lossy decode keeps that count and the
/// ASCII prefix of the model — the treatment [`read_comm_from`] and
/// [`read_cmdline_from`] already give the raw bytes the kernel stores in
/// `/proc/<pid>/*`.
fn read_cpu_info_from(path: &str) -> Result<(String, usize)> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {path}"))?;
    let cpuinfo = String::from_utf8_lossy(&bytes);
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

/// The host's memory and the effective value rules scale by, in whole GB,
/// from one /proc/meminfo read: the host reading is [`effective_memory_gb`]
/// with no limit to apply, so the two agree unless a cgroup limit binds
/// below the host.
fn read_memory_total_gb() -> Result<(u64, u64)> {
    let meminfo = fs::read_to_string("/proc/meminfo").context("failed to read /proc/meminfo")?;
    let host_kb = parse_meminfo_total_kb(&meminfo);
    Ok((
        effective_memory_gb(host_kb, 0),
        effective_memory_gb(host_kb, read_cgroup_memory_limit_kb()),
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
    read_cgroup_memory_limit_kb_at(Path::new("/sys/fs/cgroup"), Path::new("/proc/self/cgroup"))
}

/// Path-injectable form of [`read_cgroup_memory_limit_kb`]: `root` is the
/// cgroup filesystem root and `self_cgroup_path` the process's membership
/// file, both parameters so the read is assertable against a synthetic tree.
///
/// Read as bytes: a cgroup directory name may be any byte except '/' and
/// '\0' (kernfs names; `cgroup_mkdir` additionally rejects '\n'), so a name
/// outside UTF-8 is legal. A strict `read_to_string` rejected the whole
/// file on such a name, which left the membership empty and silently
/// answered from the mount-root approximation instead of the chain that
/// binds the process.
fn read_cgroup_memory_limit_kb_at(root: &Path, self_cgroup_path: &Path) -> u64 {
    let self_cgroup = fs::read(self_cgroup_path).unwrap_or_default();
    cgroup_memory_limit_kb_from(root, &self_cgroup)
}

/// The memory limit that binds a process: the smallest real limit from its
/// accounting chain, 0 when nothing on that chain limits memory. A legacy v1
/// parent with hierarchy disabled is outside that accounting chain.
///
/// The root file alone is not the process's limit. Within a hierarchy, limits
/// are inherited, so the binding limit is the
/// minimum over the chain: a systemd unit with `MemoryMax=4G` on a 64 GB
/// host, or a `--memory 2g` container run with host-shared cgroup
/// namespaces, keeps its deeper `memory.max` while the root file still reads
/// `max`. Reading only the root file made `effective_memory_gb` — the one
/// number every memory-scaled rule consumes — report the host's RAM for a
/// process that actually gets 4 GB, so dirty-page, watermark and huge-page
/// sizing all mis-scaled.
///
/// `root` is the cgroup filesystem root (`/sys/fs/cgroup`) and
/// `self_cgroup` the raw bytes of the process's `/proc/self/cgroup`; both
/// are parameters so the navigation is testable against a synthetic tree.
/// The membership is never decoded: a cgroup name is an arbitrary byte
/// string (kernfs names), and a path rebuilt from a lossy decode would name
/// a directory that does not exist — the walk would fail on every level and
/// the approximation below would silently answer instead.
fn cgroup_memory_limit_kb_from(root: &Path, self_cgroup: &[u8]) -> u64 {
    // cgroup v2: the "0::<path>" line. Preferred over v1, mirroring the
    // root-file order, but only when the chain actually offers memory.max —
    // a hybrid host delegates the memory controller to v1, so the v2 walk
    // finds no file at all and must not answer "no limit" from that.
    if let Some(rel) = cgroup_v2_relative_path(self_cgroup) {
        if let Some(kb) = chain_min_value(root, &rel, "memory.max", cgroup_v2_limit_kb) {
            return kb;
        }
    }
    // cgroup v1: the line of the memory controller, walked under the
    // controller's own mount (`/sys/fs/cgroup/memory`).
    if let Some(rel) = cgroup_v1_relative_path(self_cgroup) {
        if let Some(kb) = chain_min_value(
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
    if let Some(kb) = chain_min_value(root, Path::new("/"), "memory.max", cgroup_v2_limit_kb) {
        return kb;
    }
    chain_min_value(
        &root.join("memory"),
        Path::new("/"),
        "memory.limit_in_bytes",
        cgroup_v1_limit_kb_from_content,
    )
    .unwrap_or(0)
}

/// `/proc/self/cgroup` lines as raw byte slices: split on '\n' with one
/// trailing '\r' removed, the byte-level counterpart of `str::lines`.
fn cgroup_lines(content: &[u8]) -> impl Iterator<Item = &[u8]> {
    content
        .split(|&b| b == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
}

/// A parsed path's raw bytes as a filesystem path: no UTF-8 decode, so the
/// path names the directory the kernel has, byte for byte.
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsStr::from_bytes(bytes))
}

/// A membership path as the directory to join under a mount root: the path
/// is absolute within its hierarchy, so leading separators are dropped
/// before the join (`Path::join` would otherwise let the absolute path
/// replace the mount root entirely).
fn cgroup_membership_dir(rel: &Path) -> PathBuf {
    let bytes = rel.as_os_str().as_bytes();
    let start = bytes.iter().position(|&b| b != b'/').unwrap_or(bytes.len());
    path_from_bytes(&bytes[start..])
}

/// Pure /proc/self/cgroup parsing: the relative cgroup path of the unified
/// (v2) hierarchy line `0::<path>`, when present, as raw bytes.
fn cgroup_v2_relative_path(content: &[u8]) -> Option<PathBuf> {
    cgroup_lines(content)
        .find_map(|line| line.strip_prefix(b"0::"))
        .map(path_from_bytes)
}

/// Pure /proc/self/cgroup parsing: the relative cgroup path of the v1 line
/// for `controller` (`<hierarchy>:<controllers>:<path>`), when present. The
/// controller list decides, so the v2 line (empty controller field) and other
/// controllers' lines never match. The line is split into exactly three fields
/// because the path runs to the end of the line: a cgroup directory may itself
/// be named with a colon (kernfs only forbids '/' and '\0' in names), so an
/// unbounded split truncates the membership at the colon inside the name — the
/// same idiom the CPU membership reader already uses (`splitn(3, ':')`,
/// cpu_hierarchy). Names are kept as bytes.
fn cgroup_v1_controller_relative_path(content: &[u8], controller: &str) -> Option<PathBuf> {
    cgroup_lines(content).find_map(|line| {
        let mut fields = line.splitn(3, |&b| b == b':');
        let hierarchy = fields.next()?;
        let controllers = fields.next()?;
        let path = fields.next()?;
        if !hierarchy.is_empty()
            && controllers
                .split(|&b| b == b',')
                .any(|c| c == controller.as_bytes())
        {
            Some(path_from_bytes(path))
        } else {
            None
        }
    })
}

/// The memory controller's membership line
/// ([`cgroup_v1_controller_relative_path`], kept for the callers that only ever
/// need memory).
fn cgroup_v1_relative_path(content: &[u8]) -> Option<PathBuf> {
    cgroup_v1_controller_relative_path(content, "memory")
}

/// Walk the cgroup chain from `rel` up to `root`, reading `file` at every
/// level; the effective value is the smallest real one seen. A v1 memory walk
/// stops before a parent that disables hierarchical accounting. Levels with no
/// value (the v2 `max` sentinel, the v1 unlimited constant, unparsable content)
/// are skipped.
/// `None` when no level of the chain offers the file at all, so the caller
/// can fall back to the next hierarchy. `parse` sets the unit: the memory
/// walks read kilobytes, the cpuset walk reads the CPUs in a mask.
fn chain_min_value(root: &Path, rel: &Path, file: &str, parse: fn(&str) -> u64) -> Option<u64> {
    // Namespace-relative parent components must never walk above this mount
    // — the same guard cpu_chain_limit carries for the CPU walk.
    let rel = cgroup_membership_dir(rel);
    if rel
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }
    let own_dir = root.join(rel);
    let mut dir = own_dir.clone();
    let mut saw_limit_file = false;
    let mut best: u64 = 0;
    loop {
        // Legacy v1 children are charged to a parent only while that parent
        // enables hierarchy. Unknown state keeps the existing ancestor walk.
        if file == "memory.limit_in_bytes"
            && dir != own_dir
            && fs::read_to_string(dir.join("memory.use_hierarchy"))
                .is_ok_and(|state| state.trim() == "0")
        {
            // The child of such a level is charged to the kernel's root memcg
            // (v5.10 mm/memcontrol.c:5364-5367), whose limit cannot be set
            // (`:3852-3856`), not to the mount root: inside a container the
            // mount root is the container's own cgroup, whose limit that charge
            // never meets. Nothing above the cut binds, so the walk ends here.
            break;
        }
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
    read_cgroup_cpu_limit_cores_at(Path::new("/sys/fs/cgroup"), Path::new("/proc/self/cgroup"))
}

/// Path-injectable form of [`read_cgroup_cpu_limit_cores`]: `root` is the
/// cgroup filesystem root and `self_cgroup_path` the process's membership
/// file, both parameters so the read is assertable against a synthetic tree.
///
/// Read as bytes for the same reason as
/// [`read_cgroup_memory_limit_kb_at`]: a non-UTF-8 cgroup name is legal,
/// and a strict read turned the whole membership file into the empty string,
/// silently answering from the mount-root approximation.
fn read_cgroup_cpu_limit_cores_at(root: &Path, self_cgroup_path: &Path) -> u64 {
    let self_cgroup = fs::read(self_cgroup_path).unwrap_or_default();
    cgroup_cpu_limit_cores_from(root, &self_cgroup)
}

// CPU bandwidth is constrained by both the process's cgroup and its ancestors.
// A readable unlimited v2 chain must not fall through to an unrelated v1 tree;
// an absent v2 CPU controller on a hybrid hierarchy must allow v1 fallback.
//
// The cpuset controller bounds the same number by a second mechanism: it
// decides which CPUs the process may run on, and a container pinned to two of
// a host's sixty-four (a `--cpuset-cpus` container, a Kubernetes pod under the
// static CPU-manager policy, a systemd unit with `AllowedCPUs=`) runs at most
// two CPUs' worth of work in parallel. Sizing cpu-scaled rules for the host's
// sixty-four is the same mis-scaling the bandwidth quota is read to prevent,
// so the effective count is the smaller of the two limits.
fn cgroup_cpu_limit_cores_from(root: &Path, self_cgroup: &[u8]) -> u64 {
    let quota = cgroup_cpu_quota_cores_from(root, self_cgroup);
    let cpuset = cgroup_cpuset_core_count_from(root, self_cgroup);
    match (quota, cpuset) {
        (0, count) | (count, 0) => count,
        (quota, cpuset) => quota.min(cpuset),
    }
}

fn cgroup_cpu_quota_cores_from(root: &Path, self_cgroup: &[u8]) -> u64 {
    if let Some(rel) = cgroup_v2_relative_path(self_cgroup) {
        if let Some(cores) = cpu_chain_limit(root, &rel, true) {
            return cores;
        }
    }
    let cpu_rel = cgroup_lines(self_cgroup).find_map(|line| {
        let mut fields = line.splitn(3, |&b| b == b':');
        fields.next()?;
        let controllers = fields.next()?;
        let rel = fields.next()?;
        controllers
            .split(|&b| b == b',')
            .any(|c| c == b"cpu")
            .then(|| path_from_bytes(rel))
    });
    // The common v1 layouts mount cpu alone or together with cpuacct.
    let v1_roots = [
        root.join("cpu"),
        root.join("cpu,cpuacct"),
        root.join("cpuacct,cpu"),
    ];
    if let Some(rel) = cpu_rel {
        for cpu_root in &v1_roots {
            if let Some(cores) = cpu_chain_limit(cpu_root, &rel, false) {
                return cores;
            }
        }
    }
    // Preserve the root approximation when membership cannot be followed.
    if let Some(cores) = cpu_chain_limit(root, Path::new("/"), true) {
        return cores;
    }
    v1_roots
        .iter()
        .find_map(|r| cpu_chain_limit(r, Path::new("/"), false))
        .unwrap_or(0)
}

/// The CPUs the cpuset controller grants this process: the process's own
/// effective mask when it is readable, otherwise the smallest mask on the chain
/// from its cgroup up to the mount root, 0 when no readable mask exists at all
/// (the controller lives on the other hierarchy, or no mask is set) — 0 clamps
/// nothing, exactly like an unlimited quota.
///
/// cgroup v2 keeps the mask in `cpuset.cpus.effective` of the cgroup itself: a
/// read-only file the kernel fills with the CPUs the parent actually grants,
/// so it already folds in the ancestors. cgroup v1 keeps the matching
/// `cpuset.effective_cpus` under the controller's own mount. Reading the
/// process's own cgroup (rather than the root file) is what makes a container
/// or a unit with a narrower mask bind.
///
/// The own file is authoritative, not merely the narrowest: a cgroup v2
/// partition root hands CPUs to a child partition by REMOVING them from its own
/// `cpuset.cpus.effective` (`cpumask_andnot(cp->effective_cpus,
/// cp->effective_cpus, cp->subparts_cpus)`, v6.6 kernel/cgroup/cpuset.c
/// `update_cpumasks_hier`), while the child's mask is `(parent.effective_cpus U
/// parent.subparts_cpus) ∩ cpus_allowed` (`compute_effective_cpumask`,
/// cpuset.c:1255-1267) — wider than its parent's. Taking the smallest count of
/// the chain then reported the parent's smaller count for a process that runs
/// on the child's larger set: an 8-CPU partition root above a 120-CPU child
/// answered 8. The ancestor walk stays as the fallback for a cgroup whose own
/// file is absent or empty (an empty mask means "inherit", not "no CPUs").
fn cgroup_cpuset_core_count_from(root: &Path, self_cgroup: &[u8]) -> u64 {
    if let Some(rel) = cgroup_v2_relative_path(self_cgroup) {
        // The membership path is absolute-looking and relative to the mount
        // (cgroup_membership_dir strips the leading separators); joining it
        // verbatim would read the host's root instead of the fixture's.
        let own_dir = root.join(cgroup_membership_dir(&rel));
        if let Some(cpus) = own_cpuset_mask(&own_dir, "cpuset.cpus.effective") {
            return cpus;
        }
        if let Some(cpus) = chain_min_value(root, &rel, "cpuset.cpus.effective", cpuset_mask_cpus) {
            return cpus;
        }
    }
    if let Some(rel) = cgroup_v1_controller_relative_path(self_cgroup, "cpuset") {
        let own_dir = root.join("cpuset").join(cgroup_membership_dir(&rel));
        if let Some(cpus) = own_cpuset_mask(&own_dir, "cpuset.effective_cpus") {
            return cpus;
        }
        if let Some(cpus) = chain_min_value(
            &root.join("cpuset"),
            &rel,
            "cpuset.effective_cpus",
            cpuset_mask_cpus,
        ) {
            return cpus;
        }
    }
    // Membership that cannot be followed: the mount root's own mask still
    // bounds what the process may use — the container root in the common
    // private-cgroup-namespace case.
    if let Some(cpus) = chain_min_value(
        root,
        Path::new("/"),
        "cpuset.cpus.effective",
        cpuset_mask_cpus,
    ) {
        return cpus;
    }
    chain_min_value(
        &root.join("cpuset"),
        Path::new("/"),
        "cpuset.effective_cpus",
        cpuset_mask_cpus,
    )
    .unwrap_or(0)
}

/// The cgroup's own effective cpuset mask, `None` when the file is absent,
/// unreadable or empty. An empty mask means the cgroup inherits its parent's
/// set rather than running on zero CPUs, and a malformed mask is no evidence of
/// a restriction, so both fall back to the ancestor walk.
fn own_cpuset_mask(cgroup_dir: &Path, file: &str) -> Option<u64> {
    let count = cpuset_mask_cpus(&fs::read_to_string(cgroup_dir.join(file)).ok()?);
    (count > 0).then_some(count)
}

/// A cpuset mask (`cpuset.cpus`, cpuset v1's `cpuset.effective_cpus`, and v2's
/// `cpuset.cpus.effective` share the format) → the number of CPUs it lists:
/// comma-separated single CPUs and `first-last` ranges, so `0-3,8,10-11` is
/// seven CPUs. 0 when the mask is empty or unparsable, which clamps nothing:
/// an empty mask means the cgroup inherits its parent's set, not that it may
/// use zero CPUs, and a malformed mask is no evidence of a restriction.
fn cpuset_mask_cpus(raw: &str) -> u64 {
    let mut cpus = 0u64;
    for part in raw.trim().split(',') {
        let part = part.trim();
        match part.split_once('-') {
            Some((first, last)) => {
                match (first.trim().parse::<u64>(), last.trim().parse::<u64>()) {
                    (Ok(first), Ok(last)) if first <= last => cpus += last - first + 1,
                    _ => return 0,
                }
            }
            None => match part.parse::<u64>() {
                Ok(_) => cpus += 1,
                Err(_) => return 0,
            },
        }
    }
    cpus
}

fn cpu_chain_limit(root: &Path, rel: &Path, v2: bool) -> Option<u64> {
    // Namespace-relative parent components must never walk above this mount.
    let rel = cgroup_membership_dir(rel);
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
    read_disk_info_from(Path::new("/sys/block"))
}

/// Path-injectable form of [`read_disk_info`] (the `_at` idiom): `block_dir` is
/// the sysfs block tree the entries and their `queue/` attributes are read
/// from, so the entry filter and the per-disk reads are assertable against a
/// synthetic tree.
pub(crate) fn read_disk_info_from(block_dir: &Path) -> Result<Vec<DiskInfo>> {
    let mut disks = Vec::new();

    if let Ok(entries) = fs::read_dir(block_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy().to_string();

            // Storage that is not a disk of its own: loop is a file, ram and
            // zram are memory, sr a removable CD, nbd a remote block device
            // whose userspace server does the reading, and dm-* recomputes its
            // window from the stacked limits on every table load
            // (`disk_update_readahead()` from `dm_table_set_restrictions`,
            // v6.6 drivers/md/dm-table.c:2029). An md array stays: it is the
            // device its filesystem is mounted on, its members are the ones
            // `holders` excludes, and no md code recomputes its window (that
            // dm call is the only one in drivers/md).
            if name.starts_with("loop")
                || name.starts_with("ram")
                || name.starts_with("dm-")
                || name.starts_with("sr")
                || name.starts_with("zram")
                || name.starts_with("nbd")
            {
                continue;
            }

            let dir = entry.path();
            let disk_type = detect_disk_type(&dir, &name);
            let scheduler = read_current_scheduler(&dir);
            let available_schedulers = read_available_schedulers(&dir);
            let nr_requests = read_nr_requests(&dir);
            let read_ahead_kb = read_read_ahead_kb(&dir);
            let rq_affinity = read_rq_affinity(&dir);
            let hidden = read_disk_hidden(&dir.join("hidden"));
            let holders = read_disk_holders(&dir.join("holders"));

            disks.push(DiskInfo {
                name,
                disk_type,
                scheduler,
                available_schedulers,
                nr_requests,
                read_ahead_kb,
                rq_affinity,
                hidden,
                holders,
            });
        }
    }

    resolve_md_array_types(&mut disks, block_dir);
    Ok(disks)
}

/// Re-type an `md*` array from the members that carry it.
///
/// md derives the array's rotational flag from its members: it keeps
/// `QUEUE_FLAG_NONROT` only while every active member is non-rotational and the
/// array is not degraded (v6.6 drivers/md/md.c:6087-6100), and clears it
/// otherwise. That flag is not the medium for a member that is a virtual disk,
/// which [`detect_disk_type`] types by the hypervisor on the grounds that the
/// flag follows the host rather than the backing medium — so an array over
/// those members read `rotational=1` (HDD) while every member read `SSD`, and
/// the medium-gated rules followed the array, the device the filesystem is
/// actually on. The members' answer wins; an array whose members the inventory
/// does not hold keeps its own.
///
/// A member is a whole disk *or a partition of one*: the holder link is created
/// on the claimed bdev (`block/holder.c`), so an array assembled from
/// partitions — the usual cloud layout, RAID1 on `vda1`/`vdb1` — records it on
/// the partition's kobject, which the whole-disk `holders` directory cannot
/// see. Such an array never counted a member, kept the members' rotational flag
/// and was typed HDD while its disks read SSD. [`md_member_disk_names`] closes
/// that gap from the array's own `slaves` directory.
fn resolve_md_array_types(disks: &mut [DiskInfo], block_dir: &Path) {
    for index in 0..disks.len() {
        let name = disks[index].name.clone();
        if !name.starts_with("md") || disks[index].disk_type != DiskType::HDD {
            continue;
        }

        let member_disks = md_member_disk_names(&block_dir.join(&name));
        let mut members = 0usize;
        let mut solid_state = true;
        for member in disks.iter() {
            if member.holders.iter().any(|holder| holder == &name)
                || member_disks.iter().any(|disk| disk == &member.name)
            {
                members += 1;
                solid_state &= matches!(member.disk_type, DiskType::SSD | DiskType::NVMe);
            }
        }

        if members > 0 && solid_state {
            disks[index].disk_type = DiskType::SSD;
        }
    }
}

/// The whole-disk names an `md*` directory holds in its `slaves` directory:
/// every entry resolved to the disk that owns it.
///
/// A member may be a partition — sysfs marks one with a `partition` attribute
/// — and its kobject then lives under the whole disk's, so the disk the
/// inventory holds is the parent directory. The whole-disk member itself
/// resolves to its own directory.
fn md_member_disk_names(md_dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(md_dir.join("slaves")) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let target = fs::canonicalize(entry.path()).ok()?;
            let name = target.file_name()?.to_string_lossy().to_string();
            if target.join("partition").exists() {
                Some(target.parent()?.file_name()?.to_string_lossy().to_string())
            } else {
                Some(name)
            }
        })
        .collect()
}

/// Medium of the device in `dir`, named `name`: the one place the rotational
/// flag is interpreted, so a rule that resolves another device directory (a
/// swap file's backing device) reads the same answer as the inventory.
pub(crate) fn detect_disk_type(dir: &Path, name: &str) -> DiskType {
    if name.starts_with("nvme") {
        return DiskType::NVMe;
    }

    // virtio-blk (vd*) and Xen (xvd*) cloud disks frequently report
    // rotational=1 even when backed by SSD/network storage, so the bare
    // rotational flag would mislabel them HDD and apply spinning-disk tuning.
    // Treat them as SSD-like (the SSD optimizations are safe and beneficial,
    // and a real spinning disk presented as vd* in modern clouds is vanishingly
    // rare). The emulated disks that arrive as ordinary SCSI devices — Hyper-V's
    // storvsc and QEMU's scsi-hd — are the same shape under a name that carries
    // nothing (`sd*` is a real SCSI disk elsewhere), so they are identified by
    // their device identity instead.
    let is_virtual =
        name.starts_with("vd") || name.starts_with("xvd") || is_emulated_virtual_disk(dir);

    let rotational_path = dir.join("queue/rotational");
    if let Ok(val) = fs::read_to_string(rotational_path) {
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

/// Whether the block device in `dir` is an emulated virtual disk whose
/// rotational flag follows the hypervisor rather than the backing medium.
///
/// The storvsc driver presents Hyper-V's disks as ordinary SCSI devices with
/// the vendor/product pair vmbus hands it — "Msft" and "Virtual Disk" — and,
/// like virtio and Xen, the rotational flag follows what the hypervisor
/// reports rather than the backing medium: every disk of a Hyper-V or Azure VM
/// and of a WSL2 instance reads 1, including on an all-NVMe machine.
///
/// QEMU's `scsi-hd` disks (the device behind `virtio-scsi` and every guest
/// disk attached to a SCSI HBA) are the same shape: the drive defaults to
/// vendor "QEMU" / product "QEMU HARDDISK" with `rotation_rate` 0, which VPD
/// page 0xb1 reports as "rotation rate not reported" — and the SCSI midlayer
/// only clears the rotational flag for a reported rate of exactly 1
/// (drivers/scsi/sd.c), so such a disk reads 1 whatever medium backs it. A
/// Proxmox/libvirt/OpenStack guest therefore had its disk typed HDD: the
/// streaming arm raised `read_ahead_kb` on non-rotating storage and the
/// SSD-gated advice (the scheduler, `vm.page-cluster`, the database window)
/// never fired.
///
/// The name carries nothing to key on, so the device identity decides.
fn is_emulated_virtual_disk(dir: &Path) -> bool {
    const IDENTITIES: [(&str, &str); 2] = [("Msft", "Virtual Disk"), ("QEMU", "QEMU HARDDISK")];
    let identity = |entry: &str| {
        fs::read_to_string(dir.join("device").join(entry))
            .map(|value| value.trim().to_string())
            .unwrap_or_default()
    };
    let (vendor, model) = (identity("vendor"), identity("model"));
    IDENTITIES.contains(&(vendor.as_str(), model.as_str()))
}

fn read_current_scheduler(dir: &Path) -> String {
    let path = dir.join("queue/scheduler");
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

fn read_available_schedulers(dir: &Path) -> Vec<String> {
    let path = dir.join("queue/scheduler");
    if let Ok(content) = fs::read_to_string(&path) {
        content
            .split_whitespace()
            .map(|s| s.trim_matches(|c| c == '[' || c == ']').to_string())
            .collect()
    } else {
        Vec::new()
    }
}

fn read_nr_requests(dir: &Path) -> Option<u64> {
    let path = dir.join("queue/nr_requests");
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

fn read_read_ahead_kb(dir: &Path) -> Option<u64> {
    let path = dir.join("queue/read_ahead_kb");
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// Whether the kernel hides the gendisk (`GENHD_FL_HIDDEN`). A hidden disk
/// registers no device number (v6.6 `block/genhd.c`:454, 7.3-rc6 :498) and its
/// `block_device` never reaches the inode hash (v6.6 `block/genhd.c`:508,
/// 7.3-rc6 :410), so nothing can open it, let alone mount a filesystem. The
/// NVMe multipath path devices (`nvmeXcYnZ`) are hidden exactly this way
/// (v6.6 `drivers/nvme/host/core.c`:3618-3620, 7.3-rc6 :4290-4292), while the
/// head device that carries the filesystem is not. The path parameter keeps
/// the reader runnable against a synthetic `/sys/block` tree.
///
/// An unreadable attribute reads as "not hidden", which keeps the advice
/// emitting rather than silently dropping it.
fn read_disk_hidden(hidden_path: &Path) -> bool {
    fs::read_to_string(hidden_path)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(0)
        == 1
}

/// The names under a disk's `holders` directory — the block devices holding it
/// open as a member. `bd_link_disk_holder()` names each entry after the holder
/// disk (v6.6 `block/holder.c`:106, 7.3-rc6 :108); dm, md, bcache and drbd are
/// its callers, so a non-empty directory means the disk belongs to a stacked
/// device rather than to a filesystem. Sorted so the read is deterministic; an
/// unreadable directory reads as no holder, the conservative side for advice.
fn read_disk_holders(holders_dir: &Path) -> Vec<String> {
    let mut holders: Vec<String> = fs::read_dir(holders_dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    holders.sort();
    holders
}

fn read_rq_affinity(dir: &Path) -> Option<u64> {
    let path = dir.join("queue/rq_affinity");
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
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
    // The interfaces this pass dropped as plumbing, and the ones that survived
    // it together with the devices they report the link state of. A device
    // that only forwards a piece of plumbing — a VLAN over a pair end, a
    // bridge whose only port is one — shows the same fixed rate, so the skip
    // below is transitive (see the propagation after the loop).
    let mut plumbing: Vec<String> = Vec::new();
    let mut kept: Vec<(String, u64, Vec<String>)> = Vec::new();

    if let Ok(entries) = fs::read_dir(net_dir) {
        let entries: Vec<fs::DirEntry> = entries.filter_map(|e| e.ok()).collect();
        // Every ifindex this namespace has, for the peer test below.
        let local_ifindexes: Vec<u64> = entries
            .iter()
            .filter_map(|entry| read_sysfs_u64(&entry.path().join("ifindex")))
            .collect();
        for entry in &entries {
            let name = entry.file_name().to_string_lossy().to_string();
            // The tun driver creates tun/tap devices and reports a fixed
            // SPEED_10000 for every one of them — `tun_setup()` seeds the link
            // ksettings with `tun_default_link_ksettings()` (v6.6
            // drivers/net/tun.c:2327/3553-3563; master:2408/3640-3650), and the
            // sysfs `speed` attribute prints that cached default. It is a
            // driver default, not a negotiated rate: a host with a 1 GbE NIC
            // and an UP tunnel was judged a 10 GbE host, and every rule gated
            // on a 10 GbE link then sized its recommendation for a link the
            // machine does not have.
            // The name prefixes catch the conventional names, but TUNSETIFF
            // accepts any device name a caller asks for: `ip tuntap add dev
            // myvpn0`, OpenVPN with an explicit `dev`, and Tailscale's
            // `tailscale0` (wireguard-go's tun) all still get the driver's
            // fixed SPEED_10000. The tun driver exposes its own marker for
            // exactly this: every device it registers carries a `tun_flags`
            // sysfs attribute (v6.6 drivers/net/tun.c:2720, `tun_dev_attrs`
            // in `tun_attr_group`), which no other netdev has. Skip on the
            // marker so a custom-named tunnel cannot masquerade as the host
            // link either.
            let is_tun_device = entry.path().join("tun_flags").exists();
            // The veth driver reports the same fixed SPEED_10000
            // (`veth_get_link_ksettings`, v6.6 drivers/net/veth.c:126-134) for
            // every device it registers, and its devices are the container
            // plumbing: the host end of a pair is named by the CNI rather than
            // `veth*` (Calico's `cali*`, Cilium's `lxc<id>` and
            // `cilium_host`). A pair whose two ends share one namespace is
            // plumbing just the same — both ends carry the same fixed default —
            // and `dev_valid_name` lets whoever creates the pair pick both
            // names (`ip link add h1 type veth peer name h2`), so the name
            // prefixes above cannot be the whole guard. Skip on the marker,
            // like the tun marker above.
            let speed_mbps = read_speed_mbps(&entry.path());
            let is_veth_end = is_veth_end(&entry.path(), speed_mbps);
            // The link shapes that derive a reading from another device (a VLAN
            // its parent, a bridge port its bridge, an attached tunnel the
            // device it runs over) are told apart by `iflink`: one that names
            // an interface of this namespace keeps its reading, one that names
            // a device outside it — or nothing at all — does not. A veth end is
            // already skipped above and never reaches this test, because its
            // peer's index names a device of the PEER's namespace and could
            // collide with a local one.
            let peer_is_external = link_peer_leaves_namespace(&entry.path(), &local_ifindexes);
            if name == "lo"
                || name.starts_with("veth")
                || name.starts_with("br-")
                || name.starts_with("virbr")
                || name == "docker0"
                || name == "bonding_masters"
                || name.starts_with("tun")
                || name.starts_with("tap")
                || is_tun_device
                || is_veth_end
                || peer_is_external
            {
                plumbing.push(name);
                continue;
            }

            kept.push((name, speed_mbps, lower_link_names(&entry.path())));
        }

        // The skip is transitive because the rate is. A VLAN and a bridge do
        // not negotiate a link of their own: `vlan_dev_get_link_ksettings`
        // forwards the real device's ksettings verbatim (v6.6
        // net/8021q/vlan_dev.c:662-667) and `br_get_link_ksettings` reports
        // the best UP/running port's (v6.6 net/bridge/br_device.c:267-292), so
        // a VLAN over a pair end or a bridge whose only port is one carries
        // the veth driver's fixed 10000 Mbps (drivers/net/veth.c:126-134) as
        // the host link — the false positive the `veth*`/`cali*` marker and
        // the pair test above close one device too low. A device is therefore
        // plumbing when every device it reports the state of is plumbing; a
        // macvlan over a real NIC, a bond over real ports and a bridge with a
        // real port keep their reading, and a chain (a VLAN over a bridge over
        // a pair end) settles over the repeats.
        //
        // A bridge with both a real port and a pair/tap port is the mixed
        // case: the driver reports the MAXIMUM over its ports, so the veth
        // driver's fixed 10000 is the bridge's reading even when the real
        // uplink is 1 GbE. That reading is plumbing too; the real NIC remains
        // in the list with its own rate. A genuinely faster bridge (25/40 G)
        // does not equal the veth default and keeps its reading.
        loop {
            let mut survivors: Vec<(String, u64, Vec<String>)> = Vec::new();
            let mut dropped = false;
            for (name, speed_mbps, lowers) in kept {
                let plumbing_reading = lowers.iter().any(|lower| plumbing.contains(lower));
                if !lowers.is_empty()
                    && (lowers.iter().all(|lower| plumbing.contains(lower))
                        || (speed_mbps == VETH_SPEED_MBPS && plumbing_reading))
                {
                    plumbing.push(name);
                    dropped = true;
                } else {
                    survivors.push((name, speed_mbps, lowers));
                }
            }
            kept = survivors;
            if !dropped {
                break;
            }
        }
        nets.extend(
            kept.into_iter()
                .map(|(name, speed_mbps, _)| NetInfo { name, speed_mbps }),
        );
    }

    Ok(nets)
}

/// Whether the interface in `dir` links to a device outside this network
/// namespace.
///
/// `iflink` names the ifindex of the device an interface is linked to, and
/// `dev_get_iflink`'s contract is that a physical interface links to itself
/// (net/core/dev.c). A link target that is not one of `local_ifindexes`
/// identifies a device whose peer sits on the other side of a namespace - an
/// unattached tunnel keeps `iflink` 0, a `macvlan`/`ipvlan` names a device
/// outside the namespace it was moved to - or no device at all, so the
/// reading it would report is not this host's link. The shapes that derive a
/// real reading keep it: a VLAN links to its parent (`vlan_dev_get_iflink`),
/// a bridge port to its bridge, an attached tunnel to the device it runs
/// over, a bond to itself for the lack of a link handler. An unreadable
/// `iflink` or `ifindex` is not evidence, and keeps the interface.
///
/// A veth end never reaches this test: its peer is always another veth end and
/// its `iflink` therefore names a device of that PEER's namespace - a
/// container's `eth0` is the first device of its own namespace, so its ifindex
/// is 2 and the host end of the pair reports `iflink = 2`, which on the host is
/// the first real NIC. The caller skips every veth end by its driver marker
/// before asking, so no local index is ever compared against a foreign one.
fn link_peer_leaves_namespace(dir: &Path, local_ifindexes: &[u64]) -> bool {
    let (Some(ifindex), Some(iflink)) = (
        read_sysfs_u64(&dir.join("ifindex")),
        read_sysfs_u64(&dir.join("iflink")),
    ) else {
        return false;
    };
    if iflink == ifindex {
        return false;
    }
    !local_ifindexes.contains(&iflink)
}

/// The link speed the veth driver reports for every device it registers
/// (`veth_get_link_ksettings`, v6.6 drivers/net/veth.c:126-134): a driver
/// default, not a negotiated rate.
const VETH_SPEED_MBPS: u64 = 10_000;

/// One interface's `speed` attribute through the sanitizer the report uses:
/// an absent, unreadable or bogus attribute is 0, never a rate.
fn read_speed_mbps(dir: &Path) -> u64 {
    fs::read_to_string(dir.join("speed"))
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .map(sanitize_speed_mbps)
        .unwrap_or(0)
}

/// Whether `dir` is a veth end, one side of a pair whose driver default says
/// nothing about this host's link — the two ends share one namespace or are
/// split across two, and both report the same fixed rate.
///
/// veth carries no name the kernel guarantees (the host end is named by the
/// CNI: Calico's `cali*`, Cilium's `lxc<id>` and `cilium_host`; a pair created
/// by hand carries whatever names its creator passed) and no bus device, so it
/// is recognised by what it lacks: no `device` symlink (it is not backed by a
/// bus device), no `DEVTYPE=` line in its uevent (the drivers that declare one:
/// `DEVTYPE=vlan`, `bridge`, `bond`, `wireguard`), no `lower_` adjacency link
/// (see below), and the driver's fixed [`VETH_SPEED_MBPS`]. The speed
/// condition keeps the DEVTYPE-less devices that report no rate at all out of
/// this branch (a sit/ip6tnl tunnel: `ip6_tnl_get_iflink` returns the
/// underlying link and their `speed` is unreadable), and the link test below
/// keeps the DEVTYPE-less devices that link to themselves out of it (dummy,
/// ifb, team).
///
/// The link test is `iflink`, which names the interface's peer inside the
/// PEER's namespace (`veth_get_iflink`, v6.6 drivers/net/veth.c:1538-1550,
/// 0 once the peer is gone). A veth end therefore never names itself - its
/// `iflink` is its peer's index, a different device - so an interface that
/// links to itself cannot be one. That is the test a pair sharing one
/// namespace passes just like the cross-namespace pair; the index is not
/// compared with anything here, because it belongs to the peer's namespace.
///
/// The adjacency link is what separates a veth end from the macvlan/ipvlan/
/// macvtap family. Those declare no `DEVTYPE=` either, have no bus device, and
/// on a 10 GbE parent they show 10000 Mbps — but that is the parent's
/// negotiated rate, which the driver forwards
/// (`macvlan_ethtool_get_link_ksettings` reads the lower device's ksettings,
/// v6.6 drivers/net/macvlan.c:1077-1082; `ipvlan_ethtool_get_link_ksettings`,
/// v6.6 drivers/net/ipvlan/ipvlan_main.c:402-407), not a driver default. They
/// record the parent relationship as a `lower_<name>` symlink
/// (`netdev_upper_dev_link()`, v6.6 net/core/dev.c:7509-7510), and no veth code
/// creates one (`drivers/net/veth.c` never calls it), so a device with a
/// `lower_` link reports the lower device's reading and is not a pair end.
fn is_veth_end(dir: &Path, speed_mbps: u64) -> bool {
    if speed_mbps != VETH_SPEED_MBPS || dir.join("device").exists() || has_lower_link(dir) {
        return false;
    }
    if let (Some(ifindex), Some(iflink)) = (
        read_sysfs_u64(&dir.join("ifindex")),
        read_sysfs_u64(&dir.join("iflink")),
    ) {
        if iflink == ifindex {
            return false;
        }
    }
    // The uevent embeds the interface name in its `INTERFACE=` line, and a
    // name the kernel accepts may be non-UTF-8 (`dev_valid_name` rejects only
    // `/`, `:` and whitespace), so `read_to_string` rejected the whole file
    // and answered "not a veth end" for a veth end: the caller's `iflink`
    // test then decided it, a cross-namespace peer index can collide with a
    // local one, and the pair's fixed 10000 Mbps was counted as the host's
    // link speed. Decode lossily, like every other reader of kernel-provided
    // text — the marker tested below is an ASCII line prefix, which a lossy
    // decode keeps.
    match fs::read(dir.join("uevent")) {
        Ok(bytes) => !String::from_utf8_lossy(&bytes)
            .lines()
            .any(|line| line.starts_with("DEVTYPE=")),
        // An unreadable uevent is not evidence of a veth end.
        Err(_) => false,
    }
}

/// Whether `dir` lists a `lower_<name>` adjacency link: the kernel's record
/// that another device runs over this one, whose link state this device
/// reports (`netdev_upper_dev_link()` creates the symlink pair, v6.6
/// net/core/dev.c:7509-7510). A veth end never has one; a macvlan, ipvlan or
/// macvtap always does.
fn has_lower_link(dir: &Path) -> bool {
    fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .filter_map(|e| e.ok())
            .any(|entry| entry.file_name().to_string_lossy().starts_with("lower_"))
    })
}

/// The names of the devices `dir`'s interface reports the link state of: its
/// `lower_<name>` adjacency links ([`has_lower_link`]).
fn lower_link_names(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .filter_map(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .strip_prefix("lower_")
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A sysfs attribute holding one unsigned number, `None` when the file is
/// absent or unparsable.
fn read_sysfs_u64(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn read_sysctl_values() -> Result<SysctlValues> {
    Ok(SysctlValues {
        swappiness: read_sysctl_u64("/proc/sys/vm/swappiness"),
        dirty_ratio: read_sysctl_u64("/proc/sys/vm/dirty_ratio"),
        dirty_background_ratio: read_sysctl_u64("/proc/sys/vm/dirty_background_ratio"),
        somaxconn: read_somaxconn_from(Path::new("/proc/sys/net/core/somaxconn")),
        thp_enabled: read_thp_enabled(),
    })
}

/// `net.core.somaxconn`, `None` when the file could not be read or parsed:
/// the 0 the shared reader falls back to sits below every backlog floor, so
/// it would be reported as the host's value and answered with a raise.
pub(crate) fn read_somaxconn_from(path: &Path) -> Option<u64> {
    read_sysfs_u64(path)
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
///
/// The `prometheus` PREFIX clause covers the collectors that carry the
/// server's name (`prometheus-node-exporter`, `prometheus-agent`); the server
/// itself is the monitoring host's workload and is the entry
/// `services::detect_services` lists as `Prometheus`, so the bare name must
/// not be dropped as a helper of itself. The clause swallowed it and made
/// that entry unreachable — the same way the runtime marker table made the
/// RabbitMQ entry unreachable.
pub(crate) fn is_monitoring_helper_name(name: &str) -> bool {
    let name = name.to_lowercase();
    (name.starts_with("prometheus") && name != "prometheus")
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
/// run decides — argv[0], the payload of a `sh -c` wrapper, the jar a JVM
/// runs, or the script an interpreter launches — because a bare `exporter`
/// token in a later argument (a config path, say) does not make the process a
/// collector.
fn cmdline_names_helper<'a>(args: impl Iterator<Item = &'a str>) -> bool {
    let args: Vec<&str> = args.collect();
    let Some(&program) = args.first() else {
        return false;
    };
    if is_monitoring_helper_name(program.rsplit('/').next().unwrap_or(program)) {
        return true;
    }
    if is_shell_program(program) && args.get(1) == Some(&"-c") {
        // `sh -c "node_exporter --web.listen-address=..."` runs the collector as
        // the payload, not as argv[0]; the payload's first word is its command.
        return args
            .get(2)
            .and_then(|&payload| payload.split_whitespace().next())
            .is_some_and(|program| {
                is_monitoring_helper_name(program.rsplit('/').next().unwrap_or(program))
            });
    }
    // The kernel rewrites every shebang exec to
    // `[interpreter, script, ...]`, so a directly executed
    // `postgres_exporter.sh` and `python3 /opt/kafka_exporter.py` put the
    // program-to-run in the argument after the interpreter.
    if let Some(script) = interpreter_script(program, &args) {
        if is_monitoring_helper_name(script.rsplit('/').next().unwrap_or(script)) {
            return true;
        }
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

/// The script argument of an interpreter command line, when the first argument
/// after the interpreter names a file rather than an option or an inline
/// payload.
///
/// A shebang exec is rewritten by the kernel to
/// `[interpreter, (optional arg), script, ...]` (`fs/binfmt_script.c`), for
/// every interpreter, not only shells. The script is the program being run, so
/// the collector/tool filters have to look at it: `python3
/// /opt/kafka_exporter.py` is a collector even though argv[0] is the
/// interpreter. `-c`/`-m`/`-e` payloads and option values are not script
/// paths, so a leading `-` answers `None`; the common direct form is covered
/// without guessing which options take a value.
fn interpreter_script<'a>(program: &str, args: &[&'a str]) -> Option<&'a str> {
    let basename = program.rsplit('/').next().unwrap_or(program);
    let interpreter = is_shell_program(program)
        || matches!(
            basename,
            "python" | "python3" | "node" | "nodejs" | "ruby" | "perl"
        );
    if !interpreter {
        return None;
    }
    let script = *args.get(1)?;
    (!script.starts_with('-')).then_some(script)
}

/// ClickHouse's client-side entry points. The vendor's `clickhouse-client`
/// package ships every one of them as a symlink to the single multi-call
/// `clickhouse` binary, so the kernel truncates the client's comm to a prefix
/// the server binary's own name shares (`clickhouse-clie` / `clickhouse-serv`)
/// and the name alone cannot tell the two apart. The program name can.
///
/// `clickhouse-copier` is the same class one step outside the multi-call
/// symlinks: it is the separate client-side data-copy utility (its own binary
/// through 23.8, removed in 24.3) whose only job is copying between clusters.
/// A host running a long copier job and no server is not a database host.
const CLICKHOUSE_CLIENT_TOOLS: &[&str] = &[
    "clickhouse-benchmark",
    "clickhouse-client",
    "clickhouse-compressor",
    "clickhouse-copier",
    "clickhouse-format",
    "clickhouse-local",
    "clickhouse-obfuscator",
];

/// Companion programs of the comm-named services the engine acts on.
///
/// A service identified by its own comm (`consul`, `grafana`) is not the only
/// program carrying its name: `consul-template` renders templates from a
/// Consul cluster running elsewhere and `grafana-agent` ships metrics to a
/// Grafana stack, and both keep the service's name as their first name run —
/// which is what `has_process` matches. Such a host reported the service,
/// classified as its workload (Consul is in `profile::classify`'s cache class)
/// and ran its cache rules. A companion is a separate program, not a role name
/// of the daemon (`nginx: worker process` keeps matching), so the program name
/// is what decides, the same way ClickHouse's client tools are listed.
///
/// `clickhouse-keeper` is the same shape one step further: ClickHouse ships
/// its coordination service (the Raft replacement for ZooKeeper) as its own
/// daemon, `clickhouse-keeper`, whose name keeps `clickhouse` as its first
/// name run — so a Keeper-only host reported ClickHouse and was handed the
/// whole database program (swappiness, the dirty limits, the read-ahead
/// window, `kernel.shmmni`, `vm.nr_hugepages`), none of which a Raft
/// coordination node runs. Keeper is a separate program with its own
/// configuration, so the program name decides, exactly as it does for the
/// client tools of the same package.
///
/// `oracle-cloud-agent` is the same shape for the Oracle database: it is the
/// cloud platform's instance-management agent (with its `-updater` sibling),
/// not an instance of the database, and both keep `oracle` as their first name
/// run. A host running only the agent — the default on that platform's images
/// — reported an Oracle database, classified as its workload and was handed
/// the database/cache program (swappiness, the dirty limits,
/// `vm.vfs_cache_pressure`, `kernel.shmmni`, `vm.nr_hugepages`, the busy-poll
/// pair), while the real database binary is the process named `oracle`.
/// `consul-dataplane` is the same shape for Consul: it is the Kubernetes
/// data-plane sidecar that talks to Consul servers running elsewhere, not the
/// in-memory KV server, but its comm truncates to `consul-dataplan` and its
/// first name run is `consul`. A node that only runs the sidecar reported
/// Consul and took the cache program (swappiness, the dirty limits, THP off,
/// `vm.vfs_cache_pressure`, the busy-poll pair).
const SERVICE_COMPANION_TOOLS: &[&str] = &[
    "consul-template",
    "consul-dataplane",
    "consul-esm",
    "grafana-agent",
    "clickhouse-keeper",
    "oracle-cloud-agent",
    "oracle-cloud-agent-updater",
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
/// rules and workload classification. The companions of a comm-named service
/// are listed separately, in [`SERVICE_COMPANION_TOOLS`].
fn program_names_service_tool(program: &str) -> bool {
    let basename = program.rsplit('/').next().unwrap_or(program);
    if CLICKHOUSE_CLIENT_TOOLS.contains(&basename) || SERVICE_COMPANION_TOOLS.contains(&basename) {
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
///
/// ClickHouse's multi-call binary is the one program argv[0] cannot decide: the
/// vendor documents the subcommand form as well as the symlinks, and `clickhouse
/// client` carries the same argv[0] and the same comm as `clickhouse server`, so
/// only the subcommand separates a client session from the daemon.
fn is_service_tool(pid: &str) -> bool {
    read_cmdline_from(&format!("/proc/{pid}/cmdline"))
        .is_some_and(|cmdline| cmdline_names_service_tool(&cmdline))
}

/// Pure core of [`is_service_tool`], assertable against a synthetic command
/// line the way [`runtime_service_from_cmdline`] is.
fn cmdline_names_service_tool(cmdline: &str) -> bool {
    let args: Vec<&str> = cmdline.split('\0').filter(|arg| !arg.is_empty()).collect();
    let Some(&program) = args.first() else {
        return false;
    };
    let next = args.get(1).copied();
    if program_names_service_tool(program) {
        return true;
    }
    if program_is_clickhouse_multi_call(program) {
        // The binary runs the server only under its `server` subcommand; every
        // other entry point (`client`, `local`, `benchmark`, `keeper`, ...) is
        // the client-side or companion program the symlink list names, and a
        // host whose only ClickHouse process is one of those is not a database
        // host.
        return next != Some("server");
    }
    // A script is run by an interpreter, and the kernel puts its path in the
    // first argument after the interpreter for every shebang, not only `sh`.
    interpreter_script(program, &args).is_some_and(program_names_service_tool)
}

/// Whether `program` (an argv[0]) is ClickHouse's multi-call binary, whose name
/// the server, the client tools and the keeper all share.
fn program_is_clickhouse_multi_call(program: &str) -> bool {
    program.rsplit('/').next().unwrap_or(program) == "clickhouse"
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
    // ZooKeeper maps its server package — `bin/zkServer.sh` runs
    // org.apache.zookeeper.server.quorum.QuorumPeerMain (or
    // org.apache.zookeeper.server.ZooKeeperServerMain for a standalone
    // server) — and NOT the bare `org.apache.zookeeper` root, for the reason
    // the Kafka markers above spell out: `bin/zkCli.sh` execs the JVM with
    // org.apache.zookeeper.ZooKeeperMain, the client's own class, so the
    // tool's argv[0] is gone by the time the process is sampled and the bare
    // root read a host that only administers a remote ensemble as a member of
    // it.
    ("org.apache.zookeeper.server", "zookeeper"),
    ("zookeeper", "zookeeper"),
    // Flink maps the packages its daemons live in — bin/flink-daemon.sh runs
    // org.apache.flink.runtime.taskexecutor.TaskManagerRunner,
    // org.apache.flink.runtime.entrypoint.StandaloneSessionClusterEntrypoint,
    // org.apache.flink.container.entrypoint
    // .StandaloneApplicationClusterEntryPoint,
    // org.apache.flink.runtime.webmonitor.history.HistoryServer and
    // org.apache.flink.runtime.zookeeper.FlinkZooKeeperQuorumPeer, while the
    // Kubernetes and YARN deployments add their own entry packages — and NOT
    // the bare `org.apache.flink` root, for the reason the Kafka markers above
    // spell out: `bin/flink` execs the JVM with
    // org.apache.flink.client.cli.CliFrontend, the client's own class, so only
    // the class is left to decide. The SQL gateway is the one daemon outside
    // those packages.
    ("org.apache.flink.runtime", "flink"),
    ("org.apache.flink.container.entrypoint", "flink"),
    ("org.apache.flink.kubernetes.entrypoint", "flink"),
    ("org.apache.flink.yarn.entrypoint", "flink"),
    ("org.apache.flink.table.gateway.SqlGateway", "flink"),
    ("flink", "flink"),
    // Spark maps its standalone daemons (sbin/start-master.sh,
    // start-worker.sh, start-history-server.sh) and its Connect server
    // (SparkConnectServer) — and NOT the bare `org.apache.spark` root, for
    // the reason the Kafka markers above spell out: `bin/spark-submit` execs
    // the JVM with org.apache.spark.deploy.SparkSubmit, the client's own
    // class, so only the class is left to decide once the launcher has exec'd
    // and the bare root read a host that only submits jobs as the cluster.
    ("org.apache.spark.deploy.master.Master", "spark"),
    ("org.apache.spark.deploy.worker.Worker", "spark"),
    ("org.apache.spark.deploy.history.HistoryServer", "spark"),
    (
        "org.apache.spark.sql.connect.service.SparkConnectServer",
        "spark",
    ),
    ("spark", "spark"),
    // Cassandra maps the daemon package — `bin/cassandra` sets
    // classname=org.apache.cassandra.service.CassandraDaemon — and NOT the
    // bare `org.apache.cassandra` root, for the reason the Kafka markers
    // above spell out: `bin/nodetool` (org.apache.cassandra.tools.NodeTool)
    // and `tools/bin/cassandra-stress` (org.apache.cassandra.stress.Stress)
    // exec the JVM with the tool's own main class, so only the class is left
    // to decide and the bare root reported a host that only administers a
    // remote cluster as a database host.
    ("org.apache.cassandra.service", "cassandra"),
    ("cassandra", "cassandra"),
    // HBase's daemons run their main classes under org.apache.hadoop.hbase,
    // i.e. as a package run the ("org.apache.hadoop","hadoop") marker below
    // owns from the root, so a table in that order reported every HBase
    // cluster as Hadoop: the ("hbase","hbase") entry can only match a class
    // whose own name or package run starts with `hbase`, and no HBase daemon
    // class does (the last component is `HMaster`, `HRegionServer`, ...). The
    // mislabel also fired the streaming read-ahead advice on a random-IO HBase
    // host. The daemons `bin/hbase` can start are mapped here, ahead of the
    // root, which keeps the jobs and clients of the family.
    ("org.apache.hadoop.hbase.master.HMaster", "hbase"),
    (
        "org.apache.hadoop.hbase.regionserver.HRegionServer",
        "hbase",
    ),
    // The rest of the servers HBase's own launcher starts run under the same
    // package root for the same reason: `bin/hbase rest | thrift | zookeeper`
    // execs org.apache.hadoop.hbase.rest.RESTServer,
    // ...hbase.thrift.ThriftServer and ...hbase.zookeeper.HQuorumPeer (HBase's
    // own ZooKeeper peer), so the Hadoop root claimed them exactly as it did
    // the master and region server, and their hosts were reported as Hadoop.
    // All three are long-lived servers of the deployment, which is what this
    // table decides, not the jobs and clients the root is right about.
    ("org.apache.hadoop.hbase.rest.RESTServer", "hbase"),
    ("org.apache.hadoop.hbase.thrift.ThriftServer", "hbase"),
    ("org.apache.hadoop.hbase.zookeeper.HQuorumPeer", "hbase"),
    ("org.apache.hadoop", "hadoop"),
    ("hadoop", "hadoop"),
    ("hbase", "hbase"),
    ("org.apache.solr", "solr"),
    ("solr", "solr"),
    ("logstash", "logstash"),
    // A BookKeeper bookie (Pulsar's storage node) runs
    // org.apache.bookkeeper.server.Main; Pulsar up to 2.6 and BookKeeper up
    // to 4.5 launched org.apache.bookkeeper.proto.BookieServer directly. Only
    // these entry points are mapped: AutoRecoveryMain and BookieShell live
    // under the same root but keep no ledger storage.
    ("org.apache.bookkeeper.server", "bookkeeper"),
    ("org.apache.bookkeeper.proto.BookieServer", "bookkeeper"),
    // Pulsar maps its five daemon starters — the entry points `bin/pulsar`
    // execs for `broker`, `standalone`, `proxy`, `websocket` and
    // `functions-worker` (apache/pulsar bin/pulsar) — and NOT the bare
    // `org.apache.pulsar` package root, for the reason the Kafka markers
    // above spell out: the root is shared with the clients and every
    // maintenance tool. `bin/pulsar-admin`, `bin/pulsar-perf` and
    // `bin/pulsar-client` exec the JVM with the tool's own main class
    // (org.apache.pulsar.admin.cli.PulsarAdminTool,
    // org.apache.pulsar.testclient.PulsarPerfTestTool,
    // org.apache.pulsar.client.cli.PulsarClientTool), so the tool's argv[0]
    // is gone by the time the process is sampled and only the class is left
    // to match. Mapping the root made a host that only runs a client or a
    // load generator report "Pulsar" in `check.services` and classify as the
    // streaming workload, firing the streaming and read-ahead rules on a host
    // that stores no ledgers.
    ("org.apache.pulsar.PulsarBrokerStarter", "pulsar"),
    ("org.apache.pulsar.PulsarStandaloneStarter", "pulsar"),
    (
        "org.apache.pulsar.proxy.server.ProxyServiceStarter",
        "pulsar",
    ),
    (
        "org.apache.pulsar.websocket.service.WebSocketServiceStarter",
        "pulsar",
    ),
    (
        "org.apache.pulsar.functions.worker.FunctionWorkerStarter",
        "pulsar",
    ),
    ("pulsar", "pulsar"),
    ("org.apache.catalina", "tomcat"),
    ("catalina", "tomcat"),
    ("tomcat", "tomcat"),
    ("jenkins", "jenkins"),
    // RabbitMQ runs on the Erlang VM: its launcher execs `beam.smp`, so the
    // process is named after the VM and the broker can only be identified
    // from the command line. The launcher passes the node name as
    // `-sname rabbit@<host>` (RABBITMQ_NODENAME's default prefix is
    // `rabbit`, and the same name repeats in the
    // `-boot .../mnesia/rabbit@<host>/rabbit` path), which is the one token
    // of a real cmdline that names the service: the
    // `-pa .../rabbitmq_server-<v>/ebin` directory is matched by its
    // basename (`ebin`) by design. Without this marker `has_process`
    // ("rabbitmq") can never be true for the packaged daemon, so the table's
    // ("rabbitmq", "RabbitMQ") entry was unreachable and a RabbitMQ host
    // reported no service at all. A cluster whose node name was deliberately
    // renamed (`-sname orders@host`) is not recognised — the marker matches
    // the default prefix, exactly as the jar and main-class markers match
    // upstream's own names.
    ("rabbit", "rabbitmq"),
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
/// Java argument scanning stops at an explicit entrypoint. Launches using
/// unexpanded argument files retain legacy matching because the entrypoint
/// cannot be located from the visible arguments alone.
///
/// Each candidate argument is matched by its file-name component only (a
/// directory named after a service is not the program), and a marker matches
/// only as a whole consecutive run of name tokens, so `sparklesh` or
/// `etcdbackup` no longer satisfy `spark`/`etcd`.
fn runtime_service_from_cmdline(cmdline: &str) -> Option<&'static str> {
    // Java exposes one entrypoint; retain the existing candidate scan for
    // other runtimes whose launch contracts differ.
    let mut candidates: Vec<&str> = Vec::new();
    let nonempty: fn(&&str) -> bool = |value| !value.is_empty();
    let mut args = cmdline.split('\0').filter(nonempty);
    let is_java = args
        .clone()
        .next()
        .is_some_and(|program| program.rsplit('/').next() == Some("java"));
    if is_java {
        let _ = args.next();
    }
    let mut bound_java_entrypoint = is_java;
    let mut argument_files_enabled = true;
    let is_argument_file =
        |value: &str| value.len() > 1 && value.starts_with('@') && !value.starts_with("@@");
    while let Some(arg) = args.next() {
        if bound_java_entrypoint && arg == "--disable-@files" {
            argument_files_enabled = false;
            continue;
        } else if bound_java_entrypoint && argument_files_enabled && is_argument_file(arg) {
            // An argument file can end with an option whose value is supplied
            // on the command line. Preserve the old scan instead of guessing.
            bound_java_entrypoint = false;
            candidates.clear();
            args = cmdline.split('\0').filter(nonempty);
            continue;
        } else if bound_java_entrypoint && arg.starts_with("-D") {
            // An application started by a generic launcher carries its identity
            // nowhere else on the command line (see IDENTITY_JVM_PROPERTIES).
            if let Some(service) = identity_property_service(arg) {
                return Some(service);
            }
        } else if arg == "-jar" || (bound_java_entrypoint && matches!(arg, "-m" | "--module")) {
            if let Some(entrypoint) = args.next() {
                if bound_java_entrypoint && argument_files_enabled && is_argument_file(entrypoint) {
                    bound_java_entrypoint = false;
                    candidates.clear();
                    args = cmdline.split('\0').filter(nonempty);
                    continue;
                }
                candidates.push(entrypoint);
            }
            if bound_java_entrypoint {
                break;
            }
        } else if bound_java_entrypoint && arg.starts_with("--module=") {
            candidates.push(&arg["--module=".len()..]);
            break;
        } else if arg == "-cp"
            || arg == "-classpath"
            || arg == "--class-path"
            || (bound_java_entrypoint
                && matches!(
                    arg,
                    "-p" | "--module-path"
                        | "--upgrade-module-path"
                        | "--add-modules"
                        | "--limit-modules"
                        | "--add-exports"
                        | "--add-opens"
                        | "--add-reads"
                        | "--patch-module"
                        | "--enable-native-access"
                        | "--source"
                ))
        {
            // Search paths and JVM option values describe the launch
            // environment, not the application entrypoint.
            if let Some(value) = args.next() {
                if bound_java_entrypoint && argument_files_enabled && is_argument_file(value) {
                    bound_java_entrypoint = false;
                    candidates.clear();
                    args = cmdline.split('\0').filter(nonempty);
                }
            }
        } else if !arg.starts_with('-') {
            candidates.push(arg);
            // Java passes everything after its main class/source or JAR to
            // the application; those values cannot identify the JVM's service.
            if bound_java_entrypoint {
                break;
            }
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

/// JVM system properties that identify the application a generic launcher
/// starts.
///
/// A launcher that runs a jar it does not own leaves no candidate for the
/// marker table: `bin/solr` execs
/// `java <SOLR_START_OPTS> -jar start.jar --module=http` (apache/solr
/// `solr/bin/solr`, the `exec "$JAVA" "${SOLR_START_OPTS[@]}" … -jar start.jar`
/// line, with `-Dsolr.solr.home=`, `-Dsolr.install.dir=` and
/// `-Dsolr.install.symDir=` in SOLR_START_OPTS), so the jar is Jetty's generic
/// `start.jar` and every other argument is a JVM option whose value the scan
/// deliberately skips. The property is then the only identity the command line
/// carries. Only property names unique to one launcher belong here — `start.jar`
/// alone must not be mapped, since any Jetty application starts that way.
const IDENTITY_JVM_PROPERTIES: &[(&str, &str)] =
    &[("solr.solr.home", "solr"), ("solr.install.dir", "solr")];

/// The service a `-D<name>=<value>` argument identifies, if any.
fn identity_property_service(arg: &str) -> Option<&'static str> {
    let property = arg.strip_prefix("-D")?;
    let name = property.split('=').next().unwrap_or(property);
    IDENTITY_JVM_PROPERTIES
        .iter()
        .find(|(property_name, _)| *property_name == name)
        .map(|(_, service)| *service)
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

    /// Whether a Node.js runtime is running, under either name the
    /// distributions ship: `node` (upstream, nvm, the RHEL module) or `nodejs`
    /// (Debian/Ubuntu's `nodejs` package). Both are exact matches, so a
    /// collector named after the runtime (`node_exporter`) cannot stand in for
    /// it.
    pub fn has_node_runtime(&self) -> bool {
        self.has_process_exact("node") || self.has_process_exact("nodejs")
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
            cpu_cores_host: 1,
            numa_nodes: 1,
            memory_total_gb: 1,
            memory_gb_host: 1,
            disks: vec![],
            network: vec![],
            sysctl: SysctlValues {
                swappiness: 60,
                dirty_ratio: 20,
                dirty_background_ratio: 10,
                somaxconn: Some(128),
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
        // The host readings travel with the effective values the rules scale
        // by and can never sit below them: the effective value is the host
        // reading clamped down to the cgroup limit, nothing else.
        assert!(info.cpu_cores_host >= info.cpu_cores);
        assert!(info.memory_gb_host >= info.memory_total_gb);
    }

    /// A virtual-disk member of an md array reports `rotational=1`, so the
    /// array — whose flag md derives from those members (v6.6
    /// drivers/md/md.c:6087-6100) — read HDD while the members it is built from
    /// read SSD. The array is the device the filesystem mounts, so it has to
    /// carry the members' answer, or every medium-gated rule sizes a
    /// solid-state device as a mechanical one.
    #[test]
    fn md_array_inherits_the_medium_of_its_members() {
        let dir = std::env::temp_dir().join(format!(
            "ktuner_md_medium_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();

        // (name, rotational flag, array it is a member of)
        for (name, rotational, holder) in [
            ("vda", "1", Some("md0")),
            ("vdb", "1", Some("md0")),
            // A real spinning member keeps the array mechanical.
            ("sdc", "1", Some("md1")),
            ("md0", "1", None),
            ("md1", "1", None),
            // An array whose members the inventory does not hold keeps its own
            // answer.
            ("md2", "1", None),
            // Solid-state members that need no carve-out.
            ("sdd", "0", Some("md3")),
            ("md3", "0", None),
        ] {
            let disk = dir.join(name);
            std::fs::create_dir_all(disk.join("queue")).expect("create queue");
            std::fs::create_dir_all(disk.join("holders")).expect("create holders");
            std::fs::write(disk.join("queue/rotational"), format!("{rotational}\n"))
                .expect("write rotational");
            std::fs::write(disk.join("queue/scheduler"), "none\n").expect("write scheduler");
            std::fs::write(disk.join("hidden"), "0\n").expect("write hidden");
            if let Some(holder) = holder {
                std::fs::write(disk.join("holders").join(holder), "").expect("write holder");
            }
        }

        let disks = read_disk_info_from(&dir).expect("read the synthetic block tree");
        let type_of = |name: &str| {
            disks
                .iter()
                .find(|d| d.name == name)
                .map(|d| d.disk_type.clone())
        };

        assert_eq!(type_of("vda"), Some(DiskType::SSD));
        assert_eq!(type_of("sdc"), Some(DiskType::HDD));
        assert_eq!(
            type_of("md0"),
            Some(DiskType::SSD),
            "an array of virtual (SSD-typed) members is solid-state"
        );
        assert_eq!(
            type_of("md1"),
            Some(DiskType::HDD),
            "one spinning member keeps the array mechanical"
        );
        assert_eq!(
            type_of("md2"),
            Some(DiskType::HDD),
            "an array with no member in the inventory keeps its own answer"
        );
        assert_eq!(type_of("md3"), Some(DiskType::SSD));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn md_array_over_partitions_inherits_its_members_medium() {
        // The holder link is created on the claimed bdev, so an array
        // assembled from partitions - the usual cloud layout, RAID1 on
        // vda1/vdb1 - records it on the partition's kobject, not in the whole
        // disk's `holders` directory. Neither member was counted, the array
        // kept the members' rotational flag and was typed HDD while both disks
        // read SSD: the streaming arm raised its read_ahead_kb and the
        // SSD-gated advice never fired.
        let dir = std::env::temp_dir().join(format!(
            "ktuner_md_partition_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();

        for (name, rotational) in [
            ("vda", "1"),
            ("vdb", "1"),
            ("md0", "1"),
            ("sdc", "1"),
            ("md1", "1"),
        ] {
            let disk = dir.join(name);
            std::fs::create_dir_all(disk.join("queue")).expect("create queue");
            std::fs::create_dir_all(disk.join("holders")).expect("create holders");
            std::fs::write(disk.join("queue/rotational"), format!("{rotational}\n"))
                .expect("write rotational");
            std::fs::write(disk.join("queue/scheduler"), "none\n").expect("write scheduler");
            std::fs::write(disk.join("hidden"), "0\n").expect("write hidden");
        }

        // md0 is assembled from the partitions vda1/vdb1; md1 from the whole
        // spinning sdc, which keeps it mechanical.
        std::fs::create_dir_all(dir.join("md0/slaves")).expect("create slaves");
        for (disk, part) in [("vda", "vda1"), ("vdb", "vdb1")] {
            std::fs::create_dir_all(dir.join(disk).join(part).join("holders"))
                .expect("create the partition");
            std::fs::write(dir.join(disk).join(part).join("partition"), "1\n")
                .expect("write the partition marker");
            std::os::unix::fs::symlink(
                dir.join(disk).join(part),
                dir.join("md0/slaves").join(part),
            )
            .expect("link the member");
            std::os::unix::fs::symlink(
                dir.join("md0"),
                dir.join(disk).join(part).join("holders/md0"),
            )
            .expect("link the holder");
        }
        std::fs::write(dir.join("sdc/holders/md1"), "").expect("write holder");

        let disks = read_disk_info_from(&dir).expect("read the synthetic block tree");
        let type_of = |name: &str| {
            disks
                .iter()
                .find(|d| d.name == name)
                .map(|d| d.disk_type.clone())
        };
        assert_eq!(type_of("vda"), Some(DiskType::SSD));
        assert_eq!(type_of("vdb"), Some(DiskType::SSD));
        assert_eq!(
            type_of("md0"),
            Some(DiskType::SSD),
            "an array over those members' partitions is solid-state too"
        );
        assert_eq!(
            type_of("md1"),
            Some(DiskType::HDD),
            "a spinning whole-disk member keeps its array mechanical"
        );

        std::fs::remove_dir_all(&dir).ok();
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
    fn parse_os_release_reads_quotes_and_escapes_like_the_shell() {
        // os-release(5): values may be enclosed in double or single quotes,
        // and "$", quotes, backslash and backtick are escaped with a
        // backslash following shell style.
        assert_eq!(parse_os_release("PRETTY_NAME='Alinux 3'\n"), "Alinux 3");
        assert_eq!(
            parse_os_release("PRETTY_NAME=\"Acme \\\"Edge\\\" 2\"\n"),
            "Acme \"Edge\" 2"
        );
        assert_eq!(
            parse_os_release("PRETTY_NAME=\"Cost \\$0 \\\\ \\`x\\`\"\n"),
            "Cost $0 \\ `x`"
        );
        // A backslash before any other character is literal inside double
        // quotes, and single quotes take everything literally.
        assert_eq!(parse_os_release("PRETTY_NAME=\"a\\nb\"\n"), "a\\nb");
        assert_eq!(parse_os_release("PRETTY_NAME='a\\$b'\n"), "a\\$b");
        // Unquoted values are optional for plain words.
        assert_eq!(
            parse_os_release("NAME=Alinux\nVERSION='3 (Hupo)'\n"),
            "Alinux 3 (Hupo)"
        );
    }

    #[test]
    fn os_distro_falls_back_to_the_vendor_os_release() {
        // os-release(5): read /etc/os-release when it exists, otherwise
        // /usr/lib/os-release — never both. Minimal images often ship only the
        // vendor copy.
        let dir = std::env::temp_dir().join(format!(
            "ktuner_os_release_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let etc = dir.join("etc-os-release");
        let usr_lib = dir.join("usr-lib-os-release");
        fs::write(&usr_lib, "PRETTY_NAME=\"Vendor 9\"\n").unwrap();
        let paths = [etc.to_str().unwrap(), usr_lib.to_str().unwrap()];

        assert_eq!(read_os_distro_from(&paths), "Vendor 9");

        // The /etc file takes precedence whenever it exists.
        fs::write(&etc, "PRETTY_NAME=\"Local 1\"\n").unwrap();
        assert_eq!(read_os_distro_from(&paths), "Local 1");

        fs::remove_file(&usr_lib).unwrap();
        fs::remove_file(&etc).unwrap();
        assert_eq!(read_os_distro_from(&paths), "Unknown");
        fs::remove_dir_all(&dir).ok();
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
    fn read_cpu_info_survives_non_utf8_bytes() {
        // `/proc/cpuinfo`'s model-name line is the kernel's verbatim copy of
        // the CPU brand string: `get_model_name()` memcpy's 48 bytes out of
        // the CPUID leaves and only trims spaces, with no encoding check
        // (v6.6 arch/x86/kernel/cpu/common.c:788-792,
        // 7.3.0-rc6:835-839), and under a hypervisor those bytes are whatever
        // the VMM put in the guest's CPUID. One byte outside UTF-8 used to
        // make the read fail,
        // which aborted `gather_system_info()` and every command with it --
        // although the model string is never reported and the processor count
        // from the same file is what the engine consumes.
        let dir = std::env::temp_dir().join(format!(
            "ktuner_cpuinfo_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cpuinfo");
        let mut content = Vec::new();
        content.extend_from_slice(b"processor\t: 0\nmodel name\t: Test CPU \xff\xfe\n\n");
        content.extend_from_slice(b"processor\t: 1\nmodel name\t: Test CPU \xff\xfe\n\n");
        fs::write(&path, &content).unwrap();

        let (model, cores) = read_cpu_info_from(path.to_str().unwrap())
            .expect("a non-UTF-8 model name must not hide the processors");
        assert_eq!(cores, 2);
        assert!(model.starts_with("Test CPU "), "{model}");

        fs::remove_dir_all(&dir).ok();
    }

    /// The release string is kernel text: `/proc/sys/kernel/osrelease` is the
    /// byte array `init_uts_ns.name.release` handed to the reader verbatim
    /// (v6.6 `kernel/utsname_sysctl.c`:93-99), and it is composed from build
    /// inputs — `KERNELRELEASE` is `VERSION.PATCHLEVEL.SUBLEVEL` plus
    /// `EXTRAVERSION`/`LOCALVERSION`, written into
    /// `include/config/kernel.release` (v6.6 `Makefile`:368-370, :1173-1180)
    /// and only length-checked before it becomes `UTS_RELEASE` (v6.6
    /// `Makefile`:1232-1239, a `wc -c` against 64, no encoding check). One byte
    /// outside UTF-8 therefore used to make `read_kernel_version()` fail,
    /// aborting `gather_system_info()` and every command with it.
    #[test]
    fn read_kernel_version_survives_non_utf8_bytes() {
        let dir = std::env::temp_dir().join(format!(
            "ktuner_release_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let release = dir.join("osrelease");
        std::fs::write(&release, b"6.6.0-vendor\xffbuild\n").unwrap();

        let version = read_kernel_version_from(release.to_str().unwrap())
            .expect("one byte outside UTF-8 in the release must not abort every command");
        assert_eq!(version, "6.6.0-vendor\u{fffd}build");

        fs::remove_dir_all(&dir).ok();
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
            cgroup_v2_relative_path(b"11:blkio:/init.scope\n0::/system.slice/ktuner.service\n")
                .as_deref(),
            Some(Path::new("/system.slice/ktuner.service"))
        );
        // A container with a private cgroup namespace sits at the root.
        assert_eq!(
            cgroup_v2_relative_path(b"0::/\n").as_deref(),
            Some(Path::new("/"))
        );
        // No unified line -> None.
        assert_eq!(cgroup_v2_relative_path(b"11:memory:/init.scope\n"), None);
        assert_eq!(cgroup_v2_relative_path(b""), None);
    }

    #[test]
    fn cgroup_v1_relative_path_reads_the_memory_controller_line() {
        let content =
            b"10:cpu,cpuacct:/user.slice\n11:memory:/system.slice/db.service\n0::/init.scope\n";
        assert_eq!(
            cgroup_v1_relative_path(content).as_deref(),
            Some(Path::new("/system.slice/db.service"))
        );
        // The controller list decides: other controllers' lines and the v2
        // line (empty controller field) are not the memory line.
        assert_eq!(
            cgroup_v1_relative_path(b"10:cpu,cpuacct:/user.slice\n"),
            None
        );
        assert_eq!(cgroup_v1_relative_path(b"0::/init.scope\n"), None);
        assert_eq!(cgroup_v1_relative_path(b""), None);
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
            cgroup_memory_limit_kb_from(&root, b"0::/system.slice/ktuner.service\n"),
            4 * 1024 * 1024,
            "the unit's own 4 GB limit must bind, not the root's max"
        );
        // A container with a private cgroup namespace sits at the root and
        // keeps seeing exactly the root's value (no limit here).
        assert_eq!(cgroup_memory_limit_kb_from(&root, b"0::/\n"), 0);
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
            cgroup_memory_limit_kb_from(&root, b"0::/system.slice/app.service\n"),
            2 * 1024 * 1024
        );
        // Deeper than the sandbox limit, with no own limit: the walk stops at
        // the first real limit on the way up.
        assert_eq!(
            cgroup_memory_limit_kb_from(&root, b"0::/kubepods/pod123/abc\n"),
            4 * 1024 * 1024
        );
        // A cgroup path deeper than anything in the tree still walks up.
        assert_eq!(
            cgroup_memory_limit_kb_from(&root, b"0::/system.slice/app.service/child\n"),
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
            cgroup_memory_limit_kb_from(&root, b"11:memory:/system.slice/db.service\n"),
            2 * 1024 * 1024,
            "the v1 memory line must navigate to its own cgroup's limit"
        );

        // No /proc/self/cgroup information at all: the root files decide,
        // exactly as before the chain walk existed.
        fs::write(root.join("memory.max"), b"4294967296\n").expect("write v2 root limit");
        assert_eq!(cgroup_memory_limit_kb_from(&root, b""), 4 * 1024 * 1024);

        // Nothing readable anywhere -> 0 (no limit known).
        let empty = cgroup_tree(&[]);
        assert_eq!(cgroup_memory_limit_kb_from(&empty, b"0::/deep/chain\n"), 0);
        assert_eq!(cgroup_memory_limit_kb_from(&empty, b""), 0);
        fs::remove_dir_all(&root).ok();
        fs::remove_dir_all(&empty).ok();
    }

    #[test]
    fn cgroup_v1_relative_path_keeps_colons_in_the_path() {
        // A cgroup directory may itself be named with a colon (kernfs only
        // forbids '/' and '\0'), and the membership line's path field runs
        // to the end of the line. An unbounded split truncated the path at
        // the first colon inside the name, so the walk navigated to an
        // unrelated directory.
        assert_eq!(
            cgroup_v1_relative_path(b"11:memory:/lxc:web/db\n").as_deref(),
            Some(Path::new("/lxc:web/db"))
        );
        // Multiple colons in the name survive whole as well.
        assert_eq!(
            cgroup_v1_relative_path(b"11:memory:/a:b:c\n").as_deref(),
            Some(Path::new("/a:b:c"))
        );
        // The controller list still decides, colons notwithstanding.
        assert_eq!(cgroup_v1_relative_path(b"10:cpu,cpuacct:/lxc:web\n"), None);
    }

    #[test]
    fn cgroup_limit_follows_a_colon_named_v1_cgroup() {
        // The truncated membership walked the wrong chain: a cgroup named
        // "lxc:web" holding a real 2 GB limit read as its colon-free prefix
        // "lxc" (an unrelated sibling with an 8 GB limit), so every
        // memory-scaled rule sized for a host that gets a quarter of what
        // the process actually has.
        let root = std::env::temp_dir().join(format!(
            "ktuner_cgroup_colon_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let leaf = root.join("memory/lxc:web");
        fs::create_dir_all(&leaf).expect("create colon-named cgroup");
        fs::write(leaf.join("memory.limit_in_bytes"), b"2147483648\n").expect("write leaf limit");
        let sibling = root.join("memory/lxc");
        fs::create_dir_all(&sibling).expect("create colon-prefix sibling");
        fs::write(sibling.join("memory.limit_in_bytes"), b"8589934592\n").expect("write sibling");
        fs::write(
            root.join("memory/memory.limit_in_bytes"),
            b"9223372036854775807\n", // v1's unlimited sentinel at the root
        )
        .expect("write root limit");
        assert_eq!(
            cgroup_memory_limit_kb_from(&root, b"11:memory:/lxc:web\n"),
            2 * 1024 * 1024,
            "the colon-named cgroup's own 2 GB limit must bind"
        );
        fs::remove_dir_all(&root).ok();
    }

    /// A cgroup directory name is a byte string — `cgroup_mkdir` rejects
    /// only '\n' on top of the '/' and '\0' that paths and C strings already
    /// exclude — so a name outside UTF-8 is legal, and the membership line
    /// carrying it is not valid UTF-8 either. Reading /proc/self/cgroup as a
    /// str rejected the whole file on such a name: the membership came out
    /// empty and the readers answered from the mount-root approximation,
    /// reporting the host's RAM and processors for a process whose own
    /// cgroup carries the real limits.
    #[test]
    fn non_utf8_v2_cgroup_name_binds_memory_and_cpu() {
        use std::os::unix::ffi::OsStrExt;

        let base = std::env::temp_dir().join(format!(
            "ktuner_cgroup_bytes_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("root");
        let leaf = root.join(std::ffi::OsStr::from_bytes(b"unit-\xff\xfe"));
        fs::create_dir_all(&leaf).expect("create non-UTF-8 cgroup directory");
        fs::write(leaf.join("memory.max"), b"2147483648\n").expect("write memory.max");
        fs::write(leaf.join("cpu.max"), b"200000 100000\n").expect("write cpu.max");
        // The mount-root files carry different limits, so the approximation
        // the failed read used to fall back to is visibly not the answer.
        fs::write(root.join("memory.max"), b"8589934592\n").expect("write root memory.max");
        fs::write(root.join("cpu.max"), b"800000 100000\n").expect("write root cpu.max");
        let membership = base.join("self-cgroup");
        fs::write(&membership, b"0::/unit-\xff\xfe\n").expect("write the membership file");

        assert_eq!(
            read_cgroup_memory_limit_kb_at(&root, &membership),
            2 * 1024 * 1024,
            "the non-UTF-8 cgroup's own 2 GB limit must bind, not the root's 8 GB"
        );
        assert_eq!(
            read_cgroup_cpu_limit_cores_at(&root, &membership),
            2,
            "the non-UTF-8 cgroup's own two-core quota must bind, not the root's eight"
        );
        fs::remove_dir_all(&base).ok();
    }

    /// The two readings a report carries come from one host read and one
    /// cgroup read: with nothing limiting the process they agree, and with a
    /// limit they split — the effective value rules scale by takes the limit
    /// while the host reading stays the host's. The published fields are
    /// exactly these two numbers.
    #[test]
    fn a_cgroup_limit_is_the_only_thing_that_splits_the_host_and_effective_readings() {
        let base = std::env::temp_dir().join(format!(
            "ktuner_host_readings_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("root");
        let unlimited = root.join("unlimited");
        let limited = root.join("limited");
        fs::create_dir_all(&unlimited).expect("create cgroup directory");
        fs::create_dir_all(&limited).expect("create cgroup directory");
        // The kernel's own no-limit spelling in the first cgroup, and a
        // 2-core quota plus a 2-GB limit in the second.
        fs::write(unlimited.join("memory.max"), b"max\n").expect("write memory.max");
        fs::write(unlimited.join("cpu.max"), b"max 100000\n").expect("write cpu.max");
        fs::write(limited.join("memory.max"), b"2147483648\n").expect("write memory.max");
        fs::write(limited.join("cpu.max"), b"200000 100000\n").expect("write cpu.max");

        let (host_cores, host_kb) = (16usize, 16 * 1024 * 1024u64);
        let membership = base.join("self-cgroup");

        fs::write(&membership, b"0::/unlimited\n").expect("write the membership file");
        let cgroup_cores = read_cgroup_cpu_limit_cores_at(&root, &membership);
        let cgroup_kb = read_cgroup_memory_limit_kb_at(&root, &membership);
        assert_eq!(cgroup_cores, 0, "the no-limit sentinel is not a limit");
        assert_eq!(cgroup_kb, 0, "the no-limit sentinel is not a limit");
        // The host field the report publishes is this same computation with
        // no limit to apply, so without one the two readings agree.
        assert_eq!(effective_cpu_cores(host_cores, cgroup_cores), host_cores);
        assert_eq!(
            effective_memory_gb(host_kb, cgroup_kb),
            effective_memory_gb(host_kb, 0)
        );

        fs::write(&membership, b"0::/limited\n").expect("write the membership file");
        let cgroup_cores = read_cgroup_cpu_limit_cores_at(&root, &membership);
        let cgroup_kb = read_cgroup_memory_limit_kb_at(&root, &membership);
        let host_gb = effective_memory_gb(host_kb, 0);
        let effective_cores = effective_cpu_cores(host_cores, cgroup_cores);
        let effective_gb = effective_memory_gb(host_kb, cgroup_kb);
        assert_eq!((effective_cores, effective_gb), (2, 2));
        assert!(
            effective_cores < host_cores,
            "the quota must sit below the host count"
        );
        assert!(
            effective_gb < host_gb,
            "the memory limit must sit below the host's memory"
        );
        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn non_utf8_v1_cgroup_name_binds_the_memory_chain() {
        use std::os::unix::ffi::OsStrExt;

        let base = std::env::temp_dir().join(format!(
            "ktuner_cgroup_bytes_v1_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("root");
        let leaf = root
            .join("memory")
            .join(std::ffi::OsStr::from_bytes(b"unit-\xff\xfe"));
        fs::create_dir_all(&leaf).expect("create non-UTF-8 v1 cgroup directory");
        fs::write(leaf.join("memory.limit_in_bytes"), b"4294967296\n").expect("write leaf limit");
        fs::write(root.join("memory/memory.limit_in_bytes"), b"8589934592\n")
            .expect("write root limit");
        let membership = base.join("self-cgroup");
        fs::write(&membership, b"11:memory:/unit-\xff\xfe\n").expect("write the membership file");

        assert_eq!(
            read_cgroup_memory_limit_kb_at(&root, &membership),
            4 * 1024 * 1024,
            "the non-UTF-8 v1 cgroup's own 4 GB limit must bind, not the root's 8 GB"
        );
        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn chain_min_value_refuses_to_walk_above_the_mount() {
        // Parity with cpu_chain_limit: a membership line whose path carries
        // a parent component must not let the walk read limit files outside
        // the mount root it was given.
        let base = std::env::temp_dir().join(format!(
            "ktuner_chain_guard_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("root");
        fs::create_dir_all(&root).expect("create mount root");
        fs::write(root.join("memory.max"), b"max\n").expect("write root limit");
        let outside = base.join("escape");
        fs::create_dir_all(&outside).expect("create outside dir");
        fs::write(outside.join("memory.max"), b"4294967296\n").expect("write outside limit");

        assert_eq!(
            chain_min_value(
                &root,
                Path::new("/../escape"),
                "memory.max",
                cgroup_v2_limit_kb
            ),
            None,
            "a parent component must not escape the mount"
        );
        // End to end: the v2 walk refuses, no v1 line exists, and the root
        // approximation answers from the mount root itself.
        assert_eq!(
            cgroup_memory_limit_kb_from(&root, b"0::/../escape\n"),
            0,
            "the escapee file outside the mount must not become the limit"
        );
        fs::remove_dir_all(&base).ok();
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
        assert_eq!(
            read_current_scheduler(Path::new("ktuner_no_such_disk")),
            "unknown"
        );
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
        let dir = Path::new("/sys/block/nvme0n1");
        assert_eq!(detect_disk_type(dir, "nvme0n1"), DiskType::NVMe);
        assert_eq!(detect_disk_type(dir, "nvme1n1"), DiskType::NVMe);
    }

    #[test]
    fn storvsc_virtual_disks_are_not_hdds() {
        // Hyper-V's storvsc driver presents the host's disks as SCSI devices
        // with the vendor/product pair "Msft"/"Virtual Disk", and the
        // rotational flag follows the hypervisor rather than the backing
        // medium: every disk of a Hyper-V or Azure VM and of a WSL2 instance
        // reads 1, including on an all-NVMe machine. The name cannot tell them
        // from a real spinning SCSI disk, so the device identity decides - the
        // carve-out virtio (vd*) and Xen (xvd*) already have.
        let hyperv = device_tree(&[
            ("device/", ""),
            ("device/vendor", "Msft\n"),
            ("device/model", "Virtual Disk\n"),
            ("queue/", ""),
            ("queue/rotational", "1\n"),
        ]);
        assert_eq!(detect_disk_type(&hyperv, "sda"), DiskType::SSD);

        // A real spinning SCSI disk keeps its type: same rotational flag, other
        // identity.
        let spinning = device_tree(&[
            ("device/", ""),
            ("device/vendor", "SEAGATE\n"),
            ("device/model", "ST4000NM0033\n"),
            ("queue/", ""),
            ("queue/rotational", "1\n"),
        ]);
        assert_eq!(detect_disk_type(&spinning, "sdb"), DiskType::HDD);

        // A virtual disk that says it is non-rotational is SSD either way, and
        // a directory without the identity files is not assumed virtual.
        let bare = device_tree(&[("queue/", ""), ("queue/rotational", "1\n")]);
        assert_eq!(detect_disk_type(&bare, "sdc"), DiskType::HDD);
    }

    /// A synthetic `/sys/block/<disk>` directory built from `(name, content)`
    /// entries; a name ending in `/` becomes a subdirectory, the shape a
    /// `holders` entry has (std-only, like the other tree fixtures).
    fn device_tree(entries: &[(&str, &str)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ktuner_device_tree_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create device dir");
        for (name, content) in entries {
            match name.strip_suffix('/') {
                Some(sub) => fs::create_dir_all(dir.join(sub)).expect("create subdirectory"),
                None => fs::write(dir.join(name), content.as_bytes()).expect("write entry"),
            }
        }
        dir
    }

    #[test]
    fn qemu_scsi_disks_are_not_hdds() {
        // QEMU's scsi-hd - the device behind virtio-scsi and every guest disk
        // attached to a SCSI HBA - defaults to vendor "QEMU" / product "QEMU
        // HARDDISK" with rotation_rate 0, which VPD page 0xb1 reports as
        // "rotation rate not reported"; the SCSI midlayer clears the
        // rotational flag only for a reported rate of exactly 1
        // (drivers/scsi/sd.c), so the guest disk reads rotational=1 whatever
        // medium backs it. Typed HDD, a streaming host raised its
        // read_ahead_kb and the SSD-gated advice (the scheduler,
        // `vm.page-cluster`, the database window) never fired.
        let qemu = device_tree(&[
            ("device/", ""),
            // sysfs pads the product string, so the identity compare trims.
            ("device/vendor", "QEMU\n"),
            ("device/model", "QEMU HARDDISK   \n"),
            ("queue/", ""),
            ("queue/rotational", "1\n"),
        ]);
        assert_eq!(detect_disk_type(&qemu, "sda"), DiskType::SSD);

        // The vendor alone is not the identity: another QEMU product keeps the
        // rotational flag's answer, so a future emulated device is not swept in
        // by the vendor string.
        let other_product = device_tree(&[
            ("device/", ""),
            ("device/vendor", "QEMU\n"),
            ("device/model", "QEMU DVD-ROM\n"),
            ("queue/", ""),
            ("queue/rotational", "1\n"),
        ]);
        assert_eq!(detect_disk_type(&other_product, "sdb"), DiskType::HDD);
    }

    #[test]
    fn holders_read_the_holding_disks() {
        // `/sys/block/<disk>/holders/<holder>` is one symlink per holder named
        // after the holder's gendisk (dm-0, md0, ...); the names are all the
        // reader needs.
        let held = device_tree(&[("holders/dm-0/", ""), ("holders/md0/", "")]);
        assert_eq!(
            read_disk_holders(&held.join("holders")),
            vec!["dm-0".to_string(), "md0".to_string()]
        );

        // An empty holders directory is the normal case for a mount source.
        let bare = device_tree(&[("holders/", "")]);
        assert!(read_disk_holders(&bare.join("holders")).is_empty());
        // A missing directory (or one that cannot be read) must not invent a
        // holder: a bare device stays a mount candidate.
        assert!(read_disk_holders(&bare.join("missing")).is_empty());
    }

    #[test]
    fn hidden_attribute_reads_only_one_as_hidden() {
        // Every disk exposes `hidden`: 1 marks a GENHD_FL_HIDDEN disk (the
        // NVMe multipath path devices), 0 an ordinary one.
        for (content, expected) in [
            ("1\n", true),
            ("0\n", false),
            ("", false),
            ("garbage", false),
        ] {
            let disk = device_tree(&[("hidden", content)]);
            let got = read_disk_hidden(&disk.join("hidden"));
            assert_eq!(
                got, expected,
                "hidden file {content:?} must read as {expected}"
            );
        }
        // An unreadable attribute reads as not hidden, keeping the advice.
        assert!(!read_disk_hidden(Path::new("/nonexistent/ktuner/hidden")));
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

    /// The tun driver reports a fixed SPEED_10000 for every device it creates
    /// (`tun_setup()` seeds the link ksettings with `tun_default_link_ksettings()`
    /// and `speed_show()` prints that default), so an UP tunnel is not evidence
    /// of a 10 GbE host link. A 1 GbE host running OpenVPN's default `tun0`
    /// used to be judged 10 GbE and got the 万兆 recommendations.
    #[test]
    fn network_info_ignores_the_tun_drivers_fixed_speed() {
        let dir = std::env::temp_dir().join(format!(
            "ktuner_net_tunnel_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&dir).ok();

        // A 1 GbE NIC next to the tunnel devices the tun driver creates.
        for (name, speed) in [("eth0", "1000\n"), ("tun0", "10000\n"), ("tap0", "10000\n")] {
            let iface = dir.join(name);
            fs::create_dir_all(&iface).expect("create fake interface dir");
            fs::write(iface.join("speed"), speed).expect("write speed");
        }

        let mut info = info_with_processes(&[]);
        info.network = read_network_info_from(&dir).expect("read fake sysfs tree");
        assert_eq!(
            info.max_net_speed(),
            1000,
            "a tunnel's fixed 10 Gb/s default must not decide the host link speed: {:?}",
            info.network
                .iter()
                .map(|n| (n.name.as_str(), n.speed_mbps))
                .collect::<Vec<_>>()
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// A tun/tap device can be created with any name (TUNSETIFF takes the
    /// caller's), and every device the tun driver registers still reports the
    /// fixed SPEED_10000 - Tailscale's `tailscale0` and an explicitly named
    /// OpenVPN `myvpn0` are the shapes the name prefixes miss. But the driver
    /// marks its own devices: each one carries a `tun_flags` sysfs attribute
    /// (v6.6 drivers/net/tun.c:2720), which no other netdev has, so the
    /// marker must close the hole the prefixes leave.
    #[test]
    fn network_info_ignores_a_custom_named_tun_device() {
        let dir = std::env::temp_dir().join(format!(
            "ktuner_net_custom_tun_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&dir).ok();

        // A 1 GbE NIC next to custom-named tunnels (no tun/tap prefix), each
        // carrying the tun driver's own marker attribute.
        for (name, speed, is_tun) in [
            ("eth0", "1000\n", false),
            ("tailscale0", "10000\n", true),
            ("myvpn0", "10000\n", true),
        ] {
            let iface = dir.join(name);
            fs::create_dir_all(&iface).expect("create fake interface dir");
            fs::write(iface.join("speed"), speed).expect("write speed");
            if is_tun {
                fs::write(iface.join("tun_flags"), "0x1000\n").expect("write tun_flags");
            }
        }

        let mut info = info_with_processes(&[]);
        info.network = read_network_info_from(&dir).expect("read fake sysfs tree");
        assert_eq!(
            info.max_net_speed(),
            1000,
            "a custom-named tun device's fixed 10 Gb/s default must not decide the host link speed: {:?}",
            info.network
                .iter()
                .map(|n| (n.name.as_str(), n.speed_mbps))
                .collect::<Vec<_>>()
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// The veth driver reports the same fixed SPEED_10000 for every device it
    /// registers (`veth_get_link_ksettings`, v6.6 drivers/net/veth.c:126-132;
    /// master:129-135), and its devices are the container plumbing: the host
    /// end of a pair is named by the CNI rather than `veth*` (Calico's
    /// `cali*`, Cilium's `lxc<id>` and `cilium_host`), and inside a container
    /// the same driver backs the workload's own `eth0`. What identifies the
    /// pair from this side is `iflink`: it names the peer's ifindex
    /// (`veth_get_iflink` returns it, 0 once the peer is gone), and a peer in
    /// another namespace is not one of THIS namespace's interfaces — while
    /// every other `ndo_get_iflink` user links inside it (a VLAN its parent,
    /// `vlan_dev_get_iflink`, so a VLAN keeps its parent's real reading; a
    /// bridge port its bridge; an attached tunnel the device it runs over).
    #[test]
    fn network_info_ignores_a_veth_peered_outside_the_namespace() {
        let dir = std::env::temp_dir().join(format!(
            "ktuner_net_veth_peer_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&dir).ok();

        // A 1 GbE NIC, a VLAN on it (its iflink names the NIC, which resolves
        // here), and two CNI host ends whose peers (ifindex 77/78) live in the
        // containers' namespaces.
        for (name, speed, ifindex, iflink) in [
            ("eth0", "1000\n", 2, 2),
            ("eth0.100", "1000\n", 3, 2),
            ("cali1234abcd", "10000\n", 4, 77),
            ("lxc30c0b5a", "10000\n", 5, 78),
        ] {
            let iface = dir.join(name);
            fs::create_dir_all(&iface).expect("create fake interface dir");
            fs::write(iface.join("speed"), speed).expect("write speed");
            fs::write(iface.join("ifindex"), format!("{ifindex}\n")).expect("write ifindex");
            fs::write(iface.join("iflink"), format!("{iflink}\n")).expect("write iflink");
        }

        let mut info = info_with_processes(&[]);
        info.network = read_network_info_from(&dir).expect("read fake sysfs tree");
        assert_eq!(
            info.max_net_speed(),
            1000,
            "a veth whose peer lives outside this namespace must not decide the host link speed: {:?}",
            info.network
                .iter()
                .map(|n| (n.name.as_str(), n.speed_mbps))
                .collect::<Vec<_>>()
        );
        assert!(
            info.network.iter().any(|n| n.name == "eth0.100"),
            "a link that resolves inside this namespace keeps its reading: {:?}",
            info.network
                .iter()
                .map(|n| (n.name.as_str(), n.speed_mbps))
                .collect::<Vec<_>>()
        );
        // The same list is the interface count the ARP-tuning rules qualify
        // hosts by (`network.len() >= 2`, rules/mod.rs arp_tuning_skipped):
        // container plumbing is not a second NIC, so only the two real
        // interfaces may remain.
        assert_eq!(
            info.network.len(),
            2,
            "the ARP rules' 2+ interface premise must not be satisfied by a veth pair: {:?}",
            info.network
                .iter()
                .map(|n| n.name.as_str())
                .collect::<Vec<_>>()
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// The same list, with the collision no local-index comparison could see:
    /// an ifindex only names a device inside its own namespace, so a
    /// container's `eth0` (ifindex 2 - the first device of the namespace it was
    /// created in) makes the host end of its pair report `iflink = 2`, which on
    /// the host is the first NIC. The pair was kept as a local interface and its
    /// driver's fixed 10000 Mbps decided the host's link speed. The veth marker
    /// is what skips it now: whether an `iflink` names a device here or in
    /// another namespace, the driver default is not this host's link.
    #[test]
    fn network_info_ignores_a_cni_host_end_whose_peer_index_collides() {
        let dir = std::env::temp_dir().join(format!(
            "ktuner_net_veth_collision_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&dir).ok();

        // eth0 is this namespace's first NIC (ifindex 2, self-linked, backed by
        // a bus device, so no veth marker); cali1234abcd is the host end of a
        // pair whose peer is the container's eth0 (ifindex 2 in ITS namespace),
        // so its iflink collides with eth0; eth0.100 is a VLAN on eth0 whose
        // reading comes from its parent and must stay.
        let eth0 = dir.join("eth0");
        fs::create_dir_all(&eth0).expect("create eth0");
        fs::write(eth0.join("speed"), "1000\n").expect("write speed");
        fs::write(eth0.join("ifindex"), "2\n").expect("write ifindex");
        fs::write(eth0.join("iflink"), "2\n").expect("write iflink");
        fs::create_dir_all(eth0.join("device")).expect("create the bus-device link");
        fs::write(eth0.join("uevent"), "INTERFACE=eth0\nIFINDEX=2\n").expect("write uevent");

        let cali = dir.join("cali1234abcd");
        fs::create_dir_all(&cali).expect("create cali host end");
        fs::write(cali.join("speed"), "10000\n").expect("write speed");
        fs::write(cali.join("ifindex"), "4\n").expect("write ifindex");
        fs::write(cali.join("iflink"), "2\n").expect("write iflink");
        fs::write(cali.join("uevent"), "INTERFACE=cali1234abcd\nIFINDEX=4\n")
            .expect("write uevent");

        let vlan = dir.join("eth0.100");
        fs::create_dir_all(&vlan).expect("create VLAN");
        fs::write(vlan.join("speed"), "1000\n").expect("write speed");
        fs::write(vlan.join("ifindex"), "3\n").expect("write ifindex");
        fs::write(vlan.join("iflink"), "2\n").expect("write iflink");
        fs::write(vlan.join("uevent"), "DEVTYPE=vlan\nINTERFACE=eth0.100\n").expect("write uevent");

        let mut info = info_with_processes(&[]);
        info.network = read_network_info_from(&dir).expect("read fake sysfs tree");
        let names: Vec<&str> = info.network.iter().map(|n| n.name.as_str()).collect();
        assert!(
            !names.contains(&"cali1234abcd"),
            "a CNI host end whose peer index collides with a local NIC must not decide the host link speed: {names:?}"
        );
        assert_eq!(
            info.max_net_speed(),
            1000,
            "the host's own NIC is the only real link here: {names:?}"
        );
        assert!(
            names.contains(&"eth0.100"),
            "a VLAN's reading comes from its parent and stays: {names:?}"
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// `dev_valid_name` accepts any interface name without `/`, `:` or
    /// whitespace, so a CNI (or an admin) can name a host end with non-UTF-8
    /// bytes — the crate's own speed lookup is built from the raw name for the
    /// same reason. The uevent file embeds that name in its `INTERFACE=` line,
    /// so `read_to_string` rejected the whole file and the reader answered
    /// "not a veth end" for a veth end: the caller's `iflink` test decided it
    /// and lost to the peer-index collision, the pair's fixed 10000 Mbps was
    /// counted as the host's link speed, and every 10 GbE rule sized its advice
    /// for a link the machine does not have.
    #[test]
    fn network_info_ignores_a_veth_end_whose_name_holds_raw_bytes() {
        use std::os::unix::ffi::OsStrExt;

        let dir = std::env::temp_dir().join(format!(
            "ktuner_net_veth_raw_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&dir).ok();

        // eth0 is this namespace's first NIC (ifindex 2, backed by a bus
        // device, so no veth marker): the local device the container-side peer
        // index collides with.
        let eth0 = dir.join("eth0");
        fs::create_dir_all(&eth0).expect("create eth0");
        fs::write(eth0.join("speed"), "1000\n").expect("write speed");
        fs::write(eth0.join("ifindex"), "2\n").expect("write ifindex");
        fs::write(eth0.join("iflink"), "2\n").expect("write iflink");
        fs::create_dir_all(eth0.join("device")).expect("create the bus-device link");
        fs::write(eth0.join("uevent"), "INTERFACE=eth0\nIFINDEX=2\n").expect("write uevent");

        // The host end of a container pair, named with a raw 0xE9 byte: its
        // peer is the container's eth0 (ifindex 2 in ITS namespace), so its
        // iflink collides with this host's eth0. Its uevent carries the raw
        // name, exactly as the kernel writes it.
        let raw = dir.join(std::ffi::OsStr::from_bytes(b"lxc\xe9a1b2"));
        fs::create_dir_all(&raw).expect("create the veth host end");
        fs::write(raw.join("speed"), "10000\n").expect("write speed");
        fs::write(raw.join("ifindex"), "4\n").expect("write ifindex");
        fs::write(raw.join("iflink"), "2\n").expect("write iflink");
        fs::write(raw.join("uevent"), b"INTERFACE=lxc\xe9a1b2\nIFINDEX=4\n").expect("write uevent");

        let mut info = info_with_processes(&[]);
        info.network = read_network_info_from(&dir).expect("read fake sysfs tree");
        let names: Vec<&str> = info.network.iter().map(|n| n.name.as_str()).collect();
        assert!(
            !names.contains(&"lxc\u{fffd}a1b2"),
            "a veth end whose name holds raw bytes must not decide the host link speed: {names:?}"
        );
        assert_eq!(
            info.max_net_speed(),
            1000,
            "the host's own NIC is the only real link here: {names:?}"
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// The controls the veth marker must not catch: a DEVTYPE-less link that
    /// reports a real negotiated rate (a team of 10 GbE ports, which like a
    /// bond links to itself and so fails the marker's link test) and a
    /// DEVTYPE-less link whose `speed` cannot be read at all (the attribute
    /// reads nothing while a device is down; a sit/ip6tnl tunnel has none
    /// either, and its `iflink` names the device it runs over). Neither is a
    /// veth end, so neither may be dropped.
    #[test]
    fn network_info_keeps_a_ten_gigabit_link_that_is_not_a_veth_end() {
        let dir = std::env::temp_dir().join(format!(
            "ktuner_net_ten_gig_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&dir).ok();

        let eth0 = dir.join("eth0");
        fs::create_dir_all(&eth0).expect("create eth0");
        fs::write(eth0.join("speed"), "1000\n").expect("write speed");
        fs::write(eth0.join("ifindex"), "2\n").expect("write ifindex");
        fs::write(eth0.join("iflink"), "2\n").expect("write iflink");
        fs::create_dir_all(eth0.join("device")).expect("create the bus-device link");

        let team0 = dir.join("team0");
        fs::create_dir_all(&team0).expect("create team0");
        fs::write(team0.join("speed"), "10000\n").expect("write speed");
        fs::write(team0.join("ifindex"), "3\n").expect("write ifindex");
        fs::write(team0.join("iflink"), "3\n").expect("write iflink");
        fs::write(team0.join("uevent"), "INTERFACE=team0\nIFINDEX=3\n").expect("write uevent");

        let down = dir.join("sit0");
        fs::create_dir_all(&down).expect("create sit0");
        fs::write(down.join("ifindex"), "4\n").expect("write ifindex");
        fs::write(down.join("iflink"), "2\n").expect("write iflink");
        fs::write(down.join("uevent"), "INTERFACE=sit0\nIFINDEX=4\n").expect("write uevent");

        let mut info = info_with_processes(&[]);
        info.network = read_network_info_from(&dir).expect("read fake sysfs tree");
        let names: Vec<&str> = info.network.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains(&"team0"),
            "a DEVTYPE-less link that links to itself keeps its own reading: {names:?}"
        );
        assert!(
            names.contains(&"sit0"),
            "an unreadable speed is not evidence of a veth end: {names:?}"
        );
        assert_eq!(info.max_net_speed(), 10000);

        fs::remove_dir_all(&dir).ok();
    }

    /// A veth pair whose two ends stay in ONE namespace is plumbing just like
    /// the cross-namespace pair: each end's `iflink` names the other end's
    /// ifindex, both of which are local, so the peer test kept both and the
    /// driver's fixed SPEED_10000 decided the host's link speed. The kernel
    /// gives a veth no name it must keep — `dev_valid_name` allows anything
    /// but `/`, `:` and whitespace — and a caller picks the names it asks for
    /// (`ip link add h1 type veth peer name h2`), so "a pair inside one
    /// namespace is already skipped by name" only holds for a pair someone
    /// happened to call `veth*`.
    #[test]
    fn network_info_ignores_a_veth_pair_that_stays_inside_the_namespace() {
        let dir = std::env::temp_dir().join(format!(
            "ktuner_net_local_veth_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&dir).ok();

        // The one real NIC, backed by a bus device.
        let eth0 = dir.join("eth0");
        fs::create_dir_all(&eth0).expect("create eth0");
        fs::write(eth0.join("speed"), "1000\n").expect("write speed");
        fs::write(eth0.join("ifindex"), "2\n").expect("write ifindex");
        fs::write(eth0.join("iflink"), "2\n").expect("write iflink");
        fs::create_dir_all(eth0.join("device")).expect("create the bus-device link");

        // A veth pair created in this namespace under names of the caller's
        // choosing: no bus device, no DEVTYPE, the driver's fixed 10000, and
        // each end's iflink naming the other end.
        for (name, ifindex, iflink) in [("h1", 7, 8), ("h2", 8, 7)] {
            let end = dir.join(name);
            fs::create_dir_all(&end).expect("create the veth end");
            fs::write(end.join("speed"), "10000\n").expect("write speed");
            fs::write(end.join("ifindex"), format!("{ifindex}\n")).expect("write ifindex");
            fs::write(end.join("iflink"), format!("{iflink}\n")).expect("write iflink");
            fs::write(
                end.join("uevent"),
                format!("INTERFACE={name}\nIFINDEX={ifindex}\n"),
            )
            .expect("write uevent");
        }

        let mut info = info_with_processes(&[]);
        info.network = read_network_info_from(&dir).expect("read fake sysfs tree");
        let names: Vec<&str> = info.network.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            names,
            ["eth0"],
            "a locally paired veth is not the host's uplink: {names:?}"
        );
        assert_eq!(
            info.max_net_speed(),
            1000,
            "the driver's fixed rate must not decide the host link speed: {names:?}"
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// A macvlan on a 10 GbE parent reports its parent's rate: the driver
    /// forwards the lower device's link ksettings
    /// (`macvlan_ethtool_get_link_ksettings`, v6.6
    /// drivers/net/macvlan.c:1077-1082). It also declares no `DEVTYPE=` in its
    /// uevent, has no bus device, and names its parent through `iflink`
    /// (`macvlan_dev_get_iflink`) — every shape the veth marker tests — so the
    /// 10000 Mbps it shows was taken for a pair's default and the interface was
    /// dropped. That left `network.len()` one interface short on a
    /// multi-homed host, and the ARP-tuning family, which qualifies hosts by
    /// `network.len() >= 2`, treated it as single-homed. The kernel records the
    /// parent relationship as a `lower_<name>` adjacency link
    /// (`netdev_upper_dev_link()`, v6.6 net/core/dev.c:7509-7510), and a veth
    /// end never has one.
    #[test]
    fn network_info_keeps_a_macvlan_that_reads_its_parent_rate() {
        let dir = std::env::temp_dir().join(format!(
            "ktuner_net_macvlan_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&dir).ok();

        // The 10 GbE NIC the macvlan runs over: a real link backed by a bus
        // device, linked to itself.
        let nic = dir.join("eth10g");
        fs::create_dir_all(&nic).expect("create the NIC");
        fs::write(nic.join("speed"), "10000\n").expect("write speed");
        fs::write(nic.join("ifindex"), "2\n").expect("write ifindex");
        fs::write(nic.join("iflink"), "2\n").expect("write iflink");
        fs::create_dir_all(nic.join("device")).expect("create the bus-device link");

        // The macvlan: the parent's 10000 Mbps, no DEVTYPE, and the adjacency
        // link naming what it runs over.
        let macvlan = dir.join("mv0");
        fs::create_dir_all(&macvlan).expect("create the macvlan");
        fs::write(macvlan.join("speed"), "10000\n").expect("write speed");
        fs::write(macvlan.join("ifindex"), "7\n").expect("write ifindex");
        fs::write(macvlan.join("iflink"), "2\n").expect("write iflink");
        fs::write(macvlan.join("uevent"), "INTERFACE=mv0\nIFINDEX=7\n").expect("write uevent");
        std::os::unix::fs::symlink(&nic, macvlan.join("lower_eth10g"))
            .expect("link the lower device");

        // Control: a veth end of the same shape, with no adjacency link.
        let veth = dir.join("h1");
        fs::create_dir_all(&veth).expect("create the veth end");
        fs::write(veth.join("speed"), "10000\n").expect("write speed");
        fs::write(veth.join("ifindex"), "8\n").expect("write ifindex");
        fs::write(veth.join("iflink"), "9\n").expect("write iflink");
        fs::write(veth.join("uevent"), "INTERFACE=h1\nIFINDEX=8\n").expect("write uevent");

        let mut info = info_with_processes(&[]);
        info.network = read_network_info_from(&dir).expect("read fake sysfs tree");
        let mut names: Vec<&str> = info.network.iter().map(|n| n.name.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            ["eth10g", "mv0"],
            "the macvlan keeps its parent's reading, the pair end stays out: {names:?}"
        );
        assert_eq!(info.max_net_speed(), 10000);

        fs::remove_dir_all(&dir).ok();
    }

    /// A VLAN and a bridge do not negotiate a link of their own: the drivers
    /// forward the device they run over (`vlan_dev_get_link_ksettings`, v6.6
    /// net/8021q/vlan_dev.c:662-667; `br_get_link_ksettings`,
    /// net/bridge/br_device.c:267-292), so a VLAN over a veth end or a bridge
    /// whose only port is one carries the veth driver's fixed 10000 Mbps
    /// (drivers/net/veth.c:126-134) as the host link. The veth marker and the
    /// pair test drop the ends, but the device one step above them kept the
    /// default and every 10 GbE-gated rule sized for a link the machine does
    /// not have - the same false positive #6883/#7207/#7314 close one device
    /// lower.
    #[test]
    fn network_info_ignores_a_vlan_and_bridge_over_a_veth_end() {
        let dir = std::env::temp_dir().join(format!(
            "ktuner_net_plumbing_upper_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&dir).ok();

        // The one real NIC, backed by a bus device.
        let eth0 = dir.join("eth0");
        fs::create_dir_all(&eth0).expect("create eth0");
        fs::write(eth0.join("speed"), "1000\n").expect("write speed");
        fs::write(eth0.join("ifindex"), "2\n").expect("write ifindex");
        fs::write(eth0.join("iflink"), "2\n").expect("write iflink");
        fs::create_dir_all(eth0.join("device")).expect("create the bus-device link");

        // The pair end the container uplink runs over: no bus device, no
        // DEVTYPE, the driver's fixed 10000.
        let veth = dir.join("h1");
        fs::create_dir_all(&veth).expect("create the veth end");
        fs::write(veth.join("speed"), "10000\n").expect("write speed");
        fs::write(veth.join("ifindex"), "8\n").expect("write ifindex");
        fs::write(veth.join("iflink"), "9\n").expect("write iflink");
        fs::write(veth.join("uevent"), "INTERFACE=h1\nIFINDEX=8\n").expect("write uevent");

        // A VLAN over that end: DEVTYPE=vlan, the forwarded 10000, and the
        // adjacency link naming the lower device.
        let vlan = dir.join("h1.100");
        fs::create_dir_all(&vlan).expect("create the vlan");
        fs::write(vlan.join("speed"), "10000\n").expect("write speed");
        fs::write(vlan.join("ifindex"), "10\n").expect("write ifindex");
        fs::write(vlan.join("iflink"), "8\n").expect("write iflink");
        fs::write(vlan.join("uevent"), "DEVTYPE=vlan\nINTERFACE=h1.100\n").expect("write uevent");
        std::os::unix::fs::symlink(&veth, vlan.join("lower_h1")).expect("link the lower device");

        // A bridge whose only port is that same end.
        let bridge = dir.join("br0");
        fs::create_dir_all(&bridge).expect("create the bridge");
        fs::write(bridge.join("speed"), "10000\n").expect("write speed");
        fs::write(bridge.join("ifindex"), "11\n").expect("write ifindex");
        fs::write(bridge.join("iflink"), "11\n").expect("write iflink");
        fs::write(bridge.join("uevent"), "DEVTYPE=bridge\nINTERFACE=br0\n").expect("write uevent");
        std::os::unix::fs::symlink(&veth, bridge.join("lower_h1")).expect("link the lower device");

        // A VLAN over the real NIC keeps its parent's reading, and a bridge
        // with the real NIC as one of its ports reports the best port's — the
        // veth port's fixed 10000, even though the real uplink is 1 GbE.
        let real_vlan = dir.join("eth0.200");
        fs::create_dir_all(&real_vlan).expect("create the vlan");
        fs::write(real_vlan.join("speed"), "1000\n").expect("write speed");
        fs::write(real_vlan.join("ifindex"), "12\n").expect("write ifindex");
        fs::write(real_vlan.join("iflink"), "2\n").expect("write iflink");
        fs::write(
            real_vlan.join("uevent"),
            "DEVTYPE=vlan\nINTERFACE=eth0.200\n",
        )
        .expect("write uevent");
        std::os::unix::fs::symlink(&eth0, real_vlan.join("lower_eth0")).expect("link the lower");
        let mixed_bridge = dir.join("br1");
        fs::create_dir_all(&mixed_bridge).expect("create the bridge");
        // br_get_link_ksettings reports the maximum over the up ports, so the
        // veth port's fixed 10000 is the bridge's reading on a 1 GbE uplink.
        fs::write(mixed_bridge.join("speed"), "10000\n").expect("write speed");
        fs::write(mixed_bridge.join("ifindex"), "13\n").expect("write ifindex");
        fs::write(mixed_bridge.join("iflink"), "13\n").expect("write iflink");
        fs::write(
            mixed_bridge.join("uevent"),
            "DEVTYPE=bridge\nINTERFACE=br1\n",
        )
        .expect("write uevent");
        std::os::unix::fs::symlink(&eth0, mixed_bridge.join("lower_eth0"))
            .expect("link the lower device");
        std::os::unix::fs::symlink(&veth, mixed_bridge.join("lower_h1"))
            .expect("link the lower device");

        let mut info = info_with_processes(&[]);
        info.network = read_network_info_from(&dir).expect("read fake sysfs tree");
        let mut names: Vec<&str> = info.network.iter().map(|n| n.name.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            ["eth0", "eth0.200"],
            "only the devices that read a real link survive: {names:?}"
        );
        assert_eq!(
            info.max_net_speed(),
            1000,
            "the pair end's fixed rate must not decide the host link speed: {names:?}"
        );

        // Control: a bridge whose real port is genuinely 25 GbE reads 25000,
        // above the veth default, so the new clause must keep it and the
        // physical NIC's rate must still decide. The devices are added after
        // the first read so the assertions above keep measuring the 1 GbE
        // shape.
        let eth25g = dir.join("eth25g");
        fs::create_dir_all(&eth25g).expect("create eth25g");
        fs::write(eth25g.join("speed"), "25000\n").expect("write speed");
        fs::write(eth25g.join("ifindex"), "14\n").expect("write ifindex");
        fs::write(eth25g.join("iflink"), "14\n").expect("write iflink");
        fs::create_dir_all(eth25g.join("device")).expect("create the bus-device link");
        let fast_bridge = dir.join("br2");
        fs::create_dir_all(&fast_bridge).expect("create the bridge");
        fs::write(fast_bridge.join("speed"), "25000\n").expect("write speed");
        fs::write(fast_bridge.join("ifindex"), "15\n").expect("write ifindex");
        fs::write(fast_bridge.join("iflink"), "15\n").expect("write iflink");
        fs::write(
            fast_bridge.join("uevent"),
            "DEVTYPE=bridge\nINTERFACE=br2\n",
        )
        .expect("write uevent");
        std::os::unix::fs::symlink(&eth25g, fast_bridge.join("lower_eth25g"))
            .expect("link the lower device");
        std::os::unix::fs::symlink(&veth, fast_bridge.join("lower_h1"))
            .expect("link the lower device");

        let mut control = info_with_processes(&[]);
        control.network = read_network_info_from(&dir).expect("read the control tree");
        let mut names: Vec<&str> = control.network.iter().map(|n| n.name.as_str()).collect();
        names.sort();
        assert!(
            names.contains(&"br2") && names.contains(&"eth25g"),
            "a bridge whose real port outruns the veth default is not plumbing: {names:?}"
        );
        assert_eq!(
            control.max_net_speed(),
            25000,
            "the physical 25 GbE NIC still decides: {names:?}"
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
            cpu_cores_host: 1,
            numa_nodes: 1,
            memory_total_gb: 1,
            memory_gb_host: 1,
            disks: vec![],
            network: vec![],
            sysctl: SysctlValues {
                swappiness: 60,
                dirty_ratio: 20,
                dirty_background_ratio: 10,
                somaxconn: Some(128),
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

    /// `dumb-init` is a container's PID 1, not a bare host's init system: it is
    /// "designed to run as PID 1 inside minimal container environments", and
    /// outside containers it runs as a child of a supervisor rather than as
    /// PID 1. A container with no marker file left — the cgroup-v2 shape whose
    /// `/proc/1/cgroup` is the namespace root `0::/` — must report Container,
    /// exactly like its peer shim `tini`, which the whitelist never listed.
    #[test]
    fn runtime_env_reads_dumb_init_as_a_container_init() {
        let root = std::env::temp_dir().join(format!(
            "ktuner_dumb_init_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&root).ok();
        let proc1 = root.join("proc/1");
        fs::create_dir_all(&proc1).expect("create temp proc dir");
        // A private cgroup namespace: PID 1's own cgroup is the namespace root,
        // so the cgroup check sees no container path.
        fs::write(proc1.join("cgroup"), b"0::/\n").expect("write cgroup");

        fs::write(proc1.join("sched"), b"dumb-init (1, #threads: 1)\n").expect("write sched");
        assert_eq!(
            runtime_env_from(&root),
            RuntimeEnv::Container,
            "dumb-init is a container's PID 1, not a bare host's init"
        );

        // Its peer shim, never whitelisted, already reads as a container.
        fs::write(proc1.join("sched"), b"tini (1, #threads: 1)\n").expect("write sched");
        assert_eq!(runtime_env_from(&root), RuntimeEnv::Container);

        // Guard: a real host init with the same cgroup is still a bare host.
        fs::write(proc1.join("sched"), b"systemd (1, #threads: 1)\n").expect("write sched");
        assert_eq!(runtime_env_from(&root), RuntimeEnv::BareHost);

        fs::remove_dir_all(&root).ok();
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

    /// `/proc/<pid>/sched` is registered only when the kernel is built with
    /// CONFIG_SCHED_DEBUG (v6.6 `fs/proc/base.c`:3255-3257 for the group
    /// leader, :3606-3608 for a thread; upstream removed the gate in v6.15),
    /// while `/proc/<pid>/comm` is registered unconditionally (:3264, :3609)
    /// and names exactly what the sched dump prints first: its first line is
    /// `"%s (%d, #threads: %d)"` of `p->comm` (v6.6 `kernel/sched/debug.c`).
    /// On a kernel without the debug option the sched file does not exist at
    /// all, so the fallback used to be skipped and a container whose PID 1
    /// has an unknown init stayed a bare host.
    #[test]
    fn runtime_env_reads_pid1_comm_without_the_sched_file() {
        let root = std::env::temp_dir().join(format!(
            "ktuner_pid1_comm_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&root).ok();
        let proc1 = root.join("proc/1");
        fs::create_dir_all(&proc1).expect("create temp proc dir");
        // A private cgroup namespace: PID 1's own cgroup is the namespace
        // root, so only the init's name can decide.
        fs::write(proc1.join("cgroup"), b"0::/\n").expect("write cgroup");

        // No `sched` file — the shape of a kernel built without
        // CONFIG_SCHED_DEBUG.
        fs::write(proc1.join("comm"), b"dumb-init\n").expect("write comm");
        assert_eq!(
            runtime_env_from(&root),
            RuntimeEnv::Container,
            "PID 1's comm decides an unknown init even without /proc/1/sched"
        );
        // Non-UTF-8 comm bytes survive the lossy read, as for the sched file.
        fs::write(proc1.join("comm"), b"app\xff\n").expect("write comm");
        assert_eq!(runtime_env_from(&root), RuntimeEnv::Container);
        // A known init is still a bare host, from the same source.
        fs::write(proc1.join("comm"), b"systemd\n").expect("write comm");
        assert_eq!(runtime_env_from(&root), RuntimeEnv::BareHost);

        // The older source still decides when comm is the missing one.
        fs::remove_file(proc1.join("comm")).ok();
        fs::write(proc1.join("sched"), b"s6-svscan (1, #threads: 1)\n").expect("write sched");
        assert_eq!(runtime_env_from(&root), RuntimeEnv::BareHost);
        fs::write(proc1.join("sched"), b"nginx (1, #threads: 1)\n").expect("write sched");
        assert_eq!(runtime_env_from(&root), RuntimeEnv::Container);

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

    /// Spawn an executable the test itself has just written or copied.
    ///
    /// On the CI runners' overlay filesystem the write-side close of that
    /// fresh file can lag the `exec`-side open, so an immediate single-shot
    /// spawn fails with `ETXTBSY` under load (#6638). `fs::write` and
    /// `fs::copy` have both returned by then, so this is an incidental
    /// timing assumption of the runner, not a property under test — retry
    /// the spawn for a short bounded window, the same way `wait_for_cmdline`
    /// retries the argv read.
    fn spawn_fresh_executable(
        command: &mut std::process::Command,
        context: &str,
    ) -> std::process::Child {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            match command.spawn() {
                Ok(child) => return child,
                Err(err)
                    if err.raw_os_error() == Some(libc::ETXTBSY)
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(err) => panic!("{context}: {err}"),
            }
        }
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

        let mut command = std::process::Command::new(&exporter_path);
        command.arg("30").stdout(std::process::Stdio::null());
        let mut exporter = spawn_fresh_executable(&mut command, "spawn exporter-named process");
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

        let mut command = std::process::Command::new(&client_path);
        command.arg("30").stdout(std::process::Stdio::null());
        let mut client = spawn_fresh_executable(&mut command, "spawn a client-named process");
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

    /// A companion program that carries a comm-named service's name with a `-`
    /// boundary is a different program, not the service: `consul-template`
    /// renders templates from a Consul cluster running elsewhere and
    /// `grafana-agent` ships metrics to a Grafana stack. `has_process` matches
    /// the name's first run, so such a host reported the service, classified
    /// as its workload (Consul is in `profile::classify`'s cache class) and
    /// ran its rules.
    #[test]
    fn service_companion_is_not_the_service() {
        let root = std::env::temp_dir().join(format!("ktuner_companion_{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(&root).expect("create temp dir");

        for (name, service, label) in [
            ("consul-template", "consul", "Consul"),
            // The Kubernetes data-plane sidecar: a separate client-side
            // program that talks to Consul servers, not the KV server itself.
            ("consul-dataplane", "consul", "Consul"),
            ("grafana-agent", "grafana", "Grafana"),
            // ClickHouse's coordination daemon: `has_process("clickhouse")` is
            // satisfied by its first name run, and the whole database program
            // (including `vm.nr_hugepages` and `kernel.shmmni`) then ran on a
            // host whose only ClickHouse process is a Raft store.
            ("clickhouse-keeper", "clickhouse", "ClickHouse"),
            // Oracle's instance-management agent and its updater: both keep
            // `oracle` as their first name run, so a host that runs only the
            // platform's default agents reported an Oracle database.
            ("oracle-cloud-agent", "oracle", "Oracle"),
            ("oracle-cloud-agent-updater", "oracle", "Oracle"),
        ] {
            let path = root.join(name);
            fs::copy("/bin/sleep", &path).expect("copy sleep to the companion's name");
            let mut command = std::process::Command::new(&path);
            command.arg("60").stdout(std::process::Stdio::null());
            let mut child = spawn_fresh_executable(&mut command, "spawn a companion-named process");
            let pid = child.id().to_string();
            wait_for_cmdline(&pid, name.as_bytes());

            let info = gather_system_info().expect("gather system info");
            let listed = info.has_process(service);
            let reported = crate::services::detect_services(&info).contains(&label);

            child.kill().ok();
            child.wait().ok();

            assert!(
                !listed,
                "{name} is a companion of {service}, not the service"
            );
            assert!(
                !reported,
                "the report must not name {label} for a host running only {name}"
            );
        }

        fs::remove_dir_all(&root).ok();
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

        let mut command = std::process::Command::new(&script);
        command.stdout(std::process::Stdio::null());
        let mut tool = spawn_fresh_executable(&mut command, "spawn a tool-named script");
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

    /// A write handle still open on a fresh script fails an immediate
    /// single-shot spawn with ETXTBSY — what the runners' lagging write-side
    /// close looks like to exec (#6638) — so the spawn helper must retry
    /// past it and win once the holder drops the file.
    #[test]
    fn spawn_retries_while_the_script_is_text_busy() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("ktuner_txtbusy_{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(&root).expect("create temp dir");
        let script = root.join("busy-tool.sh");
        fs::write(&script, b"#!/bin/sh\nsleep 30\n").expect("write tool script");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("make it runnable");

        let mut command = std::process::Command::new(&script);
        command.stdout(std::process::Stdio::null());

        let (opened, busy) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _held = std::fs::OpenOptions::new()
                .write(true)
                .open(&script)
                .expect("hold the script open for writing");
            opened.send(()).expect("signal the handle is held");
            std::thread::sleep(std::time::Duration::from_millis(150));
        });
        busy.recv().expect("wait for the write handle");

        let mut tool = spawn_fresh_executable(&mut command, "spawn a busy script");

        let pid = tool.id().to_string();
        wait_for_cmdline(&pid, b"busy-tool.sh");
        holder.join().expect("holder thread");
        tool.kill().ok();
        tool.wait().ok();
        fs::remove_dir_all(&root).ok();
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
            // The separate client-side copier binary (through 23.8).
            "clickhouse-copier",
            "clickhouse-format",
            "clickhouse-local",
            "clickhouse-obfuscator",
            "/usr/bin/clickhouse-client",
            // ClickHouse's coordination daemon, a separate program of the same
            // package with `clickhouse` as its first name run.
            "clickhouse-keeper",
            "/usr/bin/clickhouse-keeper",
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
            // The services whose companions SERVICE_COMPANION_TOOLS lists.
            "consul",
            "grafana",
        ] {
            assert!(
                !program_names_service_tool(daemon),
                "{daemon} is the service, not a tool"
            );
        }
    }

    /// ClickHouse documents the subcommand form beside the symlinks, and
    /// `clickhouse client` carries the server's argv[0] *and* its comm: without
    /// the subcommand a host running a client session against a remote server
    /// looked like a database host and took the whole database program.
    #[test]
    fn clickhouse_subcommand_separates_the_client_from_the_daemon() {
        for tool in [
            "/usr/bin/clickhouse\0client\0--host\0db1",
            "/usr/bin/clickhouse\0local\0--query\0select 1",
            "/usr/bin/clickhouse\0benchmark\0--host\0db1",
            "/usr/bin/clickhouse\0compressor\0--codec\0LZ4",
            "/usr/bin/clickhouse\0format\0--help",
            "/usr/bin/clickhouse\0obfuscator\0--seed\0x",
            // The keeper is the coordination daemon — the companion the symlink
            // side of this filter already drops under its own name — not the
            // server.
            "/usr/bin/clickhouse\0keeper\0--config\0/etc/clickhouse-keeper",
            // No subcommand at all prints the usage text.
            "/usr/bin/clickhouse",
        ] {
            assert!(
                cmdline_names_service_tool(tool),
                "{tool:?} runs a tool, not the service"
            );
        }

        for daemon in [
            // The binary in its server mode, however it is addressed.
            "/usr/bin/clickhouse\0server",
            "clickhouse\0server\0--config\0/etc/clickhouse-server/config.xml",
            // The packaged symlinks keep their own argv[0].
            "/usr/bin/clickhouse-server",
            "/usr/bin/clickhouse-serv",
        ] {
            assert!(
                !cmdline_names_service_tool(daemon),
                "{daemon:?} is the service, not a tool"
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
    fn test_runtime_service_reads_the_hbase_daemons() {
        // HBase's daemons run main classes under org.apache.hadoop.hbase, so
        // the ("org.apache.hadoop","hadoop") marker matched them first and a
        // real HBase cluster was reported as Hadoop: the table's own
        // ("hbase","hbase") entry can only match a class whose own name or
        // package run starts with hbase, which no HBase daemon class does.
        // The mislabel also made the streaming read-ahead advice fire on a
        // random-IO HBase host (eval_read_ahead_kb's is_streaming list).
        for class in [
            "org.apache.hadoop.hbase.master.HMaster",
            "org.apache.hadoop.hbase.regionserver.HRegionServer",
        ] {
            let cmdline = format!("java\u{0}-Xmx4g\u{0}{class}");
            assert_eq!(
                runtime_service_from_cmdline(&cmdline),
                Some("hbase"),
                "{class}"
            );
        }

        // The rest of the Hadoop family keeps the root mapping.
        for class in [
            "org.apache.hadoop.hdfs.server.namenode.NameNode",
            "org.apache.hadoop.hdfs.server.datanode.DataNode",
            "org.apache.hadoop.mapreduce.v2.app.MRAppMaster",
        ] {
            let cmdline = format!("java\u{0}{class}");
            assert_eq!(
                runtime_service_from_cmdline(&cmdline),
                Some("hadoop"),
                "{class}"
            );
        }
    }

    #[test]
    fn test_runtime_service_reads_every_hbase_server() {
        // The other servers HBase's own launcher starts — `bin/hbase rest`,
        // `thrift` and `zookeeper` exec RESTServer, ThriftServer and
        // HQuorumPeer — run under the same org.apache.hadoop.hbase packages,
        // so the Hadoop root claimed them exactly as it did the master and the
        // region server, and their hosts were reported as Hadoop. HQuorumPeer
        // is HBase's own ZooKeeper peer: it serves the deployment's znodes.
        for class in [
            "org.apache.hadoop.hbase.rest.RESTServer",
            "org.apache.hadoop.hbase.thrift.ThriftServer",
            "org.apache.hadoop.hbase.zookeeper.HQuorumPeer",
        ] {
            let cmdline = format!("java\u{0}-Xmx4g\u{0}{class}");
            assert_eq!(
                runtime_service_from_cmdline(&cmdline),
                Some("hbase"),
                "{class}"
            );
        }

        // Only those classes are mapped, so the jobs and clients of the family
        // still read as Hadoop rather than as a server of the deployment.
        for class in [
            "org.apache.hadoop.hbase.mapreduce.Export",
            "org.apache.hadoop.hbase.zookeeper.ZKMainServer",
        ] {
            let cmdline = format!("java\u{0}-cp\u{0}hbase-server.jar\u{0}{class}");
            assert_eq!(
                runtime_service_from_cmdline(&cmdline),
                Some("hadoop"),
                "{class}"
            );
        }
    }

    #[test]
    fn test_runtime_service_matches_the_bookkeeper_bookie() {
        // A Pulsar storage node runs BookKeeper bookies, not the broker: both
        // `pulsar bookie` and BookKeeper's own `bookkeeper bookie` exec
        // org.apache.bookkeeper.server.Main. The bookie owns the server
        // package from the root, like org.apache.kafka.connect for Kafka.
        let cmdline =
            "java\0-Xmx2g\0org.apache.bookkeeper.server.Main\0--conf\0/pulsar/conf/bookkeeper.conf";
        assert_eq!(runtime_service_from_cmdline(cmdline), Some("bookkeeper"));

        // Pulsar up to 2.6 and BookKeeper up to 4.5 started the bookie through
        // its own entry point instead.
        let cmdline =
            "java\0org.apache.bookkeeper.proto.BookieServer\0--conf\0/pulsar/conf/bookkeeper.conf";
        assert_eq!(runtime_service_from_cmdline(cmdline), Some("bookkeeper"));

        // The autorecovery daemon and the shell share the bookkeeper root but
        // keep no ledger storage, so they are not the bookie.
        for class in [
            "org.apache.bookkeeper.replication.AutoRecoveryMain",
            "org.apache.bookkeeper.bookie.BookieShell",
        ] {
            let cmdline = format!("java\0{class}\0--conf\0/pulsar/conf/bookkeeper.conf");
            assert_eq!(runtime_service_from_cmdline(&cmdline), None, "{class}");
        }

        // The broker is still Pulsar.
        let cmdline = "java\0org.apache.pulsar.PulsarBrokerStarter\0--broker-conf";
        assert_eq!(runtime_service_from_cmdline(cmdline), Some("pulsar"));
    }

    #[test]
    fn test_runtime_service_ignores_the_pulsar_client_tools() {
        // `bin/pulsar-admin`, `bin/pulsar-perf` and `bin/pulsar-client` exec
        // the JVM with the tool's own main class, so by the time the process
        // is sampled its argv[0] is `java` and the `pulsar-*` name the tool
        // filter looks for is gone — only the class is left to decide. Mapping
        // the bare `org.apache.pulsar` package root (the way Kafka's markers
        // deliberately do NOT map `org.apache.kafka`) classified a host that
        // only runs a client or the load generator as a Pulsar broker:
        // `check.services` reported "Pulsar" and the workload classified as
        // streaming, so the streaming and read-ahead rules fired on a host
        // that stores no ledgers.
        for class in [
            "org.apache.pulsar.admin.cli.PulsarAdminTool",
            "org.apache.pulsar.testclient.PulsarPerfTestTool",
            "org.apache.pulsar.client.cli.PulsarClientTool",
            // The broker ships its own maintenance tools under the same root.
            "org.apache.pulsar.broker.tools.BrokerTool",
            "org.apache.pulsar.compaction.CompactorTool",
            "org.apache.pulsar.PulsarVersionStarter",
        ] {
            let cmdline = format!("java\0-Xmx2g\0{class}\0--url\0pulsar://broker:6650");
            assert_eq!(runtime_service_from_cmdline(&cmdline), None, "{class}");
        }

        // Every daemon `bin/pulsar` execs is still the service itself.
        for class in [
            "org.apache.pulsar.PulsarBrokerStarter",
            "org.apache.pulsar.PulsarStandaloneStarter",
            "org.apache.pulsar.proxy.server.ProxyServiceStarter",
            "org.apache.pulsar.websocket.service.WebSocketServiceStarter",
            "org.apache.pulsar.functions.worker.FunctionWorkerStarter",
        ] {
            let cmdline = format!("java\0-Xmx2g\0{class}\0--config\0/pulsar/conf/broker.conf");
            assert_eq!(
                runtime_service_from_cmdline(&cmdline),
                Some("pulsar"),
                "{class}"
            );
        }
    }

    #[test]
    fn test_runtime_service_ignores_the_zookeeper_client() {
        // `bin/zkCli.sh` execs the JVM with the client's own main class
        // (apache/zookeeper bin/zkCli.sh: org.apache.zookeeper.ZooKeeperMain),
        // so by the time the process is sampled its argv[0] is `java` and only
        // the class is left to decide — the case the Pulsar markers above
        // spell out for the tools whose launcher execs the JVM. The bare
        // `org.apache.zookeeper` root read the client's class as an ensemble
        // member, so a host that only administers a remote ensemble reported
        // "ZooKeeper" and classified as the in-memory coordination-KV workload,
        // earning the THP and vfs_cache_pressure advice for a service that
        // runs elsewhere.
        let cmdline = concat!(
            "java\0-Dzookeeper.log.dir=/var/log/zookeeper\0",
            "-cp\0/usr/share/zookeeper/lib/*:conf\0",
            "org.apache.zookeeper.ZooKeeperMain\0-server\0zk1:2181",
        );
        assert_eq!(runtime_service_from_cmdline(cmdline), None);

        // The ensemble members keep deciding: `zkServer.sh` runs
        // org.apache.zookeeper.server.quorum.QuorumPeerMain, and its
        // ZOO_MAIN_CLASS override names the same package in standalone form.
        for class in [
            "org.apache.zookeeper.server.quorum.QuorumPeerMain",
            "org.apache.zookeeper.server.ZooKeeperServerMain",
        ] {
            let cmdline = format!("java\0-Xmx1g\0{class}\0/opt/zookeeper/conf/zoo.cfg");
            assert_eq!(
                runtime_service_from_cmdline(&cmdline),
                Some("zookeeper"),
                "{class}"
            );
        }
    }

    #[test]
    fn test_runtime_service_ignores_the_spark_client() {
        // `bin/spark-submit` execs the JVM with the client's own main class
        // (apache/spark bin/spark-submit: org.apache.spark.deploy.SparkSubmit
        // through spark-class), so the `spark-submit` argv[0] every
        // `spark-*` name check looks for is gone by the time the process is
        // sampled — `program_names_service_tool` even names spark-submit as an
        // executable that "can only be one of its tools", and the bare
        // `org.apache.spark` root read the class anyway. A host that only
        // submits jobs to a cluster running elsewhere reported "Spark",
        // classified as the streaming workload and earned the 2048 KB
        // read-ahead window on spinning disks.
        let cmdline = concat!(
            "java\0-cp\0/opt/spark/conf:/opt/spark/jars/*\0",
            "org.apache.spark.deploy.SparkSubmit\0--master\0spark://master:7077\0app.jar",
        );
        assert_eq!(runtime_service_from_cmdline(cmdline), None);

        // The standalone cluster daemons keep deciding: sbin/start-master.sh,
        // start-worker.sh and start-history-server.sh run these classes, and
        // the Spark Connect server is a daemon of its own.
        for class in [
            "org.apache.spark.deploy.master.Master",
            "org.apache.spark.deploy.worker.Worker",
            "org.apache.spark.deploy.history.HistoryServer",
            "org.apache.spark.sql.connect.service.SparkConnectServer",
        ] {
            let cmdline = format!("java\0-Xmx1g\0{class}\0--host\0node1");
            assert_eq!(
                runtime_service_from_cmdline(&cmdline),
                Some("spark"),
                "{class}"
            );
        }
    }

    #[test]
    fn test_runtime_service_ignores_the_cassandra_tools() {
        // `bin/nodetool` runs org.apache.cassandra.tools.NodeTool, the
        // administration client, and `tools/bin/cassandra-stress` runs
        // org.apache.cassandra.stress.Stress, the load generator. Both exec
        // the JVM with their own main class, and the bare
        // `org.apache.cassandra` root read them as the database itself — the
        // same client/load-generator shape the Pulsar markers above refuse —
        // so a host that only administers or benchmarks a cluster running
        // elsewhere reported "Cassandra" in `check.services`.
        for class in [
            "org.apache.cassandra.tools.NodeTool",
            "org.apache.cassandra.stress.Stress",
        ] {
            let cmdline = format!("java\0-cp\0/opt/cassandra/lib/*\0{class}\0status");
            assert_eq!(runtime_service_from_cmdline(&cmdline), None, "{class}");
        }

        // `bin/cassandra` sets classname=org.apache.cassandra.service.
        // CassandraDaemon, which keeps deciding.
        let cmdline = concat!(
            "java\0-Dcassandra.logdir=/var/log/cassandra\0-cp\0/opt/cassandra/lib/*\0",
            "org.apache.cassandra.service.CassandraDaemon\0-f",
        );
        assert_eq!(runtime_service_from_cmdline(cmdline), Some("cassandra"));
    }

    #[test]
    fn test_runtime_service_ignores_the_flink_client() {
        // `bin/flink` execs the JVM with the client's own main class
        // (flink-dist flink-bin/bin/flink: org.apache.flink.client.cli.
        // CliFrontend), so the CLI's argv[0] is gone by the time the process
        // is sampled — only the class is left to decide, and the bare
        // `org.apache.flink` root read it as the cluster. A host that only
        // submits jobs to a cluster running elsewhere reported "Flink",
        // classified as the streaming workload and earned the read-ahead
        // advice.
        let cmdline = concat!(
            "java\0-cp\0/opt/flink/lib/*\0",
            "org.apache.flink.client.cli.CliFrontend\0run\0-m\0yarn-cluster\0app.jar",
        );
        assert_eq!(runtime_service_from_cmdline(cmdline), None);

        // The daemons keep deciding: bin/flink-daemon.sh maps its entrypoint
        // names to these classes, and the Kubernetes and YARN deployments
        // ship their own entry packages; the SQL gateway is the one daemon
        // outside them.
        for class in [
            "org.apache.flink.runtime.taskexecutor.TaskManagerRunner",
            "org.apache.flink.runtime.entrypoint.StandaloneSessionClusterEntrypoint",
            "org.apache.flink.container.entrypoint.StandaloneApplicationClusterEntryPoint",
            "org.apache.flink.runtime.webmonitor.history.HistoryServer",
            "org.apache.flink.runtime.zookeeper.FlinkZooKeeperQuorumPeer",
            "org.apache.flink.kubernetes.entrypoint.KubernetesSessionClusterEntrypoint",
            "org.apache.flink.yarn.entrypoint.YarnSessionClusterEntrypoint",
            "org.apache.flink.table.gateway.SqlGateway",
        ] {
            let cmdline =
                format!("java\0-Xmx1g\0{class}\0--config\0/opt/flink/conf/flink-conf.yaml");
            assert_eq!(
                runtime_service_from_cmdline(&cmdline),
                Some("flink"),
                "{class}"
            );
        }
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

        // Pulsar's main class likewise carries the service only in its package
        // run, not in the class name.
        let cmdline = "java\0org.apache.pulsar.PulsarBrokerStarter\0--broker-conf";
        assert_eq!(runtime_service_from_cmdline(cmdline), Some("pulsar"));

        // Solr is started by a generic launcher: `bin/solr` execs
        // `java <opts> -jar start.jar --module=http`, so the jar is Jetty's and
        // the service is only in the property the launcher always sets. The
        // previous fixture (`java org.apache.solr.servlet.SolrDispatchFilter`)
        // was a servlet filter with no main, a command line no Solr can
        // produce, and it certified the two markers as reachable while every
        // stock deployment ran as plain Java.
        let cmdline = "java\0-server\0-Xms512m\0-Dsolr.log.dir=/var/solr/logs\0\
                       -Djetty.port=8983\0-Djetty.home=/opt/solr/server\0\
                       -Dsolr.solr.home=/var/solr\0-Dsolr.install.dir=/opt/solr\0\
                       -jar\0start.jar\0--module=http";
        assert_eq!(runtime_service_from_cmdline(cmdline), Some("solr"));
        // The same command line without Solr's property is another Jetty app.
        let cmdline = "java\0-Djetty.home=/opt/jetty\0-jar\0start.jar\0--module=http";
        assert_eq!(runtime_service_from_cmdline(cmdline), None);

        // Elasticsearch, including a versioned jar basename.
        let cmdline = "java\0-jar\0/usr/share/elasticsearch-8.12.0/lib/elasticsearch-8.12.0.jar";
        assert_eq!(runtime_service_from_cmdline(cmdline), Some("elasticsearch"));
    }

    /// Every canonical name the runtime markers resolve has to be reportable:
    /// `read_processes` pushes that name into the same process list the report's
    /// `services` field filters, so a marker without a matching table entry is a
    /// service the engine detects (and, for hadoop, one a rule acts on) while a
    /// host running only that service reports no service at all.
    #[test]
    fn every_runtime_marker_resolves_to_a_reported_service() {
        for (_, service) in RUNTIME_SERVICE_MARKERS {
            assert!(
                !crate::services::detect_services(&info_with_processes(&[*service])).is_empty(),
                "{service} is resolved by the runtime markers but the report cannot name it"
            );
        }
    }

    #[test]
    fn test_runtime_service_reads_the_rabbitmq_erlang_node() {
        // RabbitMQ's launcher execs the Erlang VM, so the process is
        // `beam.smp` and the service is only visible in the command line.
        // The node name gives it away (RABBITMQ_NODENAME defaults to
        // `rabbit@<host>`), and the same token comes back from the launcher's
        // own arguments: `-s rabbit boot` names RABBITMQ_BOOT_MODULE, whose
        // default is `rabbit`, and the boot file lives under
        // `.../mnesia/rabbit@<host>/rabbit`. The `-pa` directory names nothing
        // (its basename is `ebin`), which is why the table's
        // ("rabbitmq", "RabbitMQ") entry could never match before: the
        // packaged daemon is never a process named `rabbitmq`.
        let cmdline = concat!(
            "/usr/lib/erlang/erts-13.2.6.5/bin/beam.smp\0-W\0w\0-MBas\0ageffcbf\0",
            "-MHas\0ageffcbf\0-MBlmbcs\0512\0-MMmcs\030\0",
            "-sname\0rabbit@node1\0-s\0rabbit\0boot\0",
            "-boot\0/var/lib/rabbitmq/mnesia/rabbit@node1/rabbit\0",
            "-pa\0/usr/lib/rabbitmq/lib/rabbitmq_server-3.13.7/ebin\0",
            "-kernel\0prevent_overlapping_partitions\0false\0-noinput",
        );
        assert_eq!(runtime_service_from_cmdline(cmdline), Some("rabbitmq"));

        // An unrelated Erlang application keeps its VM name: only the default
        // RabbitMQ node/boot prefix is claimed.
        let cmdline = concat!(
            "/usr/lib/erlang/erts-13.2.6.5/bin/beam.smp\0-sname\0myapp@node1\0",
            "-boot\0/var/lib/myapp/mnesia/myapp@node1/start\0-pa\0/opt/myapp/ebin",
        );
        assert_eq!(runtime_service_from_cmdline(cmdline), None);
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

        let mut exporter = {
            let mut command = std::process::Command::new(&exporter_path);
            command.arg("30");
            spawn_fresh_executable(&mut command, "spawn exporter-named process")
        };
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
            // The collectors that carry the server's name.
            "prometheus-node-exporter",
            "prometheus-agent",
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
            // The Prometheus server is the workload of a monitoring host — the
            // services table lists it as `Prometheus` — not a collector of
            // itself. The prefix clause used to drop it and made that entry
            // unreachable.
            "prometheus",
            "/usr/local/bin/prometheus",
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
    fn collector_directories_do_not_hide_daemon_programs() {
        for cmdline in [
            "/opt/exporter/postgres\0-D\0/var/lib/postgresql/data",
            "/opt/exporter/nginx\0-g\0daemon off;",
            "sh\0-c\0/opt/exporter/postgres -D /var/lib/postgresql/data",
            "/opt/exporter/bin/java\0com.example.Main",
        ] {
            assert!(!cmdline_names_helper(cmdline.split('\0')), "{cmdline:?}");
        }
    }

    #[test]
    fn collector_programs_and_raw_task_names_keep_matching() {
        for cmdline in [
            "/usr/local/bin/postgres_exporter\0--web.listen-address=:9187",
            "/opt/exporter/postgres_exporter\0--web.listen-address=:9187",
            "sh\0-c\0/opt/exporter/postgres_exporter --web.listen-address=:9187",
            "java\0-jar\0/opt/client/jmx_prometheus_httpserver.jar",
            // A script is run by its interpreter; argv[0] is not the program.
            "sh\0/opt/postgres_exporter.sh\0--web.listen-address=:9187",
            "/usr/bin/python3\0/opt/postgres_exporter.py\0--web.listen-address=:9187",
            "node\0/opt/node_exporter.js\0--web.listen-address=:9100",
        ] {
            assert!(cmdline_names_helper(cmdline.split('\0')), "{cmdline:?}");
        }
        assert!(is_monitoring_helper_name("node_exporter/x"));
    }

    #[test]
    fn a_live_daemon_under_a_collector_directory_stays_visible() {
        let root = std::env::temp_dir().join(format!("ktuner_daemon_path_{}", std::process::id()));
        let dir = root.join("exporter");
        fs::create_dir_all(&dir).expect("create private fixture");
        let program = dir.join("postgres");
        fs::copy("/bin/sleep", &program).expect("copy inert executable");
        let mut command = std::process::Command::new(&program);
        command.arg("30").stdout(std::process::Stdio::null());
        let mut child = spawn_fresh_executable(&mut command, "spawn inert daemon-named process");
        let pid = child.id().to_string();
        wait_for_cmdline(&pid, b"exporter/postgres");
        let comm = read_comm_from(&format!("/proc/{pid}/comm"));
        let filtered = is_monitoring_helper(&pid);
        child.kill().ok();
        child.wait().ok();
        fs::remove_dir_all(&root).ok();
        assert_eq!(comm.as_deref(), Some("postgres"));
        assert!(
            !filtered,
            "the program's parent directory is not its identity"
        );
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
        // An interpreter option is not a script path, and a plain script's
        // later config path still does not decide.
        assert!(!cmdline_names_helper(
            "python3\0-c\0import postgres_exporter".split('\0')
        ));
        assert!(!cmdline_names_helper(
            "python3\0/opt/manage.py\0--config=/etc/exporter.conf".split('\0')
        ));
        assert!(!cmdline_names_helper("".split('\0')));
    }

    /// The tool-side twin of the collector filter: the same kernel shape, and
    /// the same documented rule that the program decides.
    #[test]
    fn service_tool_scripts_run_by_any_interpreter_are_tools() {
        for tool in [
            "/usr/bin/python3\0/opt/kafka-topics.py",
            "/usr/bin/node\0/opt/elasticsearch-keystore.js",
            "/bin/bash\0/opt/spark-submit.sh",
            "/opt/kafka-topics.sh\0--list",
        ] {
            assert!(
                cmdline_names_service_tool(tool),
                "{tool:?} runs a tool, not the service"
            );
        }
        for daemon in [
            "/usr/bin/python3\0/opt/manage.py",
            "/usr/bin/python3\0-c\0import kafka",
            "/usr/bin/node\0-e\0require('kafka')",
        ] {
            assert!(
                !cmdline_names_service_tool(daemon),
                "{daemon:?} is not a service tool"
            );
        }
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

#[cfg(test)]
#[path = "memory_hierarchy_tests.rs"]
mod memory_hierarchy_tests;
