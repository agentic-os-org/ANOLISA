use sha2::{Digest as _, Sha256};

use super::*;

#[test]
fn task_only_manifest_and_digest_are_pinned() {
    let profile = GatewayCapabilityProfile::task_only_v1();
    let actual_digest = format!("{:x}", Sha256::digest(profile.canonical_manifest()));

    assert_eq!(profile.id().as_str(), TASK_ONLY_V1_PROFILE);
    assert!(profile
        .canonical_manifest()
        .starts_with(CAPABILITY_PROFILE_MANIFEST_DOMAIN));
    assert_eq!(
        profile.canonical_manifest(),
        TASK_ONLY_V1_CANONICAL_MANIFEST
    );
    assert_eq!(
        profile.manifest_digest().as_str(),
        TASK_ONLY_V1_MANIFEST_DIGEST
    );
    assert_eq!(actual_digest, TASK_ONLY_V1_MANIFEST_DIGEST);
    assert_eq!(profile.runtime_tools(), [ASK_USER_QUESTION_TOOL]);
    assert_eq!(profile.verify_identity(&profile.identity()), Ok(()));
    let target = profile.governed_target();
    assert_eq!(target.kind.as_str(), "workspace");
    assert_eq!(target.authority.as_str(), "cosh");
    assert_eq!(target.identifier.as_str(), TASK_ONLY_V1_PROFILE);
    assert!(profile.canonical_manifest().contains(&format!(
        "target:\n{}/{}/{}\n",
        target.kind.as_str(),
        target.authority.as_str(),
        target.identifier.as_str(),
    )));
}

#[test]
fn workspace_checkpoint_manifest_and_digest_are_pinned() {
    let profile = GatewayCapabilityProfile::workspace_checkpoint_v1();
    let actual_digest = format!("{:x}", Sha256::digest(profile.canonical_manifest()));

    assert_eq!(profile.id().as_str(), WORKSPACE_CHECKPOINT_V1_PROFILE);
    assert!(profile
        .canonical_manifest()
        .starts_with(CAPABILITY_PROFILE_MANIFEST_DOMAIN));
    assert_eq!(
        profile.canonical_manifest(),
        WORKSPACE_CHECKPOINT_V1_CANONICAL_MANIFEST
    );
    assert_eq!(
        profile.manifest_digest().as_str(),
        WORKSPACE_CHECKPOINT_V1_MANIFEST_DIGEST
    );
    assert_eq!(actual_digest, WORKSPACE_CHECKPOINT_V1_MANIFEST_DIGEST);
    assert_eq!(
        profile.runtime_tools(),
        [ASK_USER_QUESTION_TOOL, WORKSPACE_CHECKPOINT_CREATE_TOOL]
    );
    assert_eq!(profile.providers(), [CapabilityProviderId::WsCkpt]);
    assert_eq!(profile.verify_identity(&profile.identity()), Ok(()));
    let target = profile.governed_target();
    assert_eq!(target.kind.as_str(), "workspace");
    assert_eq!(target.authority.as_str(), "cosh");
    assert_eq!(target.identifier.as_str(), WORKSPACE_CHECKPOINT_V1_PROFILE);
    assert!(profile.canonical_manifest().contains(&format!(
        "target:\n{}/{}/{}\n",
        target.kind.as_str(),
        target.authority.as_str(),
        target.identifier.as_str(),
    )));
    assert!(profile
        .canonical_manifest()
        .ends_with(&format!("providers:\n{WS_CKPT_PROVIDER}\n")));
}

