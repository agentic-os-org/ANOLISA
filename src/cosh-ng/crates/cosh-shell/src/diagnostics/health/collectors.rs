use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use wait_timeout::ChildExt;

use crate::config::HealthConfig;

use super::builder::HealthReportBuilder;
use super::model::{
    HealthCollector, HealthFactCategory, HealthFactSource, HealthFactValue, HealthSeverity,
    HealthUnavailableReason,
};

const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_millis(750);
const DEFAULT_COMMAND_OUTPUT_LIMIT: usize = 16 * 1024;
const SERVICE_COMMAND_TIMEOUT: Duration = Duration::from_millis(300);
const SERVICE_TOTAL_BUDGET: Duration = Duration::from_millis(800);
static TEMP_OUTPUT_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HealthCommandConfig {
    pub(crate) timeout: Duration,
    pub(crate) output_limit_bytes: usize,
}

impl Default for HealthCommandConfig {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_COMMAND_TIMEOUT,
            output_limit_bytes: DEFAULT_COMMAND_OUTPUT_LIMIT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HealthCommandOutput {
    pub(crate) exit_code: Option<i32>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) stdout_truncated: bool,
    pub(crate) stderr_truncated: bool,
    pub(crate) elapsed_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HealthCommandError {
    CommandMissing,
    PermissionDenied,
    Timeout,
    Io,
}

impl HealthCommandError {
    pub(crate) fn unavailable_reason(&self) -> HealthUnavailableReason {
        match self {
            Self::CommandMissing => HealthUnavailableReason::CommandMissing,
            Self::PermissionDenied => HealthUnavailableReason::PermissionDenied,
            Self::Timeout => HealthUnavailableReason::Timeout,
            Self::Io => HealthUnavailableReason::ParseError,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CpuLoadFacts {
    pub(crate) cores: u64,
    pub(crate) load_1m: f64,
    pub(crate) load_5m: f64,
    pub(crate) load_15m: f64,
    pub(crate) load_per_core_1m: f64,
    pub(crate) load_per_core_5m: f64,
}

impl CpuLoadFacts {
    pub(crate) fn record(&self, builder: &mut HealthReportBuilder, elapsed_ms: u128) {
        builder
            .add_fact(
                HealthFactCategory::Cpu,
                "cpu.cores",
                HealthFactValue::Unsigned(self.cores),
                None,
                HealthFactSource::ProcLoadavg,
                elapsed_ms,
            )
            .add_fact(
                HealthFactCategory::Cpu,
                "cpu.load_1m",
                HealthFactValue::Float(self.load_1m),
                None,
                HealthFactSource::ProcLoadavg,
                elapsed_ms,
            )
            .add_fact(
                HealthFactCategory::Cpu,
                "cpu.load_5m",
                HealthFactValue::Float(self.load_5m),
                None,
                HealthFactSource::ProcLoadavg,
                elapsed_ms,
            )
            .add_fact(
                HealthFactCategory::Cpu,
                "cpu.load_15m",
                HealthFactValue::Float(self.load_15m),
                None,
                HealthFactSource::ProcLoadavg,
                elapsed_ms,
            )
            .add_fact(
                HealthFactCategory::Cpu,
                "cpu.load_per_core_1m",
                HealthFactValue::Float(self.load_per_core_1m),
                None,
                HealthFactSource::Derived,
                elapsed_ms,
            )
            .add_fact(
                HealthFactCategory::Cpu,
                "cpu.load_per_core_5m",
                HealthFactValue::Float(self.load_per_core_5m),
                None,
                HealthFactSource::Derived,
                elapsed_ms,
            );
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MemoryFacts {
    pub(crate) total_mib: u64,
    pub(crate) available_mib: u64,
    pub(crate) available_ratio: f64,
    pub(crate) used_ratio: f64,
    pub(crate) swap_total_mib: u64,
    pub(crate) swap_used_mib: u64,
    pub(crate) swap_used_ratio: f64,
}

impl MemoryFacts {
    pub(crate) fn record(&self, builder: &mut HealthReportBuilder, elapsed_ms: u128) {
        builder
            .add_fact(
                HealthFactCategory::Memory,
                "memory.total_mib",
                HealthFactValue::Unsigned(self.total_mib),
                Some("MiB".to_string()),
                HealthFactSource::ProcMeminfo,
                elapsed_ms,
            )
            .add_fact(
                HealthFactCategory::Memory,
                "memory.available_mib",
                HealthFactValue::Unsigned(self.available_mib),
                Some("MiB".to_string()),
                HealthFactSource::ProcMeminfo,
                elapsed_ms,
            )
            .add_fact(
                HealthFactCategory::Memory,
                "memory.available_ratio",
                HealthFactValue::Float(self.available_ratio),
                None,
                HealthFactSource::Derived,
                elapsed_ms,
            )
            .add_fact(
                HealthFactCategory::Memory,
                "memory.used_ratio",
                HealthFactValue::Float(self.used_ratio),
                None,
                HealthFactSource::Derived,
                elapsed_ms,
            )
            .add_fact(
                HealthFactCategory::Memory,
                "memory.swap_total_mib",
                HealthFactValue::Unsigned(self.swap_total_mib),
                Some("MiB".to_string()),
                HealthFactSource::ProcMeminfo,
                elapsed_ms,
            )
            .add_fact(
                HealthFactCategory::Memory,
                "memory.swap_used_mib",
                HealthFactValue::Unsigned(self.swap_used_mib),
                Some("MiB".to_string()),
                HealthFactSource::Derived,
                elapsed_ms,
            )
            .add_fact(
                HealthFactCategory::Memory,
                "memory.swap_used_ratio",
                HealthFactValue::Float(self.swap_used_ratio),
                None,
                HealthFactSource::Derived,
                elapsed_ms,
            );
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DiskFacts {
    pub(crate) riskiest_mount: String,
    pub(crate) riskiest_type: String,
    pub(crate) max_used_ratio: f64,
    pub(crate) available_gib: f64,
    pub(crate) root_used_ratio: Option<f64>,
    pub(crate) root_available_gib: Option<f64>,
}

impl DiskFacts {
    pub(crate) fn record(&self, builder: &mut HealthReportBuilder, elapsed_ms: u128) {
        builder
            .add_fact(
                HealthFactCategory::Disk,
                "filesystem.riskiest_mount",
                HealthFactValue::String(self.riskiest_mount.clone()),
                None,
                HealthFactSource::DfP,
                elapsed_ms,
            )
            .add_fact(
                HealthFactCategory::Disk,
                "filesystem.riskiest_type",
                HealthFactValue::String(self.riskiest_type.clone()),
                None,
                HealthFactSource::DfP,
                elapsed_ms,
            )
            .add_fact(
                HealthFactCategory::Disk,
                "filesystem.max_used_ratio",
                HealthFactValue::Float(self.max_used_ratio),
                None,
                HealthFactSource::DfP,
                elapsed_ms,
            )
            .add_fact(
                HealthFactCategory::Disk,
                "filesystem.available_gib",
                HealthFactValue::Float(self.available_gib),
                Some("GiB".to_string()),
                HealthFactSource::DfP,
                elapsed_ms,
            );
        if let Some(root_used_ratio) = self.root_used_ratio {
            builder.add_fact(
                HealthFactCategory::Disk,
                "filesystem.root_used_ratio",
                HealthFactValue::Float(root_used_ratio),
                None,
                HealthFactSource::DfP,
                elapsed_ms,
            );
        }
        if let Some(root_available_gib) = self.root_available_gib {
            builder.add_fact(
                HealthFactCategory::Disk,
                "filesystem.root_available_gib",
                HealthFactValue::Float(root_available_gib),
                Some("GiB".to_string()),
                HealthFactSource::DfP,
                elapsed_ms,
            );
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct KernelSignalFacts {
    pub(crate) oom_latest_age_seconds: Option<u64>,
    pub(crate) oom_killed_process: Option<String>,
    pub(crate) oom_latest_pid: Option<u64>,
    pub(crate) oom_latest_constraint: Option<String>,
    pub(crate) oom_latest_task_cgroup: Option<String>,
    pub(crate) oom_latest_oom_cgroup: Option<String>,
    pub(crate) oom_event_count_last_1h: u64,
    pub(crate) oom_event_count_last_24h: u64,
    pub(crate) oom_latest_source_line_count: u64,
    pub(crate) oom_latest_confidence: Option<String>,
    pub(crate) panic_recent: bool,
    pub(crate) log_source: String,
}

impl KernelSignalFacts {
    pub(crate) fn record(&self, builder: &mut HealthReportBuilder, elapsed_ms: u128) {
        if let Some(age) = self.oom_latest_age_seconds {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_latest_age_seconds",
                HealthFactValue::Unsigned(age),
                Some("seconds".to_string()),
                kernel_source(&self.log_source),
                elapsed_ms,
            );
        }
        if let Some(process) = &self.oom_killed_process {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_killed_process",
                HealthFactValue::String(process.clone()),
                None,
                kernel_source(&self.log_source),
                elapsed_ms,
            );
        }
        if let Some(pid) = self.oom_latest_pid {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_latest_pid",
                HealthFactValue::Unsigned(pid),
                None,
                kernel_source(&self.log_source),
                elapsed_ms,
            );
        }
        if let Some(constraint) = &self.oom_latest_constraint {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_latest_constraint",
                HealthFactValue::String(constraint.clone()),
                None,
                kernel_source(&self.log_source),
                elapsed_ms,
            );
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_latest_scope_label_id",
                HealthFactValue::String(oom_scope_label_id_from_constraint(constraint).to_string()),
                None,
                kernel_source(&self.log_source),
                elapsed_ms,
            );
        }
        if let Some(cgroup) = &self.oom_latest_task_cgroup {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_latest_task_cgroup",
                HealthFactValue::String(cgroup.clone()),
                None,
                kernel_source(&self.log_source),
                elapsed_ms,
            );
        }
        if let Some(cgroup) = &self.oom_latest_oom_cgroup {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_latest_oom_cgroup",
                HealthFactValue::String(cgroup.clone()),
                None,
                kernel_source(&self.log_source),
                elapsed_ms,
            );
        }
        if self.oom_event_count_last_1h > 0 {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_event_count_last_1h",
                HealthFactValue::Unsigned(self.oom_event_count_last_1h),
                None,
                kernel_source(&self.log_source),
                elapsed_ms,
            );
        }
        if self.oom_event_count_last_24h > 0 {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_event_count_last_24h",
                HealthFactValue::Unsigned(self.oom_event_count_last_24h),
                None,
                kernel_source(&self.log_source),
                elapsed_ms,
            );
        }
        if self.oom_latest_source_line_count > 0 {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_latest_source_line_count",
                HealthFactValue::Unsigned(self.oom_latest_source_line_count),
                None,
                kernel_source(&self.log_source),
                elapsed_ms,
            );
        }
        if let Some(confidence) = &self.oom_latest_confidence {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_latest_confidence",
                HealthFactValue::String(confidence.clone()),
                None,
                kernel_source(&self.log_source),
                elapsed_ms,
            );
        }
        builder
            .add_fact(
                HealthFactCategory::Kernel,
                "kernel.panic_recent",
                HealthFactValue::Bool(self.panic_recent),
                None,
                kernel_source(&self.log_source),
                elapsed_ms,
            )
            .add_fact(
                HealthFactCategory::Kernel,
                "kernel.log_source",
                HealthFactValue::String(self.log_source.clone()),
                None,
                kernel_source(&self.log_source),
                elapsed_ms,
            );
    }
}

