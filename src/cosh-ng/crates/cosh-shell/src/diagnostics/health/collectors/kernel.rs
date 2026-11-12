use std::fs;
use std::time::Instant;

use super::super::builder::HealthReportBuilder;
use super::super::model::{
    HealthCollector, HealthFactCategory, HealthFactCoverage, HealthFactScope, HealthFactSource,
    HealthFactValue, HealthSeverity, HealthUnavailableReason,
};
use super::system::system_program;
use super::{
    run_health_command_filtered, unavailable_from_stderr, HealthCommandConfig, HealthCommandError,
    HealthLineFilter,
};
use oom::{
    count_oom_events, is_oom_line, merge_oom_signal, oom_scope_label_id_from_constraint, OomSignal,
};

mod oom;

const JOURNAL_SIGNAL_PATTERN: &str = "(?i)oom-kill|out of memory|killed process|oom_reaper|killed as a result of limit of|kernel panic|panic:";

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

pub(crate) fn collect_kernel_signals(builder: &mut HealthReportBuilder, now_epoch_seconds: u64) {
    let started = Instant::now();
    match system_program("journalctl") {
        Ok(program) => match read_journal_signals(HealthCommandConfig::default(), |args, config| {
            super::run_health_command_filtered(
                program,
                args,
                config,
                HealthLineFilter {
                    newest_first: true,
                    ..HealthLineFilter::default()
                },
                is_kernel_signal_line,
            )
        }) {
            Ok(output)
                if output.exit_code == Some(0)
                    || !output.stdout.trim().is_empty()
                    || (output.exit_code == Some(1) && output.stderr.trim().is_empty()) =>
            {
                let mut facts = parse_reverse_journal_signals(&output.stdout, now_epoch_seconds);
                // A partial read cannot prove which event was the most recent one.
                if output.stdout_truncated && facts.oom_latest_confidence.as_deref() == Some("high")
                {
                    facts.oom_latest_confidence = Some("low".to_string());
                }
                facts.record(builder, output.elapsed_ms);
                builder.add_check_done("kernel_signal");
            }
            Ok(output) => {
                builder.add_unavailable(
                    HealthCollector::KernelSignal,
                    kernel_unavailable_reason(output.exit_code, &output.stderr),
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

fn read_journal_signals(
    config: HealthCommandConfig,
    mut run: impl FnMut(
        &[&str],
        HealthCommandConfig,
    ) -> Result<super::HealthFilteredCommandOutput, HealthCommandError>,
) -> Result<super::HealthCommandOutput, HealthCommandError> {
    let started = Instant::now();
    let args = [
        "--quiet",
        "-k",
        "--since",
        "24 hours ago",
        "--no-pager",
        "--reverse",
        "-n",
        "200",
        "-o",
        "short-unix",
        "--grep",
        JOURNAL_SIGNAL_PATTERN,
    ];
    let output = run(&args, config)?;
    // Localized option errors still name --grep; permission failures do not.
    if !matches!(output.exit_code, Some(code) if code != 0)
        || !output.filtered.lines.is_empty()
        || !output.stderr.contains("--grep")
    {
        return Ok(journal_lines_output(output, 0));
    }
    let remaining = config.timeout.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return Err(HealthCommandError::Timeout);
    }
    let compatible = run(
        &args[..args.len() - 2],
        HealthCommandConfig {
            timeout: remaining,
            ..config
        },
    )?;
    Ok(journal_lines_output(compatible, output.elapsed_ms))
}

fn journal_lines_output(
    output: super::HealthFilteredCommandOutput,
    prior_elapsed_ms: u128,
) -> super::HealthCommandOutput {
    super::HealthCommandOutput {
        exit_code: output.exit_code,
        stdout: output.filtered.lines.join("\n"),
        stderr: output.stderr,
        stdout_truncated: !output.filtered.complete || output.filtered.truncated_lines > 0,
        stderr_truncated: false,
        elapsed_ms: output.elapsed_ms + prior_elapsed_ms,
    }
}

fn is_kernel_signal_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    is_oom_line(&lower) || lower.contains("kernel panic") || lower.contains("panic:")
}

fn kernel_unavailable_reason(exit_code: Option<i32>, stderr: &str) -> HealthUnavailableReason {
    if exit_code.is_none() {
        // No observed exit means a budget cut, not a failure to parse the log.
        return HealthUnavailableReason::Timeout;
    }
    unavailable_from_stderr(stderr)
}

pub(super) fn parse_reverse_journal_signals(
    content: &str,
    now_epoch_seconds: u64,
) -> KernelSignalFacts {
    // Group event fragments chronologically, including those within the same second.
    let chronological = content.lines().rev().collect::<Vec<_>>().join("\n");
    parse_kernel_signals(&chronological, "journalctl", now_epoch_seconds)
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
    let mut oom_events = Vec::new();
    let mut unclocked_oom_lines = 0;
    let mut panic_recent = false;
    for line in content.lines() {
        let lower = line.to_ascii_lowercase();
        if is_oom_line(&lower) {
            if let Some(epoch) = parse_time_prefix(line, boot_epoch_seconds) {
                merge_oom_signal(
                    &mut oom_events,
                    OomSignal::from_line(epoch, line),
                    lower.contains("invoked oom-killer"),
                );
            } else {
                unclocked_oom_lines += 1;
            }
        }
        if lower.contains("kernel panic") || lower.contains("panic:") {
            panic_recent = true;
        }
    }
    let latest_oom = oom_events.iter().max_by_key(|event| event.epoch);
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
        oom_event_count_last_1h: count_oom_events(&oom_events, now_epoch_seconds, 3600),
        oom_event_count_last_24h: count_oom_events(&oom_events, now_epoch_seconds, 86400),
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

fn collect_kernel_from_dmesg(
    builder: &mut HealthReportBuilder,
    now_epoch_seconds: u64,
    started: Instant,
) {
    let boot_epoch_seconds =
        read_proc_uptime_seconds(builder).map(|uptime| now_epoch_seconds.saturating_sub(uptime));
    let program = match system_program("dmesg") {
        Ok(program) => program,
        Err(err) => {
            builder.add_unavailable(
                HealthCollector::KernelSignal,
                err.unavailable_reason(),
                HealthSeverity::Unavailable,
                started.elapsed().as_millis(),
            );
            return;
        }
    };
    match run_health_command_filtered(
        program,
        &[],
        HealthCommandConfig::default(),
        HealthLineFilter::default(),
        is_kernel_signal_line,
    ) {
        Ok(output) => {
            if output.exit_code != Some(0) && output.filtered.lines.is_empty() {
                builder.add_unavailable(
                    HealthCollector::KernelSignal,
                    kernel_unavailable_reason(output.exit_code, &output.stderr),
                    HealthSeverity::Unavailable,
                    output.elapsed_ms,
                );
                return;
            }
            let mut facts = parse_kernel_signals_with_boot_epoch(
                &output.filtered.lines.join("\n"),
                "dmesg",
                now_epoch_seconds,
                boot_epoch_seconds,
            );
            // A partial read cannot prove which event was the most recent one.
            if (!output.filtered.complete || output.filtered.truncated_lines > 0)
                && facts.oom_latest_confidence.as_deref() == Some("high")
            {
                facts.oom_latest_confidence = Some("low".to_string());
            }
            facts.record(builder, output.elapsed_ms);
            builder.add_check_done("kernel_signal");
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
mod tests;