#[test]
fn workspace_write_manifest_and_digest_are_pinned() {
    let profile = GatewayCapabilityProfile::workspace_write_v1();
    let actual_digest = format!("{:x}", Sha256::digest(profile.canonical_manifest()));

    assert_eq!(profile.id().as_str(), WORKSPACE_WRITE_V1_PROFILE);
    assert_eq!(
        profile.canonical_manifest(),
        WORKSPACE_WRITE_V1_CANONICAL_MANIFEST
    );
    assert_eq!(actual_digest, WORKSPACE_WRITE_V1_MANIFEST_DIGEST);
    assert_eq!(
        profile.runtime_tools(),
        [ASK_USER_QUESTION_TOOL, WRITE_FILE_TOOL]
    );
    assert!(profile.providers().is_empty());
    assert_eq!(profile.verify_identity(&profile.identity()), Ok(()));
    assert_eq!(
        profile.governed_target().identifier.as_str(),
        WORKSPACE_WRITE_V1_PROFILE
    );
}

#[test]
fn delegated_acp_manifest_and_digest_are_pinned() {
    let profile = GatewayCapabilityProfile::delegated_acp_v1();
    let actual_digest = format!("{:x}", Sha256::digest(profile.canonical_manifest()));

    assert_eq!(profile.id().as_str(), DELEGATED_ACP_V1_PROFILE);
    assert_eq!(
        profile.canonical_manifest(),
        DELEGATED_ACP_V1_CANONICAL_MANIFEST
    );
    assert_eq!(
        profile.manifest_digest().as_str(),
        DELEGATED_ACP_V1_MANIFEST_DIGEST
    );
    assert_eq!(actual_digest, DELEGATED_ACP_V1_MANIFEST_DIGEST);
    assert!(profile.runtime_tools().is_empty());
    assert!(profile.providers().is_empty());
    assert!(profile.delegates_provider_native());
    assert_eq!(profile.verify_identity(&profile.identity()), Ok(()));
    assert!(profile
        .canonical_manifest()
        .ends_with("delegation:\nprovider-native-allow-once\n"));
}

#[test]
fn optional_profile_never_alters_the_task_only_contract() {
    let task_only = GatewayCapabilityProfile::task_only_v1();
    let checkpoint = GatewayCapabilityProfile::workspace_checkpoint_v1();

    // The Task-only manifest is byte-identical to its original revision, so
    // the private Core v3 wire mirror keeps verifying the pinned digest.
    assert_eq!(
        task_only.manifest_digest().as_str(),
        TASK_ONLY_V1_MANIFEST_DIGEST
    );
    assert!(!task_only.canonical_manifest().contains("providers:"));
    assert_eq!(task_only.providers(), []);
    assert_eq!(task_only.verify_providers(&[]), Ok(()));
    assert_ne!(task_only.governed_target(), checkpoint.governed_target());
    assert_ne!(task_only.manifest_digest(), checkpoint.manifest_digest());
    assert_eq!(
        task_only.verify_identity(&checkpoint.identity()),
        Err(CapabilityProfileVerificationError::ProfileMismatch)
    );
    assert_eq!(
        checkpoint.verify_identity(&task_only.identity()),
        Err(CapabilityProfileVerificationError::ProfileMismatch)
    );
}

#[test]
fn provider_sets_are_exact_and_never_widened_by_installation() {
    let task_only = GatewayCapabilityProfile::task_only_v1();
    let checkpoint = GatewayCapabilityProfile::workspace_checkpoint_v1();

    // A host that happens to run ws-ckpt is not authority for a Task-only
    // instance; the empty sealed set rejects the installed provider.
    assert_eq!(
        task_only.verify_providers(&[CapabilityProviderId::WsCkpt]),
        Err(CapabilityProfileVerificationError::ProviderSetMismatch)
    );
    assert_eq!(
        checkpoint.verify_providers(&[CapabilityProviderId::WsCkpt]),
        Ok(())
    );
    assert_eq!(
        checkpoint.verify_providers(&[]),
        Err(CapabilityProfileVerificationError::ProviderSetMismatch)
    );
    assert_eq!(
        checkpoint.verify_providers(&[CapabilityProviderId::WsCkpt, CapabilityProviderId::WsCkpt]),
        Err(CapabilityProfileVerificationError::ProviderSetMismatch)
    );
}

