//! Capacity-card delivery admission: which members a card reports, and how
//! their live episodes, recoveries and siblings gate a re-post.

use super::{CandidateEpisode, EpisodeLedger, VolumeRecovery};
use super::{EVENT_MERGE_MS, RECURRENCE_GATE_MS, STATE_COOLDOWN_MS};

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
        if now_ms.saturating_sub(episode.last_delivered_ms) >= STATE_COOLDOWN_MS {
            return true;
        }
        // Admission covers every member the card lists: the key's active
        // episode must not hide a companion member that has neither its own
        // live delivery nor a recovery gate.
        return candidate
            .companion_keys
            .iter()
            .any(|key| !member_gated(ledger, key, now_ms));
    }
    if candidate.quota_branches.is_some() {
        if let Some(episode) = ledger.quota.get(&candidate.key) {
            if candidate.severity > episode.severity {
                return true;
            }
            return now_ms.saturating_sub(episode.last_delivered_ms) >= STATE_COOLDOWN_MS;
        }
    }
    // This member's own recovery is the documented recurrence boundary, so it
    // takes precedence over a sibling's delivery record. Companions still get
    // the same admission as a live card: a first-time member the card lists
    // must not wait behind this member's recovery gate.
    if let Some(recovery) = ledger.recoveries.get(&candidate.key) {
        let cooldown = if card_still_alerting(ledger, recovery, &candidate.key) {
            STATE_COOLDOWN_MS
        } else {
            RECURRENCE_GATE_MS
        };
        if now_ms.saturating_sub(recovery.last_delivered_ms) >= cooldown {
            return true;
        }
        return candidate
            .companion_keys
            .iter()
            .any(|key| !member_gated(ledger, key, now_ms));
    }
    // A sibling's recovery gates this card only when the earlier delivery
    // provably reported this member; a byte-only card's recovery must not
    // hide a first inode exhaustion.
    for sibling in &candidate.sibling_keys {
        if let Some(recovery) = ledger.recoveries.get(sibling) {
            if recovery
                .members
                .iter()
                .any(|member| member == &candidate.key)
                && now_ms.saturating_sub(recovery.last_delivered_ms) < STATE_COOLDOWN_MS
            {
                return false;
            }
        }
    }
    true
}

/// Whether another member of the card this recovery belongs to is still
/// alerting, i.e. the card never fully recovered. A member re-entering after
/// its own recovery supersedes the record and starts a new card, which falls
/// back to the shorter recurrence gate; a severity re-post of a live episode
/// is not a re-entry and keeps the state cooldown.
fn card_still_alerting(ledger: &EpisodeLedger, recovery: &VolumeRecovery, key: &str) -> bool {
    !recovery.superseded
        && recovery
            .members
            .iter()
            .any(|member| member != key && ledger.volume.contains_key(member))
}

/// Whether this capacity member's own delivery or recovery still gates a card
/// within the state cooldown.
fn member_gated(ledger: &EpisodeLedger, key: &str, now_ms: u64) -> bool {
    let live = ledger.volume.get(key).is_some_and(|episode| {
        now_ms.saturating_sub(episode.last_delivered_ms) < STATE_COOLDOWN_MS
    });
    let recovered = ledger.recoveries.get(key).is_some_and(|recovery| {
        !recovery.superseded
            && now_ms.saturating_sub(recovery.last_delivered_ms) < STATE_COOLDOWN_MS
    });
    live || recovered
}
