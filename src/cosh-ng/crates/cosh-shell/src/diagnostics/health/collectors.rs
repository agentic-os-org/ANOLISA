use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::config::HealthConfig;

use super::builder::HealthReportBuilder;
use super::model::{
    HealthCollector, HealthFactCategory, HealthFactSource, HealthFactValue, HealthSeverity,
    HealthUnavailableReason,
};

mod command;
mod disk;
mod kernel;
mod memory;
#[cfg(test)]
use disk::collect_disk;
pub(super) use disk::read_disk_sample;
pub(crate) use disk::{parse_df_p, DiskFacts};
mod system;
pub(super) use memory::read_memory;
pub(crate) use memory::{parse_proc_meminfo, MemoryFacts};
use system::run_system_command;

pub(super) use command::{
    begin_worker_check, enter_worker_job, stop_worker_job, worker_command_stopped,
};
pub(crate) use command::{
    run_health_command, HealthCommandConfig, HealthCommandError, HealthCommandOutput,
};
#[cfg(test)]
use kernel::parse_kernel_signals_with_boot_epoch;
pub(crate) use kernel::{collect_kernel_signals, parse_kernel_signals, KernelSignalFacts};

const SERVICE_COMMAND_TIMEOUT: Duration = Duration::from_millis(300);
const SERVICE_TOTAL_BUDGET: Duration = Duration::from_millis(800);

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
    if let Ok(host) = fs::read_to_string("/proc/sys/kernel/hostname") {
        let host = host.trim();
        if !host.is_empty() {
            builder.set_host(host).add_fact(
                HealthFactCategory::Host,
                "host.name",
                HealthFactValue::String(host.to_string()),
                None,
                HealthFactSource::Hostname,
                started.elapsed().as_millis(),
            );
        }
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
        match run_system_command(
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

pub(crate) fn parse_systemctl_is_active(
    service_name: &str,
    output: &HealthCommandOutput,
) -> Option<ServiceStatusFact> {
    if output.stdout_truncated || output.stderr_truncated {
        return None;
    }
    let status = output.stdout.lines().next()?.trim();
    if status.is_empty() {
        return None;
    }
    Some(ServiceStatusFact {
        name: service_name.to_string(),
        status: status.to_string(),
    })
}

fn unavailable_from_stderr(stderr: &str) -> HealthUnavailableReason {
    let stderr = stderr.to_ascii_lowercase();
    if stderr.contains("permission denied")
        || stderr.contains("insufficient permissions")
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

fn trim_os_release_value(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_string()
}

#[cfg(test)]
mod tests;
