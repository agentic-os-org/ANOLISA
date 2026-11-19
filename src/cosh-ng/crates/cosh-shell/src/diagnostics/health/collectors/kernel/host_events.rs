//! E06-E10 host kernel events from the same bounded log read: hung tasks and
//! soft lockups can prove recent W notices; MCE/EDAC, segfaults and netdev
//! timeouts stay diagnostics-only without hardware/process/netns proof.

use super::super::super::builder::HealthReportBuilder;
use super::super::super::model::{HealthFactCategory, HealthFactSource, HealthFactValue};
use super::attribution::{journal_message, journal_usec, journal_verified, AttributionContext};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HostEventKind {
    HungTask,
    SoftLockup,
    HardwareError,
    Segfault,
    NetdevTimeout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostEvent {
    pub kind: HostEventKind,
    pub epoch: u64,
    pub monotonic_ns: Option<u64>,
    pub process: Option<String>,
    pub pid: Option<u64>,
    pub seconds: Option<u64>,
    pub cpu: Option<u64>,
    pub interface: Option<String>,
    pub category: Option<&'static str>,
    pub verified: bool,
}

impl HostEvent {
    fn age_seconds(&self, ctx: &AttributionContext, now_epoch: u64) -> u64 {
        if let (Some(now), Some(event)) = (ctx.now_monotonic_ns, self.monotonic_ns) {
            return now.saturating_sub(event) / 1_000_000_000;
        }
        now_epoch.saturating_sub(self.epoch)
    }

    fn identity(&self) -> String {
        self.monotonic_ns.map_or_else(
            || format!("epoch:{}", self.epoch),
            |time| format!("uptime-ns:{time}"),
        )
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct HostEvents {
    pub events: Vec<HostEvent>,
    pub unverified: u64,
    pub source: &'static str,
}

/// Complete "INFO: task comm:pid blocked for more than N seconds" with N>0.
fn parse_hung_task(message: &str) -> Option<(String, u64, u64)> {
    let rest = message.split("INFO: task ").nth(1)?;
    let (identity, tail) = rest.split_once(" blocked for more than ")?;
    let (process, pid) = identity.rsplit_once(':')?;
    let pid = pid.trim().parse::<u64>().ok()?;
    let seconds = tail.split_whitespace().next()?.parse::<u64>().ok()?;
    (seconds > 0 && !process.is_empty()).then(|| (process.to_string(), pid, seconds))
}

/// Complete "watchdog: BUG: soft lockup - CPU#N stuck for Ms!" with duration.
fn parse_soft_lockup(message: &str) -> Option<(u64, u64)> {
    let lower = message.to_ascii_lowercase();
    if !lower.contains("soft lockup") {
        return None;
    }
    let cpu_part = lower.split("cpu#").nth(1)?;
    let cpu = cpu_part
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .parse::<u64>()
        .ok()?;
    let stuck = lower.split("stuck for ").nth(1)?;
    let seconds = stuck
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .parse::<u64>()
        .ok()?;
    Some((cpu, seconds))
}

fn hardware_category(message: &str) -> Option<&'static str> {
    let lower = message.to_ascii_lowercase();
    if lower.contains("fatal") || lower.contains("uncorrected") {
        if lower.contains("nonfatal") || lower.contains("non-fatal") {
            return Some("uncorrected-nonfatal");
        }
        if lower.contains("deferred") {
            return Some("deferred");
        }
        return Some("fatal-candidate");
    }
    if lower.contains("corrected") || lower.contains(" ce ") || lower.ends_with(" ce") {
        return Some("corrected");
    }
    None
}

fn parse_segfault(message: &str) -> Option<(String, u64)> {
    let rest = message.split("segfault at ").nth(1)?;
    let _address = rest.split_whitespace().next()?;
    let prefix = message.split("segfault at ").next()?.trim();
    let (process, pid_part) = prefix.rsplit_once('[')?;
    let pid = pid_part.split(']').next()?.trim().parse::<u64>().ok()?;
    let process = process.trim();
    (!process.is_empty()).then(|| (process.to_string(), pid))
}

fn parse_netdev_timeout(message: &str) -> Option<String> {
    let rest = message.split("NETDEV WATCHDOG: ").nth(1)?;
    let interface = rest.split_whitespace().next()?.trim_end_matches(':');
    (!interface.is_empty()).then(|| interface.to_string())
}

pub(crate) fn classify_host_line(message: &str) -> Option<HostEventKind> {
    if message.contains("INFO: task ") && message.contains("blocked for more than ") {
        return parse_hung_task(message).map(|_| HostEventKind::HungTask);
    }
    if message.contains("soft lockup") && message.contains("stuck for ") {
        return parse_soft_lockup(message).map(|_| HostEventKind::SoftLockup);
    }
    if message.contains("NETDEV WATCHDOG:") && message.contains("timed out") {
        return parse_netdev_timeout(message).map(|_| HostEventKind::NetdevTimeout);
    }
    if message.contains("segfault at ") {
        return parse_segfault(message).map(|_| HostEventKind::Segfault);
    }
    if message.contains("Hardware Error")
        || message.contains("EDAC")
        || message.contains("mce:")
        || message.contains("Machine check")
    {
        return Some(HostEventKind::HardwareError);
    }
    None
}

fn build_event(
    kind: HostEventKind,
    message: &str,
    epoch: u64,
    monotonic_ns: Option<u64>,
    verified: bool,
) -> HostEvent {
    let mut event = HostEvent {
        kind: kind.clone(),
        epoch,
        monotonic_ns,
        process: None,
        pid: None,
        seconds: None,
        cpu: None,
        interface: None,
        category: None,
        verified,
    };
    match kind {
        HostEventKind::HungTask => {
            if let Some((process, pid, seconds)) = parse_hung_task(message) {
                event.process = Some(process);
                event.pid = Some(pid);
                event.seconds = Some(seconds);
            }
        }
        HostEventKind::SoftLockup => {
            if let Some((cpu, seconds)) = parse_soft_lockup(message) {
                event.cpu = Some(cpu);
                event.seconds = Some(seconds);
            }
        }
        HostEventKind::HardwareError => event.category = hardware_category(message),
        HostEventKind::Segfault => {
            if let Some((process, pid)) = parse_segfault(message) {
                event.process = Some(process);
                event.pid = Some(pid);
            }
        }
        HostEventKind::NetdevTimeout => event.interface = parse_netdev_timeout(message),
    }
    event
}

pub(crate) fn parse_journal_host_events(content: &str, ctx: &AttributionContext) -> HostEvents {
    let mut events = HostEvents {
        source: "journalctl",
        ..HostEvents::default()
    };
    for raw in content.lines() {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(raw) else {
            continue;
        };
        let Some(message) = journal_message(&entry) else {
            continue;
        };
        let Some(kind) = classify_host_line(&message) else {
            continue;
        };
        let verified = journal_verified(&entry, ctx);
        let epoch = journal_usec(&entry, "__REALTIME_TIMESTAMP").map(|usec| usec / 1_000_000);
        let monotonic_ns = journal_usec(&entry, "__MONOTONIC_TIMESTAMP")
            .filter(|usec| *usec > 0)
            .map(|usec| usec * 1000);
        if !verified || epoch.is_none() {
            events.unverified += 1;
            continue;
        }
        events.events.push(build_event(
            kind,
            &message,
            epoch.unwrap_or(0),
            monotonic_ns,
            true,
        ));
    }
    events
}

pub(crate) fn parse_text_host_events(content: &str, source: &'static str) -> HostEvents {
    let mut events = HostEvents {
        source,
        ..HostEvents::default()
    };
    for raw in content.lines() {
        let Some(kind) = classify_host_line(raw) else {
            continue;
        };
        events.events.push(build_event(kind, raw, 0, None, false));
    }
    events
}

pub(crate) fn record_host_events(
    builder: &mut HealthReportBuilder,
    events: &HostEvents,
    ctx: &AttributionContext,
    now_epoch: u64,
    elapsed_ms: u128,
) {
    fn add(builder: &mut HealthReportBuilder, key: &str, value: HealthFactValue, elapsed_ms: u128) {
        builder.add_fact(
            HealthFactCategory::Kernel,
            key,
            value,
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        );
    }
    let verified: Vec<&HostEvent> = events.events.iter().filter(|e| e.verified).collect();
    if verified.is_empty() && events.unverified == 0 {
        return;
    }
    add(
        builder,
        "kernel.host_event_source",
        HealthFactValue::String(events.source.to_string()),
        elapsed_ms,
    );
    if events.unverified > 0 {
        add(
            builder,
            "kernel.host_event_unverified",
            HealthFactValue::Unsigned(events.unverified),
            elapsed_ms,
        );
    }
    let hung: Vec<&&HostEvent> = verified
        .iter()
        .filter(|e| e.kind == HostEventKind::HungTask)
        .collect();
    if let Some(latest) = hung.iter().min_by_key(|e| e.age_seconds(ctx, now_epoch)) {
        add(
            builder,
            "kernel.hung_task_latest_age_seconds",
            HealthFactValue::Unsigned(latest.age_seconds(ctx, now_epoch)),
            elapsed_ms,
        );
        add(
            builder,
            "kernel.hung_task_event_identity",
            HealthFactValue::String(latest.identity()),
            elapsed_ms,
        );
        add(
            builder,
            "kernel.hung_task_event_count_1h",
            HealthFactValue::Unsigned(hung.len() as u64),
            elapsed_ms,
        );
        if let Some(process) = &latest.process {
            add(
                builder,
                "kernel.hung_task_process",
                HealthFactValue::String(process.clone()),
                elapsed_ms,
            );
        }
        if let Some(pid) = latest.pid {
            add(
                builder,
                "kernel.hung_task_pid",
                HealthFactValue::Unsigned(pid),
                elapsed_ms,
            );
        }
        if let Some(seconds) = latest.seconds {
            add(
                builder,
                "kernel.hung_task_blocked_seconds",
                HealthFactValue::Unsigned(seconds),
                elapsed_ms,
            );
        }
    }
    let lockup: Vec<&&HostEvent> = verified
        .iter()
        .filter(|e| e.kind == HostEventKind::SoftLockup)
        .collect();
    if let Some(latest) = lockup.iter().min_by_key(|e| e.age_seconds(ctx, now_epoch)) {
        add(
            builder,
            "kernel.soft_lockup_latest_age_seconds",
            HealthFactValue::Unsigned(latest.age_seconds(ctx, now_epoch)),
            elapsed_ms,
        );
        add(
            builder,
            "kernel.soft_lockup_event_identity",
            HealthFactValue::String(latest.identity()),
            elapsed_ms,
        );
        add(
            builder,
            "kernel.soft_lockup_event_count_1h",
            HealthFactValue::Unsigned(lockup.len() as u64),
            elapsed_ms,
        );
        if let Some(cpu) = latest.cpu {
            add(
                builder,
                "kernel.soft_lockup_cpu",
                HealthFactValue::Unsigned(cpu),
                elapsed_ms,
            );
        }
        if let Some(seconds) = latest.seconds {
            add(
                builder,
                "kernel.soft_lockup_stuck_seconds",
                HealthFactValue::Unsigned(seconds),
                elapsed_ms,
            );
        }
    }
    let hardware: Vec<&&HostEvent> = verified
        .iter()
        .filter(|e| e.kind == HostEventKind::HardwareError)
        .collect();
    if !hardware.is_empty() {
        add(
            builder,
            "kernel.hardware_error_count_1h",
            HealthFactValue::Unsigned(hardware.len() as u64),
            elapsed_ms,
        );
        let category = hardware
            .iter()
            .find_map(|e| e.category)
            .unwrap_or("unknown");
        add(
            builder,
            "kernel.hardware_error_category",
            HealthFactValue::String(category.to_string()),
            elapsed_ms,
        );
    }
    let segfault: Vec<&&HostEvent> = verified
        .iter()
        .filter(|e| e.kind == HostEventKind::Segfault)
        .collect();
    if let Some(latest) = segfault
        .iter()
        .min_by_key(|e| e.age_seconds(ctx, now_epoch))
    {
        add(
            builder,
            "kernel.segfault_latest_age_seconds",
            HealthFactValue::Unsigned(latest.age_seconds(ctx, now_epoch)),
            elapsed_ms,
        );
        add(
            builder,
            "kernel.segfault_count_1h",
            HealthFactValue::Unsigned(segfault.len() as u64),
            elapsed_ms,
        );
        if let Some(process) = &latest.process {
            add(
                builder,
                "kernel.segfault_process",
                HealthFactValue::String(process.clone()),
                elapsed_ms,
            );
        }
        if let Some(pid) = latest.pid {
            add(
                builder,
                "kernel.segfault_pid",
                HealthFactValue::Unsigned(pid),
                elapsed_ms,
            );
        }
    }
    let netdev: Vec<&&HostEvent> = verified
        .iter()
        .filter(|e| e.kind == HostEventKind::NetdevTimeout)
        .collect();
    if let Some(latest) = netdev.iter().min_by_key(|e| e.age_seconds(ctx, now_epoch)) {
        add(
            builder,
            "kernel.netdev_timeout_latest_age_seconds",
            HealthFactValue::Unsigned(latest.age_seconds(ctx, now_epoch)),
            elapsed_ms,
        );
        add(
            builder,
            "kernel.netdev_timeout_count_1h",
            HealthFactValue::Unsigned(netdev.len() as u64),
            elapsed_ms,
        );
        if let Some(interface) = &latest.interface {
            add(
                builder,
                "kernel.netdev_timeout_interface",
                HealthFactValue::String(interface.clone()),
                elapsed_ms,
            );
        }
    }
}

#[cfg(test)]
mod tests;
