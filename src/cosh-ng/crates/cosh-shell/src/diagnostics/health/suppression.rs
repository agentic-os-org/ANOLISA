use std::collections::{BTreeMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::model::{
    HealthFactValue, HealthFinding, HealthFindingCategory, HealthScanReport, HealthSeverity,
};

const MAX_ENTRIES: usize = 128;
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
}

impl HealthSuppressionStore {
    pub(crate) fn load_default() -> Self {
        health_suppression_store_path()
            .map(|path| Self::load_from_path(&path))
            .unwrap_or_default()
    }

    pub(crate) fn load_from_path(path: &Path) -> Self {
        let Ok(content) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let entries = content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(parse_entry_line)
            .collect();
        Self {
            entries,
            session_seen: HashSet::new(),
        }
    }

    pub(crate) fn should_show(
        &self,
        host_id: &str,
        finding: &HealthFinding,
        metrics: &BTreeMap<String, f64>,
        now_ms: u64,
    ) -> SuppressionDecision {
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
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                format!("create health suppression store directory failed: {err}")
            })?;
        }
        std::fs::write(path, serialize_entries(&self.entries))
            .map_err(|err| format!("write health suppression store failed: {err}"))
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
    Some(HealthSuppressionEntry {
        host_id,
        finding_fingerprint,
        severity,
        shown_at_ms,
        metrics: parts.next().map(parse_metrics).unwrap_or_default(),
    })
}

