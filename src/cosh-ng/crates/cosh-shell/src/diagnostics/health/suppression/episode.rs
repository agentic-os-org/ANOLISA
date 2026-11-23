//! v3.7 episode sub-ledgers: volume state recovery/cooldown and OOM event
//! identity/counter baselines. State and events stay independent; unknown
//! never recovers, aging never replays.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::private_state;

mod candidate;
mod signals;
pub(crate) use candidate::candidate_for_finding;
pub(crate) use signals::{counter_sample, signals_from_report};

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

/// Branch-aware R05 combination episode: max/high enter independently and the
/// combination closes only when every entered branch has validly exited with
/// no unknown branch left, or when same-group PSI proves dual-window recovery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct QuotaEpisode {
    pub severity: u8,
    pub last_delivered_ms: u64,
    pub max_entered: bool,
    pub high_entered: bool,
    pub max_exited: bool,
    pub high_exited: bool,
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
    pub quota: BTreeMap<String, QuotaEpisode>,
    #[serde(default)]
    pub recoveries: BTreeMap<String, VolumeRecovery>,
    #[serde(default)]
    pub counter: Option<CounterBaseline>,
    #[serde(default)]
    pub pids_counter: Option<CounterBaseline>,
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
    pub quota: Vec<QuotaComboSignal>,
    pub counter: Option<CounterSample>,
    pub pids_counter: Option<CounterSample>,
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
pub(crate) struct QuotaBranchSignal {
    pub entry: bool,
    pub exit: bool,
    pub valid: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct QuotaComboSignal {
    pub key: String,
    pub max: QuotaBranchSignal,
    pub high: QuotaBranchSignal,
    pub psi_valid: bool,
    pub psi_recovery: bool,
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
    pub quota_branches: Option<(bool, bool)>,
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
            + ledger.quota.len()
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

pub(crate) fn volume_member_key(mount: &str, member: &str, device: &str, fsid: &str) -> String {
    format!("{mount}|{member}|{device}|{fsid}")
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

/// E12 pids.events:max increment proof; diagnostics-only until non-probe
/// attribution is proven, so no delivery state is attached.
pub(crate) fn peek_pids_increment(
    ledger: &EpisodeLedger,
    sample: &CounterSample,
) -> Option<(u64, u64, u64)> {
    let baseline = ledger.pids_counter.as_ref()?;
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
    for combo in &signal.quota {
        let Some(episode) = ledger.quota.get_mut(&combo.key) else {
            continue;
        };
        apply_quota_branch(
            &mut episode.max_entered,
            &mut episode.max_exited,
            &combo.max,
        );
        apply_quota_branch(
            &mut episode.high_entered,
            &mut episode.high_exited,
            &combo.high,
        );
        let entered_all_exited = (!episode.max_entered || episode.max_exited)
            && (!episode.high_entered || episode.high_exited);
        let branches_known = combo.max.valid && combo.high.valid;
        let closed =
            (combo.psi_valid && combo.psi_recovery) || (entered_all_exited && branches_known);
        if closed {
            if let Some(episode) = ledger.quota.remove(&combo.key) {
                ledger.recoveries.insert(
                    combo.key.clone(),
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
    if let Some(sample) = &signal.pids_counter {
        ledger.pids_counter = Some(CounterBaseline {
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

fn apply_quota_branch(entered: &mut bool, exited: &mut bool, signal: &QuotaBranchSignal) {
    if !signal.valid {
        return;
    }
    if signal.entry {
        *entered = true;
        *exited = false;
    } else if signal.exit && *entered {
        *exited = true;
    }
}

fn prune(ledger: &mut EpisodeLedger, now_ms: u64) {
    ledger
        .volume
        .retain(|_, episode| now_ms.saturating_sub(episode.last_delivered_ms) < VOLUME_RETAIN_MS);
    ledger
        .quota
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
    if candidate.quota_branches.is_some() {
        if let Some(episode) = ledger.quota.get(&candidate.key) {
            if candidate.severity > episode.severity {
                return true;
            }
            return now_ms.saturating_sub(episode.last_delivered_ms) >= STATE_COOLDOWN_MS;
        }
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
        if let Some((max_entry, high_entry)) = candidate.quota_branches {
            let episode = ledger
                .quota
                .entry(candidate.key.clone())
                .or_insert(QuotaEpisode {
                    severity: candidate.severity,
                    last_delivered_ms: now_ms,
                    max_entered: max_entry,
                    high_entered: high_entry,
                    max_exited: false,
                    high_exited: false,
                });
            episode.severity = candidate.severity;
            episode.last_delivered_ms = now_ms;
        } else {
            ledger.volume.insert(
                candidate.key.clone(),
                VolumeEpisode {
                    severity: candidate.severity,
                    last_delivered_ms: now_ms,
                },
            );
        }
    }
    prune(ledger, now_ms);
}

#[cfg(test)]
mod tests;
