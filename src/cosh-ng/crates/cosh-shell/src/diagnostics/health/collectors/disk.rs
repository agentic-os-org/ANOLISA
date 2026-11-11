use super::super::builder::HealthReportBuilder;
use super::super::model::{
    HealthCollector, HealthFactCategory, HealthFactScope, HealthFactSource, HealthFactValue,
    HealthSeverity, HealthUnavailableReason,
};
use super::system::run_system_command;
use super::{unavailable_from_stderr, HealthCommandConfig};
use crate::config::HealthConfig;
use std::collections::BTreeSet;
#[cfg(target_os = "linux")]
use std::io::Read;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct DiskMountFacts {
    pub(super) mount: String,
    fs_type: String,
    used_ratio: f64,
    available_gib: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DiskFacts {
    pub(crate) riskiest_mount: String,
    pub(crate) riskiest_type: String,
    pub(crate) max_used_ratio: f64,
    pub(crate) available_gib: f64,
    pub(crate) root_used_ratio: Option<f64>,
    pub(crate) root_available_gib: Option<f64>,
    pub(super) mounts: Vec<DiskMountFacts>,
    scope_incomplete: bool,
}

impl DiskFacts {
    fn record_summary(&self, builder: &mut HealthReportBuilder, elapsed_ms: u128) {
        builder
            .add_fact(
                HealthFactCategory::Disk,
                "filesystem.riskiest_mount",
                HealthFactValue::String(self.riskiest_mount.clone()),
                None,
                HealthFactSource::DfP,
                elapsed_ms,
            )
            .add_fact(
                HealthFactCategory::Disk,
                "filesystem.riskiest_type",
                HealthFactValue::String(self.riskiest_type.clone()),
                None,
                HealthFactSource::DfP,
                elapsed_ms,
            )
            .add_fact(
                HealthFactCategory::Disk,
                "filesystem.max_used_ratio",
                HealthFactValue::Float(self.max_used_ratio),
                None,
                HealthFactSource::DfP,
                elapsed_ms,
            )
            .add_fact(
                HealthFactCategory::Disk,
                "filesystem.available_gib",
                HealthFactValue::Float(self.available_gib),
                Some("GiB".to_string()),
                HealthFactSource::DfP,
                elapsed_ms,
            );
        if let Some(root_used_ratio) = self.root_used_ratio {
            builder.add_fact(
                HealthFactCategory::Disk,
                "filesystem.root_used_ratio",
                HealthFactValue::Float(root_used_ratio),
                None,
                HealthFactSource::DfP,
                elapsed_ms,
            );
        }
        if let Some(root_available_gib) = self.root_available_gib {
            builder.add_fact(
                HealthFactCategory::Disk,
                "filesystem.root_available_gib",
                HealthFactValue::Float(root_available_gib),
                Some("GiB".to_string()),
                HealthFactSource::DfP,
                elapsed_ms,
            );
        }
    }
}

pub(crate) fn parse_df_p(content: &str, critical_mounts: &[String]) -> Option<DiskFacts> {
    DiskFacts::from_mounts(
        content
            .lines()
            .skip(1)
            .filter_map(|line| parse_df_line(line, critical_mounts))
            .map(|row| DiskMountFacts {
                mount: row.riskiest_mount,
                fs_type: row.riskiest_type,
                used_ratio: row.max_used_ratio,
                available_gib: row.available_gib,
            })
            .collect(),
    )
}

fn parse_df_line(line: &str, critical_mounts: &[String]) -> Option<DiskFacts> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    let with_type = fields.len() >= 7;
    let (fs_type, available_1k, capacity, mount) = if with_type {
        (fields[1], fields[4], fields[5], fields[6])
    } else {
        (
            "unknown",
            fields.get(3).copied()?,
            fields.get(4).copied()?,
            fields.get(5).copied()?,
        )
    };
    if is_ignored_filesystem(fs_type) && !critical_mounts.iter().any(|item| item == mount) {
        return None;
    }
    let used_percent = capacity.trim_end_matches('%').parse::<f64>().ok()?;
    let available_gib = available_1k.parse::<f64>().ok()? / 1024.0 / 1024.0;
    Some(DiskFacts {
        riskiest_mount: mount.to_string(),
        riskiest_type: fs_type.to_string(),
        max_used_ratio: used_percent / 100.0,
        available_gib,
        root_used_ratio: None,
        root_available_gib: None,
        mounts: Vec::new(),
        scope_incomplete: false,
    })
}

