//! Device resolution and fact recording for proven filesystem event chains.

use std::fs;
use std::io::Read;

use serde::{Deserialize, Serialize};

use super::super::super::super::builder::HealthReportBuilder;
use super::super::super::super::model::{HealthFactCategory, HealthFactSource, HealthFactValue};
use super::{FsChain, FsLines, FS_EVENT_QUOTA};

/// One eligible chain offered to the parent for device→mount binding. The
/// kernel worker resolves current sysfs dev_t but cannot see the filesystem
/// worker's volume facts, so it offers every bounded candidate and the parent
/// selects the newest one that actually binds to a scanned volume.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FsCandidate {
    device: String,
    dev_t: String,
    age_seconds: u64,
    identity: String,
    epoch: u64,
    operation: Option<String>,
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

const FS_MECHANISMS: [&str; 6] = [
    "ext4-forced-ro",
    "ext4-error",
    "jbd2-abort",
    "jbd2-flush",
    "xfs-shutdown",
    "block-io",
];

/// Offer up to FS_EVENT_QUOTA eligible chains per mechanism as a candidate
/// list fact (current sysfs dev_t resolved per chain); the parent binds them
/// against the full volume fact set and records the newest bound one. Unbound
/// devices and admin/truncated chains stay D.
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
    for mechanism in FS_MECHANISMS {
        let prefix = format!("kernel.fs_{}", mechanism.replace('-', "_"));
        let candidates = chains.iter().filter(|chain| chain.mechanism == mechanism);
        let mut eligible: Vec<&FsChain> =
            candidates.clone().filter(|chain| chain.eligible).collect();
        eligible.sort_by_key(|chain| chain.age_seconds);
        if !eligible.is_empty() {
            let offered: Vec<FsCandidate> = eligible
                .iter()
                .take(FS_EVENT_QUOTA)
                .filter_map(|chain| {
                    resolve_dev_t(&chain.fs_device).map(|dev_t| FsCandidate {
                        device: chain.device.clone(),
                        dev_t,
                        age_seconds: chain.age_seconds,
                        identity: chain.identity.clone(),
                        epoch: chain.epoch,
                        operation: chain.operation.map(str::to_string),
                    })
                })
                .collect();
            if !offered.is_empty() {
                if let Ok(json) = serde_json::to_string(&offered) {
                    add(
                        builder,
                        format!("{prefix}_candidates"),
                        HealthFactValue::String(json),
                        elapsed_ms,
                    );
                }
                continue;
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
                HealthFactValue::Unsigned(eligible[0].age_seconds),
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

/// Bind offered candidates against the full scan fact set and promote the
/// newest candidate that binds to a scanned volume. Each sample source runs in
/// its own worker process, so the kernel worker cannot see the filesystem
/// worker's volume facts; the parent binds them before judgement. Selecting
/// among bound candidates (rather than the globally newest event) keeps a
/// newer event on an unselected device from shadowing a selected one.
pub(crate) fn bind_fs_chain_mounts(builder: &mut HealthReportBuilder, elapsed_ms: u128) {
    for mechanism in FS_MECHANISMS {
        let prefix = format!("kernel.fs_{}", mechanism.replace('-', "_"));
        let mount_key = format!("{prefix}_mount");
        let candidates_key = format!("{prefix}_candidates");
        let (has_mount, offered) = {
            let facts = &builder.report().facts;
            let has_mount = facts.iter().any(|fact| fact.key == mount_key);
            let offered = facts.iter().find_map(|fact| {
                if fact.key != candidates_key {
                    return None;
                }
                match &fact.value {
                    HealthFactValue::String(value) => {
                        serde_json::from_str::<Vec<FsCandidate>>(value).ok()
                    }
                    _ => None,
                }
            });
            (has_mount, offered)
        };
        let Some(offered) = offered else {
            continue;
        };
        if has_mount || offered.is_empty() {
            continue;
        }
        let bound = offered
            .iter()
            .filter_map(|candidate| {
                mount_for_dev_t(builder, &candidate.dev_t).map(|mount| (candidate, mount))
            })
            .min_by_key(|(candidate, _)| candidate.age_seconds);
        match bound {
            Some((candidate, mount)) => {
                for (key, value) in [
                    (
                        format!("{prefix}_age_seconds"),
                        HealthFactValue::Unsigned(candidate.age_seconds),
                    ),
                    (
                        format!("{prefix}_device"),
                        HealthFactValue::String(candidate.device.clone()),
                    ),
                    (
                        format!("{prefix}_dev_t"),
                        HealthFactValue::String(candidate.dev_t.clone()),
                    ),
                    (mount_key.clone(), HealthFactValue::String(mount)),
                    (
                        format!("{prefix}_identity"),
                        HealthFactValue::String(candidate.identity.clone()),
                    ),
                    (
                        format!("{prefix}_epoch"),
                        HealthFactValue::Unsigned(candidate.epoch),
                    ),
                ] {
                    builder.add_fact(
                        HealthFactCategory::Kernel,
                        key,
                        value,
                        None,
                        HealthFactSource::Derived,
                        elapsed_ms,
                    );
                }
                if let Some(operation) = &candidate.operation {
                    builder.add_fact(
                        HealthFactCategory::Kernel,
                        format!("{prefix}_operation"),
                        HealthFactValue::String(operation.clone()),
                        None,
                        HealthFactSource::Derived,
                        elapsed_ms,
                    );
                }
            }
            None => {
                let age = offered
                    .iter()
                    .map(|candidate| candidate.age_seconds)
                    .min()
                    .unwrap_or(0);
                builder.add_fact(
                    HealthFactCategory::Kernel,
                    format!("{prefix}_degraded_reason"),
                    HealthFactValue::String("device-unbound".to_string()),
                    None,
                    HealthFactSource::Derived,
                    elapsed_ms,
                );
                builder.add_fact(
                    HealthFactCategory::Kernel,
                    format!("{prefix}_degraded_age_seconds"),
                    HealthFactValue::Unsigned(age),
                    None,
                    HealthFactSource::Derived,
                    elapsed_ms,
                );
            }
        }
    }
}
