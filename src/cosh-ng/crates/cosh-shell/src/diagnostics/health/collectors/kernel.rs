use std::fs;
use std::time::Instant;

use super::super::builder::HealthReportBuilder;
use super::super::model::{
    HealthCollector, HealthFactCategory, HealthFactCoverage, HealthFactScope, HealthFactSource,
    HealthFactValue, HealthSeverity,
};
use super::system::{run_system_command, system_program};
use super::{unavailable_from_stderr, HealthCommandConfig, HealthCommandError};

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
    coverage_since_epoch: Option<u64>,
    sampled_at_epoch: u64,
}

impl KernelSignalFacts {
    pub(crate) fn record(&self, builder: &mut HealthReportBuilder, elapsed_ms: u128) {
        if builder.current_check_id() == Some("kernel.oom") {
            builder.set_coverage(HealthFactCoverage::ReadWindow {
                since_ms: self
                    .coverage_since_epoch
                    .map(|epoch| u128::from(epoch) * 1000),
                until_ms: Some(u128::from(self.sampled_at_epoch) * 1000),
                max_records: 200,
                complete: false,
            });
            let scope = match self.oom_latest_constraint.as_deref() {
                Some("CONSTRAINT_MEMCG") => self
                    .oom_latest_oom_cgroup
                    .as_ref()
                    .map(|path| HealthFactScope::Cgroup(path.clone()))
                    .unwrap_or(HealthFactScope::Unknown),
                Some("CONSTRAINT_NONE") => HealthFactScope::Host,
                _ => HealthFactScope::Unknown,
            };
            builder.set_scope(scope);
        }
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
        if builder.current_check_id() == Some("kernel.oom") {
            builder.set_scope(HealthFactScope::Host);
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

pub(crate) fn collect_kernel_signals(builder: &mut HealthReportBuilder, now_epoch_seconds: u64) {
    let started = Instant::now();
    match system_program("journalctl") {
        Ok(program) => match super::run_health_command(
            program,
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
        ) {
            Ok(output) if output.exit_code == Some(0) => {
                parse_kernel_signals(&output.stdout, "journalctl", now_epoch_seconds)
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
            Err(error) => {
                builder.add_unavailable(
                    HealthCollector::KernelSignal,
                    error.unavailable_reason(),
                    HealthSeverity::Unavailable,
                    started.elapsed().as_millis(),
                );
            }
        },
        Err(HealthCommandError::CommandMissing) => {
            collect_kernel_from_dmesg(builder, now_epoch_seconds, started)
        }
        Err(error) => {
            builder.add_unavailable(
                HealthCollector::KernelSignal,
                error.unavailable_reason(),
                HealthSeverity::Unavailable,
                started.elapsed().as_millis(),
            );
        }
    }
}

pub(crate) fn parse_kernel_signals(
    content: &str,
    log_source: impl Into<String>,
    now_epoch_seconds: u64,
) -> KernelSignalFacts {
    parse_kernel_signals_with_boot_epoch(content, log_source, now_epoch_seconds, None)
}

pub(super) fn parse_kernel_signals_with_boot_epoch(
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
        coverage_since_epoch: content
            .lines()
            .filter_map(|line| parse_time_prefix(line, boot_epoch_seconds))
            .min(),
        sampled_at_epoch: now_epoch_seconds,
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

fn collect_kernel_from_dmesg(
    builder: &mut HealthReportBuilder,
    now_epoch_seconds: u64,
    started: Instant,
) {
    let boot_epoch_seconds =
        read_proc_uptime_seconds(builder).map(|uptime| now_epoch_seconds.saturating_sub(uptime));
    match run_system_command("dmesg", &[], HealthCommandConfig::default()) {
        Ok(output) if output.exit_code == Some(0) => {
            let mut lines = output.stdout.lines().rev().take(200).collect::<Vec<_>>();
            lines.reverse();
            parse_kernel_signals_with_boot_epoch(
                &lines.join("\n"),
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

fn read_proc_uptime_seconds(builder: &HealthReportBuilder) -> Option<u64> {
    let report = builder.report();
    if let Some(fact) = report
        .facts
        .iter()
        .find(|fact| fact.key == "host.uptime_seconds")
    {
        return match fact.value {
            HealthFactValue::Unsigned(seconds) => Some(seconds),
            _ => None,
        };
    }
    if report.check_results.iter().any(|check| {
        check.collector == Some(HealthCollector::Host)
            && check.status != super::super::model::HealthCheckStatus::Disabled
    }) {
        return None;
    }
    let content = fs::read_to_string("/proc/uptime").ok()?;
    super::parse_proc_uptime_seconds(&content)
}

#[cfg(test)]
mod sample_tests {
    use super::*;

    #[test]
    fn dmesg_reuses_the_already_sampled_host_uptime() {
        let mut builder = HealthReportBuilder::for_started_at(1);
        builder.start_check_id(
            "host.uptime",
            HealthCollector::Host,
            HealthFactScope::Host,
            HealthFactCoverage::Snapshot,
            1,
        );
        builder.add_fact(
            HealthFactCategory::Host,
            "host.uptime_seconds",
            HealthFactValue::Unsigned(123),
            Some("seconds".into()),
            HealthFactSource::ProcUptime,
            0,
        );
        builder.add_check_done("host.uptime").complete_check();
        assert_eq!(read_proc_uptime_seconds(&builder), Some(123));
    }
}
