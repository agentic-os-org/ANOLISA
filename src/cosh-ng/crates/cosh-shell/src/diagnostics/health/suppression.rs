use crate::config::private_state;
use std::collections::{BTreeMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub(crate) mod claim;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::model::{
    HealthFactValue, HealthFinding, HealthFindingCategory, HealthScanReport, HealthSeverity,
};

const MAX_ENTRIES: usize = 128;
const MAX_BYTES: usize = 128 * 1024;
const RETAIN_WINDOW_MS: u64 = 7 * 24 * 60 * 60 * 1000;
const WARNING_TTL_MS: u64 = 4 * 60 * 60 * 1000;
const CRITICAL_TTL_MS: u64 = 30 * 60 * 1000;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HealthSuppressionEntry {
    pub(crate) host_id: String,
    pub(crate) finding_fingerprint: String,
    pub(crate) severity: HealthSeverity,
    pub(crate) shown_at_ms: u64,
    pub(crate) metrics: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SuppressionDecision {
    Show,
    Suppress,
}

#[derive(Debug, Default)]
pub(crate) struct HealthSuppressionStore {
    entries: Vec<HealthSuppressionEntry>,
    session_seen: HashSet<String>,
    unavailable: bool,
}

impl HealthSuppressionStore {
    pub(crate) fn load_default() -> Self {
        health_suppression_store_path()
            .map(|path| Self::load_from_path(&path))
            .unwrap_or_else(Self::unavailable)
    }

    fn unavailable() -> Self {
        Self {
            unavailable: true,
            ..Self::default()
        }
    }

    pub(crate) fn load_from_path(path: &Path) -> Self {
        let content = match read_history(path) {
            Ok(Some(bytes)) => match String::from_utf8(bytes) {
                Ok(text) => text,
                Err(_) => return Self::unavailable(),
            },
            Ok(None) => return Self::default(),
            Err(_) => return Self::unavailable(),
        };
        let entries: Option<Vec<_>> = content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(parse_entry_line)
            .collect();
        match entries {
            Some(entries) if entries.len() <= MAX_ENTRIES => Self {
                entries,
                ..Self::default()
            },
            _ => Self::unavailable(),
        }
    }

    pub(crate) fn should_show(
        &self,
        host_id: &str,
        finding: &HealthFinding,
        metrics: &BTreeMap<String, f64>,
        now_ms: u64,
    ) -> SuppressionDecision {
        if self.unavailable {
            return SuppressionDecision::Suppress;
        }
        let fingerprint = finding_fingerprint(finding);
        let key = session_key(host_id, &fingerprint);
        if self.session_seen.contains(&key) {
            return SuppressionDecision::Suppress;
        }
        let Some(entry) = self
            .entries
            .iter()
            .rev()
            .find(|entry| entry.host_id == host_id && entry.finding_fingerprint == fingerprint)
        else {
            return SuppressionDecision::Show;
        };
        if finding.severity.precedence() > entry.severity.precedence() {
            return SuppressionDecision::Show;
        }
        if metrics_worsened(&entry.metrics, metrics) {
            return SuppressionDecision::Show;
        }
        if now_ms.saturating_sub(entry.shown_at_ms) < ttl_ms(finding.severity) {
            SuppressionDecision::Suppress
        } else {
            SuppressionDecision::Show
        }
    }

    pub(crate) fn record_shown(
        &mut self,
        host_id: impl Into<String>,
        finding: &HealthFinding,
        metrics: BTreeMap<String, f64>,
        now_ms: u64,
    ) {
        let host_id = host_id.into();
        let fingerprint = finding_fingerprint(finding);
        self.session_seen
            .insert(session_key(&host_id, &fingerprint));
        self.entries.push(HealthSuppressionEntry {
            host_id,
            finding_fingerprint: fingerprint,
            severity: finding.severity,
            shown_at_ms: now_ms,
            metrics,
        });
        self.prune(now_ms);
    }

    pub(crate) fn write_to_path(&self, path: &Path) -> Result<(), String> {
        let write = || -> Result<(), private_state::Error> {
            if self.unavailable {
                return Err(private_state::Error::UnsafePath);
            }
            let parent = path.parent().ok_or(private_state::Error::UnsafePath)?;
            private_state::ensure_directory(parent)?;
            if let Ok(file) = open_legacy_history(path) {
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            private_state::atomic_write(
                path,
                serialize_entries(&self.entries).as_bytes(),
                MAX_BYTES,
            )
        };
        write().map_err(|error| format!("write health suppression store failed: {error:?}"))
    }

    pub(crate) fn record_and_persist(
        &mut self,
        path: &Path,
        host_id: impl Into<String>,
        finding: &HealthFinding,
        metrics: BTreeMap<String, f64>,
        now_ms: u64,
    ) -> Result<(), String> {
        self.record_shown(host_id, finding, metrics, now_ms);
        self.write_to_path(path)
    }

    fn prune(&mut self, now_ms: u64) {
        self.entries
            .retain(|entry| now_ms.saturating_sub(entry.shown_at_ms) <= RETAIN_WINDOW_MS);
        if self.entries.len() > MAX_ENTRIES {
            let drop_count = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(0..drop_count);
        }
    }
}

fn open_legacy_history(path: &Path) -> Result<std::fs::File, private_state::Error> {
    let parent = path.parent().ok_or(private_state::Error::UnsafePath)?;
    private_state::validate_directory(parent)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != unsafe { nix::libc::geteuid() }
        || metadata.mode() & 0o777 != 0o644
    {
        return Err(private_state::Error::UnsafePath);
    }
    Ok(file)
}

fn read_history(path: &Path) -> Result<Option<Vec<u8>>, private_state::Error> {
    match private_state::read_bounded(path, MAX_BYTES) {
        Err(private_state::Error::UnsafePath) => {
            let file = open_legacy_history(path)?;
            if file.metadata()?.len() > MAX_BYTES as u64 {
                return Err(private_state::Error::TooLarge);
            }
            let mut bytes = Vec::new();
            file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
            if bytes.len() > MAX_BYTES {
                return Err(private_state::Error::TooLarge);
            }
            Ok(Some(bytes))
        }
        result => result,
    }
}

pub(crate) fn health_suppression_store_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("COSH_SHELL_HEALTH_SUPPRESSION_STORE") {
        return Some(PathBuf::from(path));
    }
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .map(|home| health_suppression_store_path_in_dir(&home.join(".copilot-shell/cosh")))
}

pub(crate) fn health_suppression_store_path_in_dir(cosh_dir: &Path) -> PathBuf {
    cosh_dir.join("health-suppression")
}

pub(crate) fn host_id_for_report(report: &HealthScanReport, machine_id: Option<&str>) -> String {
    let host = report.host.as_deref().unwrap_or("unknown-host");
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    host.hash(&mut hasher);
    machine_id.unwrap_or("").hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

pub(crate) fn health_machine_id() -> Option<String> {
    ["/etc/machine-id", "/var/lib/dbus/machine-id"]
        .into_iter()
        .find_map(|path| {
            let value = std::fs::read_to_string(path).ok()?;
            let value = value.trim();
            if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            }
        })
}

pub(crate) fn metrics_for_report(report: &HealthScanReport) -> BTreeMap<String, f64> {
    report
        .facts
        .iter()
        .filter_map(|fact| numeric_fact_value(&fact.value).map(|value| (fact.key.clone(), value)))
        .collect()
}

pub(crate) fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

fn ttl_ms(severity: HealthSeverity) -> u64 {
    match severity {
        HealthSeverity::Critical => CRITICAL_TTL_MS,
        HealthSeverity::Warning | HealthSeverity::Degraded | HealthSeverity::Unavailable => {
            WARNING_TTL_MS
        }
        HealthSeverity::Ok => 0,
    }
}

fn finding_fingerprint(finding: &HealthFinding) -> String {
    let mut fingerprint = format!("{}:{:?}", finding.id, finding.category);
    for key in ["mount", "service", "process"] {
        if let Some(value) = finding.detail_args.get(key) {
            fingerprint.push(':');
            fingerprint.push_str(key);
            fingerprint.push('=');
            fingerprint.push_str(value);
        }
    }
    fingerprint
}

fn session_key(host_id: &str, fingerprint: &str) -> String {
    format!("{host_id}\t{fingerprint}")
}

fn metrics_worsened(previous: &BTreeMap<String, f64>, current: &BTreeMap<String, f64>) -> bool {
    for (key, current_value) in current {
        let Some(previous_value) = previous.get(key) else {
            continue;
        };
        if key.contains("available") && current_value <= &(previous_value * 0.5) {
            return true;
        }
        if key.contains("available") {
            continue;
        }
        if (key.ends_with("_ratio") || key.contains("used_ratio"))
            && current_value - previous_value >= 0.10
        {
            return true;
        }
        if key == "kernel.oom_latest_age_seconds"
            && *current_value <= 300.0
            && *previous_value > 300.0
        {
            return true;
        }
    }
    false
}

fn serialize_entries(entries: &[HealthSuppressionEntry]) -> String {
    let mut content = String::from(
        "# cosh-shell health suppression; format: host<TAB>fingerprint<TAB>severity<TAB>shown_at_ms<TAB>metrics\n",
    );
    for entry in entries {
        if valid_field(&entry.host_id) && valid_field(&entry.finding_fingerprint) {
            content.push_str(&entry.host_id);
            content.push('\t');
            content.push_str(&entry.finding_fingerprint);
            content.push('\t');
            content.push_str(entry.severity.label());
            content.push('\t');
            content.push_str(&entry.shown_at_ms.to_string());
            content.push('\t');
            content.push_str(&serialize_metrics(&entry.metrics));
            content.push('\n');
        }
    }
    content
}

fn parse_entry_line(line: &str) -> Option<HealthSuppressionEntry> {
    let mut parts = line.split('\t');
    let host_id = parts.next()?.to_string();
    let finding_fingerprint = parts.next()?.to_string();
    let severity = HealthSeverity::parse(parts.next()?)?;
    let shown_at_ms = parts.next()?.parse::<u64>().ok()?;
    if !valid_field(&host_id) || !valid_field(&finding_fingerprint) {
        return None;
    }
    let metrics = parse_metrics(parts.next().unwrap_or(""))?;
    if parts.next().is_some() {
        return None;
    }
    Some(HealthSuppressionEntry {
        host_id,
        finding_fingerprint,
        severity,
        shown_at_ms,
        metrics,
    })
}

fn serialize_metrics(metrics: &BTreeMap<String, f64>) -> String {
    serde_json::Value::Object(
        metrics
            .iter()
            .filter(|(key, value)| valid_field(key) && value.is_finite())
            .map(|(key, value)| (key.clone(), serde_json::Value::from(*value)))
            .collect(),
    )
    .to_string()
}

fn parse_metrics(value: &str) -> Option<BTreeMap<String, f64>> {
    if value.is_empty() {
        return Some(BTreeMap::new());
    }
    if value.starts_with('{') {
        let metrics: BTreeMap<String, f64> = serde_json::from_str(value).ok()?;
        return metrics
            .iter()
            .all(|(key, value)| valid_field(key) && value.is_finite())
            .then_some(metrics);
    }
    value
        .split(',')
        .map(|item| {
            let (key, value) = item.split_once('=')?;
            let value = value.parse::<f64>().ok()?;
            (valid_field(key) && value.is_finite()).then(|| (key.to_string(), value))
        })
        .collect()
}

fn numeric_fact_value(value: &HealthFactValue) -> Option<f64> {
    match value {
        HealthFactValue::Integer(value) => Some(*value as f64),
        HealthFactValue::Unsigned(value) => Some(*value as f64),
        HealthFactValue::Float(value) => Some(*value),
        _ => None,
    }
}

fn valid_field(value: &str) -> bool {
    !value.is_empty()
        && !value
            .chars()
            .any(|ch| matches!(ch, '\n' | '\r' | '\t' | '\0'))
}

#[cfg(test)]
mod tests;
