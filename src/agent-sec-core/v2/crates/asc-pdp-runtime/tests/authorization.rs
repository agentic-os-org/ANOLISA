use asc_daemon_protocol::authorization::*;
use asc_pdp_runtime::PdpRuntime;
use asc_policy_types::cedar::{
    AuthorizationPolicy, AuthorizationPolicySet, CedarPolicySource, CedarSchemaReference,
};
use asc_policy_types::identifiers::{ResourceId, Revision};
use serde_json::json;

fn source(text: &str) -> CedarPolicySource {
    CedarPolicySource {
        schema: CedarSchemaReference {
            schema_id: ResourceId::new("seccore.system").unwrap(),
            revision: Revision::new(1).unwrap(),
        },
        text: text.to_owned(),
    }
}

fn permissions() -> OsPermissionSet {
    serde_json::from_value(json!({
        "version": 1,
        "filesystem": {"default": "deny", "rules": [
            {"effect": "allow", "path": "/usr/bin/python3", "rights": ["read"]},
            {"effect": "allow", "path": "/work", "rights": ["read", "write"]}
        ]},
        "execution": {"deny_paths": []}, "network": {"mode": "off"}
    }))
    .unwrap()
}

#[test]
fn pdp_projects_subtree_exceptions_without_overriding_requested_denies() {
    let policy = snapshot(
        r#"
permit(principal, action == SecCore::Action::"file.read", resource);
forbid(principal, action == SecCore::Action::"file.read", resource)
when { resource.path == "/work" || resource.path like "/work/*" }
unless { resource.path == "/work/current" || resource.path like "/work/current/*" };
forbid(principal, action == SecCore::Action::"file.write", resource)
when { resource.path == "/work/current/readonly" || resource.path like "/work/current/readonly/*" };
"#,
    );
    let runtime = PdpRuntime::from_policy_set(policy).unwrap();
    let mut input = request();
    input.context.launch.cwd = "/work/current".into();
    input
        .context
        .requested_permissions
        .filesystem
        .rules
        .push(FilesystemRule {
            effect: RuleEffect::Allow,
            path: "/".into(),
            rights: vec![FileRight::Read],
            excluded_paths: vec![],
        });
    let decision = runtime.evaluate(&input, 100).unwrap();
    assert_eq!(
        decision.verdict,
        DecisionVerdict::Modify,
        "{}",
        decision.reason
    );
    let effective = decision.effective_permissions.unwrap();
    assert_eq!(effective.version, 1);
    let deny = effective
        .filesystem
        .rules
        .iter()
        .find(|rule| rule.path == "/work" && rule.effect == RuleEffect::Deny)
        .unwrap();
    assert_eq!(deny.excluded_paths, ["/work/current"]);
    assert!(
        effective
            .filesystem
            .rules
            .iter()
            .any(|rule| rule.path == "/work/current/readonly"
                && rule.effect == RuleEffect::Deny
                && rule.rights.contains(&FileRight::Write))
    );
    input.context.requested_permissions = effective;
    let unchanged = runtime.evaluate(&input, 101).unwrap();
    assert_eq!(
        unchanged.verdict,
        DecisionVerdict::Allow,
        "{}",
        unchanged.reason
    );
    input
        .context
        .requested_permissions
        .filesystem
        .rules
        .push(FilesystemRule {
            effect: RuleEffect::Deny,
            path: "/work".into(),
            rights: vec![FileRight::Read],
            excluded_paths: vec![],
        });
    let denied = runtime.evaluate(&input, 102).unwrap();
    assert_eq!(denied.verdict, DecisionVerdict::Deny);
}

const FILES: &str = r#"
permit(principal, action == SecCore::Action::"file.read", resource)
when { resource.path == "/usr/bin/python3" || resource.path == "/work" || resource.path like "/work/*" };
permit(principal, action == SecCore::Action::"file.write", resource)
when { resource.path == "/work" || resource.path like "/work/*" };
"#;
const COMMAND: &str = r#"
permit(principal, action == SecCore::Action::"process.spawn", resource)
when { resource.program == "python3" && principal has agentType && principal.agentType == "codex" };
"#;

fn member(id: &str, text: &str) -> AuthorizationPolicy {
    AuthorizationPolicy {
        policy_id: ResourceId::new(id).unwrap(),
        revision: Revision::new(1).unwrap(),
        source: source(&text.replace("principal,", "principal is SecCore::Subject,")),
    }
}

fn snapshot(extra: &str) -> AuthorizationPolicySet {
    AuthorizationPolicySet {
        version: "snapshot-1".into(),
        policies: vec![member("policy", &format!("{FILES}{COMMAND}{extra}"))],
    }
}

fn request() -> OsAuthorizationRequest {
    OsAuthorizationRequest::new(
        "request-1".into(),
        OsAuthorizationContext {
            execution_id: "execution-1".into(),
            agent: AgentPrincipal {
                agent_type: Some("codex".into()),
            },
            launch: CommandRequest {
                program: "python3".into(),
                resolved_program: "/usr/bin/python3".into(),
                args: vec!["agent.py".into()],
                cwd: "/work".into(),
            },
            requested_permissions: permissions(),
        },
    )
}

fn effective_runtime(sources: &[&str]) -> Result<PdpRuntime, asc_pdp_runtime::PdpError> {
    use asc_policy_types::cedar::{AuthorizationPolicy, AuthorizationPolicySet};
    let policies = sources
        .iter()
        .enumerate()
        .map(|(index, text)| AuthorizationPolicy {
            policy_id: ResourceId::new(format!("member-{index}")).unwrap(),
            revision: Revision::new(1).unwrap(),
            source: source(text),
        })
        .collect();
    PdpRuntime::from_policy_set(AuthorizationPolicySet {
        version: "effective-1".into(),
        policies,
    })
}

