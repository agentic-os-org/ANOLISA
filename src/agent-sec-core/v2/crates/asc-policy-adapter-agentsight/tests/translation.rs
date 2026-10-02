use asc_policy_adapter_agentsight::{
    ACTPLANE_POLICY_MEDIA_TYPE, AGENTSIGHT_BINDING_PLAN_FORMAT,
    AGENTSIGHT_BINDING_PLAN_SCHEMA_VERSION, AgentSightAdapter, AgentSightBindingPlan,
    AgentSightScopePlan,
};
use asc_policy_types::Validate;
use asc_policy_types::binding::PreparedBinding;
use asc_policy_types::identifiers::Revision;
use asc_policy_types::ir::SubjectRemediation;
use asc_policy_types::resource::{FileResolution, PathMatcher, ResourceSelector};
use asc_policy_types::scope::ScopeSelector;
use asc_policy_types::target::TranslationOutcome;

const COMPLETE_BINDING_FIXTURE: &str =
    include_str!("../../asc-policy-types/tests/fixtures/prepared-binding.json");
const AGENTSIGHT_BINDING_PLAN_FIXTURE: &str = include_str!(
    "../../../fixtures/adapters/agentsight/prevent-file-deletion/agentsight-binding-plan.json"
);

fn complete_binding_fixture() -> PreparedBinding {
    serde_json::from_str(COMPLETE_BINDING_FIXTURE).unwrap()
}

fn binding_with_first_path(path: PathMatcher) -> PreparedBinding {
    let mut binding = complete_binding_fixture();
    let ResourceSelector::File { matchers } =
        &mut binding.policy.canonical_policy.payload.resources[0].selector
    else {
        panic!("expected file resource set");
    };
    matchers[0].path = path;
    binding
}

#[test]
fn complete_binding_translates_to_the_frozen_agentsight_output() {
    let fixture_json: serde_json::Value = serde_json::from_str(COMPLETE_BINDING_FIXTURE).unwrap();
    let binding = complete_binding_fixture();

    binding.validate().unwrap();
    assert_eq!(serde_json::to_value(&binding).unwrap(), fixture_json);

    let outcome = AgentSightAdapter.translate(&binding).unwrap();
    let TranslationOutcome::Translated(plan) = outcome else {
        panic!("expected translated target plan");
    };

    let decoded_plan: AgentSightBindingPlan = serde_json::from_slice(&plan.content).unwrap();
    let expected_plan: serde_json::Value =
        serde_json::from_str(AGENTSIGHT_BINDING_PLAN_FIXTURE).unwrap();
    assert_eq!(serde_json::to_value(&decoded_plan).unwrap(), expected_plan);
    assert_eq!(
        decoded_plan.schema_version,
        AGENTSIGHT_BINDING_PLAN_SCHEMA_VERSION
    );
    assert_eq!(decoded_plan.policy.media_type, ACTPLANE_POLICY_MEDIA_TYPE);
    assert_eq!(
        decoded_plan.policy.content,
        concat!(
            "source AGENT = exec \"**\"\n",
            "rule agentseccore-unlink-0000:\n",
            "  block unlink file \"/etc/agent/config.yaml\" if AGENT\n",
            "  because \"AgentSecCore file deletion policy\"\n",
            "rule agentseccore-unlink-0001:\n",
            "  block unlink file \"/workspace/important/**\" if AGENT\n",
            "  because \"AgentSecCore file deletion policy\"\n",
        )
    );
    assert_eq!(
        decoded_plan.scope,
        AgentSightScopePlan::ProcessTree { root_pid: 4242 }
    );
    assert_eq!(plan.format, AGENTSIGHT_BINDING_PLAN_FORMAT);
}

#[test]
fn translation_is_deterministic_for_the_complete_binding() {
    let binding = complete_binding_fixture();
    let first = AgentSightAdapter.translate(&binding).unwrap();
    let second = AgentSightAdapter.translate(&binding).unwrap();
    assert_eq!(first, second);
}

#[test]
fn source_preserves_scope_revision_independently_of_binding_revision() {
    for revision in [3, 4] {
        let mut binding = complete_binding_fixture();
        binding.scope.revision = Revision::new(revision).unwrap();
        let TranslationOutcome::Translated(plan) = AgentSightAdapter.translate(&binding).unwrap()
        else {
            panic!("expected translated target plan");
        };
        let decoded: AgentSightBindingPlan = serde_json::from_slice(&plan.content).unwrap();
        assert_eq!(decoded.source.scope_revision, binding.scope.revision);
        assert_eq!(decoded.source.binding_revision, binding.binding_revision);
        assert_ne!(
            decoded.source.scope_revision,
            decoded.source.binding_revision
        );
    }
}

