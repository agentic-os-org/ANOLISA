//! The adapter reports coverage for the same partial Cedar it actually translates.

use asc_policy_cedar::{
    deployment::selected_pst,
    schema::principal_reference,
    selection::{Direction, SelectedStatement, ValidatedPolicy, ValidatedStatement},
};
use asc_policy_translator_agentsight::{
    AgentSightBindingPlan, AgentSightTranslator, selection::SELECTOR, validate_file_protection,
};
use asc_policy_types::{
    authoring::PolicySource,
    binding::PreparedBinding,
    cedar::CedarPolicySource,
    identifiers::{ResourceId, Revision},
    policy::{DeploymentBackend, DeploymentSelection},
    target::TranslationOutcome,
};

#[test]
fn qoder_fragment_compiles_with_operation_specific_deviations() {
    let source = CedarPolicySource {
        schema: principal_reference(),
        text:
            r#"forbid(principal,action == SecCore::Action::"file.write",resource is SecCore::File)
            when { principal has agentType && principal.agentType == "qoder" }
            when { resource.path == "/work/reports" || resource.path like "/work/reports/*" }
            when { context.input.operation == SecCore::WriteOperation::"write" ||
                   context.input.operation == SecCore::WriteOperation::"delete" ||
                   context.input.operation == SecCore::WriteOperation::"rename" };"#
                .into(),
    };
    let validated = ValidatedPolicy::parse(&source).unwrap();
    let selected = (SELECTOR.select)(&ValidatedStatement {
        policy_id: &ResourceId::new("report-policy").unwrap(),
        revision: Revision::new(5).unwrap(),
        schema: &source.schema,
        rule: &validated.statements[0],
    });
    let SelectedStatement::Fragment { cedar, branches } = selected.selected else {
        panic!("expected partial selection: {:?}", selected.selected);
    };
    let operations: Vec<_> = branches.iter().map(|b| b.operation.as_str()).collect();
    assert_eq!(operations, ["write", "delete"]);
    assert!(cedar.contains("qoder") && !cedar.contains("rename"));
    assert_eq!(selected.deviations.len(), 2);
    assert!(
        selected
            .deviations
            .iter()
            .any(|d| d.direction == Direction::Weaker && d.branch == branches[0])
    );
    assert!(
        selected
            .deviations
            .iter()
            .any(|d| d.direction == Direction::Stricter && d.branch == branches[1])
    );
    let scope = DeploymentSelection {
        backend: DeploymentBackend::ActPlane,
        statement_ids: [selected.source.statement_id.clone()].into(),
        fragments: [(selected.source.statement_id, cedar)].into(),
    };
    let protection = binding_plan(source, scope);
    assert_eq!(protection.targets.len(), 1);
    assert_eq!(protection.targets[0].path, "/work/reports");
    assert!(
        protection.targets[0].protections.ordinary_write
            && protection.targets[0].protections.delete
    );
}