#[test]
fn provider_names_are_exact_and_fail_closed() {
    assert_eq!(
        CapabilityProviderId::parse(WS_CKPT_PROVIDER),
        Ok(CapabilityProviderId::WsCkpt)
    );
    assert_eq!(CapabilityProviderId::WsCkpt.as_str(), "ws-ckpt");
    for unknown in ["", "ws_ckpt", "WS-CKPT", "ws-ckpt-v1", "shell"] {
        assert_eq!(
            CapabilityProviderId::parse(unknown),
            Err(CapabilityProviderParseError)
        );
    }
    assert_eq!(
        serde_json::to_value(CapabilityProviderId::WsCkpt).expect("provider ID serializes"),
        serde_json::json!(WS_CKPT_PROVIDER)
    );
}

#[test]
fn profile_names_are_exact_and_fail_closed() {
    assert_eq!(
        GatewayCapabilityProfileId::parse(TASK_ONLY_V1_PROFILE),
        Ok(GatewayCapabilityProfileId::TaskOnlyV1)
    );
    assert_eq!(
        GatewayCapabilityProfileId::TaskOnlyV1.profile(),
        GatewayCapabilityProfile::task_only_v1()
    );
    assert_eq!(
        GatewayCapabilityProfileId::parse(WORKSPACE_CHECKPOINT_V1_PROFILE),
        Ok(GatewayCapabilityProfileId::WorkspaceCheckpointV1)
    );
    assert_eq!(
        GatewayCapabilityProfileId::WorkspaceCheckpointV1.profile(),
        GatewayCapabilityProfile::workspace_checkpoint_v1()
    );
    assert_eq!(
        GatewayCapabilityProfileId::parse(WORKSPACE_WRITE_V1_PROFILE),
        Ok(GatewayCapabilityProfileId::WorkspaceWriteV1)
    );
    assert_eq!(
        GatewayCapabilityProfileId::WorkspaceWriteV1.profile(),
        GatewayCapabilityProfile::workspace_write_v1()
    );
    assert_eq!(
        GatewayCapabilityProfileId::parse(DELEGATED_ACP_V1_PROFILE),
        Ok(GatewayCapabilityProfileId::DelegatedAcpV1)
    );
    assert_eq!(
        GatewayCapabilityProfileId::DelegatedAcpV1.profile(),
        GatewayCapabilityProfile::delegated_acp_v1()
    );
    for unknown in [
        "",
        "task-only",
        "task-only-v2",
        "TASK-ONLY-V1",
        // The contract rejected this provider-shaped name; it must not be
        // revived as an alias for the capability-shaped profile name.
        "ws-ckpt-v1",
        "ws-ckpt",
        "workspace-checkpoint",
        "workspace-checkpoint-v2",
        "WORKSPACE-CHECKPOINT-V1",
        "workspace-write",
        "workspace-write-v2",
        "WORKSPACE-WRITE-V1",
        "delegated-acp",
        "delegated-acp-v2",
        "DELEGATED-ACP-V1",
    ] {
        assert_eq!(
            GatewayCapabilityProfileId::parse(unknown),
            Err(CapabilityProfileParseError)
        );
    }
}

#[test]
fn profile_identity_rejects_digest_drift() {
    let profile = GatewayCapabilityProfile::task_only_v1();
    let drifted = GatewayCapabilityProfileIdentity {
        profile_id: GatewayCapabilityProfileId::TaskOnlyV1,
        manifest_digest: Digest::parse("0".repeat(64)).expect("test digest is canonical"),
    };

    assert_eq!(
        profile.verify_identity(&drifted),
        Err(CapabilityProfileVerificationError::ManifestDigestMismatch)
    );
}

