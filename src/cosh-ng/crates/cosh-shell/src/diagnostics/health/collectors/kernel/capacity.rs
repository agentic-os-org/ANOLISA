//! E13/E11 capacity events from the same bounded kernel log read. First batch
//! stays diagnostics-only: VFS lines lack non-probe causal binding and
//! conntrack lines lack netns proof, so neither creates a notice episode.

use super::super::super::builder::HealthReportBuilder;
use super::super::super::model::{HealthFactCategory, HealthFactSource, HealthFactValue};
use super::attribution::{journal_message, journal_usec, journal_verified, AttributionContext};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapacityEvent {
    pub epoch: u64,
    pub age_seconds: u64,
    pub identity: String,
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CapacityEvents {
    pub vfs_filemax: Vec<CapacityEvent>,
    pub conntrack_full: Vec<CapacityEvent>,
    pub unverified: u64,
    pub source: &'static str,
}

pub(crate) fn is_vfs_filemax_line(lower: &str) -> bool {
    lower.contains("vfs: file-max limit") && lower.contains("reached")
}

pub(crate) fn is_conntrack_full_line(lower: &str) -> bool {
    lower.contains("nf_conntrack: table full, dropping packet")
}

pub(crate) fn parse_filemax_limit(message: &str) -> Option<u64> {
    let lower = message.to_ascii_lowercase();
    let rest = lower.split("vfs: file-max limit ").nth(1)?;
    rest.split_whitespace().next()?.parse().ok()
}

fn event_age(
    monotonic_ns: Option<u64>,
    epoch: u64,
    ctx: &AttributionContext,
    now_epoch: u64,
) -> u64 {
    if let (Some(now), Some(event)) = (ctx.now_monotonic_ns, monotonic_ns) {
        return now.saturating_sub(event) / 1_000_000_000;
    }
    now_epoch.saturating_sub(epoch)
}

fn event_identity(monotonic_ns: Option<u64>, epoch: u64) -> String {
    monotonic_ns.map_or_else(
        || format!("epoch:{epoch}"),
        |time| format!("uptime-ns:{time}"),
    )
}

pub(crate) fn parse_journal_capacity_events(
    content: &str,
    ctx: &AttributionContext,
    now_epoch: u64,
) -> CapacityEvents {
    let mut events = CapacityEvents {
        source: "journalctl",
        ..CapacityEvents::default()
    };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(message) = journal_message(&entry) else {
            continue;
        };
        let lower = message.to_ascii_lowercase();
        let vfs = is_vfs_filemax_line(&lower);
        let conntrack = is_conntrack_full_line(&lower);
        if !vfs && !conntrack {
            continue;
        }
        if !journal_verified(&entry, ctx) {
            events.unverified += 1;
            continue;
        }
        let Some(epoch) = journal_usec(&entry, "__REALTIME_TIMESTAMP").map(|usec| usec / 1_000_000)
        else {
            events.unverified += 1;
            continue;
        };
        let monotonic_ns = journal_usec(&entry, "__MONOTONIC_TIMESTAMP")
            .filter(|usec| *usec > 0)
            .map(|usec| usec * 1000);
        let event = CapacityEvent {
            epoch,
            age_seconds: event_age(monotonic_ns, epoch, ctx, now_epoch),
            identity: event_identity(monotonic_ns, epoch),
            limit: vfs.then(|| parse_filemax_limit(&message)).flatten(),
        };
        if vfs {
            events.vfs_filemax.push(event);
        } else {
            events.conntrack_full.push(event);
        }
    }
    events
}

/// Text/dmesg fallback keeps counts diagnostics-only without time eligibility.
pub(crate) fn parse_text_capacity_events(content: &str, source: &'static str) -> CapacityEvents {
    let mut events = CapacityEvents {
        source,
        ..CapacityEvents::default()
    };
    for line in content.lines() {
        let lower = line.to_ascii_lowercase();
        if is_vfs_filemax_line(&lower) {
            events.vfs_filemax.push(CapacityEvent {
                epoch: 0,
                age_seconds: 0,
                identity: String::new(),
                limit: parse_filemax_limit(line),
            });
        } else if is_conntrack_full_line(&lower) {
            events.conntrack_full.push(CapacityEvent {
                epoch: 0,
                age_seconds: 0,
                identity: String::new(),
                limit: None,
            });
        }
    }
    events
}

pub(crate) fn record_capacity_events(
    builder: &mut HealthReportBuilder,
    events: &CapacityEvents,
    elapsed_ms: u128,
) {
    let verified = events.source == "journalctl";
    for (prefix, list) in [
        ("kernel.vfs_filemax", &events.vfs_filemax),
        ("kernel.conntrack_full", &events.conntrack_full),
    ] {
        if list.is_empty() {
            continue;
        }
        let mut add = |key: String, value: HealthFactValue| {
            builder.add_fact(
                HealthFactCategory::Kernel,
                key,
                value,
                None,
                HealthFactSource::Derived,
                elapsed_ms,
            );
        };
        add(
            format!("{prefix}_event_count_24h"),
            HealthFactValue::Unsigned(list.len() as u64),
        );
        add(
            format!("{prefix}_source"),
            HealthFactValue::String(events.source.to_string()),
        );
        if verified {
            if let Some(latest) = list.iter().min_by_key(|event| event.age_seconds) {
                add(
                    format!("{prefix}_latest_age_seconds"),
                    HealthFactValue::Unsigned(latest.age_seconds),
                );
                add(
                    format!("{prefix}_latest_epoch"),
                    HealthFactValue::Unsigned(latest.epoch),
                );
                add(
                    format!("{prefix}_event_identity"),
                    HealthFactValue::String(latest.identity.clone()),
                );
                if let Some(limit) = latest.limit {
                    add(format!("{prefix}_limit"), HealthFactValue::Unsigned(limit));
                }
            }
        }
    }
    if events.unverified > 0 {
        builder.add_fact(
            HealthFactCategory::Kernel,
            "kernel.capacity_unverified_count",
            HealthFactValue::Unsigned(events.unverified),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        );
    }
}

#[cfg(test)]
mod tests;
