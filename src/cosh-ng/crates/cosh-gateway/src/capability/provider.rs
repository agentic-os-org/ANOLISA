//! Sealed side-effect provider registry admitted by trusted configuration.
//!
//! The registry is the single place where an admitted capability profile is
//! matched against the real execution targets an instance owns, and therefore the
//! only boundary at which an instance gains side-effect authority.
//!
//! Concrete targets remain owned by the composition root. This registry seals
//! the exact provider inventory before any target configuration is opened.

use cosh_gateway_contracts::profile::{
    CapabilityProfileVerificationError, CapabilityProviderId, GatewayCapabilityProfile,
};
use thiserror::Error;

/// Fail-closed failure raised before an instance owns any side-effect authority.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SealedProviderAdmissionError {
    /// Configuration admitted a provider set the profile does not seal.
    #[error(transparent)]
    ProviderSet(#[from] CapabilityProfileVerificationError),
}

/// Complete set of side-effect providers one Gateway instance may reach.
pub struct SealedCapabilityProviderRegistry {
    profile: GatewayCapabilityProfile,
    providers: Vec<CapabilityProviderId>,
}

impl SealedCapabilityProviderRegistry {
    /// Admits the exact provider set sealed into one capability profile.
    ///
    /// `requested` is the provider set trusted per-instance configuration asks
    /// for. A Task-only instance that requests a provider is rejected instead of
    /// widened: an installed ws-ckpt daemon is never authority on its own.
    ///
    /// # Errors
    ///
    /// Returns a provider-set mismatch when the requested set differs from the set
    /// the profile seals.
    pub fn admit(
        profile: GatewayCapabilityProfile,
        requested: &[CapabilityProviderId],
    ) -> Result<Self, SealedProviderAdmissionError> {
        // Provider admission is exact; host availability never widens a profile.
        profile.verify_providers(requested)?;
        Ok(Self {
            profile,
            providers: requested.to_vec(),
        })
    }

    /// Admits the empty provider set required by a portable Task-only instance.
    ///
    /// # Errors
    ///
    /// Returns a provider-set mismatch when the profile seals a provider.
    pub fn task_only(
        profile: GatewayCapabilityProfile,
    ) -> Result<Self, SealedProviderAdmissionError> {
        Self::admit(profile, &[])
    }

    /// Returns the canonical admitted capability profile.
    #[must_use]
    pub const fn profile(&self) -> GatewayCapabilityProfile {
        self.profile
    }

    /// Returns the exact ordered provider set admitted for this instance.
    #[must_use]
    pub fn providers(&self) -> &[CapabilityProviderId] {
        &self.providers
    }
}

impl std::fmt::Debug for SealedCapabilityProviderRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SealedCapabilityProviderRegistry")
            .field("profile", &self.profile.id())
            .field("providers", &self.providers)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "provider/tests.rs"]
mod tests;