#[test]
fn runtime_inventory_rejects_missing_or_additional_tools() {
    let profile = GatewayCapabilityProfile::task_only_v1();

    assert_eq!(
        profile.verify_runtime_tools(&[ASK_USER_QUESTION_TOOL]),
        Ok(())
    );
    for drifted in [
        &[][..],
        &[ASK_USER_QUESTION_TOOL, WORKSPACE_CHECKPOINT_CREATE_TOOL][..],
        &["ask_user"][..],
    ] {
        assert_eq!(
            profile.verify_runtime_tools(drifted),
            Err(CapabilityProfileVerificationError::RuntimeToolInventoryMismatch)
        );
    }
}

#[test]
fn checkpoint_inventory_rejects_missing_extra_and_reordered_tools() {
    let profile = GatewayCapabilityProfile::workspace_checkpoint_v1();

    assert_eq!(
        profile.verify_runtime_tools(&[ASK_USER_QUESTION_TOOL, WORKSPACE_CHECKPOINT_CREATE_TOOL]),
        Ok(())
    );
    for drifted in [
        &[][..],
        &[ASK_USER_QUESTION_TOOL][..],
        &[WORKSPACE_CHECKPOINT_CREATE_TOOL][..],
        &[WORKSPACE_CHECKPOINT_CREATE_TOOL, ASK_USER_QUESTION_TOOL][..],
        &[
            ASK_USER_QUESTION_TOOL,
            WORKSPACE_CHECKPOINT_CREATE_TOOL,
            "workspace_checkpoint_rollback",
        ][..],
        &[ASK_USER_QUESTION_TOOL, "workspace_checkpoint"][..],
    ] {
        assert_eq!(
            profile.verify_runtime_tools(drifted),
            Err(CapabilityProfileVerificationError::RuntimeToolInventoryMismatch)
        );
    }
}

#[test]
fn workspace_write_inventory_rejects_missing_extra_and_reordered_tools() {
    let profile = GatewayCapabilityProfile::workspace_write_v1();

    assert_eq!(
        profile.verify_runtime_tools(&[ASK_USER_QUESTION_TOOL, WRITE_FILE_TOOL]),
        Ok(())
    );
    for drifted in [
        &[][..],
        &[ASK_USER_QUESTION_TOOL][..],
        &[WRITE_FILE_TOOL, ASK_USER_QUESTION_TOOL][..],
        &[ASK_USER_QUESTION_TOOL, WRITE_FILE_TOOL, "edit"][..],
        &[ASK_USER_QUESTION_TOOL, "write"][..],
    ] {
        assert_eq!(
            profile.verify_runtime_tools(drifted),
            Err(CapabilityProfileVerificationError::RuntimeToolInventoryMismatch)
        );
    }
}

#[test]
fn profile_identity_uses_canonical_wire_names() {
    for (profile, name, digest) in [
        (
            GatewayCapabilityProfile::task_only_v1(),
            TASK_ONLY_V1_PROFILE,
            TASK_ONLY_V1_MANIFEST_DIGEST,
        ),
        (
            GatewayCapabilityProfile::workspace_checkpoint_v1(),
            WORKSPACE_CHECKPOINT_V1_PROFILE,
            WORKSPACE_CHECKPOINT_V1_MANIFEST_DIGEST,
        ),
        (
            GatewayCapabilityProfile::workspace_write_v1(),
            WORKSPACE_WRITE_V1_PROFILE,
            WORKSPACE_WRITE_V1_MANIFEST_DIGEST,
        ),
        (
            GatewayCapabilityProfile::delegated_acp_v1(),
            DELEGATED_ACP_V1_PROFILE,
            DELEGATED_ACP_V1_MANIFEST_DIGEST,
        ),
    ] {
        let identity = profile.identity();
        let encoded = serde_json::to_value(&identity).expect("profile identity serializes");

        assert_eq!(encoded["profile_id"], name);
        assert_eq!(encoded["manifest_digest"], digest);
        assert_eq!(
            serde_json::from_value::<GatewayCapabilityProfileIdentity>(encoded)
                .expect("profile identity deserializes"),
            identity
        );
    }
}
