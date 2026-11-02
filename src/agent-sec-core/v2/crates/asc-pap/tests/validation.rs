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
            "@id(\"unsupported\") forbid(principal, action == SecCore::Action::\"file.write\", resource);",
            1,
            "annotations",
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