fn is_ignored_filesystem(fs_type: &str) -> bool {
    matches!(
        fs_type,
        "tmpfs" | "devtmpfs" | "overlay" | "iso9660" | "squashfs" | "udf"
    )
}

impl DiskFacts {
    fn from_mounts(mounts: Vec<DiskMountFacts>) -> Option<Self> {
        let riskiest = mounts.iter().max_by(|left, right| {
            left.used_ratio
                .total_cmp(&right.used_ratio)
                .then_with(|| right.available_gib.total_cmp(&left.available_gib))
        })?;
        let root = mounts.iter().find(|row| row.mount == "/");
        Some(Self {
            riskiest_mount: riskiest.mount.clone(),
            riskiest_type: riskiest.fs_type.clone(),
            max_used_ratio: riskiest.used_ratio,
            available_gib: riskiest.available_gib,
            root_used_ratio: root.map(|row| row.used_ratio),
            root_available_gib: root.map(|row| row.available_gib),
            mounts,
            scope_incomplete: false,
        })
    }

    pub(crate) fn record(&self, builder: &mut HealthReportBuilder, elapsed_ms: u128) {
        let id = builder.current_check_id().map(str::to_owned);
        let rows = self
            .mounts
            .iter()
            .filter(|row| match id.as_deref() {
                Some("filesystem.root") => row.mount == "/",
                Some("filesystem.mounts") => row.mount != "/",
                _ => true,
            })
            .cloned()
            .collect::<Vec<_>>();
        if id.as_deref() == Some("filesystem.root") && rows.is_empty() {
            builder.add_unavailable(
                HealthCollector::Disk,
                HealthUnavailableReason::ParseError,
                HealthSeverity::Degraded,
                elapsed_ms,
            );
            return;
        }
        for row in &rows {
            builder.set_scope(HealthFactScope::Path(row.mount.clone()));
            for (name, value, unit) in [
                ("path", HealthFactValue::String(row.mount.clone()), None),
                ("used_ratio", HealthFactValue::Float(row.used_ratio), None),
                (
                    "available_gib",
                    HealthFactValue::Float(row.available_gib),
                    Some("GiB".into()),
                ),
            ] {
                builder.add_fact(
                    HealthFactCategory::Disk,
                    format!("filesystem.mount.{}.{}", row.mount, name),
                    value,
                    unit,
                    HealthFactSource::DfP,
                    elapsed_ms,
                );
            }
        }
        if id.as_deref() == Some("filesystem.root") {
            if let Some(root) = rows.first() {
                builder.add_fact(
                    HealthFactCategory::Disk,
                    "filesystem.root_used_ratio",
                    HealthFactValue::Float(root.used_ratio),
                    None,
                    HealthFactSource::DfP,
                    elapsed_ms,
                );
                builder.add_fact(
                    HealthFactCategory::Disk,
                    "filesystem.root_available_gib",
                    HealthFactValue::Float(root.available_gib),
                    Some("GiB".into()),
                    HealthFactSource::DfP,
                    elapsed_ms,
                );
            }
        } else if let Some(summary) = Self::from_mounts(rows) {
            builder.set_scope(HealthFactScope::Path(summary.riskiest_mount.clone()));
            summary.record_summary(builder, elapsed_ms);
        }
        if self.scope_incomplete && id.as_deref() != Some("filesystem.root") {
            builder.add_unavailable(
                HealthCollector::Disk,
                HealthUnavailableReason::Unsupported,
                HealthSeverity::Unavailable,
                elapsed_ms,
            );
        }
    }
}

