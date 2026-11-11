#[cfg(test)]
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/policy.rs"
));

use std::sync::Arc;

use asc_pap::{PapError, PapRepository, PapService};
use asc_policy_repository_sqlite::SqlitePolicyRepository;
use asc_policy_types::Validate;
use asc_policy_types::authoring::{Category, Effect};
use asc_policy_types::binding::PreparedBinding;
use asc_policy_types::error::ValidationError;
use asc_policy_types::scope::ScopeSelector;

fn native_template(
    source: &mut asc_policy_types::authoring::PolicySource,
) -> &mut asc_policy_types::authoring::PolicyTemplate {
    let asc_policy_types::authoring::PolicySource::SecCore(template) = source else {
        panic!("expected native test policy");
    };
    template
}

fn binding() -> PreparedBinding {
    serde_json::from_str(include_str!(
        "../../asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap()
}

#[test]
fn shared_name_validation_preserves_pap_and_snapshot_errors() {
    let repository = Arc::new(SqlitePolicyRepository::temporary().unwrap());
    let pap = PapService::new(repository.clone());
    for (name, reason) in [
        (String::new(), "must contain a visible character"),
        (" \t\n".into(), "must contain a visible character"),
        (" ".repeat(257), "must contain a visible character"),
        ("a".repeat(257), "must not exceed 256 bytes"),
        ("é".repeat(129), "must not exceed 256 bytes"),
        (
            format!("{}\n", "a".repeat(256)),
            "must not exceed 256 bytes",
        ),
        ("visible\n".into(), "must not contain control characters"),
    ] {
        let mut policy = binding().policy;
        policy.policy_name.clone_from(&name);
        assert_eq!(
            pap.create_policy(&name, &policy.template),
            Err(PapError::InvalidPolicyName(reason.into()))
        );
        assert_eq!(
            policy.validate(),
            Err(ValidationError::new(
                "policyName",
                "must contain a visible, control-free value of at most 256 bytes"
            ))
        );
    }
    assert_eq!(repository.list_policies(100, 0).unwrap().total, 0);
}

#[test]
fn validated_construction_still_produces_valid_policy_and_scope_snapshots() {
    let repository = Arc::new(SqlitePolicyRepository::temporary().unwrap());
    let pap = PapService::new(repository);
    for name in ["a".repeat(256), "é".repeat(128), " visible name ".into()] {
        let policy = pap
            .create_policy(&name, &binding().policy.template)
            .unwrap();
        assert_eq!(policy.policy_name, name);
        policy.validate().unwrap();
    }
    let mut scope = asc_policy_types::scope::PreparedScope {
        scope_id: binding().scope.scope_id,
        selector: ScopeSelector::Pid { pid: 1 },
        policy_snapshots: vec![binding().policy],
        owner_uid: None,
        status: asc_policy_types::scope::ScopeStatus::Active,
    };
    scope.validate().unwrap();
    scope.policy_snapshots.clear();
    assert!(scope.validate().is_err());
    for (selector, path) in [
        (ScopeSelector::Pid { pid: 0 }, "pid"),
        (ScopeSelector::CgroupId { cgroup_id: 0 }, "cgroupId"),
    ] {
        assert_eq!(
            pap.create_scope_assignment(&selector, &[], None),
            Err(PapError::InvalidScope(ValidationError::new(
                path,
                "must be positive"
            )))
        );
    }
}

#[test]
fn invalid_template_rejects_create_and_update_without_writing() {
    let repository = Arc::new(SqlitePolicyRepository::temporary().unwrap());
    let pap = PapService::new(repository.clone());
    let saved = pap
        .create_policy("policy", &binding().policy.template)
        .unwrap();
    let mut bad_version = binding().policy.template;
    native_template(&mut bad_version).spec_version = "99".into();
    let mut bad_category = binding().policy.template;
    native_template(&mut bad_category).rules[0].category = Category::Network;
    for (template, path) in [
        (file_policy(vec![]), "template.rules"),
        (
            file_policy(vec!["relative".into()]),
            "template.rules[0].target.path",
        ),
        (bad_version, "template.specVersion"),
        (bad_category, "template.rules[0]"),
    ] {
        for result in [
            pap.create_policy("invalid", &template),
            pap.update_policy(&saved.policy_id, "invalid", &template),
        ] {
            let Err(PapError::InvalidPolicy(error)) = result else {
                panic!("invalid template was admitted");
            };
            assert_eq!(error.path, path);
        }
        assert_eq!(
            repository.list_policies(100, 0).unwrap().items,
            vec![saved.clone()]
        );
    }
}

#[test]
fn valid_backend_unsupported_decisions_are_saved_as_reusable_policies() {
    let repository = Arc::new(SqlitePolicyRepository::temporary().unwrap());
    let pap = PapService::new(repository.clone());
    for effect in [Effect::Allow, Effect::RequireConfirmation] {
        let mut template = binding().policy.template;
        native_template(&mut template).rules[0].effect = effect;
        let saved = pap.create_policy("general policy", &template).unwrap();
        assert_eq!(saved.template, template);
        assert_eq!(
            repository
                .get_policy(&saved.policy_id, saved.revision)
                .unwrap(),
            saved
        );
    }
}

#[test]
fn cedar_admission_rejects_invalid_creates_and_updates_without_allocating_revisions() {
    use asc_policy_types::authoring::PolicySource;

    let repository = Arc::new(SqlitePolicyRepository::temporary().unwrap());
    let pap = PapService::new(repository.clone());
    let source: PolicySource = serde_json::from_value(serde_json::json!({
        "language":"cedar", "content":{
            "schema":{"schemaId":"seccore.system","revision":1},
            "text":"forbid(principal, action == SecCore::Action::\"file.write\", resource);"
        }
    }))
    .unwrap();
    let saved = pap.create_policy("cedar", &source).unwrap();
    assert_eq!(
        pap.update_policy(&saved.policy_id, "cedar", &source)
            .unwrap(),
        saved
    );
    for (text, revision, expected) in [
        ("not a Cedar policy", 1, "syntax"),
        ("forbid(principal, action, resource);", 2, "revision 2"),
        (
            "forbid(principal, action == SecCore::Action::\"file.unknown\", resource);",
            1,
            "action",
        ),
        (
            "@unknown(\"unsupported\") forbid(principal, action == SecCore::Action::\"file.write\", resource);",
            1,
            "unsupported annotation",
        ),
        (
            "forbid(principal, action == SecCore::Action::\"file.write\", resource) when { context.input.operation == \"delete\" };",
            1,
            "policy0",
        ),
    ] {
        let mut invalid = source.clone();
        let PolicySource::Cedar(cedar) = &mut invalid else {
            unreachable!()
        };
        cedar.text = text.into();
        cedar.schema.revision = asc_foundation_types::Revision::new(revision).unwrap();
        for result in [
            pap.create_policy("invalid", &invalid),
            pap.update_policy(&saved.policy_id, "invalid", &invalid),
        ] {
            let Err(PapError::InvalidCedar(diagnostic)) = result else {
                panic!("invalid source admitted: {text}");
            };
            assert!(diagnostic.contains(expected), "{diagnostic}");
        }
        assert_eq!(
            pap.list_policies(100, 0).unwrap().items,
            vec![saved.clone()]
        );
        let state = repository
            .get_policy_revision_state(&saved.policy_id)
            .unwrap()
            .unwrap();
        assert_eq!(state.last_allocated_revision, saved.revision);
    }
    let mut changed = source;
    let PolicySource::Cedar(cedar) = &mut changed else {
        unreachable!()
    };
    cedar.text.push_str("\n// A new authored revision.");
    assert_eq!(
        pap.update_policy(&saved.policy_id, "cedar", &changed)
            .unwrap()
            .revision
            .get(),
        2
    );
}

fn runtime_cedar(text: &str) -> asc_policy_types::authoring::PolicySource {
    serde_json::from_value(serde_json::json!({
        "language": "cedar", "content": {
            "schema": {"schemaId": "seccore.system", "revision": 1},
            "text": text
        }
    }))
    .unwrap()
}

#[test]
fn effective_set_tracks_membership_without_scope_assignments() {
    let repository = Arc::new(SqlitePolicyRepository::temporary().unwrap());
    let pap = PapService::new(repository);
    let empty = pap.authorization_policies().unwrap();
    pap.create_policy("native", &binding().policy.template)
        .unwrap();
    let bare = runtime_cedar(
        r#"permit(principal, action == SecCore::Action::"process.spawn", resource)
        when { principal has agent_type && principal.agent_type == "codex" };"#,
    );
    let deployment = pap.create_policy("deployment", &bare).unwrap();
    assert_eq!(pap.authorization_policies().unwrap(), empty);

    let specific = runtime_cedar(
        r#"permit(principal == SecCore::Subject::"agent-a",
        action == SecCore::Action::"process.spawn", resource);"#,
    );
    let a = pap.create_policy("subject-a", &specific).unwrap();
    let first = pap.authorization_policies().unwrap();
    assert_eq!(first.policies.len(), 1);
    assert_eq!(first.policies[0].policy_id, a.policy_id);
    assert_eq!(first.policies[0].revision, a.revision);
    let all = runtime_cedar(
        r#"forbid(principal is SecCore::Subject,
        action == SecCore::Action::"network.use", resource);"#,
    );
    let b = pap.create_policy("global restriction", &all).unwrap();
    let second = pap.authorization_policies().unwrap();
    assert_eq!(second.policies.len(), 2);
    assert_ne!(first.version, second.version);
    pap.delete_policy_revision(&b.policy_id, b.revision)
        .unwrap();
    assert_eq!(pap.authorization_policies().unwrap(), first);

    pap.update_policy(&a.policy_id, "deployment only", &bare)
        .unwrap();
    assert_eq!(pap.authorization_policies().unwrap(), empty);
    pap.update_policy(&deployment.policy_id, "now explicit", &specific)
        .unwrap();
    let enabled = pap.authorization_policies().unwrap();
    assert_eq!(enabled.policies.len(), 1);
    assert_eq!(enabled.policies[0].policy_id, deployment.policy_id);
    assert!(pap.list_scopes(100, 0).unwrap().items.is_empty());
    assert!(pap.list_bindings(100, 0).unwrap().items.is_empty());
}

