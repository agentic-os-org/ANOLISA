//! Tracks whether login effects may already have started during shell setup.

use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoginEffectSource {
    PathBootstrapProbe,
    R2CapabilityProbe,
    ManagedShell,
}

impl LoginEffectSource {
    fn mask(self) -> u8 {
        match self {
            Self::PathBootstrapProbe => 1,
            Self::R2CapabilityProbe => 2,
            Self::ManagedShell => 4,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LoginEffectGuard {
    bits: Arc<AtomicU8>,
}

impl LoginEffectGuard {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn mark_possible(&self, source: LoginEffectSource) {
        self.bits.fetch_or(source.mask(), Ordering::Relaxed);
    }

    pub(crate) fn may_have_started(&self) -> bool {
        self.bits.load(Ordering::Relaxed) != 0
    }

    pub(crate) fn has(&self, source: LoginEffectSource) -> bool {
        self.bits.load(Ordering::Relaxed) & source.mask() != 0
    }
}

#[cfg(test)]
mod tests {
    use super::{LoginEffectGuard, LoginEffectSource};

    #[test]
    fn source_masks_are_one_hot_and_independent() {
        let sources = [
            LoginEffectSource::PathBootstrapProbe,
            LoginEffectSource::R2CapabilityProbe,
            LoginEffectSource::ManagedShell,
        ];
        let masks = sources.map(LoginEffectSource::mask);

        assert_eq!(masks, [1, 2, 4]);
        for (index, mask) in masks.iter().enumerate() {
            assert_eq!(mask.count_ones(), 1, "source {index} must use one bit");
            for other in &masks[index + 1..] {
                assert_eq!(mask & other, 0, "source masks must not overlap");
            }
        }
    }

    #[test]
    fn each_login_effect_source_disarms_fallback() {
        for source in [
            LoginEffectSource::PathBootstrapProbe,
            LoginEffectSource::R2CapabilityProbe,
            LoginEffectSource::ManagedShell,
        ] {
            let guard = LoginEffectGuard::new();
            assert!(!guard.may_have_started());
            guard.mark_possible(source);
            assert!(guard.may_have_started());
            assert!(guard.has(source));
        }
    }

    #[test]
    fn effects_are_monotonic_and_composable() {
        let guard = LoginEffectGuard::new();
        guard.mark_possible(LoginEffectSource::PathBootstrapProbe);
        guard.mark_possible(LoginEffectSource::ManagedShell);
        assert!(guard.has(LoginEffectSource::PathBootstrapProbe));
        assert!(guard.has(LoginEffectSource::ManagedShell));
    }
}
