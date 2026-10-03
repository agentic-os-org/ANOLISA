use cosh_gateway_contracts::profile::GatewayCapabilityProfileId;

use super::*;

/// Every profile an instance may configure, with the provider set it seals.
fn profiles() -> [(GatewayCapabilityProfile, &'static [CapabilityProviderId]); 2] {
    [
        (GatewayCapabilityProfile::task_only_v1(), &[]),
        (
            GatewayCapabilityProfile::workspace_checkpoint_v1(),
            &[CapabilityProviderId::WsCkpt],
        ),
    ]
}

#[test]
fn task_only_instance_admits_an_empty_provider_set() {
    let registry =
        SealedCapabilityProviderRegistry::task_only(GatewayCapabilityProfile::task_only_v1())
            .expect("a Task-only instance needs no provider");

    assert_eq!(registry.profile(), GatewayCapabilityProfile::task_only_v1());
    assert_eq!(registry.providers(), []);
}

#[test]
fn task_only_instance_starts_without_any_ws_ckpt_configuration() {
    // Nothing about ws-ckpt is consulted here, which is exactly the portable
    // Task-only deployment: no socket, no directory, no daemon.
    let registry =
        SealedCapabilityProviderRegistry::admit(GatewayCapabilityProfile::task_only_v1(), &[])
            .expect("a Task-only instance starts with no checkpoint dependency");

    assert_eq!(registry.providers(), []);
}

#[test]
fn task_only_instance_rejects_a_requested_checkpoint_provider() {
    let error = SealedCapabilityProviderRegistry::admit(
        GatewayCapabilityProfile::task_only_v1(),
        &[CapabilityProviderId::WsCkpt],
    )
    .expect_err("an installed provider is not authority for a Task-only instance");

    assert_eq!(
        error,
        SealedProviderAdmissionError::ProviderSet(
            CapabilityProfileVerificationError::ProviderSetMismatch
        )
    );
}

#[test]
fn checkpoint_profile_admits_only_its_identity_only_provider() {
    let registry = SealedCapabilityProviderRegistry::admit(
        GatewayCapabilityProfile::workspace_checkpoint_v1(),
        &[CapabilityProviderId::WsCkpt],
    )
    .expect("C0 guarded requests make checkpoint admission identity-only");

    assert_eq!(
        registry.profile().id(),
        GatewayCapabilityProfileId::WorkspaceCheckpointV1
    );
    assert_eq!(registry.providers(), [CapabilityProviderId::WsCkpt]);
}

#[test]
fn checkpoint_instance_rejects_a_missing_provider() {
    let error = SealedCapabilityProviderRegistry::admit(
        GatewayCapabilityProfile::workspace_checkpoint_v1(),
        &[],
    )
    .expect_err("an unavailable provider must refuse admission");

    assert_eq!(
        error,
        SealedProviderAdmissionError::ProviderSet(
            CapabilityProfileVerificationError::ProviderSetMismatch
        )
    );
}

#[test]
fn every_profile_admits_exactly_its_sealed_provider_set() {
    for (profile, sealed) in profiles() {
        for requested in [&[][..], &[CapabilityProviderId::WsCkpt][..]] {
            match SealedCapabilityProviderRegistry::admit(profile, requested) {
                Ok(registry) => {
                    assert_eq!(requested, sealed);
                    assert_eq!(registry.providers(), sealed);
                }
                Err(SealedProviderAdmissionError::ProviderSet(_)) => {
                    assert_ne!(requested, sealed);
                }
            }
        }
    }
}