#[cfg(test)]
pub(super) fn collect_disk(builder: &mut HealthReportBuilder, config: &HealthConfig) {
    let started = Instant::now();
    match read_disk_sample(config, false, true, true) {
        Ok(facts) => {
            facts.record(builder, started.elapsed().as_millis());
            builder.add_check_done("disk");
        }
        Err(reason) => {
            builder.add_unavailable(
                HealthCollector::Disk,
                reason,
                HealthSeverity::Degraded,
                started.elapsed().as_millis(),
            );
        }
    }
}

pub(in crate::diagnostics::health) fn read_disk_sample(
    config: &HealthConfig,
    passive: bool,
    root: bool,
    mounts: bool,
) -> Result<DiskFacts, HealthUnavailableReason> {
    let local = if passive && mounts {
        local_mounts()
    } else {
        BTreeSet::new()
    };
    let mut scope_incomplete = false;
    let paths = if passive || config.scan_mounts.is_some() || !mounts {
        let mut paths = BTreeSet::new();
        if root {
            paths.insert("/".to_string());
        }
        if mounts {
            for path in config.scan_mounts.as_deref().unwrap_or_default() {
                if path == "/" {
                    continue;
                }
                let absolute = std::path::Path::new(path).is_absolute()
                    && !std::path::Path::new(path)
                        .components()
                        .any(|component| matches!(component, std::path::Component::ParentDir));
                if absolute && (!passive || local.contains(path)) {
                    paths.insert(path.clone());
                } else {
                    scope_incomplete = true;
                }
            }
        }
        if paths.is_empty() {
            return Err(HealthUnavailableReason::Unsupported);
        }
        Some(paths.into_iter().collect::<Vec<_>>())
    } else {
        None
    };
    let mut args = vec!["-k", "-P"];
    #[cfg(target_os = "linux")]
    args.push("-T");
    if let Some(paths) = paths.as_ref() {
        args.push("--");
        args.extend(paths.iter().map(String::as_str));
    }
    let output = run_system_command("df", &args, HealthCommandConfig::default())
        .map_err(|error| error.unavailable_reason())?;
    let mut facts = parse_df_p(&output.stdout, &config.critical_mounts).ok_or_else(|| {
        if output.exit_code != Some(0) {
            unavailable_from_stderr(&output.stderr)
        } else {
            HealthUnavailableReason::ParseError
        }
    })?;
    facts.scope_incomplete = scope_incomplete || output.exit_code != Some(0);
    if let Some(paths) = &paths {
        facts.scope_incomplete |= paths
            .iter()
            .filter(|path| path.as_str() != "/")
            .any(|path| !facts.mounts.iter().any(|row| row.mount == *path));
    }
    facts.mounts.retain(|row| {
        (root || row.mount != "/")
            && (mounts || row.mount == "/")
            && paths
                .as_ref()
                .is_none_or(|paths| paths.contains(&row.mount))
    });
    Ok(facts)
}

fn local_mounts() -> BTreeSet<String> {
    #[cfg(not(target_os = "linux"))]
    {
        BTreeSet::new()
    }
    #[cfg(target_os = "linux")]
    {
        let Ok(file) = std::fs::File::open("/proc/self/mountinfo") else {
            return BTreeSet::new();
        };
        let mut content = String::new();
        if file
            .take(256 * 1024 + 1)
            .read_to_string(&mut content)
            .is_err()
            || content.len() > 256 * 1024
        {
            return BTreeSet::new();
        }
        local_mounts_in(&content)
    }
}

#[cfg(target_os = "linux")]
fn local_mounts_in(content: &str) -> BTreeSet<String> {
    content
        .lines()
        .filter_map(|line| {
            let (left, right) = line.split_once(" - ")?;
            let mount = left
                .split_whitespace()
                .nth(4)?
                .replace("\\040", " ")
                .replace("\\011", "\t")
                .replace("\\134", "\\");
            matches!(
                right.split_whitespace().next()?,
                "ext2" | "ext3" | "ext4" | "xfs" | "btrfs" | "zfs" | "f2fs" | "overlay"
            )
            .then_some(mount)
        })
        .collect()
}

#[cfg(test)]
mod tests;
