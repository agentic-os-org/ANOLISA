//! Episode-backed health claim admission helpers used under the guide lock.

use std::path::PathBuf;

use crate::diagnostics::health::suppression::episode::{self, CandidateEpisode, EpisodeLedger};

pub(super) struct EpisodeClaim {
    path: PathBuf,
    ledger: EpisodeLedger,
    candidate: CandidateEpisode,
}

/// Ok(None) means no episode candidate; Err means the claim must be suppressed
/// (missing ledger path, unreadable ledger or unmet cooldown/recovery gate).
pub(super) fn prepare(
    candidate: Option<&CandidateEpisode>,
    now_ms: u64,
) -> Result<Option<EpisodeClaim>, ()> {
    let Some(candidate) = candidate else {
        return Ok(None);
    };
    let path = episode::ledger_path().ok_or(())?;
    let ledger = episode::load_or_default(&path).ok_or(())?;
    if !episode::deliverable(&ledger, candidate, now_ms) {
        return Err(());
    }
    Ok(Some(EpisodeClaim {
        path,
        ledger,
        candidate: candidate.clone(),
    }))
}

/// Record the delivery and persist; failure suppresses the claim fail-closed.
pub(super) fn commit(mut claim: EpisodeClaim, now_ms: u64) -> bool {
    episode::record_delivery(&mut claim.ledger, &claim.candidate, now_ms);
    if episode::write(&claim.path, &claim.ledger).is_ok() {
        return true;
    }
    // A rename that already landed leaves the new ledger readable even when
    // the parent-directory sync fails afterwards. Keeping the claim then
    // matches what the next load reads; rolling it back would spend the
    // cooldown of a card that was shown.
    episode::load(&claim.path).is_some_and(|landed| landed == claim.ledger)
}