fn oom_scope_label_id_from_constraint(raw: &str) -> &'static str {
    match raw {
        "CONSTRAINT_MEMCG" => "memcg",
        "CONSTRAINT_NONE" => "host",
        "CONSTRAINT_CPUSET" => "cpuset",
        "CONSTRAINT_MEMORY_POLICY" => "memory_policy",
        _ => "unknown",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceStatusFact {
    pub(crate) name: String,
    pub(crate) status: String,
}

impl ServiceStatusFact {
    pub(crate) fn record(&self, builder: &mut HealthReportBuilder, elapsed_ms: u128) {
        builder.add_fact(
            HealthFactCategory::Service,
            format!("service.{}.status", self.name),
            HealthFactValue::String(self.status.clone()),
            None,
            HealthFactSource::Systemctl,
            elapsed_ms,
        );
    }
}

pub(crate) fn collect_host(builder: &mut HealthReportBuilder) {
    let started = Instant::now();
    if !Path::new("/proc").exists() {
        builder.add_unavailable(
            HealthCollector::Host,
            HealthUnavailableReason::Unsupported,
            HealthSeverity::Unavailable,
            started.elapsed().as_millis(),
        );
        return;
    }
    match run_health_command("hostname", &[], HealthCommandConfig::default()) {
        Ok(output) if output.exit_code == Some(0) => {
            let host = output.stdout.trim();
            if !host.is_empty() {
                builder.set_host(host.to_string()).add_fact(
                    HealthFactCategory::Host,
                    "host.name",
                    HealthFactValue::String(host.to_string()),
                    None,
                    HealthFactSource::Hostname,
                    output.elapsed_ms,
                );
            }
        }
        Err(err) => {
            builder.add_unavailable(
                HealthCollector::Host,
                err.unavailable_reason(),
                HealthSeverity::Unavailable,
                started.elapsed().as_millis(),
            );
            return;
        }
        _ => {}
    }
    if let Ok(content) = fs::read_to_string("/etc/os-release") {
        if let Some(pretty_name) = parse_os_release_pretty_name(&content) {
            builder.add_fact(
                HealthFactCategory::Host,
                "os.pretty_name",
                HealthFactValue::String(pretty_name),
                None,
                HealthFactSource::OsRelease,
                started.elapsed().as_millis(),
            );
        }
    }
    match fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|content| parse_proc_uptime_seconds(&content))
    {
        Some(uptime) => {
            builder
                .add_fact(
                    HealthFactCategory::Host,
                    "host.uptime_seconds",
                    HealthFactValue::Unsigned(uptime),
                    Some("seconds".to_string()),
                    HealthFactSource::ProcUptime,
                    started.elapsed().as_millis(),
                )
                .add_check_done("host");
        }
        None => {
            builder.add_unavailable(
                HealthCollector::Host,
                HealthUnavailableReason::ParseError,
                HealthSeverity::Unavailable,
                started.elapsed().as_millis(),
            );
        }
    }
}