#[test]
fn mixed_headers_reject_whole_policy_at_admission_and_resolution() {
    let repository = Arc::new(SqlitePolicyRepository::temporary().unwrap());
    let pap = PapService::new(repository.clone());
    let mixed = runtime_cedar(
        r#"
        permit(principal is SecCore::Subject, action == SecCore::Action::"process.spawn", resource);
        forbid(principal, action == SecCore::Action::"process.spawn", resource);
    "#,
    );
    let error = pap.create_policy("mixed", &mixed).unwrap_err();
    assert!(error.to_string().contains("mixed bare and constrained"));
    // Defensive read: repositories may already contain sources predating admission checks.
    let mut stored = binding().policy;
    stored.template = mixed;
    repository.put_policy(&stored).unwrap();
    let error = pap.authorization_policies().unwrap_err().to_string();
    assert!(error.contains(stored.policy_id.as_str()));
    assert!(error.contains("mixed bare and constrained"));
}

#[test]
fn invalid_stored_sources_or_schema_never_produce_a_partial_set() {
    for (text, revision) in [
        ("not cedar", 1),
        (
            "permit(principal is SecCore::Subject, action, resource);",
            2,
        ),
    ] {
        let repository = Arc::new(SqlitePolicyRepository::temporary().unwrap());
        let pap = PapService::new(repository.clone());
        pap.create_policy(
            "valid",
            &runtime_cedar("permit(principal is SecCore::Subject, action, resource);"),
        )
        .unwrap();
        let mut stored = binding().policy;
        stored.template = runtime_cedar(text);
        let asc_policy_types::authoring::PolicySource::Cedar(source) = &mut stored.template else {
            unreachable!()
        };
        source.schema.revision = asc_foundation_types::Revision::new(revision).unwrap();
        repository.put_policy(&stored).unwrap();
        assert!(matches!(
            pap.authorization_policies(),
            Err(PapError::InvalidCedar(_))
        ));
    }
}

