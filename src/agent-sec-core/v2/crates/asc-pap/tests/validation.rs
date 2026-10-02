use std::sync::Arc;

use asc_pap::{PapError, PapRepository, PapService, PolicyCompiler};
use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_policy_types::Validate;
use asc_policy_types::authoring::TemplateEnvelope;
use asc_policy_types::binding::PreparedBinding;
use asc_policy_types::error::ValidationError;
use asc_policy_types::identifiers::{PolicyId, Revision};
use asc_policy_types::policy::PolicyEnvelope;
use asc_policy_types::scope::ScopeSelector;

fn binding() -> PreparedBinding {
    serde_json::from_str(include_str!(
        "../../asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap()
}

struct FixtureCompiler(fn(&mut PolicyEnvelope));

impl PolicyCompiler for FixtureCompiler {
    fn lower(&self, input: &TemplateEnvelope) -> Result<PolicyEnvelope, ValidationError> {
        let mut policy = binding().policy.canonical_policy;
        policy.policy_id = input.policy_id.clone();
        policy.revision = input.revision;
        (self.0)(&mut policy);
        Ok(policy)
    }
}

#[test]
fn shared_name_validation_preserves_pap_and_snapshot_errors() {
    let repository = Arc::new(ProcessLocalPapRepository::default());
    let pap = PapService::new(
        repository.clone(),
        Arc::new(FixtureCompiler(|_| panic!("invalid name reached compiler"))),
    );
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
    let repository = Arc::new(ProcessLocalPapRepository::default());
    let pap = PapService::new(repository, Arc::new(FixtureCompiler(|_| {})));
    for name in ["a".repeat(256), "é".repeat(128), " visible name ".into()] {
        let policy = pap
            .create_policy(&name, &binding().policy.template)
            .unwrap();
        assert_eq!(policy.policy_name, name);
        policy.validate().unwrap();
    }
    for selector in [
        ScopeSelector::Pid { pid: 1 },
        ScopeSelector::CgroupId { cgroup_id: 1 },
    ] {
        pap.create_scope(&selector).unwrap().validate().unwrap();
    }
    for (selector, path) in [
        (ScopeSelector::Pid { pid: 0 }, "pid"),
        (ScopeSelector::CgroupId { cgroup_id: 0 }, "cgroupId"),
    ] {
        assert_eq!(
            pap.create_scope(&selector),
            Err(PapError::InvalidScope(ValidationError::new(
                path,
                "must be positive"
            )))
        );
    }
}

#[test]
fn compiler_output_rejection_keeps_original_error_paths_and_never_writes() {
    type Mutation = fn(&mut PolicyEnvelope);
    let cases: [(Mutation, &str, &str); 3] = [
        (
            |policy| policy.policy_id = PolicyId::new("wrong-policy").unwrap(),
            "canonicalPolicy.policyId",
            "compiler output must match the authored Policy identity",
        ),
        (
            |policy| policy.revision = Revision::new(2).unwrap(),
            "canonicalPolicy.revision",
            "compiler output must match the authored Policy revision",
        ),
        (
            |policy| policy.ir_schema_version = 999,
            "irSchemaVersion",
            "unsupported IR schema version 999",
        ),
    ];
    for (mutate, path, message) in cases {
        let repository = Arc::new(ProcessLocalPapRepository::default());
        let pap = PapService::new(repository.clone(), Arc::new(FixtureCompiler(mutate)));
        assert_eq!(
            pap.create_policy("policy", &binding().policy.template),
            Err(PapError::InvalidPolicy(ValidationError::new(path, message)))
        );
        assert_eq!(repository.list_policies(100, 0).unwrap().total, 0);
    }
}
