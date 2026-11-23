//! Current-cgroup membership and oom_kill counter for R04 attribution.
//! cgroup v1 stays unsupported-details in the first batch.

use std::fs;
use std::io::Read;
use std::path::Path;

use super::super::super::builder::HealthReportBuilder;
use super::super::super::model::{
    HealthFactCategory, HealthFactScope, HealthFactSource, HealthFactValue,
};

const SCALAR_MAX_BYTES: u64 = 2048;
const MEMBERSHIP_MAX_BYTES: u64 = 8192;

mod pids;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CgroupFacts {
    pub version: u8,
    pub path: Option<String>,
    pub mount: Option<String>,
    pub object_id: Option<String>,
    pub oom_kill: Option<u64>,
    pub counter_source: Option<&'static str>,
}

pub(super) fn parse_membership_v2(content: &str) -> Option<String> {
    let mut path = None;
    for line in content.lines() {
        let mut fields = line.splitn(3, ':');
        let hierarchy = fields.next()?.trim();
        let controllers = fields.next()?.trim();
        let rest = fields.next()?.trim();
        if hierarchy != "0" || !controllers.is_empty() {
            return None;
        }
        if path.replace(rest.to_string()).is_some() {
            return None;
        }
    }
    path
}

pub(super) fn parse_events_oom_kill(content: &str) -> Option<u64> {
    content.lines().find_map(|line| {
        let (key, value) = line.split_once(' ')?;
        (key == "oom_kill").then(|| value.trim().parse().ok())?
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuotaValue {
    Unlimited,
    Finite(u64),
}

pub(super) fn parse_quota_limit(content: &str) -> Option<QuotaValue> {
    let trimmed = content.trim();
    if trimmed == "max" {
        return Some(QuotaValue::Unlimited);
    }
    trimmed.parse::<u64>().ok().map(QuotaValue::Finite)
}

pub(super) fn parse_quota_current(content: &str) -> Option<u64> {
    content.trim().parse::<u64>().ok()
}

pub(super) fn normalize_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

/// Victim domain matches the current group or a descendant under it;
/// ancestors and siblings never attribute to the current group.
pub(crate) fn in_current_domain(victim: &str, current: &str) -> bool {
    let victim = normalize_path(victim);
    let current = normalize_path(current);
    if current == "/" {
        return true;
    }
    victim == current || victim.starts_with(&format!("{current}/"))
}

fn read_bounded(path: &Path, limit: u64) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut content = String::new();
    file.take(limit + 1).read_to_string(&mut content).ok()?;
    if content.len() as u64 > limit {
        return None;
    }
    Some(content)
}

fn cgroup2_mount() -> Option<String> {
    let content = read_bounded(Path::new("/proc/self/mountinfo"), 64 * 1024)?;
    content.lines().find_map(|line| {
        let (left, right) = line.split_once(" - ")?;
        let fs_type = right.split_whitespace().next()?;
        if fs_type != "cgroup2" {
            return None;
        }
        let mount = left.split_whitespace().nth(4)?;
        Some(
            mount
                .replace("\\040", " ")
                .replace("\\011", "\t")
                .replace("\\134", "\\"),
        )
    })
}

fn object_id(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::metadata(path).ok()?;
    Some(format!("{}:{}", metadata.dev(), metadata.ino()))
}

pub(crate) fn collect_cgroup(builder: &mut HealthReportBuilder, now_epoch_seconds: u64) {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (builder, now_epoch_seconds);
    }
    #[cfg(target_os = "linux")]
    linux::collect(builder, now_epoch_seconds);
}

pub(crate) fn record(builder: &mut HealthReportBuilder, facts: &CgroupFacts, elapsed_ms: u128) {
    builder.set_scope(HealthFactScope::Cgroup(
        facts.path.clone().unwrap_or_else(|| "unknown".into()),
    ));
    let mut add = |key: &str, value: HealthFactValue| {
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
        "kernel.current_cgroup_version",
        HealthFactValue::Unsigned(u64::from(facts.version)),
    );
    if let Some(path) = &facts.path {
        add(
            "kernel.current_cgroup_path",
            HealthFactValue::String(path.clone()),
        );
    }
    if let Some(mount) = &facts.mount {
        add(
            "kernel.current_cgroup_mount",
            HealthFactValue::String(mount.clone()),
        );
    }
    if let Some(object_id) = &facts.object_id {
        add(
            "kernel.current_cgroup_id",
            HealthFactValue::String(object_id.clone()),
        );
    }
    if let Some(oom_kill) = facts.oom_kill {
        add(
            "kernel.cgroup_oom_kill_count",
            HealthFactValue::Unsigned(oom_kill),
        );
    }
    if let Some(source) = facts.counter_source {
        add(
            "kernel.cgroup_oom_kill_source",
            HealthFactValue::String(source.to_string()),
        );
    }
    builder.set_scope(HealthFactScope::Host);
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use crate::diagnostics::health::suppression::episode;

    pub(super) fn collect(builder: &mut HealthReportBuilder, now_epoch_seconds: u64) {
        let started = std::time::Instant::now();
        let membership = read_bounded(Path::new("/proc/self/cgroup"), MEMBERSHIP_MAX_BYTES);
        let path = membership.as_deref().and_then(parse_membership_v2);
        let Some(path) = path else {
            let version = match membership.as_deref() {
                None => 0,
                Some(content) if content.lines().any(|line| line.starts_with("0:")) => 0,
                Some(_) => 1,
            };
            record(
                builder,
                &CgroupFacts {
                    version,
                    path: None,
                    mount: None,
                    object_id: None,
                    oom_kill: None,
                    counter_source: None,
                },
                started.elapsed().as_millis(),
            );
            return;
        };
        let path = normalize_path(&path);
        let mount = cgroup2_mount();
        let dir = mount.as_ref().map(|mount| {
            let trimmed = path.trim_start_matches('/');
            Path::new(mount).join(trimmed)
        });
        let object_id = dir.as_deref().and_then(object_id);
        let (oom_kill, counter_source) = dir
            .as_deref()
            .and_then(|dir| {
                read_bounded(&dir.join("memory.events.local"), SCALAR_MAX_BYTES)
                    .and_then(|content| parse_events_oom_kill(&content))
                    .map(|count| (count, "memory.events.local"))
                    .or_else(|| {
                        read_bounded(&dir.join("memory.events"), SCALAR_MAX_BYTES)
                            .and_then(|content| parse_events_oom_kill(&content))
                            .map(|count| (count, "memory.events"))
                    })
            })
            .map_or((None, None), |(count, source)| (Some(count), Some(source)));
        if let (Some(count), Some(source), Some(object_id)) =
            (oom_kill, counter_source, object_id.as_ref())
        {
            let boot_id = builder.report().facts.iter().find_map(|fact| {
                (fact.key == "kernel.boot_id").then(|| match &fact.value {
                    HealthFactValue::String(value) => value.clone(),
                    _ => String::new(),
                })
            });
            if let Some(boot_id) = boot_id.filter(|boot| !boot.is_empty()) {
                let sample = episode::CounterSample {
                    boot_id,
                    cgroup_id: object_id.clone(),
                    source: source.to_string(),
                    count,
                    sampled_epoch: now_epoch_seconds,
                };
                let increment = episode::ledger_path()
                    .and_then(|path| episode::load(&path))
                    .and_then(|ledger| episode::peek_increment(&ledger, &sample));
                if let Some((start, end, delta)) = increment {
                    for (key, value) in [
                        ("kernel.cgroup_oom_kill_increment", delta),
                        ("kernel.cgroup_oom_kill_window_start_epoch", start),
                        ("kernel.cgroup_oom_kill_window_end_epoch", end),
                    ] {
                        builder.add_fact(
                            HealthFactCategory::Kernel,
                            key,
                            HealthFactValue::Unsigned(value),
                            None,
                            HealthFactSource::Derived,
                            started.elapsed().as_millis(),
                        );
                    }
                }
            }
        }
        if oom_kill.is_some() {
            builder.add_fact(
                HealthFactCategory::Kernel,
                "kernel.cgroup_oom_kill_sampled_epoch",
                HealthFactValue::Unsigned(now_epoch_seconds),
                Some("seconds".to_string()),
                HealthFactSource::Derived,
                started.elapsed().as_millis(),
            );
        }
        if let (Some(dir), Some(object_id)) = (dir.as_deref(), object_id.as_ref()) {
            record_quota_pressure(
                builder,
                dir,
                &path,
                object_id,
                started.elapsed().as_millis(),
            );
            if let Some(mount) = mount.as_deref() {
                pids::collect_pids(
                    builder,
                    Path::new(mount),
                    &path,
                    object_id,
                    started.elapsed().as_millis(),
                );
                record_pids_increment(
                    builder,
                    object_id,
                    now_epoch_seconds,
                    started.elapsed().as_millis(),
                );
            }
        }
        record(
            builder,
            &CgroupFacts {
                version: 2,
                path: Some(path),
                mount,
                object_id,
                oom_kill,
                counter_source,
            },
            started.elapsed().as_millis(),
        );
    }

    fn record_quota_pressure(
        builder: &mut HealthReportBuilder,
        dir: &Path,
        path: &str,
        object_id: &str,
        elapsed_ms: u128,
    ) {
        use crate::diagnostics::health::collectors::{read_pressure, record_pressure};
        fn add(
            builder: &mut HealthReportBuilder,
            key: &str,
            value: HealthFactValue,
            unit: Option<String>,
            elapsed_ms: u128,
        ) {
            builder.add_fact(
                HealthFactCategory::Kernel,
                key,
                value,
                unit,
                HealthFactSource::Derived,
                elapsed_ms,
            );
        }
        let current = read_bounded(&dir.join("memory.current"), SCALAR_MAX_BYTES)
            .and_then(|content| parse_quota_current(&content));
        let max = read_bounded(&dir.join("memory.max"), SCALAR_MAX_BYTES)
            .and_then(|content| parse_quota_limit(&content));
        let high = read_bounded(&dir.join("memory.high"), SCALAR_MAX_BYTES)
            .and_then(|content| parse_quota_limit(&content));
        let quota_present = current.is_some() || max.is_some() || high.is_some();
        let mut psi_present = false;
        builder.set_scope(HealthFactScope::Cgroup(path.to_string()));
        if let Some(current) = current {
            add(
                builder,
                "kernel.cgroup_memory_current_bytes",
                HealthFactValue::Unsigned(current),
                Some("bytes".to_string()),
                elapsed_ms,
            );
        }
        for (name, limit) in [("max", max), ("high", high)] {
            match limit {
                Some(QuotaValue::Finite(bytes)) => add(
                    builder,
                    &format!("kernel.cgroup_memory_{name}_bytes"),
                    HealthFactValue::Unsigned(bytes),
                    Some("bytes".to_string()),
                    elapsed_ms,
                ),
                Some(QuotaValue::Unlimited) => add(
                    builder,
                    &format!("kernel.cgroup_memory_{name}_unlimited"),
                    HealthFactValue::Bool(true),
                    None,
                    elapsed_ms,
                ),
                None => {}
            }
        }
        if quota_present {
            add(
                builder,
                "kernel.cgroup_memory_object_id",
                HealthFactValue::String(object_id.to_string()),
                None,
                elapsed_ms,
            );
        }
        for resource in ["memory", "io", "cpu"] {
            let Some(sample) = read_pressure(&dir.join(format!("{resource}.pressure"))) else {
                continue;
            };
            if sample.some.is_some() || sample.full.is_some() {
                psi_present = true;
            }
            let prefix = format!("kernel.cgroup_psi_{resource}");
            record_pressure(
                builder,
                &prefix,
                HealthFactCategory::Kernel,
                &sample,
                elapsed_ms,
            );
            if resource != "cpu" {
                if let Some(level) = crate::diagnostics::health::rules::psi::pressure_level(
                    sample.full.map(|line| (line.avg10, line.avg60)),
                ) {
                    add(
                        builder,
                        &format!("{prefix}_level"),
                        HealthFactValue::String(level.to_string()),
                        None,
                        elapsed_ms,
                    );
                }
            }
        }
        if psi_present {
            add(
                builder,
                "kernel.cgroup_psi_object_id",
                HealthFactValue::String(object_id.to_string()),
                None,
                elapsed_ms,
            );
        }
        builder.set_scope(HealthFactScope::Host);
    }

    fn record_pids_increment(
        builder: &mut HealthReportBuilder,
        object_id: &str,
        now_epoch_seconds: u64,
        elapsed_ms: u128,
    ) {
        let events_max = builder.report().facts.iter().find_map(|fact| {
            (fact.key == "kernel.cgroup_pids_events_max").then_some(match &fact.value {
                HealthFactValue::Unsigned(value) => *value,
                _ => 0,
            })
        });
        let Some(count) = events_max else {
            return;
        };
        builder.add_fact(
            HealthFactCategory::Kernel,
            "kernel.cgroup_pids_sampled_epoch",
            HealthFactValue::Unsigned(now_epoch_seconds),
            Some("seconds".to_string()),
            HealthFactSource::Derived,
            elapsed_ms,
        );
        let boot_id = builder.report().facts.iter().find_map(|fact| {
            (fact.key == "kernel.boot_id").then(|| match &fact.value {
                HealthFactValue::String(value) => value.clone(),
                _ => String::new(),
            })
        });
        let Some(boot_id) = boot_id.filter(|boot| !boot.is_empty()) else {
            return;
        };
        let sample = episode::CounterSample {
            boot_id,
            cgroup_id: object_id.to_string(),
            source: "pids.events".to_string(),
            count,
            sampled_epoch: now_epoch_seconds,
        };
        let increment = episode::ledger_path()
            .and_then(|path| episode::load(&path))
            .and_then(|ledger| episode::peek_pids_increment(&ledger, &sample));
        if let Some((start, end, delta)) = increment {
            for (key, value) in [
                ("kernel.pids_events_max_increment", delta),
                ("kernel.pids_events_max_window_start_epoch", start),
                ("kernel.pids_events_max_window_end_epoch", end),
            ] {
                builder.add_fact(
                    HealthFactCategory::Kernel,
                    key,
                    HealthFactValue::Unsigned(value),
                    None,
                    HealthFactSource::Derived,
                    elapsed_ms,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests;