fn serialize_metrics(metrics: &BTreeMap<String, f64>) -> String {
    metrics
        .iter()
        .filter(|(key, value)| valid_field(key) && value.is_finite())
        .map(|(key, value)| format!("{key}={value:.6}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn parse_metrics(value: &str) -> BTreeMap<String, f64> {
    value
        .split(',')
        .filter_map(|item| {
            let (key, value) = item.split_once('=')?;
            if !valid_field(key) {
                return None;
            }
            Some((key.to_string(), value.parse::<f64>().ok()?))
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
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::diagnostics::health::model::{HealthFindingCategory, HealthMessageId};

    #[test]
    fn suppresses_same_session_and_recent_warning() {
        let mut store = HealthSuppressionStore::default();
        let finding = finding("J06", HealthSeverity::Warning);
        let metrics = metrics(&[("memory.available_ratio", 0.08)]);

        assert_eq!(
            store.should_show("host", &finding, &metrics, 1000),
            SuppressionDecision::Show
        );
        store.record_shown("host", &finding, metrics.clone(), 1000);

        assert_eq!(
            store.should_show("host", &finding, &metrics, 2000),
            SuppressionDecision::Suppress
        );
        let reloaded = HealthSuppressionStore {
            entries: store.entries.clone(),
            session_seen: HashSet::new(),
        };
        assert_eq!(
            reloaded.should_show("host", &finding, &metrics, 2000),
            SuppressionDecision::Suppress
        );
    }

    #[test]
    fn severity_upgrade_and_metric_worsening_bypass_ttl() {
        let mut store = HealthSuppressionStore::default();
        let warning = finding("J06", HealthSeverity::Warning);
        let critical = finding("J06", HealthSeverity::Critical);
        store.record_shown(
            "host",
            &warning,
            metrics(&[
                ("memory.available_mib", 1200.0),
                ("memory.available_ratio", 0.12),
                ("memory.used_ratio", 0.50),
            ]),
            1000,
        );
        let reloaded = HealthSuppressionStore {
            entries: store.entries.clone(),
            session_seen: HashSet::new(),
        };

        assert_eq!(
            reloaded.should_show(
                "host",
                &critical,
                &metrics(&[
                    ("memory.available_mib", 1200.0),
                    ("memory.available_ratio", 0.12)
                ]),
                2000,
            ),
            SuppressionDecision::Show
        );
        assert_eq!(
            reloaded.should_show(
                "host",
                &warning,
                &metrics(&[
                    ("memory.available_mib", 500.0),
                    ("memory.available_ratio", 0.12)
                ]),
                2000,
            ),
            SuppressionDecision::Show
        );
        assert_eq!(
            reloaded.should_show(
                "host",
                &warning,
                &metrics(&[
                    ("memory.available_mib", 1200.0),
                    ("memory.available_ratio", 0.22)
                ]),
                2000,
            ),
            SuppressionDecision::Suppress
        );
        assert_eq!(
            reloaded.should_show(
                "host",
                &warning,
                &metrics(&[
                    ("memory.available_mib", 1200.0),
                    ("memory.available_ratio", 0.04)
                ]),
                2000,
            ),
            SuppressionDecision::Show
        );
        assert_eq!(
            reloaded.should_show(
                "host",
                &warning,
                &metrics(&[
                    ("memory.available_mib", 1200.0),
                    ("memory.available_ratio", 0.12),
                    ("memory.used_ratio", 0.62)
                ]),
                2000,
            ),
            SuppressionDecision::Show
        );
    }

    #[test]
    fn host_id_includes_machine_id_when_available() {
        let mut report = HealthScanReport::new("health", 0);
        report.host = Some("same-hostname".to_string());

        let without_machine = host_id_for_report(&report, None);
        let first_machine = host_id_for_report(&report, Some("machine-a"));
        let second_machine = host_id_for_report(&report, Some("machine-b"));

        assert_ne!(without_machine, first_machine);
        assert_ne!(first_machine, second_machine);
    }

    #[test]
    fn expired_warning_shows_again() {
        let mut store = HealthSuppressionStore::default();
        let finding = finding("J10", HealthSeverity::Warning);
        let metrics = metrics(&[("filesystem.max_used_ratio", 0.90)]);
        store.record_shown("host", &finding, metrics.clone(), 1000);
        let reloaded = HealthSuppressionStore {
            entries: store.entries.clone(),
            session_seen: HashSet::new(),
        };

        assert_eq!(
            reloaded.should_show("host", &finding, &metrics, 1000 + WARNING_TTL_MS + 1),
            SuppressionDecision::Show
        );
    }

    #[test]
    fn corrupted_store_lines_are_ignored_and_write_errors_degrade() {
        let path = temp_path("corrupted");
        std::fs::write(&path, "bad\nmissing\tparts\n").expect("write corrupted");

        let store = HealthSuppressionStore::load_from_path(&path);
        assert!(store.entries.is_empty());

        let dir_path = temp_path("dir-target");
        std::fs::create_dir_all(&dir_path).expect("create dir");
        let err = store
            .write_to_path(&dir_path)
            .expect_err("write to dir fails");
        assert!(err.contains("write health suppression store failed"));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&dir_path);
    }

    #[test]
    fn persists_only_bounded_metadata() {
        let path = temp_path("persist");
        let mut store = HealthSuppressionStore::default();
        let finding = finding("J11", HealthSeverity::Critical);
        store
            .record_and_persist(
                &path,
                "host",
                &finding,
                metrics(&[("kernel.oom_latest_age_seconds", 120.0)]),
                1000,
            )
            .expect("persist");

        let content = std::fs::read_to_string(&path).expect("read store");
        assert!(content.contains("kernel.oom_latest_age_seconds"));
        assert!(!content.contains("journalctl"));
        assert!(!content.contains("/tmp/"));
        let loaded = HealthSuppressionStore::load_from_path(&path);
        assert_eq!(loaded.entries.len(), 1);

        let _ = std::fs::remove_file(path);
    }

    fn finding(id: &str, severity: HealthSeverity) -> HealthFinding {
        HealthFinding {
            id: id.to_string(),
            severity,
            category: HealthFindingCategory::Anomaly,
            title_id: HealthMessageId::HealthFindingMemoryAvailableLow,
            detail_id: None,
            detail_args: BTreeMap::new(),
            evidence_fact_ids: Vec::new(),
            suggested_try_ids: Vec::new(),
        }
    }

    fn metrics(items: &[(&str, f64)]) -> BTreeMap<String, f64> {
        items
            .iter()
            .map(|(key, value)| ((*key).to_string(), *value))
            .collect()
    }

    fn temp_path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("cosh-health-suppression-{label}-{nanos}"))
    }
}