#[test]
fn unsupported_scope_is_rejected_without_a_target_plan() {
    let mut binding = complete_binding_fixture();
    binding.scope.selector = ScopeSelector::CgroupId { cgroup_id: 42 };

    let outcome = AgentSightAdapter.translate(&binding).unwrap();
    let TranslationOutcome::Rejected(rejection) = outcome else {
        panic!("unsupported Scope must not produce a target plan");
    };
    assert_eq!(rejection.code, "UNSUPPORTED_SCOPE_SELECTOR");
}

#[test]
fn final_object_resolution_is_not_silently_lowered_to_unlink() {
    let mut binding = complete_binding_fixture();
    let ResourceSelector::File { matchers } =
        &mut binding.policy.canonical_policy.payload.resources[0].selector
    else {
        panic!("expected file resource set");
    };
    matchers[0].resolution = FileResolution::FinalObject {
        follow_final_symlink: true,
        match_hardlink_identity: true,
    };

    let outcome = AgentSightAdapter.translate(&binding).unwrap();
    let TranslationOutcome::Rejected(rejection) = outcome else {
        panic!("FinalObject semantics must not produce an unlink plan");
    };
    assert_eq!(rejection.code, "UNSUPPORTED_FILE_RESOLUTION");
}

#[test]
fn unsupported_policy_guarantees_are_rejected() {
    let mut binding = complete_binding_fixture();
    binding.policy.canonical_policy.payload.rules[0]
        .outcome
        .remediation = SubjectRemediation::Freeze;

    let outcome = AgentSightAdapter.translate(&binding).unwrap();
    let TranslationOutcome::Rejected(rejection) = outcome else {
        panic!("unsupported guarantees must not produce a target plan");
    };
    assert_eq!(rejection.code, "UNSUPPORTED_GUARANTEE");
}

#[test]
fn target_unsafe_literals_are_rejected() {
    for path in ["/workspace/bad\"name", "/workspace/bad\\name"] {
        let binding = binding_with_first_path(PathMatcher::Exact {
            path: path.to_owned(),
        });
        binding.validate().unwrap();
        let outcome = AgentSightAdapter.translate(&binding).unwrap();
        let TranslationOutcome::Rejected(rejection) = outcome else {
            panic!("unsafe DSL literal must not produce a target plan");
        };
        assert_eq!(rejection.code, "UNSUPPORTED_ACTPLANE_PATTERN");
    }
}

#[test]
fn valid_globs_without_equivalent_actplane_lowering_are_rejected() {
    for pattern in [
        "/workspace/file?.txt",
        "/a/*/b",
        "/workspace/*",
        "/workspace/prefix*",
    ] {
        let binding = binding_with_first_path(PathMatcher::Glob {
            pattern: pattern.to_owned(),
        });
        binding.validate().unwrap();

        let outcome = AgentSightAdapter.translate(&binding).unwrap();
        let TranslationOutcome::Rejected(rejection) = outcome else {
            panic!("glob without equivalent ActPlane lowering must not produce a target plan");
        };
        assert_eq!(rejection.code, "UNSUPPORTED_ACTPLANE_GLOB");
    }
}

#[test]
fn lowered_patterns_at_the_actplane_63_byte_limit_are_translated() {
    for path in [
        PathMatcher::Exact {
            path: format!("/{}", "e".repeat(62)),
        },
        PathMatcher::Exact {
            path: format!("/{}aa", "界".repeat(20)),
        },
        PathMatcher::Prefix {
            path: format!("/{}", "p".repeat(61)),
        },
        PathMatcher::Glob {
            pattern: format!("/{}/**", "g".repeat(61)),
        },
    ] {
        let binding = binding_with_first_path(path);
        binding.validate().unwrap();

        let outcome = AgentSightAdapter.translate(&binding).unwrap();
        assert!(matches!(outcome, TranslationOutcome::Translated(_)));
    }
}

#[test]
fn lowered_patterns_over_the_actplane_63_byte_limit_are_rejected() {
    for path in [
        PathMatcher::Exact {
            path: format!("/{}", "e".repeat(63)),
        },
        PathMatcher::Exact {
            path: format!("/{}aaa", "界".repeat(20)),
        },
        PathMatcher::Prefix {
            path: format!("/{}", "p".repeat(62)),
        },
        PathMatcher::Glob {
            pattern: format!("/{}/**", "g".repeat(62)),
        },
    ] {
        let binding = binding_with_first_path(path);
        binding.validate().unwrap();

        let outcome = AgentSightAdapter.translate(&binding).unwrap();
        let TranslationOutcome::Rejected(rejection) = outcome else {
            panic!("pattern exceeding the ActPlane ABI must not produce a target plan");
        };
        assert_eq!(rejection.code, "ACTPLANE_PATTERN_LIMIT_EXCEEDED");
    }
}
