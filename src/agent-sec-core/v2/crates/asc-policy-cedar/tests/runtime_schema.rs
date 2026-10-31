use asc_policy_cedar::{CedarError, pst, validate};
use asc_policy_types::cedar::{CedarPolicySource, CedarSchemaReference};
use asc_policy_types::identifiers::{ResourceId, Revision};
use cedar_policy::PolicySet;

fn source(text: &str, revision: u32) -> CedarPolicySource {
    CedarPolicySource {
        schema: CedarSchemaReference {
            schema_id: ResourceId::new("seccore.system").unwrap(),
            revision: Revision::new(revision).unwrap(),
        },
        text: text.into(),
    }
}

#[test]
fn runtime_subject_and_advice_are_versioned_and_preserved() {
    let text = r#"
@advice("{\"verdict\":\"escalate\",\"reason\":\"approval_required\"}")
permit(principal == SecCore::Subject::"worker", action == SecCore::Action::"process.spawn", resource)
when { principal has agent_type && principal.agent_type == "codex" && resource.program == "gh" };
"#;
    assert!(validate(&source(text, 1)).is_err());
    validate(&source(text, 2)).unwrap();
    let policies = PolicySet::from_pst(pst(&source(text, 2)).unwrap()).unwrap();
    assert!(
        policies
            .policies()
            .next()
            .unwrap()
            .annotation("advice")
            .unwrap()
            .contains("approval_required")
    );
}

#[test]
fn runtime_advice_rejects_unknown_or_unfulfillable_shapes() {
    for annotation in [
        r#"@advice("require_confirmation")"#,
        r#"@advice("{\"verdict\":\"allow\"}")"#,
        r#"@advice("{\"verdict\":\"transform\"}")"#,
        r#"@advice("{\"verdict\":\"warn\",\"typo\":true}")"#,
        r#"@unknown("value")"#,
    ] {
        let text = format!("{annotation} permit(principal, action, resource);");
        assert!(
            matches!(
                validate(&source(&text, 2)),
                Err(CedarError::Unsupported { .. })
            ),
            "{text}"
        );
    }
    assert!(
        validate(&source(
            r#"@advice("{\"verdict\":\"escalate\"}") forbid(principal,action,resource);"#,
            2
        ))
        .is_err()
    );
}
