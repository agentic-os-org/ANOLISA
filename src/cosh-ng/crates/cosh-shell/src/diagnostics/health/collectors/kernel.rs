use super::super::builder::HealthReportBuilder;
use super::super::model::{
    HealthCollector, HealthFactCategory, HealthFactCoverage, HealthFactScope, HealthFactSource,
    HealthFactValue, HealthSeverity, HealthUnavailableReason,
};
use oom::{
    count_oom_events, is_oom_line, merge_oom_signal, oom_scope_label_id_from_constraint, OomSignal,
};

pub(crate) mod attribution;
pub(crate) mod capacity;
pub(crate) mod cgroup;
mod collect;
pub(crate) mod fs_events;
mod oom;

#[cfg(test)]
use super::{
    HealthCommandConfig, HealthCommandError, HealthCommandOutput, HealthFilteredCommandOutput,
    HealthLineFilter,
};
pub(crate) use collect::collect_kernel_signals;
#[cfg(test)]
use collect::{
    is_kernel_signal_line, kernel_unavailable_reason, read_journal_signals,
    read_proc_uptime_seconds,
};
#[cfg(test)]
use std::fs;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct KernelSignalFacts {
    pub(crate) oom_latest_event_epoch: Option<u64>,
    pub(crate) oom_latest_event_identity: Option<String>,
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
    pub(crate) oom_attributed_age_seconds: Option<u64>,
    pub(crate) oom_attributed_epoch: Option<u64>,
    pub(crate) oom_attributed_event_identity: Option<String>,
    pub(crate) oom_attributed_pid: Option<u64>,
    pub(crate) oom_attributed_process: Option<String>,
    pub(crate) oom_attributed_scope: Option<String>,
    pub(crate) oom_attributed_task_cgroup: Option<String>,
    pub(crate) oom_attributed_oom_cgroup: Option<String>,
    pub(crate) oom_attributed_confidence: Option<String>,
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
        if let Some(identity) = &self.oom_latest_event_identity {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_latest_event_identity",
                HealthFactValue::String(identity.clone()),
                None,
                kernel_source(&self.log_source),
                elapsed_ms,
            );
        }
        if let Some(epoch) = self.oom_latest_event_epoch {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_latest_event_epoch",
                HealthFactValue::Unsigned(epoch),
                Some("seconds".to_string()),
                kernel_source(&self.log_source),
                elapsed_ms,
            );
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
        if let Some(epoch) = self.oom_attributed_epoch {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_attributed_epoch",
                HealthFactValue::Unsigned(epoch),
                Some("seconds".to_string()),
                kernel_source(&self.log_source),
                elapsed_ms,
            );
        }
        if let Some(age) = self.oom_attributed_age_seconds {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_attributed_age_seconds",
                HealthFactValue::Unsigned(age),
                Some("seconds".to_string()),
                kernel_source(&self.log_source),
                elapsed_ms,
            );
        }
        if let Some(identity) = &self.oom_attributed_event_identity {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_attributed_event_identity",
                HealthFactValue::String(identity.clone()),
                None,
                kernel_source(&self.log_source),
                elapsed_ms,
            );
        }
        if let Some(pid) = self.oom_attributed_pid {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_attributed_pid",
                HealthFactValue::Unsigned(pid),
                None,
                kernel_source(&self.log_source),
                elapsed_ms,
            );
        }
        if let Some(process) = &self.oom_attributed_process {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_attributed_process",
                HealthFactValue::String(process.clone()),
                None,
                kernel_source(&self.log_source),
                elapsed_ms,
            );
        }
        if let Some(scope) = &self.oom_attributed_scope {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_attributed_scope",
                HealthFactValue::String(scope.clone()),
                None,
                kernel_source(&self.log_source),
                elapsed_ms,
            );
        }
        if let Some(cgroup) = &self.oom_attributed_task_cgroup {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_attributed_task_cgroup",
                HealthFactValue::String(cgroup.clone()),
                None,
                kernel_source(&self.log_source),
                elapsed_ms,
            );
        }
        if let Some(cgroup) = &self.oom_attributed_oom_cgroup {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_attributed_oom_cgroup",
                HealthFactValue::String(cgroup.clone()),
                None,
                kernel_source(&self.log_source),
                elapsed_ms,
            );
        }
        if let Some(confidence) = &self.oom_attributed_confidence {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.oom_attributed_confidence",
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

#[cfg(test)]
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
    parse_kernel_signals_internal(
        content,
        log_source,
        now_epoch_seconds,
        boot_epoch_seconds,
        None,
    )
}

