//! Classifies whether a login-capable probe may have started its target.

use super::BootstrapPathProbeError;
use crate::shell_host::{LoginEffectGuard, LoginEffectSource};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProbeStartOutcome {
    ProvenNotStarted,
    MayHaveStarted,
}

pub(super) fn record_probe_outcome<T>(
    effects: &LoginEffectGuard,
    source: LoginEffectSource,
    outcome: &Result<T, BootstrapPathProbeError>,
) -> ProbeStartOutcome {
    if matches!(outcome, Err(BootstrapPathProbeError::Spawn(_))) {
        return ProbeStartOutcome::ProvenNotStarted;
    }
    effects.mark_possible(source);
    ProbeStartOutcome::MayHaveStarted
}