fn evaluate_v3(text: &str, agent_type: Option<&str>) -> OsPermissionSet {
    let mut policy =
        snapshot(r#"permit(principal, action == SecCore::Action::"process.spawn", resource);"#);
    policy.policies.push(AuthorizationPolicy {
        policy_id: ResourceId::new("v3").unwrap(),
        revision: Revision::new(1).unwrap(),
        source: CedarPolicySource {
            schema: asc_policy_cedar::schema::principal_reference(),
            text: text.into(),
        },
    });
    let runtime = PdpRuntime::from_policy_set(policy).unwrap();
    let mut input = request();
    input.context.agent.agent_type = agent_type.map(str::to_owned);
    let decision = runtime.evaluate(&input, 100).unwrap();
    assert!(
        matches!(
            decision.verdict,
            DecisionVerdict::Allow | DecisionVerdict::Modify
        ),
        "{}",
        decision.reason
    );
    decision.effective_permissions.unwrap()
}

#[test]
fn selected_fragments_drive_sandbox_decisions_without_losing_path_or_subject() {
    use asc_pdp_runtime::selection::select;
    use asc_policy_cedar::selection::{SelectedStatement, ValidatedPolicy, ValidatedStatement};

    let mut member = member("qoder", "");
    member.source.schema = asc_policy_cedar::schema::principal_reference();
    member.source.text = r#"
        permit(principal, action == SecCore::Action::"process.spawn", resource);
        permit(principal, action == SecCore::Action::"file.read", resource);
        permit(principal, action == SecCore::Action::"file.write", resource);
        forbid(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
        when { principal has agentType && principal.agentType == "qoder" }
        when { resource.path == "/work/reports" || resource.path like "/work/reports/*" }
        when { context.input.operation == SecCore::WriteOperation::"write" ||
               context.input.operation == SecCore::WriteOperation::"delete" ||
               context.input.operation == SecCore::WriteOperation::"rename" };
        forbid(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
        when { (context.input.operation == SecCore::WriteOperation::"write" && resource.path == "/work/A") ||
               (context.input.operation == SecCore::WriteOperation::"delete" && resource.path == "/work/B") };
        forbid(principal, action == SecCore::Action::"network.send", resource is SecCore::HttpEndpoint);
        @advice("{\"verdict\":\"warn\",\"message\":\"Confirm the report destination\"}")
        permit(principal, action == SecCore::Action::"file.write", resource)
        when { context.input.operation == SecCore::WriteOperation::"write" ||
               context.input.operation == SecCore::WriteOperation::"delete" };
    "#.into();
    let parsed = ValidatedPolicy::parse(&member.source).unwrap();
    let statement = |rule| ValidatedStatement {
        policy_id: &member.policy_id,
        revision: member.revision,
        schema: &member.source.schema,
        rule,
    };
    let choices: Vec<_> = parsed
        .statements
        .iter()
        .map(|rule| select(&statement(rule)))
        .collect();
    let choice = |id: &str| {
        &choices
            .iter()
            .find(|s| s.source.statement_id == id)
            .unwrap()
            .selected
    };
    let SelectedStatement::Fragment { cedar, branches } = choice("policy3") else {
        panic!("expected the ordinary-write fragment");
    };
    assert_eq!(branches.len(), 1);
    assert_eq!(branches[0].operation, "write");
    assert!(cedar.contains("qoder") && cedar.contains("/work/reports"));
    assert!(!cedar.contains(r#""delete""#) && !cedar.contains(r#""rename""#));
    assert!(matches!(choice("policy5"), SelectedStatement::Unrelated));
    let mismatched = (&parsed.statements[0], &choices[1]);
    assert!(matches!(
        PdpRuntime::from_selected(
            "qoder-revision".into(),
            [(statement(mismatched.0), mismatched.1)]
        ),
        Err(asc_pdp_runtime::PdpError::Configuration(_))
    ));

    let runtime = PdpRuntime::from_selected(
        "qoder-revision".into(),
        parsed
            .statements
            .iter()
            .zip(&choices)
            .map(|(rule, selection)| (statement(rule), selection)),
    )
    .unwrap();
    for (agent, reports_restricted) in [("qoder", true), ("cosh", false)] {
        let mut input = request();
        input.context.agent.agent_type = Some(agent.into());
        let decision = runtime.evaluate(&input, 100).unwrap();
        assert!(
            decision
                .warnings
                .iter()
                .any(|message| message == "Confirm the report destination")
        );
        let effective = decision
            .effective_permissions
            .expect("the command itself is permitted");
        let write_denied = |path| {
            effective.filesystem.rules.iter().any(|rule| {
                rule.effect == RuleEffect::Deny
                    && rule.rights.contains(&FileRight::Write)
                    && rule.covers_path(path, true)
            })
        };
        assert_eq!(write_denied("/work/reports/q1.txt"), reports_restricted);
        assert!(write_denied("/work/A"));
        assert!(!write_denied("/work/B"));
    }
}

#[test]
fn v3_mixed_source_preserves_the_qoder_file_forbid_without_granting_network() {
    let text = format!(
        "{}\n{}",
        include_str!("../../../fixtures/policy/qoder-v3.cedar"),
        r#"forbid(principal, action, resource is SecCore::HttpEndpoint)
           when { resource.host == "blocked.example" };
           forbid(principal, action, resource == SecCore::HttpEndpoint::"blocked");
           forbid(principal, action, resource in SecCore::HttpEndpoint::"remote");
           forbid(principal, action, resource is SecCore::HttpEndpoint in SecCore::HttpEndpoint::"remote");"#
    );
    for (agent, restricted) in [(Some("qoder"), true), (Some("cosh"), false), (None, false)] {
        let permissions = evaluate_v3(&text, agent);
        assert_eq!(permissions.network.mode, NetworkMode::Off);
        assert_eq!(
            permissions
                .filesystem
                .rules
                .iter()
                .any(|rule| rule.effect == RuleEffect::Deny
                    && rule.rights.contains(&FileRight::Read)
                    && rule.covers_path("/work/data/secret.txt", true)),
            restricted
        );
    }
}

#[test]
fn a_file_membership_that_cannot_be_mapped_to_a_path_is_reported_in_each_schema() {
    for revision in [1, 2, 3] {
        for header in ["in", "is SecCore::File in"] {
            let text = format!(
                r#"forbid(principal is SecCore::Subject, action, resource {header} SecCore::File::"private");"#
            );
            let mut policy = member("file-member", &text);
            policy.source.schema.revision = Revision::new(revision).unwrap();
            let set = AuthorizationPolicySet {
                version: "file-membership".into(),
                policies: vec![policy],
            };
            assert!(
                matches!(PdpRuntime::from_policy_set(set), Err(asc_pdp_runtime::PdpError::Unsupported(reason)) if reason.contains("filesystem policy resources must use path predicates"))
            );
        }
    }
}

#[test]
fn v3_optional_uid_is_evaluated_with_the_original_cedar_operator() {
    for (operator, restricted) in [("&&", false), ("||", true)] {
        let text = format!(
            r#"forbid(principal, action == SecCore::Action::"file.read", resource is SecCore::File)
               when {{ resource.path == "/work/private" }}
               when {{ (principal has agentType && principal.agentType == "qoder") {operator}
                       (principal has ownerUid && principal.ownerUid == 1000) }};"#
        );
        let permissions = evaluate_v3(&text, Some("qoder"));
        let denied = |path| {
            permissions.filesystem.rules.iter().any(|rule| {
                rule.effect == RuleEffect::Deny
                    && rule.rights.contains(&FileRight::Read)
                    && rule.covers_path(path, true)
            })
        };
        assert_eq!(denied("/work/private"), restricted);
        assert!(!denied("/work/public"));
    }
}

#[test]
fn each_member_keeps_its_own_operation_contract_in_a_mixed_snapshot() {
    let operation = r#"forbid(principal, action == SecCore::Action::"file.write", resource)
        when { context.input.operation == SecCore::WriteOperation::"delete" };"#;
    let mut mixed = snapshot(operation);
    let mut v3 = member("shared", operation);
    v3.source.schema = asc_policy_cedar::schema::principal_reference();
    mixed.policies.push(v3);
    assert!(
        matches!(PdpRuntime::from_policy_set(mixed), Err(asc_pdp_runtime::PdpError::Unsupported(reason)) if reason.contains("runtime file-operation"))
    );
}

#[test]
fn v3_ordinary_write_checks_delete_predicates_and_their_sibling_paths() {
    for (condition, restrict_a) in [("", false), (r#" || resource.path == "/work/A""#, true)] {
        let text = format!(
            r#"forbid(principal, action == SecCore::Action::"file.write", resource)
               when {{ context.input.operation == SecCore::WriteOperation::"delete"{condition} }};"#
        );
        let permissions = evaluate_v3(&text, Some("qoder"));
        let denied = |path| {
            permissions.filesystem.rules.iter().any(|rule| {
                rule.effect == RuleEffect::Deny
                    && rule.rights.contains(&FileRight::Write)
                    && rule.covers_path(path, true)
            })
        };
        assert_eq!(denied("/work/A"), restrict_a);
        assert!(!denied("/work/B"));
    }
}

#[test]
fn effective_set_matches_agent_type_and_preserves_global_forbid_precedence() {
    let files = FILES.replace("principal,", "principal is SecCore::Subject,");
    let command = COMMAND.replace("principal,", "principal is SecCore::Subject,");
    // Each source has its own policy0; both must survive merging.
    let runtime = effective_runtime(&[&files, &command]).unwrap();
    assert_eq!(
        runtime.evaluate(&request(), 100).unwrap().verdict,
        DecisionVerdict::Allow
    );
    let mut other = request();
    other.context.agent.agent_type = Some("qoder".into());
    assert_eq!(
        runtime.evaluate(&other, 100).unwrap().verdict,
        DecisionVerdict::Deny
    );
    other = request();
    other.context.agent.agent_type = None;
    assert_eq!(
        runtime.evaluate(&other, 100).unwrap().verdict,
        DecisionVerdict::Deny
    );
    let deny = r#"forbid(principal is SecCore::Subject, action == SecCore::Action::"process.spawn", resource);"#;
    let runtime = effective_runtime(&[&files, &command, deny]).unwrap();
    assert_eq!(
        runtime.evaluate(&request(), 100).unwrap().verdict,
        DecisionVerdict::Deny
    );
    assert_eq!(
        effective_runtime(&[])
            .unwrap()
            .evaluate(&request(), 100)
            .unwrap()
            .verdict,
        DecisionVerdict::Deny
    );
}

#[test]
fn cwd_entry_gates_launch_on_directory_read_without_granting_its_tree() {
    let mut input = request();
    input.context.requested_permissions.execution.cwd_entry = Some("/work".into());
    input
        .context
        .requested_permissions
        .filesystem
        .rules
        .retain(|rule| rule.path != "/work");

    let gate = r#"@advice("{\"verdict\":\"escalate\",\"message\":\"Enter workspace\"}")
permit(principal, action == SecCore::Action::"file.read", resource)
when { resource.path == "/work" };"#;
    let decision = PdpRuntime::from_policy_set(snapshot(gate))
        .unwrap()
        .evaluate(&input, 100)
        .unwrap();
    assert_eq!(
        decision.verdict,
        DecisionVerdict::StepUp,
        "{}",
        decision.reason
    );
    assert_eq!(
        decision.approval_requirements,
        vec![ApprovalRequirement {
            target: ApprovalTarget::Filesystem {
                path: "/work".into(),
                right: FileRight::Read,
                excluded_paths: vec![],
            },
            message: Some("Enter workspace".into()),
        }]
    );
    assert_eq!(
        decision.effective_permissions.as_ref(),
        Some(&input.context.requested_permissions)
    );

    let forbid = r#"forbid(principal, action == SecCore::Action::"file.read", resource)
when { resource.path == "/work" };"#;
    let denied = PdpRuntime::from_policy_set(snapshot(forbid))
        .unwrap()
        .evaluate(&input, 100)
        .unwrap();
    assert_eq!(denied.verdict, DecisionVerdict::Deny);
    assert!(denied.reason.contains("/work"), "{}", denied.reason);
    assert!(denied.effective_permissions.is_none());
}

#[test]
fn invocation_advice_uses_the_requested_working_directory() {
    let gate = r#"@advice("{\"verdict\":\"escalate\",\"message\":\"Confirm this launch\"}")
permit(principal is SecCore::Subject, action == SecCore::Action::"process.spawn", resource)
when { context.input.cwd == "/work" };"#;
    let runtime = PdpRuntime::from_policy_set(snapshot(gate)).unwrap();
    let mut input = request();
    assert_eq!(
        runtime.evaluate(&input, 100).unwrap().verdict,
        DecisionVerdict::StepUp
    );
    input.context.launch.cwd = "/work/other".into();
    assert_eq!(
        runtime.evaluate(&input, 100).unwrap().verdict,
        DecisionVerdict::Allow
    );
}

#[test]
fn effective_set_keeps_advice_network_policy_and_optional_type() {
    let files = FILES.replace("principal,", "principal is SecCore::Subject,");
    let command = r#"@advice("{\"verdict\":\"escalate\"}")
        permit(principal is SecCore::Subject, action == SecCore::Action::"process.spawn", resource);"#;
    let runtime = effective_runtime(&[&files, command]).unwrap();
    let mut input = request();
    input.context.agent.agent_type = None;
    let decision = runtime.evaluate(&input, 100).unwrap();
    assert_eq!(decision.verdict, DecisionVerdict::StepUp);
    input.context.requested_permissions.network.mode = NetworkMode::Full;
    assert_eq!(
        runtime.evaluate(&input, 100).unwrap().verdict,
        DecisionVerdict::Deny
    );
    assert!(effective_runtime(&[FILES]).is_err());
}

#[test]
fn effective_set_does_not_ignore_a_forbid_evaluation_error() {
    let files = FILES.replace("principal,", "principal is SecCore::Subject,");
    let command = COMMAND.replace("principal,", "principal is SecCore::Subject,");
    let failing = r#"forbid(principal is SecCore::Subject,
        action == SecCore::Action::"process.spawn", resource)
        when { context.input.arg_count + 9223372036854775807 > 0 };"#;
    let runtime = effective_runtime(&[&files, &command, failing]).unwrap();
    let decision = runtime.evaluate(&request(), 100).unwrap();
    assert_eq!(decision.verdict, DecisionVerdict::Deny);
    assert!(decision.reason.contains("evaluation failed"));
    assert!(decision.reason.contains("member-2"));
}

#[test]
fn one_invocation_view_can_permit_but_either_view_can_forbid() {
    let runtime = PdpRuntime::from_policy_set(snapshot("")).unwrap();
    assert_eq!(
        runtime.evaluate(&request(), 100).unwrap().verdict,
        DecisionVerdict::Allow
    );
    let runtime = PdpRuntime::from_policy_set(snapshot(
        r#"
forbid(principal, action == SecCore::Action::"file.exec", resource)
when { resource.path == "/usr/bin/python3" };
"#,
    ))
    .unwrap();
    let decision = runtime.evaluate(&request(), 100).unwrap();
    assert_eq!(decision.verdict, DecisionVerdict::Deny);
    assert!(decision.effective_permissions.is_none());
}

#[test]
fn explicit_child_denies_survive_recursive_permissions() {
    let runtime = PdpRuntime::from_policy_set(snapshot(
        r#"
forbid(principal, action == SecCore::Action::"file.read", resource)
when { resource.path == "/work/secrets" || resource.path like "/work/secrets/*" };
"#,
    ))
    .unwrap();
    let decision = runtime.evaluate(&request(), 100).unwrap();
    assert_eq!(
        decision.verdict,
        DecisionVerdict::Modify,
        "{}",
        decision.reason
    );
    assert!(
        decision
            .effective_permissions
            .unwrap()
            .filesystem
            .rules
            .iter()
            .any(|rule| rule.path == "/work/secrets"
                && rule.effect == RuleEffect::Deny
                && rule.rights.contains(&FileRight::Read))
    );
}

#[test]
fn directory_point_grants_do_not_authorize_unknown_descendants() {
    let mut publication = snapshot("");
    publication.policies[0] = member(
        "policy",
        &format!(
            r#"{COMMAND}
permit(principal, action == SecCore::Action::"file.read", resource)
when {{ resource.path == "/usr/bin/python3" || resource.path == "/work" }};
"#
        ),
    );
    let decision = PdpRuntime::from_policy_set(publication)
        .unwrap()
        .evaluate(&request(), 100)
        .unwrap();
    assert_eq!(decision.verdict, DecisionVerdict::Deny);
    assert!(decision.reason.contains("directory-only"));
}

#[test]
fn new_exclusion_modifies_once_but_existing_or_redundant_rules_do_not() {
    let runtime = PdpRuntime::from_policy_set(snapshot(
        r#"
forbid(principal, action == SecCore::Action::"file.read", resource)
when { resource.path == "/work/private" || resource.path like "/work/private/*" };
"#,
    ))
    .unwrap();
    let decision = runtime.evaluate(&request(), 100).unwrap();
    assert_eq!(decision.verdict, DecisionVerdict::Modify);
    let mut input = request();
    input
        .context
        .requested_permissions
        .filesystem
        .rules
        .push(FilesystemRule {
            effect: RuleEffect::Deny,
            path: "/work/private".into(),
            rights: vec![FileRight::Read],
            excluded_paths: Vec::new(),
        });
    input
        .context
        .requested_permissions
        .filesystem
        .rules
        .reverse();
    input
        .context
        .requested_permissions
        .filesystem
        .rules
        .push(input.context.requested_permissions.filesystem.rules[1].clone());
    let unchanged = runtime.evaluate(&input, 100).unwrap();
    assert_eq!(
        unchanged.verdict,
        DecisionVerdict::Allow,
        "{}",
        unchanged.reason
    );
}

#[test]
fn additional_permits_cannot_remove_global_forbids() {
    let mut publication = snapshot(
        r#"
forbid(principal, action == SecCore::Action::"file.exec", resource);
"#,
    );
    publication
        .policies
        .push(member("additional", "permit(principal, action, resource);"));
    assert_eq!(
        PdpRuntime::from_policy_set(publication)
            .unwrap()
            .evaluate(&request(), 100)
            .unwrap()
            .verdict,
        DecisionVerdict::Deny
    );
}

#[test]
fn additional_permits_use_the_current_set_union() {
    let mut publication = snapshot("");
    publication.policies.push(member(
        "additional",
        &format!(
            r#"{FILES}
        permit(principal, action == SecCore::Action::"process.spawn", resource)
        when {{ resource.program == "another-program" }};
    "#
        ),
    ));
    assert_eq!(
        PdpRuntime::from_policy_set(publication)
            .unwrap()
            .evaluate(&request(), 100)
            .unwrap()
            .verdict,
        DecisionVerdict::Allow
    );
}

#[test]
fn warning_and_approval_advice_are_not_erased_by_plain_permits() {
    let runtime = PdpRuntime::from_policy_set(snapshot(
        r#"
@advice("{\"verdict\":\"warn\",\"message\":\"network disabled\"}")
permit(principal, action == SecCore::Action::"process.spawn", resource);
@advice("{\"verdict\":\"escalate\"}")
permit(principal, action == SecCore::Action::"file.exec", resource);
"#,
    ))
    .unwrap();
    let decision = runtime.evaluate(&request(), 100).unwrap();
    assert_eq!(decision.verdict, DecisionVerdict::StepUp);
    assert_eq!(decision.warnings, vec!["network disabled"]);
    assert_eq!(
        decision.approval_requirements,
        vec![ApprovalRequirement {
            target: ApprovalTarget::Execution {
                original_request: false
            },
            message: None,
        }]
    );
}

#[test]
fn transform_returns_final_authorization_and_preserves_original_approval_and_warning() {
    let runtime = PdpRuntime::from_policy_set(snapshot(r#"
@advice("{\"verdict\":\"transform\",\"transform\":{\"path\":\"$target.args\",\"value\":[\"safe.py\"]}}")
permit(principal, action == SecCore::Action::"process.spawn", resource);
@advice("{\"verdict\":\"escalate\"}")
permit(principal, action == SecCore::Action::"file.exec", resource)
when { context.input.args.contains({index: 0, value: "agent.py"}) };
@advice("{\"verdict\":\"warn\",\"message\":\"original warning\"}")
permit(principal, action == SecCore::Action::"process.spawn", resource)
when { context.input.args.contains({index: 0, value: "agent.py"}) };
@advice("{\"verdict\":\"warn\",\"message\":\"final warning\"}")
permit(principal, action == SecCore::Action::"process.spawn", resource)
when { context.input.args.contains({index: 0, value: "safe.py"}) };
"#)).unwrap();
    let input = request();
    let decision = runtime.evaluate(&input, 100).unwrap();
    assert_eq!(
        decision.verdict,
        DecisionVerdict::StepUp,
        "{}",
        decision.reason
    );
    assert_eq!(decision.launch.args, vec!["safe.py"]);
    assert_eq!(decision.request_digest, input.digest().unwrap());
    assert_eq!(decision.warnings, vec!["final warning", "original warning"]);
    assert!(decision.effective_permissions.is_some());
    assert_eq!(
        decision.approval_requirements,
        vec![ApprovalRequirement {
            target: ApprovalTarget::Execution {
                original_request: true
            },
            message: None,
        }]
    );
}

#[test]
fn approval_items_bind_distinct_reasons_to_final_resources_without_extra_grants() {
    let extra = r#"
@advice("{\"verdict\":\"escalate\",\"message\":\"Customer data\"}")
permit(principal, action == SecCore::Action::"file.read", resource)
when { resource.path == "/work/input" || resource.path like "/work/input/*" };
@advice("{\"verdict\":\"escalate\",\"message\":\"Shared reports\"}")
permit(principal, action == SecCore::Action::"file.write", resource)
when { resource.path == "/work/output" || resource.path like "/work/output/*" };
@advice("{\"verdict\":\"escalate\",\"message\":\"Host networking\"}")
permit(principal, action == SecCore::Action::"network.use", resource);
"#;
    let mut input = request();
    input.context.requested_permissions.network.mode = NetworkMode::Full;
    let policy = snapshot(extra);
    let decision = PdpRuntime::from_policy_set(policy)
        .unwrap()
        .evaluate(&input, 100)
        .unwrap();
    let plain = extra
        .lines()
        .filter(|line| !line.starts_with("@advice"))
        .collect::<Vec<_>>()
        .join("\n");
    let control_policy = snapshot(&plain);
    let control = PdpRuntime::from_policy_set(control_policy)
        .unwrap()
        .evaluate(&input, 100)
        .unwrap();
    assert_eq!(decision.verdict, DecisionVerdict::StepUp);
    assert_eq!(control.verdict, DecisionVerdict::Allow);
    assert_eq!(
        decision.effective_permissions,
        control.effective_permissions
    );
    assert_eq!(decision.launch, control.launch);
    assert_eq!(
        decision.approval_requirements,
        vec![
            ApprovalRequirement {
                target: ApprovalTarget::Filesystem {
                    path: "/work/input".into(),
                    right: FileRight::Read,
                    excluded_paths: vec![]
                },
                message: Some("Customer data".into()),
            },
            ApprovalRequirement {
                target: ApprovalTarget::Filesystem {
                    path: "/work/output".into(),
                    right: FileRight::Write,
                    excluded_paths: vec![]
                },
                message: Some("Shared reports".into()),
            },
            ApprovalRequirement {
                target: ApprovalTarget::Network,
                message: Some("Host networking".into())
            },
        ]
    );
}

#[test]
fn approval_regions_exclude_nested_boundaries_and_never_expose_probes_or_denied_writes() {
    let extra = r#"
@advice("{\"verdict\":\"escalate\",\"message\":\"Review workspace writes\"}")
permit(principal, action == SecCore::Action::"file.write", resource)
when { resource.path == "/work" || resource.path like "/work/*" };
forbid(principal, action == SecCore::Action::"file.write", resource)
when { resource.path == "/work/input" || resource.path like "/work/input/*" };
"#;
    let runtime = PdpRuntime::from_policy_set(snapshot(extra)).unwrap();
    let decision = runtime.evaluate(&request(), 100).unwrap();
    assert_eq!(decision.verdict, DecisionVerdict::StepUp);
    assert_eq!(
        decision.approval_requirements,
        vec![ApprovalRequirement {
            target: ApprovalTarget::Filesystem {
                path: "/work".into(),
                right: FileRight::Write,
                excluded_paths: vec!["/work/input".into()],
            },
            message: Some("Review workspace writes".into()),
        }]
    );
    assert!(
        decision
            .effective_permissions
            .unwrap()
            .filesystem
            .rules
            .iter()
            .any(|rule| {
                rule.path == "/work/input"
                    && rule.effect == RuleEffect::Deny
                    && rule.rights.contains(&FileRight::Write)
            })
    );
}

#[test]
fn transformed_resource_approval_becomes_an_execution_reason_and_does_not_claim_an_old_grant() {
    let extra = r#"
@advice("{\"verdict\":\"transform\",\"transform\":{\"path\":\"$target.args\",\"value\":[\"safe.py\"]}}")
permit(principal, action == SecCore::Action::"process.spawn", resource);
@advice("{\"verdict\":\"escalate\",\"reason\":\"WRITE_REVIEW\",\"message\":\"Shared workspace\"}")
permit(principal, action == SecCore::Action::"file.write", resource)
when { resource.path == "/work" || resource.path like "/work/*" };
"#;
    let decision = PdpRuntime::from_policy_set(snapshot(extra))
        .unwrap()
        .evaluate(&request(), 100)
        .unwrap();
    assert_eq!(decision.launch.args, vec!["safe.py"]);
    assert_eq!(decision.reason, "Shared workspace");
    assert_eq!(
        decision.approval_requirements,
        vec![
            ApprovalRequirement {
                target: ApprovalTarget::Execution {
                    original_request: true
                },
                message: Some("Shared workspace".into())
            },
            ApprovalRequirement {
                target: ApprovalTarget::Filesystem {
                    path: "/work".into(),
                    right: FileRight::Write,
                    excluded_paths: vec![]
                },
                message: Some("Shared workspace".into())
            },
        ]
    );
}

#[test]
fn machine_reason_codes_are_preserved_in_decisions_but_not_used_as_item_messages() {
    let extra = r#"
@advice("{\"verdict\":\"escalate\",\"reason\":\"EXEC_REVIEW\"}")
permit(principal, action == SecCore::Action::"process.spawn", resource);
"#;
    let decision = PdpRuntime::from_policy_set(snapshot(extra))
        .unwrap()
        .evaluate(&request(), 100)
        .unwrap();
    assert_eq!(decision.reason, "EXEC_REVIEW");
    assert_eq!(decision.approval_requirements.len(), 1);
    assert_eq!(decision.approval_requirements[0].message, None);
}

#[test]
fn transform_returns_modify_in_one_evaluation_but_cannot_bypass_final_forbid() {
    let transform = r#"
@advice("{\"verdict\":\"transform\",\"transform\":{\"path\":\"$target.args\",\"value\":[\"safe.py\"]}}")
permit(principal, action == SecCore::Action::"process.spawn", resource);
"#;
    let runtime = PdpRuntime::from_policy_set(snapshot(transform)).unwrap();
    let input = request();
    let decision = runtime.evaluate(&input, 100).unwrap();
    assert_eq!(
        decision.verdict,
        DecisionVerdict::Modify,
        "{}",
        decision.reason
    );
    assert_eq!(decision.launch.args, vec!["safe.py"]);
    assert_eq!(decision.request_digest, input.digest().unwrap());
    let mut publication = snapshot(transform);
    publication.policies.push(member(
        "additional",
        &format!(
            r#"{FILES}{COMMAND}
forbid(principal, action == SecCore::Action::"file.exec", resource)
when {{ context.input.args.contains({{index: 0, value: "safe.py"}}) }};
"#
        ),
    ));
    let denied = PdpRuntime::from_policy_set(publication)
        .unwrap()
        .evaluate(&input, 100)
        .unwrap();
    assert_eq!(denied.verdict, DecisionVerdict::Deny);
    assert!(denied.effective_permissions.is_none());
}

#[test]
fn transformed_absolute_program_can_use_prepared_facts() {
    let runtime = PdpRuntime::from_policy_set(snapshot(r#"
@advice("{\"verdict\":\"transform\",\"transform\":{\"path\":\"$target.program\",\"value\":\"/usr/bin/python3\"}}")
permit(principal, action == SecCore::Action::"process.spawn", resource);
"#)).unwrap();
    let decision = runtime.evaluate(&request(), 100).unwrap();
    assert_eq!(
        decision.verdict,
        DecisionVerdict::Modify,
        "{}",
        decision.reason
    );
    assert_eq!(decision.launch.program, "/usr/bin/python3");
    assert_eq!(decision.launch.resolved_program, "/usr/bin/python3");
}

#[test]
fn unresolved_program_defers_without_an_executable_candidate() {
    let runtime = PdpRuntime::from_policy_set(snapshot(r#"
@advice("{\"verdict\":\"transform\",\"transform\":{\"path\":\"$target.program\",\"value\":\"python-safe\"}}")
permit(principal, action == SecCore::Action::"process.spawn", resource);
"#)).unwrap();
    let input = request();
    let first = runtime.evaluate(&input, 100).unwrap();
    assert_eq!(first.verdict, DecisionVerdict::Defer, "{}", first.reason);
    assert!(first.effective_permissions.is_none());
}

#[test]
fn final_authorization_rejects_a_second_distinct_transform() {
    let runtime = PdpRuntime::from_policy_set(snapshot(r#"
@advice("{\"verdict\":\"transform\",\"transform\":{\"path\":\"$target.args\",\"value\":[\"safe.py\"]}}")
permit(principal, action == SecCore::Action::"process.spawn", resource)
when { context.input.args.contains({index: 0, value: "agent.py"}) };
@advice("{\"verdict\":\"transform\",\"transform\":{\"path\":\"$target.args\",\"value\":[\"other.py\"]}}")
permit(principal, action == SecCore::Action::"process.spawn", resource)
when { context.input.args.contains({index: 0, value: "safe.py"}) };
"#)).unwrap();
    let decision = runtime.evaluate(&request(), 100).unwrap();
    assert_eq!(decision.verdict, DecisionVerdict::Deny);
    assert!(decision.reason.contains("second distinct transform"));
}

#[test]
fn runtime_file_conditions_and_arbitrary_globs_are_not_projected() {
    for condition in [
        "context.input.program == \"/usr/bin/python3\"",
        "resource.path like \"/work/sec*\"",
    ] {
        let text = format!(
            "forbid(principal, action == SecCore::Action::\"file.read\", resource) when {{ {condition} }};"
        );
        assert!(PdpRuntime::from_policy_set(snapshot(&text)).is_err());
    }
}

#[test]
fn full_network_is_never_silently_downgraded() {
    let runtime = PdpRuntime::from_policy_set(snapshot("")).unwrap();
    let mut input = request();
    input.context.requested_permissions.network.mode = NetworkMode::Full;
    assert_eq!(
        runtime.evaluate(&input, 100).unwrap().verdict,
        DecisionVerdict::Deny
    );
}

#[test]
fn noexec_requires_backend_support_and_cannot_cover_initial_program() {
    let runtime = PdpRuntime::from_policy_set(snapshot("")).unwrap();
    let mut input = request();
    input.context.requested_permissions.execution.deny_paths = vec!["/work".into()];
    assert_eq!(
        runtime.evaluate(&input, 100).unwrap().verdict,
        DecisionVerdict::Allow
    );
    input.context.requested_permissions.execution.deny_paths = vec!["/usr/bin/python3".into()];
    assert_eq!(
        runtime.evaluate(&input, 100).unwrap().verdict,
        DecisionVerdict::Deny
    );
}

#[test]
fn evaluation_errors_are_not_ignored_when_another_rule_permits() {
    let runtime = PdpRuntime::from_policy_set(snapshot(
        r#"
permit(principal, action == SecCore::Action::"process.spawn", resource)
when { context.input.arg_count + 9223372036854775807 > 0 };
"#,
    ))
    .unwrap();
    let decision = runtime.evaluate(&request(), 100).unwrap();
    assert_eq!(decision.verdict, DecisionVerdict::Deny);
    assert!(decision.reason.contains("evaluation failed"));
}

#[test]
fn command_rules_can_distinguish_ordered_arguments() {
    let runtime = PdpRuntime::from_policy_set(snapshot(
        r#"
forbid(principal, action == SecCore::Action::"process.spawn", resource)
when { context.input.args.contains({index: 0, value: "-c"}) };
"#,
    ))
    .unwrap();
    let mut input = request();
    input.context.launch.args = vec!["-c".into(), "print(1)".into()];
    assert_eq!(
        runtime.evaluate(&input, 100).unwrap().verdict,
        DecisionVerdict::Deny
    );
    input.context.launch.args = vec!["agent.py".into(), "-c".into()];
    assert_eq!(
        runtime.evaluate(&input, 100).unwrap().verdict,
        DecisionVerdict::Allow
    );
}

#[test]
fn executable_alias_does_not_bypass_file_invocation_forbid() {
    let runtime = PdpRuntime::from_policy_set(snapshot(
        r#"
forbid(principal, action == SecCore::Action::"file.exec", resource)
when { resource.path == "/usr/bin/python3" };
"#,
    ))
    .unwrap();
    let mut input = request();
    input.context.launch.program = "/bin/python3".into();
    input.resource.id.clone_from(&input.context.launch.program);
    assert_eq!(
        runtime.evaluate(&input, 100).unwrap().verdict,
        DecisionVerdict::Deny
    );
}

#[test]
fn full_network_requires_policy_and_request_grants() {
    let publication = snapshot(
        r#"
permit(principal, action == SecCore::Action::"network.use", resource == SecCore::Network::"all");
"#,
    );
    let runtime = PdpRuntime::from_policy_set(publication).unwrap();
    for requested in [NetworkMode::Full, NetworkMode::Off] {
        let mut input = request();
        input.context.requested_permissions.network.mode = requested;
        let decision = runtime.evaluate(&input, 100).unwrap();
        assert_eq!(
            decision.verdict,
            DecisionVerdict::Allow,
            "{}",
            decision.reason
        );
        assert_eq!(
            decision.effective_permissions.unwrap().network.mode,
            requested
        );
    }
}

#[test]
fn conflicting_transform_requirements_fail_closed() {
    let runtime = PdpRuntime::from_policy_set(snapshot(r#"
@advice("{\"verdict\":\"transform\",\"transform\":{\"path\":\"$target.args\",\"value\":[\"one.py\"]}}")
permit(principal, action == SecCore::Action::"process.spawn", resource);
@advice("{\"verdict\":\"transform\",\"transform\":{\"path\":\"$target.args\",\"value\":[\"two.py\"]}}")
permit(principal, action == SecCore::Action::"file.exec", resource);
"#)).unwrap();
    let decision = runtime.evaluate(&request(), 100).unwrap();
    assert_eq!(decision.verdict, DecisionVerdict::Deny);
    assert!(decision.reason.contains("conflicting transform"));
}

#[test]
fn removed_write_grants_do_not_leave_approval_or_warning_requirements() {
    let runtime = PdpRuntime::from_policy_set(snapshot(
        r#"
forbid(principal, action == SecCore::Action::"file.read", resource)
when { resource.path == "/work/secrets" || resource.path like "/work/secrets/*" };
@advice("{\"verdict\":\"escalate\"}")
permit(principal, action == SecCore::Action::"file.write", resource)
when { resource.path == "/work/secrets" || resource.path like "/work/secrets/*" };
@advice("{\"verdict\":\"warn\",\"message\":\"writing secrets\"}")
permit(principal, action == SecCore::Action::"file.write", resource)
when { resource.path == "/work/secrets" || resource.path like "/work/secrets/*" };
"#,
    ))
    .unwrap();
    let input = request();
    let decision = runtime.evaluate(&input, 100).unwrap();
    assert_eq!(
        decision.verdict,
        DecisionVerdict::Modify,
        "{}",
        decision.reason
    );
    assert!(decision.warnings.is_empty());
    assert!(
        decision
            .effective_permissions
            .unwrap()
            .filesystem
            .rules
            .iter()
            .any(|rule| rule.path == "/work/secrets"
                && rule.effect == RuleEffect::Deny
                && rule.rights.contains(&FileRight::Read))
    );
}

#[test]
fn cwd_transform_preserves_absolute_program_resolution_and_defers_relative_programs() {
    let runtime = PdpRuntime::from_policy_set(snapshot(r#"
@advice("{\"verdict\":\"transform\",\"transform\":{\"path\":\"$target.cwd\",\"value\":\"/work/secrets\"}}")
permit(principal, action == SecCore::Action::"process.spawn", resource);
"#)).unwrap();
    let mut input = request();
    input.context.launch.program = "/bin/python3".into();
    input.resource.id.clone_from(&input.context.launch.program);
    let decision = runtime.evaluate(&input, 100).unwrap();
    assert_eq!(
        decision.verdict,
        DecisionVerdict::Modify,
        "{}",
        decision.reason
    );
    assert_eq!(decision.launch.cwd, "/work/secrets");
    assert_eq!(decision.launch.program, "/bin/python3");
    assert_eq!(decision.launch.resolved_program, "/usr/bin/python3");

    for program in ["./python3", "python3"] {
        input.context.launch.program = program.into();
        input.resource.id.clone_from(&input.context.launch.program);
        let decision = runtime.evaluate(&input, 100).unwrap();
        assert_eq!(decision.verdict, DecisionVerdict::Defer, "{program}");
        assert!(decision.effective_permissions.is_none());
    }
}

#[test]
fn wire_request_contains_only_authorization_inputs() {
    let value = serde_json::to_value(request()).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 5);
    assert_eq!(value["context"]["launch"].as_object().unwrap().len(), 4);
    for key in ["path_facts", "capabilities", "continuation"] {
        let mut extra = value.clone();
        extra[key] = json!(null);
        assert!(serde_json::from_value::<OsAuthorizationRequest>(extra).is_err());
    }
    let mut extra = value;
    extra["context"]["launch"]["env"] = json!({"SECRET":"hidden"});
    assert!(serde_json::from_value::<OsAuthorizationRequest>(extra).is_err());
}

#[test]
fn point_rights_on_an_unknown_object_require_a_local_file_check() {
    let runtime = PdpRuntime::from_policy_set(snapshot(
        r#"
forbid(principal, action == SecCore::Action::"file.read", resource)
when { resource.path == "/work/one" };"#,
    ))
    .unwrap();
    let decision = runtime.evaluate(&request(), 100).unwrap();
    assert_eq!(decision.verdict, DecisionVerdict::Modify);
    assert_eq!(decision.require_files, vec!["/work/one"]);
}

#[test]
fn decision_ttl_is_fixed_and_overflow_fails() {
    let runtime = PdpRuntime::from_policy_set(snapshot("")).unwrap();
    let decision = runtime.evaluate(&request(), 100).unwrap();
    assert_eq!(decision.expires_at, 160);
    assert!(runtime.evaluate(&request(), u64::MAX).is_err());
}