pub(crate) fn collect_cpu(builder: &mut HealthReportBuilder) {
    let started = Instant::now();
    let result = fs::read_to_string("/proc/stat")
        .ok()
        .and_then(|stat| parse_proc_stat_cpu_cores(&stat))
        .and_then(|cores| {
            fs::read_to_string("/proc/loadavg")
                .ok()
                .and_then(|load| parse_proc_loadavg(&load, cores))
        });
    match result {
        Some(facts) => {
            facts.record(builder, started.elapsed().as_millis());
            builder.add_check_done("cpu");
        }
        None => {
            builder.add_unavailable(
                HealthCollector::Cpu,
                HealthUnavailableReason::ParseError,
                HealthSeverity::Degraded,
                started.elapsed().as_millis(),
            );
        }
    }
}

pub(crate) fn collect_memory(builder: &mut HealthReportBuilder) {
    let started = Instant::now();
    match fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|content| parse_proc_meminfo(&content))
    {
        Some(facts) => {
            facts.record(builder, started.elapsed().as_millis());
            builder.add_check_done("memory");
        }
        None => {
            builder.add_unavailable(
                HealthCollector::Memory,
                HealthUnavailableReason::ParseError,
                HealthSeverity::Degraded,
                started.elapsed().as_millis(),
            );
        }
    }
}

pub(crate) fn collect_disk(builder: &mut HealthReportBuilder, config: &HealthConfig) {
    let started = Instant::now();
    match run_health_command("df", &["-P", "-T"], HealthCommandConfig::default())
        .or_else(|_| run_health_command("df", &["-P"], HealthCommandConfig::default()))
    {
        Ok(output) if output.exit_code == Some(0) => {
            if let Some(facts) = parse_df_p(&output.stdout, &config.critical_mounts) {
                facts.record(builder, output.elapsed_ms);
                builder.add_check_done("disk");
            } else {
                builder.add_unavailable(
                    HealthCollector::Disk,
                    HealthUnavailableReason::ParseError,
                    HealthSeverity::Degraded,
                    output.elapsed_ms,
                );
            }
        }
        Ok(output) => {
            builder.add_unavailable(
                HealthCollector::Disk,
                unavailable_from_stderr(&output.stderr),
                HealthSeverity::Degraded,
                output.elapsed_ms,
            );
        }
        Err(err) => {
            builder.add_unavailable(
                HealthCollector::Disk,
                err.unavailable_reason(),
                HealthSeverity::Degraded,
                started.elapsed().as_millis(),
            );
        }
    }
}

pub(crate) fn collect_kernel_signals(builder: &mut HealthReportBuilder, now_epoch_seconds: u64) {
    let started = Instant::now();
    let journal = run_health_command(
        "journalctl",
        &[
            "-k",
            "--since",
            "24 hours ago",
            "--no-pager",
            "-n",
            "200",
            "-o",
            "short-unix",
        ],
        HealthCommandConfig::default(),
    );
    match journal {
        Ok(output) if output.exit_code == Some(0) => {
            parse_kernel_signals(&output.stdout, "journalctl", now_epoch_seconds)
                .record(builder, output.elapsed_ms);
            builder.add_check_done("kernel_signal");
        }
        _ => collect_kernel_from_dmesg(builder, now_epoch_seconds, started),
    }
}

pub(crate) fn collect_configured_services(
    builder: &mut HealthReportBuilder,
    config: &HealthConfig,
) {
    if config.services.is_empty() {
        return;
    }
    let started = Instant::now();
    let deadline = started + SERVICE_TOTAL_BUDGET;
    let mut collected = false;
    for service in &config.services {
        let Some(timeout) = remaining_service_timeout(deadline) else {
            builder.add_unavailable(
                HealthCollector::ConfiguredService,
                HealthUnavailableReason::Timeout,
                HealthSeverity::Degraded,
                started.elapsed().as_millis(),
            );
            return;
        };
        match run_health_command(
            "systemctl",
            &["is-active", service.name.as_str()],
            HealthCommandConfig {
                timeout,
                ..HealthCommandConfig::default()
            },
        ) {
            Ok(output) => match record_configured_service_output(builder, &service.name, &output) {
                Ok(service_collected) => {
                    if service_collected {
                        collected = true;
                    }
                }
                Err(()) => return,
            },
            Err(err) => {
                builder.add_unavailable(
                    HealthCollector::ConfiguredService,
                    err.unavailable_reason(),
                    HealthSeverity::Degraded,
                    started.elapsed().as_millis(),
                );
                return;
            }
        }
    }
    if collected {
        builder.add_check_done("configured_service");
    }
}

