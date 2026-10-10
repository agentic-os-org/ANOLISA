#[cfg(test)]
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/policy.rs"
));

use asc_policy_adapter_agentsight::{
    ACTPLANE_POLICY_MEDIA_TYPE, AGENTSIGHT_BINDING_PLAN_FORMAT,
    AGENTSIGHT_BINDING_PLAN_SCHEMA_VERSION, AgentSightAdapter, AgentSightBindingPlan,
    AgentSightScopePlan,
};
use asc_policy_types::Validate;
use asc_policy_types::authoring::{Action, Effect, Resource};
use asc_policy_types::binding::PreparedBinding;
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

fn binding_with_files(files: Vec<String>) -> PreparedBinding {
    let mut binding = complete_binding_fixture();
    binding.policy.template = file_policy(files);
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
        AgentSightScopePlan::ProcessTree {
            root_pid: 4242,
            process: complete_binding_fixture().scope.process
        }
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
fn source_preserves_assignment_and_pinned_instance() {
    let binding = complete_binding_fixture();
    let TranslationOutcome::Translated(plan) = AgentSightAdapter.translate(&binding).unwrap()
    else {
        panic!("expected plan")
    };
    let decoded: AgentSightBindingPlan = serde_json::from_slice(&plan.content).unwrap();
    assert_eq!(decoded.source.scope_id, binding.scope.scope_id);
    assert_eq!(decoded.source.binding_revision, binding.binding_revision);
    let value: serde_json::Value = serde_json::from_slice(&plan.content).unwrap();
    assert!(value["source"].get("scopeRevision").is_none());
    assert_eq!(
        value["scope"]["process"],
        serde_json::to_value(&binding.scope.process).unwrap()
    );
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
fn invalid_rules_produce_no_plan() {
    for template in [
        file_policy(vec![]),
        file_policy(vec!["/invalid/../path".into()]),
    ] {
        let mut binding = complete_binding_fixture();
        binding.policy.template = template;
        let TranslationOutcome::Rejected(rejection) =
            AgentSightAdapter.translate(&binding).unwrap()
        else {
            panic!("invalid template must not produce a plan");
        };
        assert_eq!(rejection.code, "INVALID_BINDING");
    }
}

#[test]
fn unsupported_rule_rejects_the_entire_policy_with_its_position() {
    for (effect, action, code) in [
        (Effect::Allow, Action::Write, "RULE_1_UNSUPPORTED_EFFECT"),
        (
            Effect::RequireConfirmation,
            Action::Write,
            "RULE_1_UNSUPPORTED_EFFECT",
        ),
        (Effect::Block, Action::Read, "RULE_1_UNSUPPORTED_ACTION"),
    ] {
        let mut binding = complete_binding_fixture();
        let rule = &mut binding.policy.template.rules[1];
        rule.effect = effect;
        rule.action = action;
        if action == Action::Read {
            rule.condition = None;
        }
        binding.validate().unwrap();
        let TranslationOutcome::Rejected(rejection) =
            AgentSightAdapter.translate(&binding).unwrap()
        else {
            panic!("must not produce a partial plan");
        };
        assert_eq!(rejection.code, code);
        assert_eq!(
            asc_policy_types::target::Failure::new(
                asc_policy_types::target::FailureKind::Rejected,
                &rejection.code
            )
            .code,
            code
        );
    }
}

#[test]
fn explanations_are_preserved_and_cannot_inject_dsl() {
    let mut binding = binding_with_files(vec!["/same".into(), "/same".into()]);
    binding.policy.template.rules[0].because = Some("保护配置文件".into());
    binding.policy.template.rules[1].because = Some("Independent reason".into());
    let TranslationOutcome::Translated(plan) = AgentSightAdapter.translate(&binding).unwrap()
    else {
        panic!("expected a complete plan");
    };
    let plan: AgentSightBindingPlan = serde_json::from_slice(&plan.content).unwrap();
    assert_eq!(plan.policy.content.matches("block unlink").count(), 2);
    assert!(plan.policy.content.contains("保护配置文件"));
    assert!(plan.policy.content.contains("Independent reason"));
    for reason in ["bad\"reason", "bad\\reason", "bad\nreason"] {
        binding.policy.template.rules[1].because = Some(reason.into());
        binding.validate().unwrap();
        assert_eq!(
            AgentSightAdapter.translate(&binding).unwrap(),
            TranslationOutcome::Rejected(asc_policy_types::target::TranslationRejection {
                code: "RULE_1_UNSUPPORTED_ACTPLANE_REASON".into()
            })
        );
    }
}

#[test]
fn target_pattern_rejections_preserve_input_validation_and_dsl_limits() {
    for (paths, code) in [
        (
            vec!["/workspace/bad\"name".into()],
            "UNSUPPORTED_ACTPLANE_PATTERN",
        ),
        (
            vec!["/workspace/bad\\name".into()],
            "UNSUPPORTED_ACTPLANE_PATTERN",
        ),
        (
            vec!["/workspace/bad\nrule".into()],
            "UNSUPPORTED_ACTPLANE_PATTERN",
        ),
        (
            vec!["/workspace/file?.txt".into()],
            "UNSUPPORTED_ACTPLANE_GLOB",
        ),
        (vec!["/a/*/b".into()], "UNSUPPORTED_ACTPLANE_GLOB"),
        (vec!["/workspace/*".into()], "UNSUPPORTED_ACTPLANE_GLOB"),
        (
            vec!["/workspace/prefix*".into()],
            "UNSUPPORTED_ACTPLANE_GLOB",
        ),
        (
            vec![format!("/{}", "e".repeat(63))],
            "ACTPLANE_PATTERN_LIMIT_EXCEEDED",
        ),
        (
            vec![format!("/{}aaa", "界".repeat(20))],
            "ACTPLANE_PATTERN_LIMIT_EXCEEDED",
        ),
        (
            vec![format!("/{}/**", "g".repeat(62))],
            "ACTPLANE_PATTERN_LIMIT_EXCEEDED",
        ),
        (
            (0..129).map(|n| format!("/file-{n}")).collect(),
            "ACTPLANE_RULE_LIMIT_EXCEEDED",
        ),
    ] {
        let binding = binding_with_files(paths);
        binding.validate().unwrap();
        let TranslationOutcome::Rejected(rejection) =
            AgentSightAdapter.translate(&binding).unwrap()
        else {
            panic!("unsupported pattern must not produce a target plan");
        };
        assert_eq!(
            rejection.code,
            if code == "ACTPLANE_RULE_LIMIT_EXCEEDED" {
                code.to_owned()
            } else {
                format!("RULE_0_{code}")
            }
        );
    }
}

#[test]
fn supported_patterns_at_the_actplane_limits_are_translated() {
    for paths in [
        vec!["/".into(), "/**".into()],
        vec![format!("/{}", "e".repeat(62))],
        vec![format!("/{}aa", "界".repeat(20))],
        vec![format!("/{}/**", "g".repeat(61))],
        (0..128).map(|n| format!("/file-{n}")).collect(),
    ] {
        let binding = binding_with_files(paths);
        binding.validate().unwrap();
        assert!(matches!(
            AgentSightAdapter.translate(&binding).unwrap(),
            TranslationOutcome::Translated(_)
        ));
    }
}

#[test]
fn rule_order_is_preserved_and_changed_rules_change_the_plan() {
    let original = complete_binding_fixture();
    let expected = AgentSightAdapter.translate(&original).unwrap();
    let mut reversed = original.clone();
    reversed.policy.template.rules.reverse();
    assert_ne!(AgentSightAdapter.translate(&reversed).unwrap(), expected);
    let TranslationOutcome::Translated(plan) = AgentSightAdapter.translate(&reversed).unwrap()
    else {
        panic!("expected plan");
    };
    let decoded: AgentSightBindingPlan = serde_json::from_slice(&plan.content).unwrap();
    let Resource::File { path } = &reversed.policy.template.rules[0].target;
    assert!(decoded.policy.content.starts_with(&format!(
        "source AGENT = exec \"**\"\nrule agentseccore-unlink-0000:\n  block unlink file \"{path}\""
    )));
    assert_ne!(
        AgentSightAdapter
            .translate(&binding_with_files(vec!["/new-policy".into()]))
            .unwrap(),
        expected
    );
}

#[test]
fn broader_writes_logical_predicates_and_history_never_lower_to_unconditional_unlink() {
    for (condition, history, code) in [
        (
            serde_json::Value::Null,
            serde_json::Value::Null,
            "RULE_1_UNSUPPORTED_CONDITION",
        ),
        (
            serde_json::json!({"or":[{"operation":{"eq":"delete"}}]}),
            serde_json::Value::Null,
            "RULE_1_UNSUPPORTED_CONDITION",
        ),
        (
            serde_json::json!({"operation":{"eq":"delete"}}),
            serde_json::json!({"category":"file","action":"read","target":{"type":"file","path":"/secret"}}),
            "RULE_1_UNSUPPORTED_HISTORY",
        ),
    ] {
        let mut wire = serde_json::to_value(complete_binding_fixture()).unwrap();
        wire["policy"]["template"]["rules"][1]["where"] = condition;
        wire["policy"]["template"]["rules"][1]["previous"] = history;
        let binding: PreparedBinding = serde_json::from_value(wire).unwrap();
        binding.validate().unwrap();
        let TranslationOutcome::Rejected(rejection) =
            AgentSightAdapter.translate(&binding).unwrap()
        else {
            panic!("unsupported predicate must not disappear during translation")
        };
        assert_eq!(rejection.code, code);
    }
}
