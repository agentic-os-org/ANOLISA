use asc_policy_types::Validate;
use asc_policy_types::binding::{BindingStatus, BindingView, PreparedBinding};
use asc_policy_types::identifiers::Revision;
use asc_policy_types::policy::{PolicyEnvelope, PreparedPolicy};
use asc_policy_types::scope::PreparedScope;

const COMPLETE_BINDING: &str = include_str!("fixtures/prepared-binding.json");

fn prepared_binding() -> PreparedBinding {
    serde_json::from_str(COMPLETE_BINDING).expect("complete Binding fixture must deserialize")
}

#[test]
fn complete_binding_round_trips_and_validates_as_one_boundary_document() {
    let expected: serde_json::Value = serde_json::from_str(COMPLETE_BINDING).unwrap();
    let binding = prepared_binding();

    binding.validate().unwrap();
    assert_eq!(binding.binding_revision.get(), 7);
    assert_eq!(binding.policy.revision.get(), 1);
    assert_eq!(binding.scope.revision.get(), 3);
    assert_eq!(serde_json::to_value(binding).unwrap(), expected);
}

#[test]
fn scope_requires_an_explicit_supported_selector_including_inside_bindings() {
    let complete: serde_json::Value = serde_json::from_str(COMPLETE_BINDING).unwrap();
    for selector in [
        None,
        Some(serde_json::Value::Null),
        Some(serde_json::json!({
            "kind": "legacy_execution_domain",
            "executionDomainId": "legacy-domain"
        })),
    ] {
        let mut binding = complete.clone();
        if let Some(selector) = selector {
            binding["scope"]["selector"] = selector;
        } else {
            binding["scope"].as_object_mut().unwrap().remove("selector");
        }
        assert!(serde_json::from_value::<PreparedScope>(binding["scope"].clone()).is_err());
        assert!(serde_json::from_value::<PreparedBinding>(binding).is_err());
    }
}

#[test]
fn scope_contains_only_identity_revision_and_selector_and_rejects_removed_fields() {
    let complete: serde_json::Value = serde_json::from_str(COMPLETE_BINDING).unwrap();
    for selector in [
        serde_json::json!({"kind": "pid", "pid": 4242}),
        serde_json::json!({"kind": "cgroup_id", "cgroupId": 99}),
    ] {
        let expected = serde_json::json!({
            "scopeId": complete["scope"]["scopeId"],
            "revision": complete["scope"]["revision"],
            "selector": selector,
        });
        let scope: PreparedScope = serde_json::from_value(expected.clone()).unwrap();
        scope.validate().unwrap();
        assert_eq!(serde_json::to_value(scope).unwrap(), expected);
    }
    for (key, value) in [
        (
            "template",
            serde_json::json!({"kind":"execution_domain", "lifetime":{"expiresAt":"2030-01-01T00:00:00Z"}}),
        ),
        (
            "templateDigest",
            serde_json::json!(
                "sha256:0000000000000000000000000000000000000000000000000000000000000000"
            ),
        ),
    ] {
        let mut binding = complete.clone();
        binding["scope"][key] = value;
        assert!(serde_json::from_value::<PreparedScope>(binding["scope"].clone()).is_err());
        assert!(serde_json::from_value::<PreparedBinding>(binding).is_err());
    }
}

#[test]
fn policy_round_trips_without_template_digest_and_rejects_the_removed_field() {
    let complete: serde_json::Value = serde_json::from_str(COMPLETE_BINDING).unwrap();
    let policy: PreparedPolicy = serde_json::from_value(complete["policy"].clone()).unwrap();
    policy.validate().unwrap();
    let encoded = serde_json::to_value(policy).unwrap();
    assert_eq!(encoded, complete["policy"]);
    assert!(encoded.get("templateDigest").is_none());
    assert_eq!(encoded.as_object().unwrap().len(), 5);

    for digest in [
        serde_json::json!(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000"
        ),
        serde_json::Value::Null,
    ] {
        let mut legacy = complete.clone();
        legacy["policy"]["templateDigest"] = digest;
        assert!(serde_json::from_value::<PreparedPolicy>(legacy["policy"].clone()).is_err());
        assert!(serde_json::from_value::<PreparedBinding>(legacy).is_err());
    }
}