fn record_configured_service_output(
    builder: &mut HealthReportBuilder,
    service_name: &str,
    output: &HealthCommandOutput,
) -> Result<bool, ()> {
    if let Some(fact) = parse_systemctl_is_active(service_name, output) {
        fact.record(builder, output.elapsed_ms);
        Ok(true)
    } else if output.exit_code != Some(0) {
        builder.add_unavailable(
            HealthCollector::ConfiguredService,
            unavailable_from_stderr(&output.stderr),
            HealthSeverity::Degraded,
            output.elapsed_ms,
        );
        Err(())
    } else {
        Ok(false)
    }
}

fn remaining_service_timeout(deadline: Instant) -> Option<Duration> {
    let remaining = deadline.checked_duration_since(Instant::now())?;
    service_timeout_for_remaining(remaining)
}

fn service_timeout_for_remaining(remaining: Duration) -> Option<Duration> {
    (!remaining.is_zero()).then_some(remaining.min(SERVICE_COMMAND_TIMEOUT))
}

pub(crate) fn run_health_command(
    program: &str,
    args: &[&str],
    config: HealthCommandConfig,
) -> Result<HealthCommandOutput, HealthCommandError> {
    let started = Instant::now();
    let stdout_path = temp_output_path(program, "stdout");
    let stderr_path = temp_output_path(program, "stderr");
    let stdout = File::create(&stdout_path).map_err(|_| HealthCommandError::Io)?;
    let stderr = File::create(&stderr_path).map_err(|_| HealthCommandError::Io)?;
    let spawn = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn();
    let mut child = match spawn {
        Ok(child) => child,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            cleanup_temp_outputs(&[&stdout_path, &stderr_path]);
            return Err(HealthCommandError::CommandMissing);
        }
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            cleanup_temp_outputs(&[&stdout_path, &stderr_path]);
            return Err(HealthCommandError::PermissionDenied);
        }
        Err(_) => {
            cleanup_temp_outputs(&[&stdout_path, &stderr_path]);
            return Err(HealthCommandError::Io);
        }
    };

    let status = match child.wait_timeout(config.timeout) {
        Ok(Some(status)) => status,
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            cleanup_temp_outputs(&[&stdout_path, &stderr_path]);
            return Err(HealthCommandError::Timeout);
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            cleanup_temp_outputs(&[&stdout_path, &stderr_path]);
            return Err(HealthCommandError::Io);
        }
    };

    let (stdout, stdout_truncated) = read_limited(&stdout_path, config.output_limit_bytes);
    let (stderr, stderr_truncated) = read_limited(&stderr_path, config.output_limit_bytes);
    cleanup_temp_outputs(&[&stdout_path, &stderr_path]);
    Ok(HealthCommandOutput {
        exit_code: status.code(),
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

pub(crate) fn parse_os_release_pretty_name(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key == "PRETTY_NAME").then(|| trim_os_release_value(value))
    })
}

pub(crate) fn parse_proc_uptime_seconds(content: &str) -> Option<u64> {
    content
        .split_whitespace()
        .next()?
        .parse::<f64>()
        .ok()
        .map(|value| value.floor() as u64)
}

pub(crate) fn parse_proc_loadavg(content: &str, cores: u64) -> Option<CpuLoadFacts> {
    if cores == 0 {
        return None;
    }
    let mut parts = content.split_whitespace();
    let load_1m = parts.next()?.parse::<f64>().ok()?;
    let load_5m = parts.next()?.parse::<f64>().ok()?;
    let load_15m = parts.next()?.parse::<f64>().ok()?;
    let cores_f64 = cores as f64;
    Some(CpuLoadFacts {
        cores,
        load_1m,
        load_5m,
        load_15m,
        load_per_core_1m: load_1m / cores_f64,
        load_per_core_5m: load_5m / cores_f64,
    })
}

pub(crate) fn parse_proc_stat_cpu_cores(content: &str) -> Option<u64> {
    let cores = content
        .lines()
        .filter(|line| {
            let Some(label) = line.split_whitespace().next() else {
                return false;
            };
            label.strip_prefix("cpu").is_some_and(|suffix| {
                !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit())
            })
        })
        .count() as u64;
    (cores > 0).then_some(cores)
}

pub(crate) fn parse_proc_meminfo(content: &str) -> Option<MemoryFacts> {
    let total_kib = meminfo_kib(content, "MemTotal")?;
    let available_kib = meminfo_kib(content, "MemAvailable")?;
    let swap_total_kib = meminfo_kib(content, "SwapTotal").unwrap_or(0);
    let swap_free_kib = meminfo_kib(content, "SwapFree").unwrap_or(0);
    if total_kib == 0 {
        return None;
    }
    let swap_used_kib = swap_total_kib.saturating_sub(swap_free_kib);
    let available_ratio = available_kib as f64 / total_kib as f64;
    let swap_used_ratio = if swap_total_kib == 0 {
        0.0
    } else {
        swap_used_kib as f64 / swap_total_kib as f64
    };
    Some(MemoryFacts {
        total_mib: total_kib / 1024,
        available_mib: available_kib / 1024,
        available_ratio,
        used_ratio: 1.0 - available_ratio,
        swap_total_mib: swap_total_kib / 1024,
        swap_used_mib: swap_used_kib / 1024,
        swap_used_ratio,
    })
}

pub(crate) fn parse_df_p(content: &str, critical_mounts: &[String]) -> Option<DiskFacts> {
    let entries = content
        .lines()
        .skip(1)
        .filter_map(|line| parse_df_line(line, critical_mounts))
        .collect::<Vec<_>>();
    let root = entries
        .iter()
        .find(|entry| entry.riskiest_mount == "/")
        .cloned();
    let mut riskiest = entries.into_iter().max_by(|left, right| {
        left.max_used_ratio
            .partial_cmp(&right.max_used_ratio)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                right
                    .available_gib
                    .partial_cmp(&left.available_gib)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    })?;
    if let Some(root) = root {
        riskiest.root_used_ratio = Some(root.max_used_ratio);
        riskiest.root_available_gib = Some(root.available_gib);
    }
    Some(riskiest)
}