fn binding_plan(
    source: CedarPolicySource,
    scope: DeploymentSelection,
) -> asc_agentsight_protocol::FileProtectionPolicy {
    let syntax = selected_pst(&source, &scope).unwrap();
    validate_file_protection(&syntax).unwrap();
    let mut binding: PreparedBinding = serde_json::from_str(include_str!(
        "../../asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap();
    binding.policy.template = PolicySource::Cedar(source);
    binding.policy.deployment = Some(scope);
    let expected = AgentSightTranslator
        .translate_cedar_pst(&binding, &syntax)
        .unwrap();
    let actual = AgentSightTranslator.translate(&binding).unwrap();
    assert_eq!(actual, expected);
    let TranslationOutcome::Translated(plan) = actual else {
        panic!("expected a deployed plan");
    };
    let plan: AgentSightBindingPlan = serde_json::from_slice(&plan.content).unwrap();
    serde_json::from_str(&plan.policy.content).unwrap()
}

#[test]
fn deviations_follow_each_operation_to_its_surviving_target() {
    use asc_agentsight_protocol::FileProtectionTargetKind::{DirectoryTree, ExactFile};

    let writes = r#"(context.input.operation == SecCore::WriteOperation::"write" && resource.path == "/a")
        || (context.input.operation == SecCore::WriteOperation::"write" && (resource.path == "/b" || resource.path like "/b/*"))"#;
    let deletes = r#"(context.input.operation == SecCore::WriteOperation::"delete" && resource.path == "/gone")
        || (context.input.operation == SecCore::WriteOperation::"delete" && resource.path == "/keep")"#;
    for partial in [false, true] {
        let mut alternatives = format!("({writes}) || ({deletes})");
        if partial {
            alternatives = format!(
                r#"context.input.operation == SecCore::WriteOperation::"rename" || ({alternatives})"#
            );
        }
        let source = CedarPolicySource {
            schema: principal_reference(),
            text: format!(
                r#"forbid(principal,action == SecCore::Action::"file.write",resource is SecCore::File)
                when {{ principal has agentType && principal.agentType == "qoder" && ({alternatives}) }}
                unless {{ resource.path == "/gone" }};"#
            ),
        };
        let validated = ValidatedPolicy::parse(&source).unwrap();
        let selected = (SELECTOR.select)(&ValidatedStatement {
            policy_id: &ResourceId::new("reports").unwrap(),
            revision: Revision::new(1).unwrap(),
            schema: &source.schema,
            rule: &validated.statements[0],
        });
        let statement_id = selected.source.statement_id;
        let fragments = match selected.selected {
            SelectedStatement::Fragment { cedar, .. } if partial => {
                [(statement_id.clone(), cedar)].into()
            }
            SelectedStatement::Whole if !partial => std::collections::BTreeMap::new(),
            other => panic!("unexpected selection: {other:?}"),
        };
        let actual: Vec<_> = selected
            .deviations
            .iter()
            .map(|d| (d.branch.clause, d.branch.path.as_slice(), d.code))
            .collect();
        // Subject removal shifts both paths. Projecting the leading rename shifts them again.
        let (tree_write, surviving_delete): (&[bool], &[bool]) = if partial {
            (
                &[true, true, false, true, false],
                &[true, true, true, true, false],
            )
        } else {
            (&[true, false, true, false], &[true, true, true, false])
        };
        assert_eq!(
            actual,
            [
                (0, tree_write, "shared_writable_mapping"),
                (0, surviving_delete, "ancestor_rename_guard")
            ]
        );
        let plan = binding_plan(
            source,
            DeploymentSelection {
                backend: DeploymentBackend::ActPlane,
                statement_ids: [statement_id].into(),
                fragments,
            },
        );
        let targets: Vec<_> = plan
            .targets
            .iter()
            .map(|t| {
                (
                    t.path.as_str(),
                    t.kind,
                    t.protections.ordinary_write,
                    t.protections.delete,
                )
            })
            .collect();
        assert_eq!(
            targets,
            [
                ("/a", ExactFile, true, false),
                ("/b", DirectoryTree, true, false),
                ("/keep", ExactFile, false, true)
            ]
        );
    }
}

#[test]
fn unlowerable_file_restriction_keeps_the_candidate_in_admission() {
    let source = CedarPolicySource {
        schema: principal_reference(),
        text: r#"forbid(principal,action == SecCore::Action::"file.read",resource is SecCore::File)
            when { principal has agentType && principal.agentType == "qoder" }
            when { resource.path == "/a" };
        forbid(principal,action == SecCore::Action::"file.read",resource is SecCore::File)
            when { principal has agentType && principal.agentType == "qoder" }
            when { resource.path == "/b" && context.input.program == "/bin/cat" };"#
            .into(),
    };
    let validated = ValidatedPolicy::parse(&source).unwrap();
    let mut scope = DeploymentSelection {
        backend: DeploymentBackend::ActPlane,
        statement_ids: std::collections::BTreeSet::new(),
        fragments: std::collections::BTreeMap::new(),
    };
    for rule in &validated.statements {
        let selection = (SELECTOR.select)(&ValidatedStatement {
            policy_id: &ResourceId::new("reports").unwrap(),
            revision: Revision::new(1).unwrap(),
            schema: &source.schema,
            rule,
        });
        let expected = if rule.id == "policy0" {
            SelectedStatement::Whole
        } else {
            SelectedStatement::Undetermined
        };
        assert_eq!(selection.selected, expected);
        scope.statement_ids.insert(rule.id.clone());
    }
    let candidate = selected_pst(&source, &scope).unwrap();
    assert!(validate_file_protection(&candidate).is_err());
}

#[test]
fn untargeted_v3_file_rule_has_no_automatic_actplane_coverage() {
    let source = CedarPolicySource {
        schema: principal_reference(),
        text:
            r#"forbid(principal, action == SecCore::Action::"file.read", resource is SecCore::File)
            when { resource.path == "/reports" };"#
                .into(),
    };
    let validated = ValidatedPolicy::parse(&source).unwrap();
    let selected = (SELECTOR.select)(&ValidatedStatement {
        policy_id: &ResourceId::new("reports").unwrap(),
        revision: Revision::new(1).unwrap(),
        schema: &source.schema,
        rule: &validated.statements[0],
    });
    assert!(
        matches!(selected.selected, SelectedStatement::Unsupported(reason) if reason.contains("requires an exact principal.agentType"))
    );
}
