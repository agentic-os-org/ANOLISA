//! A partial selection must retain its original operation/path pairing and provenance.

use asc_policy_cedar::{
    deployment::selected_pst,
    schema::embedded_reference,
    selection::{SelectedStatement, ValidatedPolicy, ValidatedStatement},
};
use asc_policy_types::{
    cedar::CedarPolicySource,
    identifiers::{ResourceId, Revision},
    policy::{DeploymentBackend, DeploymentSelection},
};

const HEAD: &str = r#"@id("reports") forbid(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
    when { principal has agentType && principal.agentType == "qoder" }"#;

fn source(clauses: &str) -> CedarPolicySource {
    CedarPolicySource {
        schema: embedded_reference(),
        text: format!("{HEAD} {clauses};"),
    }
}

fn select(source: &CedarPolicySource, allowed: &[&str]) -> SelectedStatement {
    let parsed = ValidatedPolicy::parse(source).unwrap();
    ValidatedStatement {
        policy_id: &ResourceId::new("pol-reports").unwrap(),
        revision: Revision::new(2).unwrap(),
        schema: &source.schema,
        rule: &parsed.statements[0],
    }
    .select_operations(allowed)
}

#[test]
fn original_branch_path_and_combined_guards_survive_deployment() {
    let rule = source(
        r#"when { (context.input.operation == SecCore::WriteOperation::"write" && resource.path == "/a")
        || (context.input.operation == SecCore::WriteOperation::"delete" && resource.path == "/b") }
        unless { resource.path == "/excluded" }"#,
    );
    let SelectedStatement::Fragment { cedar, branches } = select(&rule, &["delete"]) else {
        panic!("expected partial selection");
    };
    assert!(cedar.contains("agentType") && cedar.contains("qoder"));
    assert_eq!(branches.len(), 1);
    assert_eq!(branches[0].clause, 1);
    assert_eq!(branches[0].path, [true, false]);
    assert_eq!(branches[0].operation, "delete");
    let persisted = DeploymentSelection {
        backend: DeploymentBackend::ActPlane,
        statement_ids: ["policy0".into()].into(),
        fragments: [("policy0".into(), cedar)].into(),
    };
    let residual = selected_pst(&rule, &persisted).unwrap();
    let (id, replayed) = residual.policies.iter().next().unwrap();
    let text = replayed.to_string();
    assert_eq!(id.to_string(), "policy0");
    assert!(text.contains("delete") && text.contains("/b") && text.contains("/excluded"));
    assert!(text.contains("reports") && text.contains("unless"));
    assert!(!text.contains(r#""/a""#) && !text.contains("agentType"));
    assert!(!text.contains(r#"WriteOperation::"write""#));
}

#[test]
fn preserve_possible_errors_and_unpartitionable_conditions() {
    let only_delete = r#"when { context.input.operation == SecCore::WriteOperation::"delete" }"#;
    assert_eq!(
        select(&source(only_delete), &["write"]),
        SelectedStatement::Unrelated
    );
    let overflow = "when { 9223372036854775807 + 1 > 0 }";
    assert_eq!(
        select(&source(&format!("{overflow} {only_delete}")), &["write"]),
        SelectedStatement::Undetermined
    );
    assert_eq!(
        select(
            &source(
                r#"when { (9223372036854775807 + 1 > 0 && context.input.operation == SecCore::WriteOperation::"delete") || context.input.operation == SecCore::WriteOperation::"write" }"#
            ),
            &["write"]
        ),
        SelectedStatement::Undetermined
    );
    assert_eq!(
        select(
            &source(r#"unless { context.input.operation == SecCore::WriteOperation::"delete" }"#),
            &["write"]
        ),
        SelectedStatement::Undetermined
    );
    assert_eq!(
        select(
            &source(
                r#"when { context.input.operation == SecCore::WriteOperation::"delete" || resource.path == "/any-operation" }"#
            ),
            &["write"]
        ),
        SelectedStatement::Undetermined
    );
}

#[test]
fn legacy_snapshot_loads_a_whole_rule_and_rejects_inconsistent_fragments() {
    let mut selection: DeploymentSelection =
        serde_json::from_str(r#"{"backend":"actPlane","statementIds":["policy0"]}"#).unwrap();
    let rule = source(r#"when { resource.path == "/a" }"#);
    let original = selected_pst(&rule, &selection).unwrap();
    assert!(
        original
            .policies
            .values()
            .next()
            .unwrap()
            .to_string()
            .contains("/a")
    );
    selection
        .fragments
        .insert("policy0".into(), rule.text.replace("qoder", "cosh"));
    assert!(selected_pst(&rule, &selection).is_err());
    selection
        .fragments
        .insert("policy1".into(), rule.text.clone());
    assert!(selected_pst(&rule, &selection).is_err());
}