pub(crate) fn parse_kernel_signals(
    content: &str,
    log_source: impl Into<String>,
    now_epoch_seconds: u64,
) -> KernelSignalFacts {
    parse_kernel_signals_with_boot_epoch(content, log_source, now_epoch_seconds, None)
}

fn parse_kernel_signals_with_boot_epoch(
    content: &str,
    log_source: impl Into<String>,
    now_epoch_seconds: u64,
    boot_epoch_seconds: Option<u64>,
) -> KernelSignalFacts {
    let mut latest_oom = None;
    let mut oom_epochs = Vec::new();
    let mut unclocked_oom_lines = 0;
    let mut panic_recent = false;
    for line in content.lines() {
        let lower = line.to_ascii_lowercase();
        if is_oom_line(&lower) {
            if let Some(epoch) = parse_time_prefix(line, boot_epoch_seconds) {
                oom_epochs.push(epoch);
                merge_oom_signal(&mut latest_oom, OomSignal::from_line(epoch, line));
            } else {
                unclocked_oom_lines += 1;
            }
        }
        if lower.contains("kernel panic") || lower.contains("panic:") {
            panic_recent = true;
        }
    }
    let oom_latest_age_seconds = latest_oom
        .as_ref()
        .map(|signal| now_epoch_seconds.saturating_sub(signal.epoch));
    let confidence = if latest_oom.is_some() {
        Some("high".to_string())
    } else if unclocked_oom_lines > 0 {
        Some("low".to_string())
    } else {
        None
    };
    KernelSignalFacts {
        oom_latest_age_seconds,
        oom_killed_process: latest_oom
            .as_ref()
            .and_then(|signal| signal.process.clone()),
        oom_latest_pid: latest_oom.as_ref().and_then(|signal| signal.pid),
        oom_latest_constraint: latest_oom
            .as_ref()
            .and_then(|signal| signal.constraint.clone()),
        oom_latest_task_cgroup: latest_oom
            .as_ref()
            .and_then(|signal| signal.task_cgroup.clone()),
        oom_latest_oom_cgroup: latest_oom
            .as_ref()
            .and_then(|signal| signal.oom_cgroup.clone()),
        oom_event_count_last_1h: count_unique_oom_events(&oom_epochs, now_epoch_seconds, 3600),
        oom_event_count_last_24h: count_unique_oom_events(&oom_epochs, now_epoch_seconds, 86400),
        oom_latest_source_line_count: latest_oom
            .as_ref()
            .map(|signal| signal.source_line_count)
            .unwrap_or(unclocked_oom_lines),
        oom_latest_confidence: confidence,
        panic_recent,
        log_source: log_source.into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OomSignal {
    epoch: u64,
    process: Option<String>,
    pid: Option<u64>,
    constraint: Option<String>,
    task_cgroup: Option<String>,
    oom_cgroup: Option<String>,
    source_line_count: u64,
}

impl OomSignal {
    fn from_line(epoch: u64, line: &str) -> Self {
        Self {
            epoch,
            process: parse_killed_process(line)
                .or_else(|| parse_oom_key_value(line, "task"))
                .or_else(|| parse_oom_key_value(line, "comm")),
            pid: parse_killed_pid(line).or_else(|| parse_oom_key_value(line, "pid")?.parse().ok()),
            constraint: parse_oom_key_value(line, "constraint"),
            task_cgroup: parse_oom_key_value(line, "task_memcg")
                .or_else(|| parse_oom_key_value(line, "task_cgroup")),
            oom_cgroup: parse_oom_key_value(line, "oom_memcg")
                .or_else(|| parse_oom_key_value(line, "oom_cgroup")),
            source_line_count: 1,
        }
    }

    fn merge_from(&mut self, other: OomSignal) {
        self.epoch = self.epoch.max(other.epoch);
        self.process = self.process.take().or(other.process);
        self.pid = self.pid.or(other.pid);
        self.constraint = self.constraint.take().or(other.constraint);
        self.task_cgroup = self.task_cgroup.take().or(other.task_cgroup);
        self.oom_cgroup = self.oom_cgroup.take().or(other.oom_cgroup);
        self.source_line_count += other.source_line_count;
    }
}

fn merge_oom_signal(latest: &mut Option<OomSignal>, signal: OomSignal) {
    let Some(current) = latest else {
        *latest = Some(signal);
        return;
    };
    if signal.epoch + 5 < current.epoch {
        return;
    }
    if signal.epoch > current.epoch + 5 {
        *latest = Some(signal);
    } else if signal.epoch >= current.epoch.saturating_sub(5) {
        current.merge_from(signal);
    }
}

fn is_oom_line(lower: &str) -> bool {
    lower.contains("oom-kill")
        || lower.contains("out of memory")
        || lower.contains("killed process")
}

fn count_unique_oom_events(epochs: &[u64], now_epoch_seconds: u64, window_seconds: u64) -> u64 {
    let mut epochs = epochs
        .iter()
        .copied()
        .filter(|epoch| now_epoch_seconds.saturating_sub(*epoch) <= window_seconds)
        .collect::<Vec<_>>();
    epochs.sort_unstable();
    let mut count = 0;
    let mut last_counted = None;
    for epoch in epochs {
        if last_counted.is_some_and(|last| epoch <= last + 5) {
            continue;
        }
        count += 1;
        last_counted = Some(epoch);
    }
    count
}

pub(crate) fn parse_systemctl_is_active(
    service_name: &str,
    output: &HealthCommandOutput,
) -> Option<ServiceStatusFact> {
    let status = output.stdout.lines().next()?.trim();
    if status.is_empty() {
        return None;
    }
    Some(ServiceStatusFact {
        name: service_name.to_string(),
        status: status.to_string(),
    })
}

fn parse_df_line(line: &str, critical_mounts: &[String]) -> Option<DiskFacts> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    let with_type = fields.len() >= 7;
    let (fs_type, available_1k, capacity, mount) = if with_type {
        (fields[1], fields[4], fields[5], fields[6])
    } else {
        (
            "unknown",
            fields.get(3).copied()?,
            fields.get(4).copied()?,
            fields.get(5).copied()?,
        )
    };
    if is_ignored_filesystem(fs_type) && !critical_mounts.iter().any(|item| item == mount) {
        return None;
    }
    let used_percent = capacity.trim_end_matches('%').parse::<f64>().ok()?;
    let available_gib = available_1k.parse::<f64>().ok()? / 1024.0 / 1024.0;
    Some(DiskFacts {
        riskiest_mount: mount.to_string(),
        riskiest_type: fs_type.to_string(),
        max_used_ratio: used_percent / 100.0,
        available_gib,
        root_used_ratio: None,
        root_available_gib: None,
    })
}

fn collect_kernel_from_dmesg(
    builder: &mut HealthReportBuilder,
    now_epoch_seconds: u64,
    started: Instant,
) {
    let boot_epoch_seconds =
        read_proc_uptime_seconds().map(|uptime| now_epoch_seconds.saturating_sub(uptime));
    match run_health_command(
        "bash",
        &["-o", "pipefail", "-c", "dmesg | tail -n 200"],
        HealthCommandConfig::default(),
    ) {
        Ok(output) if output.exit_code == Some(0) => {
            parse_kernel_signals_with_boot_epoch(
                &output.stdout,
                "dmesg",
                now_epoch_seconds,
                boot_epoch_seconds,
            )
            .record(builder, output.elapsed_ms);
            builder.add_check_done("kernel_signal");
        }
        Ok(output) => {
            builder.add_unavailable(
                HealthCollector::KernelSignal,
                unavailable_from_stderr(&output.stderr),
                HealthSeverity::Unavailable,
                output.elapsed_ms,
            );
        }
        Err(err) => {
            builder.add_unavailable(
                HealthCollector::KernelSignal,
                err.unavailable_reason(),
                HealthSeverity::Unavailable,
                started.elapsed().as_millis(),
            );
        }
    }
}

fn unavailable_from_stderr(stderr: &str) -> HealthUnavailableReason {
    let stderr = stderr.to_ascii_lowercase();
    if stderr.contains("permission denied")
        || stderr.contains("operation not permitted")
        || stderr.contains("access denied")
    {
        HealthUnavailableReason::PermissionDenied
    } else if stderr.contains("system has not been booted with systemd")
        || stderr.contains("failed to connect to bus")
    {
        HealthUnavailableReason::Unsupported
    } else {
        HealthUnavailableReason::ParseError
    }
}

fn is_ignored_filesystem(fs_type: &str) -> bool {
    matches!(
        fs_type,
        "tmpfs" | "devtmpfs" | "overlay" | "iso9660" | "squashfs" | "udf"
    )
}

fn meminfo_kib(content: &str, key: &str) -> Option<u64> {
    content.lines().find_map(|line| {
        let (line_key, rest) = line.split_once(':')?;
        if line_key != key {
            return None;
        }
        rest.split_whitespace().next()?.parse::<u64>().ok()
    })
}

fn trim_os_release_value(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_string()
}

fn parse_time_prefix(line: &str, boot_epoch_seconds: Option<u64>) -> Option<u64> {
    if let Some(uptime_seconds) = parse_dmesg_uptime_prefix(line) {
        return boot_epoch_seconds.map(|boot| boot.saturating_add(uptime_seconds));
    }
    let token = line.split_whitespace().next()?;
    let token = token.trim_end_matches(':');
    token
        .parse::<f64>()
        .ok()
        .filter(|value| *value > 0.0)
        .map(|value| value.floor() as u64)
}

fn parse_dmesg_uptime_prefix(line: &str) -> Option<u64> {
    let rest = line.strip_prefix('[')?;
    let (prefix, _) = rest.split_once(']')?;
    prefix
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| *value >= 0.0)
        .map(|value| value.floor() as u64)
}