#[test]
fn runtime_cedar_admission_validates_before_replacing_the_current_revision() {
    let repository = Arc::new(SqlitePolicyRepository::temporary().unwrap());
    let pap = PapService::new(repository.clone());
    let source = runtime_cedar(
        r#"@advice("{\"verdict\":\"warn\",\"message\":\"review output\"}")
permit(principal, action == SecCore::Action::"process.spawn", resource)
when { principal has agent_type && principal.agent_type == "codex" };"#,
    );
    let first = pap.create_policy("baseline", &source).unwrap();
    assert_eq!(first.template, source);
    assert_eq!(
        pap.create_policy("other", &source).unwrap().revision.get(),
        1
    );
    assert_eq!(
        pap.update_policy(&first.policy_id, "baseline", &source)
            .unwrap(),
        first
    );
    for invalid in [
        runtime_cedar("not Cedar"),
        runtime_cedar(r#"permit(principal, action == SecCore::Action::"missing", resource);"#),
        runtime_cedar(
            r#"@advice("{\"verdict\":\"unknown\"}") permit(principal, action, resource);"#,
        ),
    ] {
        assert!(matches!(
            pap.create_policy("invalid", &invalid),
            Err(PapError::InvalidCedar(_))
        ));
        assert!(matches!(
            pap.update_policy(&first.policy_id, "invalid", &invalid),
            Err(PapError::InvalidCedar(_))
        ));
        assert_eq!(
            pap.current_policies(std::slice::from_ref(&first.policy_id))
                .unwrap(),
            vec![first.clone()]
        );
    }
    let updated = pap
        .update_policy(
            &first.policy_id,
            "baseline",
            &runtime_cedar("forbid(principal, action, resource);"),
        )
        .unwrap();
    assert_eq!(updated.revision.get(), 2);
    assert_eq!(
        pap.current_policies(std::slice::from_ref(&first.policy_id))
            .unwrap(),
        vec![updated.clone()]
    );
    assert_eq!(
        pap.get_policy(&first.policy_id, first.revision),
        Err(PapError::NotFound)
    );
    pap.delete_policy_revision(&updated.policy_id, updated.revision)
        .unwrap();
    assert_eq!(
        pap.current_policies(&[first.policy_id]),
        Err(PapError::NotFound)
    );
}
