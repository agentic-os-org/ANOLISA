//! E02-E05 filesystem event chains from the same bounded kernel log read:
//! ext4 error/forced-ro, JBD2 abort/flush, XFS shutdown cause chains and
//! block request completion failures. Missing causal or exclusion evidence
//! degrades to diagnostics-only; admin operations never alert.

use std::fs;
use std::io::Read;
use std::path::Path;

use super::super::super::builder::HealthReportBuilder;
use super::super::super::model::{HealthFactCategory, HealthFactSource, HealthFactValue};
use super::attribution::{journal_message, journal_usec, journal_verified, AttributionContext};

pub(crate) const FS_EVENT_QUOTA: usize = 64;
pub(crate) const FS_CONTEXT_QUOTA: usize = 8;
pub(crate) const CHAIN_SPAN_SECONDS: u64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FsEventKind {
    Ext4Error,
    Ext4ForcedRo,
    JournalAbort,
    JournalFlushError,
    JournalCauseIo,
    JournalCauseCorruption,
    JournalAdminShutdown,
    XfsCause,
    XfsShutdown,
    XfsUserShutdown,
    XfsUnmount,
    BlockIoError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FsLine {
    pub kind: FsEventKind,
    pub device: String,
    pub epoch: u64,
    pub monotonic_ns: Option<u64>,
    pub operation: Option<&'static str>,
    pub verified: bool,
}

impl FsLine {
    pub(crate) fn age_seconds(&self, ctx: &AttributionContext, now_epoch: u64) -> u64 {
        if let (Some(now), Some(event)) = (ctx.now_monotonic_ns, self.monotonic_ns) {
            return now.saturating_sub(event) / 1_000_000_000;
        }
        now_epoch.saturating_sub(self.epoch)
    }

    pub(crate) fn identity(&self) -> String {
        self.monotonic_ns.map_or_else(
            || format!("epoch:{}", self.epoch),
            |time| format!("uptime-ns:{time}"),
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FsLines {
    pub events: Vec<FsLine>,
    pub context: Vec<FsLine>,
    pub dropped_events: u64,
    pub dropped_context: u64,
    pub unverified: u64,
    pub source: &'static str,
}

fn ext4_device(message: &str) -> Option<String> {
    let rest = message.split("ext4-fs error (device ").nth(1)?;
    let device = rest.split(')').next()?.trim();
    (!device.is_empty()).then(|| device.to_string())
}

fn ext4_remount_device(message: &str) -> Option<String> {
    let rest = message.split("ext4-fs (").nth(1)?;
    let device = rest.split(')').next()?.trim();
    (!device.is_empty()).then(|| device.to_string())
}

fn jbd2_device(message: &str) -> Option<String> {
    let rest = message
        .split(" on device ")
        .nth(1)
        .or_else(|| message.split(" flushing file data on ").nth(1))?;
    let device = rest.split_whitespace().next()?.trim_end_matches(',');
    (!device.is_empty()).then(|| device.to_string())
}

fn xfs_device(message: &str) -> Option<String> {
    let rest = message.split("xfs (").nth(1)?;
    let device = rest.split(')').next()?.trim();
    (!device.is_empty()).then(|| device.to_string())
}

fn block_device_operation(message: &str) -> Option<(String, Option<&'static str>)> {
    let dev = message.split(", dev ").nth(1)?.split(',').next()?.trim();
    if dev.is_empty() {
        return None;
    }
    let operation = if message.contains("op 0x0:(read)") || message.contains("(read)") {
        Some("READ")
    } else if message.contains("op 0x1:(write)") || message.contains("(write)") {
        Some("WRITE")
    } else {
        None
    };
    Some((dev.to_string(), operation))
}

/// Classify one kernel message; None for unrelated lines. Journal device
/// names keep their raw suffix so external journals never guess the main disk.
pub(crate) fn classify_fs_line(
    message: &str,
) -> Option<(FsEventKind, String, Option<&'static str>)> {
    let message = message.to_ascii_lowercase();
    let message = message.as_str();
    if message.contains("ext4-fs error (device ") {
        return ext4_device(message).map(|device| (FsEventKind::Ext4Error, device, None));
    }
    if message.contains("remounting filesystem read-only") {
        return ext4_remount_device(message)
            .map(|device| (FsEventKind::Ext4ForcedRo, device, None));
    }
    if message.contains("aborting journal on device ") {
        return jbd2_device(message).map(|device| (FsEventKind::JournalAbort, device, None));
    }
    if message.contains("detected io errors while flushing") {
        return jbd2_device(message).map(|device| (FsEventKind::JournalFlushError, device, None));
    }
    if message.contains("jbd2:") && message.contains("io error") {
        return jbd2_device(message).map(|device| (FsEventKind::JournalCauseIo, device, None));
    }
    if message.contains("jbd2:")
        && (message.contains("checksum error") || message.contains("corrupt"))
    {
        return jbd2_device(message)
            .map(|device| (FsEventKind::JournalCauseCorruption, device, None));
    }
    if message.contains("jbd2:") && message.contains("shutdown") {
        return jbd2_device(message)
            .map(|device| (FsEventKind::JournalAdminShutdown, device, None));
    }
    if message.contains("user initiated shutdown received") {
        return xfs_device(message).map(|device| (FsEventKind::XfsUserShutdown, device, None));
    }
    if message.contains("unmounting filesystem") {
        return xfs_device(message).map(|device| (FsEventKind::XfsUnmount, device, None));
    }
    if message.contains("xfs (")
        && (message.contains("corruption detected")
            || message.contains("log i/o error")
            || message.contains("metadata i/o error"))
    {
        return xfs_device(message).map(|device| (FsEventKind::XfsCause, device, None));
    }
    if message.contains("xfs (")
        && (message.contains("filesystem has been shut down")
            || message.contains("shutting down filesystem"))
    {
        return xfs_device(message).map(|device| (FsEventKind::XfsShutdown, device, None));
    }
    if message.contains("blk_update_request:") && message.contains("i/o error") {
        let (device, operation) = block_device_operation(message)?;
        return Some((FsEventKind::BlockIoError, device, operation));
    }
    None
}

fn is_context(kind: FsEventKind) -> bool {
    matches!(
        kind,
        FsEventKind::JournalAdminShutdown
            | FsEventKind::XfsUserShutdown
            | FsEventKind::XfsUnmount
            | FsEventKind::JournalCauseIo
            | FsEventKind::JournalCauseCorruption
            | FsEventKind::XfsCause
    )
}

fn push_line(lines: &mut FsLines, line: FsLine) {
    if is_context(line.kind) {
        if lines.context.len() < FS_CONTEXT_QUOTA {
            lines.context.push(line);
        } else {
            lines.dropped_context += 1;
        }
    } else if lines.events.len() < FS_EVENT_QUOTA {
        lines.events.push(line);
    } else {
        lines.dropped_events += 1;
    }
}

pub(crate) fn parse_journal_fs_lines(content: &str, ctx: &AttributionContext) -> FsLines {
    let mut lines = FsLines {
        source: "journalctl",
        ..FsLines::default()
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
        let Some((kind, device, operation)) = classify_fs_line(&message) else {
            continue;
        };
        let verified = journal_verified(&entry, ctx);
        let epoch = journal_usec(&entry, "__REALTIME_TIMESTAMP").map(|usec| usec / 1_000_000);
        let monotonic_ns = journal_usec(&entry, "__MONOTONIC_TIMESTAMP")
            .filter(|usec| *usec > 0)
            .map(|usec| usec * 1000);
        if !verified || epoch.is_none() {
            lines.unverified += 1;
            continue;
        }
        push_line(
            &mut lines,
            FsLine {
                kind,
                device,
                epoch: epoch.unwrap_or(0),
                monotonic_ns,
                operation,
                verified: true,
            },
        );
    }
    lines
}

/// Text/dmesg fallback keeps diagnostics-only lines without time eligibility.
pub(crate) fn parse_text_fs_lines(content: &str, source: &'static str) -> FsLines {
    let mut lines = FsLines {
        source,
        ..FsLines::default()
    };
    for raw in content.lines() {
        let Some((kind, device, operation)) = classify_fs_line(raw) else {
            continue;
        };
        push_line(
            &mut lines,
            FsLine {
                kind,
                device,
                epoch: 0,
                monotonic_ns: None,
                operation,
                verified: false,
            },
        );
    }
    lines
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FsChain {
    pub mechanism: &'static str,
    pub device: String,
    pub fs_device: String,
    pub age_seconds: u64,
    pub epoch: u64,
    pub identity: String,
    pub operation: Option<&'static str>,
    pub eligible: bool,
    pub degraded_reason: Option<&'static str>,
}

/// Internal journal devices carry the "-8" journal-inode suffix; stripping it
/// proves the owning filesystem device. Anything else is an external journal
/// whose owning FS is unproven without a declared mapping.
pub(crate) fn journal_base_device(device: &str) -> Option<String> {
    device.strip_suffix("-8").map(|base| base.to_string())
}

fn span_within(a: &FsLine, b: &FsLine) -> bool {
    if let (Some(left), Some(right)) = (a.monotonic_ns, b.monotonic_ns) {
        return left.abs_diff(right) <= CHAIN_SPAN_SECONDS * 1_000_000_000;
    }
    a.epoch.abs_diff(b.epoch) <= CHAIN_SPAN_SECONDS
}

fn same_device(a: &FsLine, b: &FsLine) -> bool {
    a.device == b.device
}

fn chain_age(lines: &[&FsLine], ctx: &AttributionContext, now_epoch: u64) -> u64 {
    lines
        .iter()
        .map(|line| line.age_seconds(ctx, now_epoch))
        .max()
        .unwrap_or(0)
}

fn make_chain(
    mechanism: &'static str,
    anchor: &FsLine,
    members: &[&FsLine],
    fs_device: String,
    ctx: &AttributionContext,
    now_epoch: u64,
    eligible: bool,
    degraded_reason: Option<&'static str>,
) -> FsChain {
    let (eligible, degraded_reason) = if anchor.verified {
        (eligible, degraded_reason)
    } else {
        (false, Some("source-unverified"))
    };
    FsChain {
        mechanism,
        device: anchor.device.clone(),
        fs_device,
        age_seconds: chain_age(members, ctx, now_epoch),
        epoch: anchor.epoch,
        identity: format!("{mechanism}|{}|{}", anchor.device, anchor.identity()),
        operation: anchor.operation,
        eligible,
        degraded_reason,
    }
}

/// Build proven chains; every missing causal anchor, admin marker or truncated
/// context degrades the candidate to diagnostics-only instead of guessing.
pub(crate) fn build_chains(
    lines: &FsLines,
    ctx: &AttributionContext,
    now_epoch: u64,
) -> Vec<FsChain> {
    let mut chains = Vec::new();
    let context_complete = lines.dropped_context == 0;
    let events = &lines.events;
    let context = &lines.context;
    for anchor in events
        .iter()
        .filter(|line| line.kind == FsEventKind::Ext4ForcedRo)
    {
        let error = events
            .iter()
            .filter(|line| line.kind == FsEventKind::Ext4Error && same_device(line, anchor))
            .find(|line| span_within(line, anchor));
        match error {
            Some(error) => chains.push(make_chain(
                "ext4-forced-ro",
                anchor,
                &[anchor, error],
                anchor.device.clone(),
                ctx,
                now_epoch,
                true,
                None,
            )),
            None => chains.push(make_chain(
                "ext4-forced-ro",
                anchor,
                &[anchor],
                anchor.device.clone(),
                ctx,
                now_epoch,
                false,
                Some("no-error-anchor"),
            )),
        }
    }
    for anchor in events
        .iter()
        .filter(|line| line.kind == FsEventKind::Ext4Error)
    {
        let absorbed = events.iter().any(|line| {
            line.kind == FsEventKind::Ext4ForcedRo
                && same_device(line, anchor)
                && span_within(line, anchor)
        });
        if absorbed {
            continue;
        }
        chains.push(make_chain(
            "ext4-error",
            anchor,
            &[anchor],
            anchor.device.clone(),
            ctx,
            now_epoch,
            true,
            None,
        ));
    }
    for anchor in events
        .iter()
        .filter(|line| line.kind == FsEventKind::JournalAbort)
    {
        let base = journal_base_device(&anchor.device);
        let cause = context
            .iter()
            .filter(|line| {
                matches!(
                    line.kind,
                    FsEventKind::JournalCauseIo | FsEventKind::JournalCauseCorruption
                ) && same_device(line, anchor)
            })
            .find(|line| span_within(line, anchor));
        let admin = context
            .iter()
            .filter(|line| {
                line.kind == FsEventKind::JournalAdminShutdown && same_device(line, anchor)
            })
            .find(|line| span_within(line, anchor));
        let (eligible, reason) = match (&base, cause, admin) {
            (None, _, _) => (false, Some("journal-mapping-unproven")),
            (_, _, Some(_)) => (false, Some("admin-excluded")),
            (_, None, _) if !context_complete => (false, Some("context-incomplete")),
            (_, None, _) => (false, Some("missing-cause")),
            (_, Some(_), _) if !context_complete => (false, Some("context-incomplete")),
            (_, Some(_), _) => (true, None),
        };
        let mut members = vec![anchor];
        if let Some(cause) = cause {
            members.push(cause);
        }
        chains.push(make_chain(
            "jbd2-abort",
            anchor,
            &members,
            base.unwrap_or_else(|| anchor.device.clone()),
            ctx,
            now_epoch,
            eligible,
            reason,
        ));
    }
    for anchor in events
        .iter()
        .filter(|line| line.kind == FsEventKind::JournalFlushError)
    {
        let base = journal_base_device(&anchor.device);
        let (eligible, reason) = match &base {
            None => (false, Some("journal-mapping-unproven")),
            Some(_) => (true, None),
        };
        chains.push(make_chain(
            "jbd2-flush",
            anchor,
            &[anchor],
            base.unwrap_or_else(|| anchor.device.clone()),
            ctx,
            now_epoch,
            eligible,
            reason,
        ));
    }
    for anchor in events
        .iter()
        .filter(|line| line.kind == FsEventKind::XfsShutdown)
    {
        let cause = context
            .iter()
            .filter(|line| line.kind == FsEventKind::XfsCause && same_device(line, anchor))
            .find(|line| span_within(line, anchor));
        let admin = context
            .iter()
            .filter(|line| {
                matches!(
                    line.kind,
                    FsEventKind::XfsUserShutdown | FsEventKind::XfsUnmount
                ) && same_device(line, anchor)
            })
            .find(|line| span_within(line, anchor));
        let (eligible, reason) = match (cause, admin) {
            (_, Some(_)) => (false, Some("admin-excluded")),
            (None, _) if !context_complete => (false, Some("context-incomplete")),
            (None, _) => (false, Some("missing-cause")),
            (Some(_), _) if !context_complete => (false, Some("context-incomplete")),
            (Some(_), _) => (true, None),
        };
        let mut members = vec![anchor];
        if let Some(cause) = cause {
            members.push(cause);
        }
        chains.push(make_chain(
            "xfs-shutdown",
            anchor,
            &members,
            anchor.device.clone(),
            ctx,
            now_epoch,
            eligible,
            reason,
        ));
    }
    for anchor in events
        .iter()
        .filter(|line| line.kind == FsEventKind::BlockIoError)
    {
        let (eligible, reason) = match anchor.operation {
            Some(_) => (true, None),
            None => (false, Some("non-read-write")),
        };
        chains.push(make_chain(
            "block-io",
            anchor,
            &[anchor],
            anchor.device.clone(),
            ctx,
            now_epoch,
            eligible,
            reason,
        ));
    }
    chains
}

fn valid_device_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn resolve_dev_t(device: &str) -> Option<String> {
    if !valid_device_name(device) {
        return None;
    }
    let path = format!("/sys/class/block/{device}/dev");
    let file = fs::File::open(path).ok()?;
    let mut content = String::new();
    file.take(64).read_to_string(&mut content).ok()?;
    let trimmed = content.trim();
    let (major, minor) = trimmed.split_once(':')?;
    (major.parse::<u64>().is_ok() && minor.parse::<u64>().is_ok()).then(|| trimmed.to_string())
}

fn mount_for_dev_t(builder: &HealthReportBuilder, dev_t: &str) -> Option<String> {
    builder.report().facts.iter().find_map(|fact| {
        let mount = fact
            .key
            .strip_prefix("filesystem.mount.")
            .and_then(|suffix| suffix.strip_suffix(".volume_device"))?;
        match &fact.value {
            HealthFactValue::String(value) if value == dev_t => Some(mount.to_string()),
            _ => None,
        }
    })
}

/// Record the newest eligible-and-mapped chain per mechanism plus the newest
/// degraded reason; unbound devices and admin/truncated chains stay D.
pub(crate) fn record_fs_chains(
    builder: &mut HealthReportBuilder,
    chains: &[FsChain],
    lines: &FsLines,
    elapsed_ms: u128,
) {
    fn add(
        builder: &mut HealthReportBuilder,
        key: String,
        value: HealthFactValue,
        elapsed_ms: u128,
    ) {
        builder.add_fact(
            HealthFactCategory::Kernel,
            key,
            value,
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        );
    }
    if !chains.is_empty() || lines.unverified > 0 {
        add(
            builder,
            "kernel.fs_event_source".to_string(),
            HealthFactValue::String(lines.source.to_string()),
            elapsed_ms,
        );
    }
    for (key, value) in [
        ("kernel.fs_event_dropped_events", lines.dropped_events),
        ("kernel.fs_event_dropped_context", lines.dropped_context),
        ("kernel.fs_event_unverified", lines.unverified),
    ] {
        if value > 0 {
            add(
                builder,
                key.to_string(),
                HealthFactValue::Unsigned(value),
                elapsed_ms,
            );
        }
    }
    for mechanism in [
        "ext4-forced-ro",
        "ext4-error",
        "jbd2-abort",
        "jbd2-flush",
        "xfs-shutdown",
        "block-io",
    ] {
        let prefix = format!("kernel.fs_{}", mechanism.replace('-', "_"));
        let candidates = chains.iter().filter(|chain| chain.mechanism == mechanism);
        let eligible = candidates
            .clone()
            .filter(|chain| chain.eligible)
            .min_by_key(|chain| chain.age_seconds);
        if let Some(chain) = eligible {
            if let Some(dev_t) = resolve_dev_t(&chain.fs_device) {
                if let Some(mount) = mount_for_dev_t(builder, &dev_t) {
                    add(
                        builder,
                        format!("{prefix}_age_seconds"),
                        HealthFactValue::Unsigned(chain.age_seconds),
                        elapsed_ms,
                    );
                    add(
                        builder,
                        format!("{prefix}_device"),
                        HealthFactValue::String(chain.device.clone()),
                        elapsed_ms,
                    );
                    add(
                        builder,
                        format!("{prefix}_dev_t"),
                        HealthFactValue::String(dev_t),
                        elapsed_ms,
                    );
                    add(
                        builder,
                        format!("{prefix}_mount"),
                        HealthFactValue::String(mount),
                        elapsed_ms,
                    );
                    add(
                        builder,
                        format!("{prefix}_identity"),
                        HealthFactValue::String(chain.identity.clone()),
                        elapsed_ms,
                    );
                    add(
                        builder,
                        format!("{prefix}_epoch"),
                        HealthFactValue::Unsigned(chain.epoch),
                        elapsed_ms,
                    );
                    if let Some(operation) = chain.operation {
                        add(
                            builder,
                            format!("{prefix}_operation"),
                            HealthFactValue::String(operation.to_string()),
                            elapsed_ms,
                        );
                    }
                    continue;
                }
            }
            add(
                builder,
                format!("{prefix}_degraded_reason"),
                HealthFactValue::String("device-unbound".to_string()),
                elapsed_ms,
            );
            add(
                builder,
                format!("{prefix}_degraded_age_seconds"),
                HealthFactValue::Unsigned(chain.age_seconds),
                elapsed_ms,
            );
            continue;
        }
        if let Some(chain) = candidates
            .filter(|chain| !chain.eligible)
            .min_by_key(|chain| chain.age_seconds)
        {
            add(
                builder,
                format!("{prefix}_degraded_reason"),
                HealthFactValue::String(chain.degraded_reason.unwrap_or("unknown").to_string()),
                elapsed_ms,
            );
            add(
                builder,
                format!("{prefix}_degraded_age_seconds"),
                HealthFactValue::Unsigned(chain.age_seconds),
                elapsed_ms,
            );
        }
    }
}

#[cfg(test)]
mod tests;
