use super::{digest, health_exposure, ClaimEntry, HealthExposure, InviteKind};

pub(super) fn new_health_event(
    exposures: Option<&health_exposure::HealthExposures>,
    evidence: Option<&HealthExposure<'_>>,
    entry: &ClaimEntry,
) -> bool {
    let (Some(exposures), Some(evidence)) = (exposures, evidence) else {
        return false;
    };
    if entry.kind != InviteKind::LoginHealth {
        return false;
    }
    let Some((domain, event)) = evidence.event else {
        return false;
    };
    health_exposure::paired(exposures, entry).is_some_and(|paired| {
        paired.event.as_ref().is_some_and(|previous| {
            previous.domain == domain
                && (previous.identity != digest(&[event])
                    || evidence
                        .severity
                        .zip(paired.severity)
                        .is_some_and(|(current, old)| current > old))
        })
    })
}