#[test]
fn binding_validation_rejects_inconsistent_embedded_policy_identity() {
    let mut binding = prepared_binding();
    binding.policy.canonical_policy.revision = Revision::new(2).unwrap();

    let error = binding.validate().unwrap_err();
    assert_eq!(error.path, "policy.canonicalPolicy.revision");
}

#[test]
fn binding_validation_addresses_invalid_scope_fields() {
    let mut binding: serde_json::Value = serde_json::from_str(COMPLETE_BINDING).unwrap();
    binding["scope"]["selector"]["pid"] = serde_json::json!(0);
    let binding: PreparedBinding = serde_json::from_value(binding).unwrap();

    let error = binding.validate().unwrap_err();
    assert_eq!(error.path, "scope.selector.pid");
}

#[test]
fn binding_view_exposes_status_without_duplicate_spec_identity() {
    let spec = prepared_binding();
    let view = BindingView {
        status: BindingStatus::PendingApply.into(),
        spec,
    };

    view.validate().unwrap();
    let wire = serde_json::to_value(&view).unwrap();
    assert!(wire.get("lifecycle").is_none());
    assert_eq!(wire["status"], serde_json::json!({"phase":"PENDING_APPLY"}));
    assert!(wire["spec"].get("desiredState").is_none());
    assert_eq!(serde_json::from_value::<BindingView>(wire).unwrap(), view);
}

#[test]
fn binding_lifecycle_separates_new_requests_from_worker_transitions() {
    let spec = prepared_binding();
    let pending_apply = BindingStatus::PendingApply;
    assert_eq!(spec.binding_revision.get(), 7);
    assert!(pending_apply.complete_reconcile().is_err());

    let applying = pending_apply.start_reconcile().unwrap();
    pending_apply.validate_successor(applying).unwrap();
    assert_eq!(applying, BindingStatus::Applying);

    let retry_apply = applying.retry_reconcile().unwrap();
    applying.validate_successor(retry_apply).unwrap();
    assert_eq!(retry_apply, BindingStatus::PendingApply);
    let applying = retry_apply.start_reconcile().unwrap();
    let apply_failed = applying.fail_reconcile().unwrap();
    assert_eq!(apply_failed, BindingStatus::ApplyFailed);

    let retried_apply = apply_failed.request_apply().unwrap();
    assert_eq!(retried_apply, BindingStatus::PendingApply);
    assert!(apply_failed.validate_successor(retried_apply).is_err());
    let applying = retried_apply.start_reconcile().unwrap();
    let ready = applying.complete_reconcile().unwrap();
    assert_eq!(ready, BindingStatus::Ready);
    assert_eq!(
        ready.request_apply().unwrap(),
        ready,
        "an identical PUT after successful Apply is idempotent"
    );

    let pending_delete = ready.request_delete();
    assert_eq!(pending_delete, BindingStatus::PendingDelete);
    assert!(ready.validate_successor(pending_delete).is_err());
    let deleting = pending_delete.start_reconcile().unwrap();
    assert_eq!(deleting, BindingStatus::Deleting);
    assert!(deleting.request_apply().is_err());
    assert_eq!(applying.request_delete(), BindingStatus::PendingDelete);
    assert!(BindingStatus::PendingDelete.request_apply().is_err());
    assert!(BindingStatus::DeleteFailed.request_apply().is_err());
    assert!(BindingStatus::Deleted.request_apply().is_err());

    let retry_delete = deleting.retry_reconcile().unwrap();
    assert_eq!(retry_delete, BindingStatus::PendingDelete);
    let deleting = retry_delete.start_reconcile().unwrap();
    let delete_failed = deleting.fail_reconcile().unwrap();
    assert_eq!(delete_failed, BindingStatus::DeleteFailed);

    let retried_delete = delete_failed.request_delete();
    assert_eq!(retried_delete, BindingStatus::PendingDelete);
    assert!(delete_failed.validate_successor(retried_delete).is_err());
    let deleted = retried_delete
        .start_reconcile()
        .unwrap()
        .complete_reconcile()
        .unwrap();
    assert_eq!(deleted, BindingStatus::Deleted);
    assert_eq!(deleted.request_delete(), deleted);
}