fn parse_killed_process(line: &str) -> Option<String> {
    let marker = "Killed process";
    let start = line.find(marker)? + marker.len();
    let rest = line[start..].trim_start();
    let mut tokens = rest.split_whitespace();
    tokens.next()?;
    tokens
        .next()
        .map(|token| token.trim_matches(|ch| ch == '(' || ch == ')' || ch == ':' || ch == ','))
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn parse_killed_pid(line: &str) -> Option<u64> {
    let marker = "Killed process";
    let start = line.find(marker)? + marker.len();
    line[start..].split_whitespace().next()?.parse().ok()
}

fn parse_oom_key_value(line: &str, key: &str) -> Option<String> {
    let marker = format!("{key}=");
    let start = line.find(&marker)? + marker.len();
    let value = line[start..]
        .split([',', ' ', '\t'])
        .next()?
        .trim_matches(|ch| ch == '"' || ch == '\'' || ch == ':' || ch == ';');
    (!value.is_empty()).then(|| value.to_string())
}

fn kernel_source(log_source: &str) -> HealthFactSource {
    if log_source == "dmesg" {
        HealthFactSource::Dmesg
    } else {
        HealthFactSource::JournalctlK
    }
}

fn read_proc_uptime_seconds() -> Option<u64> {
    let content = fs::read_to_string("/proc/uptime").ok()?;
    content
        .split_whitespace()
        .next()?
        .parse::<f64>()
        .ok()
        .filter(|value| *value >= 0.0)
        .map(|value| value.floor() as u64)
}

fn temp_output_path(program: &str, label: &str) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let counter = TEMP_OUTPUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let sanitized = program
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>();
    std::env::temp_dir().join(format!(
        "cosh-health-{sanitized}-{label}-{}-{millis}-{counter}",
        std::process::id()
    ))
}

fn read_limited(path: &Path, limit: usize) -> (String, bool) {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return (String::new(), false),
    };
    let mut buffer = vec![0u8; limit.saturating_add(1)];
    let read = file.read(&mut buffer).unwrap_or_default();
    let truncated = read > limit;
    buffer.truncate(read.min(limit));
    (String::from_utf8_lossy(&buffer).to_string(), truncated)
}

