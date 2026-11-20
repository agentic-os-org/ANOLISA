//! Device resolution and fact recording for proven filesystem event chains.

use std::fs;
use std::io::Read;

use super::super::super::super::builder::HealthReportBuilder;
use super::super::super::super::model::{HealthFactCategory, HealthFactSource, HealthFactValue};
use super::{FsChain, FsLines};

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