#[test]
fn delete_admission_is_total_and_preserves_existing_delete_intent() {
    use BindingStatus::{
        ApplyFailed, Applying, DeleteFailed, Deleted, Deleting, PendingApply, PendingDelete, Ready,
    };
    for state in [PendingApply, Applying, Ready, ApplyFailed, DeleteFailed] {
        assert_eq!(state.request_delete(), PendingDelete);
    }
    for state in [PendingDelete, Deleting, Deleted] {
        assert_eq!(state.request_delete(), state);
    }
}

#[test]
fn removed_legacy_fields_and_unknown_fields_are_rejected() {
    let complete: serde_json::Value = serde_json::from_str(COMPLETE_BINDING).unwrap();
    for retired in [
        serde_json::json!(false),
        serde_json::json!(true),
        serde_json::Value::Null,
    ] {
        let mut policy = complete.clone();
        policy["policy"]["retired"] = retired.clone();
        assert!(serde_json::from_value::<PreparedPolicy>(policy["policy"].clone()).is_err());
        assert!(serde_json::from_value::<PreparedBinding>(policy).is_err());

        let mut scope = complete.clone();
        scope["scope"]["retired"] = retired;
        assert!(serde_json::from_value::<PreparedScope>(scope["scope"].clone()).is_err());
        assert!(serde_json::from_value::<PreparedBinding>(scope).is_err());
    }

    for legacy_id in [serde_json::json!("legacy-domain"), serde_json::Value::Null] {
        let mut legacy: serde_json::Value = serde_json::from_str(COMPLETE_BINDING).unwrap();
        legacy["executionDomainId"] = legacy_id;
        assert!(serde_json::from_value::<PreparedBinding>(legacy).is_err());
    }

    let mut unknown: serde_json::Value = serde_json::from_str(COMPLETE_BINDING).unwrap();
    unknown["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<PreparedBinding>(unknown).is_err());

    let mut lifecycle_inside_spec: serde_json::Value =
        serde_json::from_str(COMPLETE_BINDING).unwrap();
    lifecycle_inside_spec["desiredState"] = serde_json::json!("READY");
    assert!(serde_json::from_value::<PreparedBinding>(lifecycle_inside_spec).is_err());
}

#[test]
fn canonical_policy_rejects_removed_payload_digest_at_every_embedding_boundary() {
    let complete: serde_json::Value = serde_json::from_str(COMPLETE_BINDING).unwrap();
    let envelope: PolicyEnvelope =
        serde_json::from_value(complete["policy"]["canonicalPolicy"].clone()).unwrap();
    envelope.validate().unwrap();
    assert_eq!(
        serde_json::to_value(envelope).unwrap(),
        complete["policy"]["canonicalPolicy"]
    );
    assert!(
        complete["policy"]["canonicalPolicy"]
            .get("payloadDigest")
            .is_none()
    );
    for digest in [
        serde_json::json!(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000"
        ),
        serde_json::Value::Null,
    ] {
        let mut legacy = complete.clone();
        legacy["policy"]["canonicalPolicy"]["payloadDigest"] = digest;
        assert!(
            serde_json::from_value::<PolicyEnvelope>(legacy["policy"]["canonicalPolicy"].clone())
                .is_err()
        );
        assert!(serde_json::from_value::<PreparedPolicy>(legacy["policy"].clone()).is_err());
        assert!(serde_json::from_value::<PreparedBinding>(legacy).is_err());
    }
}

#[test]
fn scheduling_rejection_allows_only_corresponding_pending_failure() {
    use BindingStatus::{ApplyFailed, DeleteFailed, PendingApply, PendingDelete};
    PendingApply.validate_successor(ApplyFailed).unwrap();
    PendingDelete.validate_successor(DeleteFailed).unwrap();
    assert!(PendingApply.validate_successor(DeleteFailed).is_err());
    assert!(PendingDelete.validate_successor(ApplyFailed).is_err());
}