fn cleanup_temp_outputs(paths: &[&Path]) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_host_text_facts() {
        let os_release = r#"
NAME="Anolis OS"
PRETTY_NAME="Anolis OS 23"
"#;

        assert_eq!(
            parse_os_release_pretty_name(os_release).as_deref(),
            Some("Anolis OS 23")
        );
        assert_eq!(parse_proc_uptime_seconds("123.45 678.90"), Some(123));
    }

    #[test]
    fn parses_loadavg_and_records_cpu_facts() {
        let facts = parse_proc_loadavg("8.00 4.00 2.00 1/100 42", 4).expect("loadavg");
        let mut builder = HealthReportBuilder::for_started_at(1);

        facts.record(&mut builder, 3);
        let report = builder.finish(5);

        assert_eq!(facts.load_per_core_1m, 2.0);
        assert_eq!(facts.load_per_core_5m, 1.0);
        assert_eq!(report.facts.len(), 6);
        assert_eq!(report.facts[0].key, "cpu.cores");
        assert_eq!(report.facts[5].key, "cpu.load_per_core_5m");
    }

    #[test]
    fn parses_proc_stat_cpu_cores() {
        let cores = parse_proc_stat_cpu_cores(
            r#"
cpu  1 2 3 4 5 6 7 8 9 10
cpu0 1 2 3 4 5 6 7 8 9 10
cpu1 1 2 3 4 5 6 7 8 9 10
intr 0
"#,
        );

        assert_eq!(cores, Some(2));
        assert_eq!(parse_proc_stat_cpu_cores("cpu 1 2 3\nintr 0"), None);
    }

    #[test]
    fn parses_meminfo_using_memavailable_and_swap_free() {
        let facts = parse_proc_meminfo(
            r#"
MemTotal:       8388608 kB
MemFree:         100000 kB
MemAvailable:    786432 kB
SwapTotal:      2097152 kB
SwapFree:        524288 kB
"#,
        )
        .expect("meminfo");

        assert_eq!(facts.total_mib, 8192);
        assert_eq!(facts.available_mib, 768);
        assert_eq!(facts.swap_total_mib, 2048);
        assert_eq!(facts.swap_used_mib, 1536);
        assert!((facts.available_ratio - 0.09375).abs() < f64::EPSILON);
        assert!((facts.swap_used_ratio - 0.75).abs() < f64::EPSILON);
    }

    #[test]
    fn parses_df_with_type_and_keeps_critical_overlay_mount() {
        let facts = parse_df_p(
            r#"
Filesystem     Type     1024-blocks    Used Available Capacity Mounted on
overlay        overlay     10485760 9437184   1048576      90% /
/dev/vdb1      xfs        104857600 83886080 20971520      80% /data
tmpfs          tmpfs        1048576       0   1048576       0% /run
"#,
            &["/".to_string()],
        )
        .expect("df");

        assert_eq!(facts.riskiest_mount, "/");
        assert_eq!(facts.riskiest_type, "overlay");
        assert_eq!(facts.max_used_ratio, 0.90);
        assert_eq!(facts.available_gib, 1.0);
        assert_eq!(facts.root_used_ratio, Some(0.90));
        assert_eq!(facts.root_available_gib, Some(1.0));
    }

    #[test]
    fn parses_df_without_type_and_ignores_tmpfs_when_type_is_available() {
        let facts = parse_df_p(
            r#"
Filesystem     1024-blocks    Used Available Capacity Mounted on
/dev/vda1        10485760 9437184   1048576      90% /
/dev/vdb1       104857600 99614720  5242880      95% /data
"#,
            &["/".to_string()],
        )
        .expect("df");

        assert_eq!(facts.riskiest_mount, "/data");
        assert_eq!(facts.riskiest_type, "unknown");
        assert_eq!(facts.max_used_ratio, 0.95);
        assert_eq!(facts.available_gib, 5.0);
        assert_eq!(facts.root_used_ratio, Some(0.90));
        assert_eq!(facts.root_available_gib, Some(1.0));
    }

    #[test]
    fn parses_df_with_type_ignores_read_only_image_mounts() {
        let facts = parse_df_p(
            r#"
Filesystem     Type     1024-blocks    Used Available Capacity Mounted on
/dev/vda2      ext4        10485760 1048576   9437184      10% /
/dev/vdb       iso9660        54352   54352         0     100% /mnt/lima-cidata
/dev/loop0     squashfs       64000   64000         0     100% /snap/core
"#,
            &["/".to_string()],
        )
        .expect("df");

        assert_eq!(facts.riskiest_mount, "/");
        assert_eq!(facts.riskiest_type, "ext4");
        assert_eq!(facts.max_used_ratio, 0.10);
        assert_eq!(facts.available_gib, 9.0);
        assert_eq!(facts.root_used_ratio, Some(0.10));
        assert_eq!(facts.root_available_gib, Some(9.0));
    }

    #[test]
    fn command_runner_caps_output_and_records_exit_code() {
        let output = run_health_command(
            "sh",
            &["-c", "printf abcdef; printf err >&2; exit 7"],
            HealthCommandConfig {
                timeout: Duration::from_secs(2),
                output_limit_bytes: 3,
            },
        )
        .expect("run command");

        assert_eq!(output.exit_code, Some(7));
        assert_eq!(output.stdout, "abc");
        assert_eq!(output.stderr, "err");
        assert!(output.stdout_truncated);
        assert!(!output.stderr_truncated);
    }

    #[test]
    fn command_runner_classifies_missing_and_timeout() {
        let missing = run_health_command(
            "cosh-health-command-missing-test",
            &[],
            HealthCommandConfig::default(),
        )
        .expect_err("missing command");
        assert_eq!(
            missing.unavailable_reason(),
            HealthUnavailableReason::CommandMissing
        );

        let timeout = run_health_command(
            "sh",
            &["-c", "sleep 2"],
            HealthCommandConfig {
                timeout: Duration::from_millis(20),
                output_limit_bytes: 1024,
            },
        )
        .expect_err("timeout");
        assert_eq!(
            timeout.unavailable_reason(),
            HealthUnavailableReason::Timeout
        );
    }

    #[test]
    fn service_timeout_caps_per_command_and_total_budget() {
        assert_eq!(
            service_timeout_for_remaining(Duration::from_millis(2_000)),
            Some(Duration::from_millis(300))
        );
        assert_eq!(
            service_timeout_for_remaining(Duration::from_millis(100)),
            Some(Duration::from_millis(100))
        );
        assert_eq!(service_timeout_for_remaining(Duration::ZERO), None);
    }

    #[test]
    fn parses_kernel_signals_from_short_unix_logs() {
        let facts = parse_kernel_signals(
            r#"
1999900.000 kernel: oom-kill: constraint=CONSTRAINT_NONE
1999950.000 kernel: Out of memory: Killed process 1234 (mysql) total-vm:1024kB
1999960.000 kernel: Kernel panic - not syncing: fatal exception
"#,
            "journalctl",
            2_000_000,
        );
        let mut builder = HealthReportBuilder::for_started_at(1);

        facts.record(&mut builder, 4);
        let report = builder.finish(8);

        assert_eq!(facts.oom_latest_age_seconds, Some(50));
        assert_eq!(facts.oom_killed_process.as_deref(), Some("mysql"));
        assert_eq!(facts.oom_latest_pid, Some(1234));
        assert_eq!(facts.oom_event_count_last_1h, 2);
        assert_eq!(facts.oom_event_count_last_24h, 2);
        assert_eq!(facts.oom_latest_confidence.as_deref(), Some("high"));
        assert!(facts.panic_recent);
        assert!(report
            .facts
            .iter()
            .any(|fact| fact.key == "kernel.oom_latest_age_seconds"));
        assert!(report
            .facts
            .iter()
            .any(|fact| fact.key == "kernel.log_source"));
        assert!(report
            .facts
            .iter()
            .any(|fact| fact.key == "kernel.oom_latest_pid"));
    }

    #[test]
    fn parses_kernel_signals_from_dmesg_uptime_prefix() {
        let facts = parse_kernel_signals_with_boot_epoch(
            r#"
[ 1990.100000] python3 invoked oom-killer: gfp_mask=0xcc0(GFP_KERNEL), order=0
[ 1995.200000] oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/cosh_kernel_oom,task=python3,pid=20495,uid=0
"#,
            "dmesg",
            2_000_000,
            Some(1_998_000),
        );

        assert_eq!(facts.oom_latest_age_seconds, Some(5));
        assert_eq!(facts.oom_latest_pid, Some(20495));
        assert_eq!(
            facts.oom_latest_constraint.as_deref(),
            Some("CONSTRAINT_MEMCG")
        );
        assert_eq!(
            facts.oom_latest_oom_cgroup.as_deref(),
            Some("/cosh_kernel_oom")
        );
        let mut builder = HealthReportBuilder::for_started_at(1);
        facts.record(&mut builder, 4);
        let report = builder.finish(8);
        assert!(report.facts.iter().any(|fact| {
            fact.key == "kernel.oom_latest_scope_label_id"
                && fact.value == HealthFactValue::String("memcg".to_string())
        }));
    }

    #[test]
    fn parses_kernel_signals_keep_latest_oom_details_together() {
        let facts = parse_kernel_signals(
            r#"
1999000.000 kernel: oom-kill:constraint=CONSTRAINT_NONE,task=old,pid=111
1999001.000 kernel: Out of memory: Killed process 111 (old) total-vm:1024kB
1999900.000 kernel: oom-kill:constraint=CONSTRAINT_MEMCG,oom_memcg=/cosh,task_memcg=/cosh/session,task=python3,pid=222
1999901.000 kernel: Memory cgroup out of memory: Killed process 222 (python3) total-vm:2048kB
"#,
            "journalctl",
            2_000_000,
        );

        assert_eq!(facts.oom_latest_age_seconds, Some(99));
        assert_eq!(facts.oom_killed_process.as_deref(), Some("python3"));
        assert_eq!(facts.oom_latest_pid, Some(222));
        assert_eq!(
            facts.oom_latest_constraint.as_deref(),
            Some("CONSTRAINT_MEMCG")
        );
        assert_eq!(
            facts.oom_latest_task_cgroup.as_deref(),
            Some("/cosh/session")
        );
        assert_eq!(facts.oom_latest_oom_cgroup.as_deref(), Some("/cosh"));
        assert_eq!(facts.oom_event_count_last_1h, 2);
        assert_eq!(facts.oom_event_count_last_24h, 2);
        assert_eq!(facts.oom_latest_source_line_count, 2);
    }

    #[test]
    fn parses_kernel_signals_unclocked_oom_as_low_confidence_only() {
        let facts = parse_kernel_signals(
            "kernel: Out of memory: Killed process 333 (worker) total-vm:1024kB",
            "dmesg",
            2_000_000,
        );

        assert_eq!(facts.oom_latest_age_seconds, None);
        assert_eq!(facts.oom_killed_process, None);
        assert_eq!(facts.oom_event_count_last_1h, 0);
        assert_eq!(facts.oom_latest_source_line_count, 1);
        assert_eq!(facts.oom_latest_confidence.as_deref(), Some("low"));
    }

    #[test]
    fn parses_systemctl_is_active_status() {
        let output = HealthCommandOutput {
            exit_code: Some(3),
            stdout: "inactive\n".to_string(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            elapsed_ms: 2,
        };
        let fact = parse_systemctl_is_active("redis", &output).expect("service status");
        let mut builder = HealthReportBuilder::for_started_at(1);

        fact.record(&mut builder, output.elapsed_ms);
        let report = builder.finish(3);

        assert_eq!(fact.status, "inactive");
        assert_eq!(report.facts[0].key, "service.redis.status");
        assert_eq!(
            report.facts[0].value,
            HealthFactValue::String("inactive".to_string())
        );
    }

    #[test]
    fn classifies_systemd_offline_as_service_unavailable() {
        let reason = unavailable_from_stderr(
            "System has not been booted with systemd as init system (PID 1). Can't operate.\n\
             Failed to connect to bus: Host is down",
        );

        assert_eq!(reason, HealthUnavailableReason::Unsupported);
    }

    #[test]
    fn configured_service_output_degrades_without_systemd_runtime() {
        let output = HealthCommandOutput {
            exit_code: Some(1),
            stdout: String::new(),
            stderr:
                "System has not been booted with systemd as init system (PID 1). Can't operate.\n\
                     Failed to connect to bus: Host is down"
                    .to_string(),
            stdout_truncated: false,
            stderr_truncated: false,
            elapsed_ms: 4,
        };
        let mut builder = HealthReportBuilder::for_started_at(1);

        let result = record_configured_service_output(
            &mut builder,
            "definitely-not-a-real-unit.service",
            &output,
        );
        let report = builder.finish(2);

        assert!(result.is_err());
        assert!(!report
            .checks_done
            .contains(&"configured_service".to_string()));
        assert!(report.unavailable.iter().any(|item| {
            item.collector == HealthCollector::ConfiguredService
                && item.severity == HealthSeverity::Degraded
                && item.reason == HealthUnavailableReason::Unsupported
        }));
    }
}