pub(crate) fn parse_kernel_signals_attributed(
    content: &str,
    log_source: impl Into<String>,
    now_epoch_seconds: u64,
    boot_epoch_seconds: Option<u64>,
    ctx: &attribution::AttributionContext,
) -> KernelSignalFacts {
    parse_kernel_signals_internal(
        content,
        log_source,
        now_epoch_seconds,
        boot_epoch_seconds,
        Some(ctx),
    )
}

fn parse_kernel_signals_internal(
    content: &str,
    log_source: impl Into<String>,
    now_epoch_seconds: u64,
    boot_epoch_seconds: Option<u64>,
    ctx: Option<&attribution::AttributionContext>,
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
    let coverage_since_epoch = content
        .lines()
        .filter_map(|line| parse_time_prefix(line, boot_epoch_seconds))
        .min();
    finish_signals(
        &oom_events,
        unclocked_oom_lines,
        panic_recent,
        log_source.into(),
        now_epoch_seconds,
        coverage_since_epoch,
        ctx,
    )
}

fn finish_signals(
    oom_events: &[OomSignal],
    unclocked_oom_lines: u64,
    panic_recent: bool,
    log_source: String,
    now_epoch_seconds: u64,
    coverage_since_epoch: Option<u64>,
    ctx: Option<&attribution::AttributionContext>,
) -> KernelSignalFacts {
    let latest_oom = oom_events.iter().max_by_key(|event| event.event_epoch());
    let oom_latest_age_seconds = latest_oom
        .as_ref()
        .map(|signal| now_epoch_seconds.saturating_sub(signal.event_epoch()));
    let confidence = if latest_oom.is_some() {
        Some("high".to_string())
    } else if unclocked_oom_lines > 0 {
        Some("low".to_string())
    } else {
        None
    };
    let attributed = ctx.and_then(|ctx| {
        attribution::select_attributed(oom_events, ctx, now_epoch_seconds).map(|event| {
            let confidence = if ctx.source_verified { "high" } else { "low" };
            (event, confidence.to_string())
        })
    });
    KernelSignalFacts {
        oom_latest_event_epoch: latest_oom.as_ref().map(|signal| signal.event_epoch()),
        oom_latest_event_identity: latest_oom
            .as_ref()
            .filter(|signal| signal.has_event_anchor)
            .map(|signal| {
                signal.monotonic_ns.map_or_else(
                    || format!("epoch:{}", signal.identity_epoch),
                    |time| format!("uptime-ns:{time}"),
                )
            }),
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
        oom_event_count_last_1h: count_oom_events(oom_events, now_epoch_seconds, 3600),
        oom_event_count_last_24h: count_oom_events(oom_events, now_epoch_seconds, 86400),
        oom_latest_source_line_count: latest_oom
            .as_ref()
            .map(|signal| signal.source_line_count)
            .unwrap_or(unclocked_oom_lines),
        oom_latest_confidence: confidence,
        oom_attributed_age_seconds: attributed.as_ref().map(|(event, _)| event.age_seconds),
        oom_attributed_epoch: attributed.as_ref().map(|(event, _)| event.epoch),
        oom_attributed_event_identity: attributed
            .as_ref()
            .map(|(event, _)| event.event_identity.clone()),
        oom_attributed_pid: attributed.as_ref().map(|(event, _)| event.pid),
        oom_attributed_process: attributed
            .as_ref()
            .and_then(|(event, _)| event.process.clone()),
        oom_attributed_scope: attributed
            .as_ref()
            .map(|(event, _)| event.scope.to_string()),
        oom_attributed_task_cgroup: attributed
            .as_ref()
            .and_then(|(event, _)| event.task_cgroup.clone()),
        oom_attributed_oom_cgroup: attributed
            .as_ref()
            .and_then(|(event, _)| event.oom_cgroup.clone()),
        oom_attributed_confidence: attributed.map(|(_, confidence)| confidence),
        panic_recent,
        log_source,
        coverage_since_epoch,
        sampled_at_epoch: now_epoch_seconds,
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

#[cfg(test)]
library_tests! {
    mod identity_tests;
}
#[cfg(test)]
mod tests;
