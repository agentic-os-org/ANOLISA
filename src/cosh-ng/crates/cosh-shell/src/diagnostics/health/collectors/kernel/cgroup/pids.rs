//! R06 pids quota and E12 counter sampling: current group plus up to seven
//! visible ancestors (observation only). Missing layers bound the claim.

use std::path::Path;

use super::super::super::super::builder::HealthReportBuilder;
use super::super::super::super::model::{HealthFactCategory, HealthFactSource, HealthFactValue};
use super::{parse_quota_limit, read_bounded, QuotaValue, SCALAR_MAX_BYTES};

pub(crate) const MAX_VISIBLE_ANCESTORS: usize = 7;

pub(super) fn parse_pids_events_max(content: &str) -> Option<u64> {
    content.lines().find_map(|line| {
        let (key, value) = line.split_once(' ')?;
        (key == "max").then(|| value.trim().parse().ok())?
    })
}

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

fn read_pair(dir: &Path) -> (Option<u64>, Option<QuotaValue>) {
    let current = read_bounded(&dir.join("pids.current"), SCALAR_MAX_BYTES)
        .and_then(|content| content.trim().parse::<u64>().ok());
    let max = read_bounded(&dir.join("pids.max"), SCALAR_MAX_BYTES)
        .and_then(|content| parse_quota_limit(&content));
    (current, max)
}

pub(crate) fn collect_pids(
    builder: &mut HealthReportBuilder,
    mount: &Path,
    path: &str,
    object_id: &str,
    elapsed_ms: u128,
) {
    let dir = mount.join(path.trim_start_matches('/'));
    let (current, max) = read_pair(&dir);
    let events_max = read_bounded(&dir.join("pids.events"), SCALAR_MAX_BYTES)
        .and_then(|content| parse_pids_events_max(&content));
    if current.is_none() && max.is_none() && events_max.is_none() {
        return;
    }
    if let Some(current) = current {
        add(
            builder,
            "kernel.cgroup_pids_current",
            HealthFactValue::Unsigned(current),
            elapsed_ms,
        );
    }
    match max {
        Some(QuotaValue::Finite(value)) => {
            add(
                builder,
                "kernel.cgroup_pids_max",
                HealthFactValue::Unsigned(value),
                elapsed_ms,
            );
        }
        Some(QuotaValue::Unlimited) => {
            add(
                builder,
                "kernel.cgroup_pids_max_unlimited",
                HealthFactValue::Bool(true),
                elapsed_ms,
            );
        }
        None => {}
    }
    if let Some(events_max) = events_max {
        add(
            builder,
            "kernel.cgroup_pids_events_max",
            HealthFactValue::Unsigned(events_max),
            elapsed_ms,
        );
    }
    add(
        builder,
        "kernel.cgroup_pids_object_id",
        HealthFactValue::String(object_id.to_string()),
        elapsed_ms,
    );
    if let (Some(current), Some(limit)) = (current, max) {
        let limit = match limit {
            QuotaValue::Finite(value) => {
                crate::diagnostics::health::rules::pids::PidsLimit::Finite(value)
            }
            QuotaValue::Unlimited => crate::diagnostics::health::rules::pids::PidsLimit::Unlimited,
        };
        if let Some(level) = crate::diagnostics::health::rules::pids::judge_pids(
            current,
            limit,
            crate::diagnostics::health::rules::pids::P_MAX_HEALTH_PATH_TIDS,
        )
        .entry
        {
            let label = match level {
                crate::diagnostics::health::rules::volume::MemberLevel::Critical => "critical",
                crate::diagnostics::health::rules::volume::MemberLevel::Warning => "warning",
            };
            add(
                builder,
                "kernel.cgroup_pids_level",
                HealthFactValue::String(label.to_string()),
                elapsed_ms,
            );
        }
    }
    let components: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let mut visible = 0usize;
    for depth in 1..=MAX_VISIBLE_ANCESTORS {
        if depth > components.len() {
            break;
        }
        let ancestor_path = format!("/{}", components[..components.len() - depth].join("/"));
        let ancestor_dir = mount.join(ancestor_path.trim_start_matches('/'));
        let (ancestor_current, ancestor_max) = read_pair(&ancestor_dir);
        if ancestor_current.is_none() && ancestor_max.is_none() {
            break;
        }
        visible = depth;
        let prefix = format!("kernel.cgroup_ancestor_{depth}");
        add(
            builder,
            &format!("{prefix}_path"),
            HealthFactValue::String(ancestor_path),
            elapsed_ms,
        );
        if let Some(value) = ancestor_current {
            add(
                builder,
                &format!("{prefix}_pids_current"),
                HealthFactValue::Unsigned(value),
                elapsed_ms,
            );
        }
        match ancestor_max {
            Some(QuotaValue::Finite(value)) => {
                add(
                    builder,
                    &format!("{prefix}_pids_max"),
                    HealthFactValue::Unsigned(value),
                    elapsed_ms,
                );
            }
            Some(QuotaValue::Unlimited) => {
                add(
                    builder,
                    &format!("{prefix}_pids_max_unlimited"),
                    HealthFactValue::Bool(true),
                    elapsed_ms,
                );
            }
            None => {}
        }
    }
    add(
        builder,
        "kernel.cgroup_ancestor_visible_depth",
        HealthFactValue::Unsigned(visible as u64),
        elapsed_ms,
    );
}

#[cfg(test)]
mod tests;
