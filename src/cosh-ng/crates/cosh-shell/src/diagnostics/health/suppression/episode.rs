//! v3.7 episode sub-ledgers: volume state recovery/cooldown and OOM event
//! identity/counter baselines. State and events stay independent; unknown
//! never recovers, aging never replays.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::private_state;

use super::super::model::{HealthFactValue, HealthScanReport};
use super::super::rules::volume::{evaluate_volumes, MemberLevel};

const FILE: &str = "health-episodes-v1.json";
const MAX_BYTES: usize = 64 * 1024;
const MAX_ENTRIES: usize = 64;
pub(crate) const STATE_COOLDOWN_MS: u64 = 2 * 60 * 60 * 1000;
const VOLUME_RETAIN_MS: u64 = 7 * 24 * 60 * 60 * 1000;
pub(crate) const RECURRENCE_GATE_MS: u64 = 10 * 60 * 1000;
pub(crate) const EVENT_MERGE_MS: u64 = 10 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct VolumeEpisode {
    pub severity: u8,
    pub last_delivered_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct VolumeRecovery {
    pub recovered_ms: u64,
    pub last_delivered_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct CounterBaseline {
    pub boot_id: String,
    pub cgroup_id: String,
    pub source: String,
    pub count: u64,
    pub sampled_epoch: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct CounterDelivery {
    pub window_start_epoch: u64,
    pub window_end_epoch: u64,
    pub identity: String,
    pub delivered_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct EventDelivery {
    pub delivered_ms: u64,
    pub severity: u8,
    pub epoch: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ChannelState {
    pub last_delivered_ms: u64,
    pub last_severity: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub(crate) struct EpisodeLedger {
    pub version: u32,
    #[serde(default)]
    pub volume: BTreeMap<String, VolumeEpisode>,
    #[serde(default)]
    pub recoveries: BTreeMap<String, VolumeRecovery>,
    #[serde(default)]
    pub counter: Option<CounterBaseline>,
    #[serde(default)]
    pub counter_delivered: Option<CounterDelivery>,
    #[serde(default)]
    pub events: BTreeMap<String, EventDelivery>,
    #[serde(default)]
    pub channels: BTreeMap<String, ChannelState>,
}

/// Compact per-scan input computed from report facts; the claim path applies it
/// under the guide lock so delivery decisions and records stay linearizable.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EpisodeSignal {
    pub volume: Vec<VolumeMemberSignal>,
    pub counter: Option<CounterSample>,
    pub candidate: Option<CandidateEpisode>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VolumeMemberSignal {
    pub key: String,
    pub entered: bool,
    pub exit: bool,
    pub valid: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CounterSample {
    pub boot_id: String,
    pub cgroup_id: String,
    pub source: String,
    pub count: u64,
    pub sampled_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CandidateEpisode {
    pub event: bool,
    pub key: String,
    pub problem: String,
    pub severity: u8,
    pub epoch: Option<u64>,
    pub counter_window: Option<(u64, u64)>,
}

pub(crate) fn ledger_path() -> Option<PathBuf> {
    super::health_suppression_store_path().map(|store| store.with_file_name(FILE))
}

pub(crate) fn load_or_default(path: &Path) -> Option<EpisodeLedger> {
    match private_state::read_bounded(path, MAX_BYTES) {
        Ok(None) => Some(EpisodeLedger {
            version: 1,
            ..EpisodeLedger::default()
        }),
        Ok(Some(bytes)) => parse_ledger(&bytes),
        Err(_) => None,
    }
}

pub(crate) fn load(path: &Path) -> Option<EpisodeLedger> {
    let bytes = private_state::read_bounded(path, MAX_BYTES).ok()??;
    parse_ledger(&bytes)
}

fn parse_ledger(bytes: &[u8]) -> Option<EpisodeLedger> {
    let ledger: EpisodeLedger = serde_json::from_slice(bytes).ok()?;
    if ledger.version != 1
        || ledger.volume.len()
            + ledger.recoveries.len()
            + ledger.events.len()
            + ledger.channels.len()
            > MAX_ENTRIES
    {
        return None;
    }
    Some(ledger)
}

pub(crate) fn write(path: &Path, ledger: &EpisodeLedger) -> Result<(), private_state::Error> {
    let parent = path.parent().ok_or(private_state::Error::UnsafePath)?;
    private_state::ensure_directory(parent)?;
    let bytes = serde_json::to_vec(ledger)
        .map_err(|_| private_state::Error::Io(std::io::ErrorKind::InvalidData))?;
    private_state::atomic_write(path, &bytes, MAX_BYTES)
}

fn string_fact(report: &HealthScanReport, key: &str) -> Option<String> {
    report.facts.iter().find_map(|fact| {
        (fact.key == key).then(|| match &fact.value {
            HealthFactValue::String(value) => value.clone(),
            _ => String::new(),
        })
    })
}

fn unsigned_fact(report: &HealthScanReport, key: &str) -> Option<u64> {
    report.facts.iter().find_map(|fact| {
        (fact.key == key).then(|| match &fact.value {
            HealthFactValue::Unsigned(value) => Some(*value),
            HealthFactValue::Integer(value) => u64::try_from(*value).ok(),
            _ => None,
        })?
    })
}

pub(crate) fn volume_member_key(mount: &str, member: &str, device: &str, fsid: &str) -> String {
    format!("{mount}|{member}|{device}|{fsid}")
}

pub(crate) fn signals_from_report(report: &HealthScanReport) -> EpisodeSignal {
    let mut volume = Vec::new();
    for judgement in evaluate_volumes(&report.facts) {
        let device = judgement.device.clone().unwrap_or_default();
        let fsid = judgement.fsid.clone().unwrap_or_default();
        for (member, signal) in [("bytes", judgement.bytes), ("inode", judgement.inodes)] {
            volume.push(VolumeMemberSignal {
                key: volume_member_key(&judgement.mount, member, &device, &fsid),
                entered: signal.entry == Some(MemberLevel::Critical),
                exit: signal.exit,
                valid: signal.valid,
            });
        }
    }
    let counter = counter_sample(report);
    EpisodeSignal {
        volume,
        counter,
        candidate: None,
    }
}

pub(crate) fn counter_sample(report: &HealthScanReport) -> Option<CounterSample> {
    match (
        string_fact(report, "kernel.boot_id"),
        string_fact(report, "kernel.current_cgroup_id"),
        string_fact(report, "kernel.cgroup_oom_kill_source"),
        unsigned_fact(report, "kernel.cgroup_oom_kill_count"),
        unsigned_fact(report, "kernel.cgroup_oom_kill_sampled_epoch"),
    ) {
        (Some(boot_id), Some(cgroup_id), Some(source), Some(count), Some(sampled_epoch))
            if !boot_id.is_empty() && !cgroup_id.is_empty() =>
        {
            Some(CounterSample {
                boot_id,
                cgroup_id,
                source,
                count,
                sampled_epoch,
            })
        }
        _ => None,
    }
}

/// Read-only increment proof for the scan path; the baseline itself is only
/// rolled by `sync` so an undelivered window keeps its conservative span.
pub(crate) fn peek_increment(
    ledger: &EpisodeLedger,
    sample: &CounterSample,
) -> Option<(u64, u64, u64)> {
    let baseline = ledger.counter.as_ref()?;
    if baseline.boot_id != sample.boot_id
        || baseline.cgroup_id != sample.cgroup_id
        || baseline.source != sample.source
    {
        return None;
    }
    (sample.count > baseline.count && sample.sampled_epoch >= baseline.sampled_epoch).then(|| {
        (
            baseline.sampled_epoch,
            sample.sampled_epoch,
            sample.count - baseline.count,
        )
    })
}

pub(crate) fn counter_identity(sample: &CounterSample, window: (u64, u64), delta: u64) -> String {
    format!(
        "counter:{}:{}:{}:{}:{delta}",
        sample.boot_id, sample.cgroup_id, window.0, window.1
    )
}

pub(crate) fn candidate_for_finding(
    report: &HealthScanReport,
    finding_id: &str,
    severity: u8,
    problem: &str,
) -> Option<CandidateEpisode> {
    let base = finding_id.split(':').next().unwrap_or(finding_id);
    let volume_active = report
        .facts
        .iter()
        .any(|fact| fact.key.ends_with(".volume_proven"));
    if matches!(base, "J09" | "J17") && volume_active {
        let mount = finding_id
            .split_once(':')
            .map(|(_, mount)| mount.to_string())
            .unwrap_or_else(|| "/".to_string());
        let member = if base == "J17" { "inode" } else { "bytes" };
        let prefix = format!("filesystem.mount.{mount}.");
        let device = string_fact(report, &format!("{prefix}volume_device")).unwrap_or_default();
        let fsid = string_fact(report, &format!("{prefix}volume_fsid")).unwrap_or_default();
        return Some(CandidateEpisode {
            event: false,
            key: volume_member_key(&mount, member, &device, &fsid),
            problem: problem.to_string(),
            severity,
            epoch: None,
            counter_window: None,
        });
    }
    if matches!(base, "J11" | "J12")
        && unsigned_fact(report, "kernel.current_cgroup_version").is_some()
    {
        if let Some(identity) = string_fact(report, "kernel.oom_attributed_event_identity") {
            let pid = unsigned_fact(report, "kernel.oom_attributed_pid")?;
            let domain = if identity.starts_with("uptime-ns:") {
                "kernel-boottime-v1"
            } else {
                "kernel-epoch-v1"
            };
            return Some(CandidateEpisode {
                event: true,
                key: format!("{domain}|{identity}|pid:{pid}"),
                problem: problem.to_string(),
                severity,
                epoch: unsigned_fact(report, "kernel.oom_attributed_epoch"),
                counter_window: None,
            });
        }
        let sample = counter_sample(report)?;
        let window = (
            unsigned_fact(report, "kernel.cgroup_oom_kill_window_start_epoch")?,
            unsigned_fact(report, "kernel.cgroup_oom_kill_window_end_epoch")?,
        );
        let delta = unsigned_fact(report, "kernel.cgroup_oom_kill_increment")?;
        return Some(CandidateEpisode {
            event: true,
            key: format!(
                "cgroup-counter-v1|{}",
                counter_identity(&sample, window, delta)
            ),
            problem: problem.to_string(),
            severity,
            epoch: Some(window.1),
            counter_window: Some(window),
        });
    }
    None
}

/// Roll the counter baseline and persist valid volume recoveries. Returns the
/// increment window when this sample proves new kills against a comparable
/// baseline; first non-zero samples and resets stay historical.
pub(crate) fn sync(
    ledger: &mut EpisodeLedger,
    signal: &EpisodeSignal,
    now_ms: u64,
) -> Option<(u64, u64, u64)> {
    for member in &signal.volume {
        if !member.valid {
            continue;
        }
        if member.exit {
            if let Some(episode) = ledger.volume.remove(&member.key) {
                ledger.recoveries.insert(
                    member.key.clone(),
                    VolumeRecovery {
                        recovered_ms: now_ms,
                        last_delivered_ms: episode.last_delivered_ms,
                    },
                );
            }
        }
    }
    let mut increment = None;
    if let Some(sample) = &signal.counter {
        let comparable = ledger.counter.as_ref().is_some_and(|baseline| {
            baseline.boot_id == sample.boot_id
                && baseline.cgroup_id == sample.cgroup_id
                && baseline.source == sample.source
        });
        if comparable {
            let baseline = ledger.counter.as_ref().expect("comparable baseline");
            if sample.count > baseline.count && sample.sampled_epoch >= baseline.sampled_epoch {
                increment = Some((
                    baseline.sampled_epoch,
                    sample.sampled_epoch,
                    sample.count - baseline.count,
                ));
            }
        }
        ledger.counter = Some(CounterBaseline {
            boot_id: sample.boot_id.clone(),
            cgroup_id: sample.cgroup_id.clone(),
            source: sample.source.clone(),
            count: sample.count,
            sampled_epoch: sample.sampled_epoch,
        });
    }
    prune(ledger, now_ms);
    increment
}

fn prune(ledger: &mut EpisodeLedger, now_ms: u64) {
    ledger
        .volume
        .retain(|_, episode| now_ms.saturating_sub(episode.last_delivered_ms) < VOLUME_RETAIN_MS);
    ledger
        .recoveries
        .retain(|_, recovery| now_ms.saturating_sub(recovery.recovered_ms) < STATE_COOLDOWN_MS);
    ledger
        .events
        .retain(|_, delivery| now_ms.saturating_sub(delivery.delivered_ms) < STATE_COOLDOWN_MS);
    ledger
        .channels
        .retain(|_, channel| now_ms.saturating_sub(channel.last_delivered_ms) < STATE_COOLDOWN_MS);
    if let Some(delivery) = &ledger.counter_delivered {
        if now_ms.saturating_sub(delivery.delivered_ms) >= STATE_COOLDOWN_MS {
            ledger.counter_delivered = None;
        }
    }
}

pub(crate) fn deliverable(
    ledger: &EpisodeLedger,
    candidate: &CandidateEpisode,
    now_ms: u64,
) -> bool {
    if candidate.event {
        if ledger.events.contains_key(&candidate.key) {
            return false;
        }
        if let Some(delivery) = &ledger.counter_delivered {
            if let Some(epoch) = candidate.epoch {
                if candidate.counter_window.is_none()
                    && epoch >= delivery.window_start_epoch
                    && epoch <= delivery.window_end_epoch
                {
                    return false;
                }
            }
        }
        if let Some(window) = candidate.counter_window {
            let covered = ledger.events.iter().any(|(_, delivery)| {
                delivery
                    .epoch
                    .is_some_and(|epoch| epoch >= window.0 && epoch <= window.1)
            });
            if covered {
                return false;
            }
        }
        if let Some(channel) = ledger.channels.get(&candidate.problem) {
            if now_ms.saturating_sub(channel.last_delivered_ms) < EVENT_MERGE_MS {
                return candidate.severity > channel.last_severity;
            }
        }
        return true;
    }
    if let Some(episode) = ledger.volume.get(&candidate.key) {
        if candidate.severity > episode.severity {
            return true;
        }
        return now_ms.saturating_sub(episode.last_delivered_ms) >= STATE_COOLDOWN_MS;
    }
    if let Some(recovery) = ledger.recoveries.get(&candidate.key) {
        return now_ms.saturating_sub(recovery.last_delivered_ms) >= RECURRENCE_GATE_MS;
    }
    true
}

pub(crate) fn record_delivery(
    ledger: &mut EpisodeLedger,
    candidate: &CandidateEpisode,
    now_ms: u64,
) {
    if candidate.event {
        ledger.events.insert(
            candidate.key.clone(),
            EventDelivery {
                delivered_ms: now_ms,
                severity: candidate.severity,
                epoch: candidate.epoch,
            },
        );
        ledger.channels.insert(
            candidate.problem.clone(),
            ChannelState {
                last_delivered_ms: now_ms,
                last_severity: candidate.severity,
            },
        );
        if let Some(window) = candidate.counter_window {
            ledger.counter_delivered = Some(CounterDelivery {
                window_start_epoch: window.0,
                window_end_epoch: window.1,
                identity: candidate.key.clone(),
                delivered_ms: now_ms,
            });
        }
    } else {
        ledger.recoveries.remove(&candidate.key);
        ledger.volume.insert(
            candidate.key.clone(),
            VolumeEpisode {
                severity: candidate.severity,
                last_delivered_ms: now_ms,
            },
        );
    }
    prune(ledger, now_ms);
}

#[cfg(test)]
mod tests;
