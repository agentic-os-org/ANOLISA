use std::collections::BTreeMap;

use asc_pdp_runtime::{PdpRuntime, PolicySnapshot};
use asc_policy_types::cedar::{CedarPolicySource, CedarSchemaReference};
use asc_policy_types::identifiers::{ResourceId, Revision};
use asc_policy_types::*;
use serde_json::json;

fn source(text: &str) -> CedarPolicySource {
    CedarPolicySource {
        schema: CedarSchemaReference {
            schema_id: ResourceId::new("seccore.system").unwrap(),
            revision: Revision::new(2).unwrap(),
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

const FILES: &str = r#"
permit(principal, action == SecCore::Action::"file.read", resource)
when { resource.path == "/usr/bin/python3" || resource.path == "/work" || resource.path like "/work/*" };
permit(principal, action == SecCore::Action::"file.write", resource)
when { resource.path == "/work" || resource.path like "/work/*" };
"#;
const COMMAND: &str = r#"
permit(principal, action == SecCore::Action::"process.spawn", resource)
when { resource.program == "python3" && principal has agent_type && principal.agent_type == "codex" };
"#;

fn snapshot(extra: &str) -> PolicySnapshot {
    PolicySnapshot {
        version: "snapshot-1".into(),
        baseline: source(&format!("{FILES}{COMMAND}{extra}")),
        task: None,
        environment_permissions: permissions(),
        ttl_seconds: 60,
    }
}

fn request() -> AuthorizationRequest {
    AuthorizationRequest {
        request_type: RequestType::Os,
        request_id: "request-1".into(),
        execution_id: "execution-1".into(),
        principal: AgentPrincipal {
            agent_id: "worker".into(),
            agent_type: "codex".into(),
        },
        launch: LaunchSpec {
            program: "python3".into(),
            resolved_program: "/usr/bin/python3".into(),
            args: vec!["agent.py".into()],
            cwd: "/work".into(),
            env: BTreeMap::new(),
        },
        requested_permissions: permissions(),
        path_facts: vec![
            PathFact {
                path: "/usr/bin/python3".into(),
                canonical_path: "/usr/bin/python3".into(),
                is_directory: false,
                device: 1,
                inode: 1,
            },
            PathFact {
                path: "/work".into(),
                canonical_path: "/work".into(),
                is_directory: true,
                device: 1,
                inode: 2,
            },
            PathFact {
                path: "/work/secrets".into(),
                canonical_path: "/work/secrets".into(),
                is_directory: true,
                device: 1,
                inode: 3,
            },
        ],
        capabilities: PepCapabilities {
            strict_filesystem: true,
            noexec: true,
            network_off: true,
            ask: true,
        },
        continuation: None,
    }
}

#[test]
fn one_invocation_view_can_permit_but_either_view_can_forbid() {
    let runtime = PdpRuntime::from_snapshot(snapshot("")).unwrap();
    assert_eq!(
        runtime.evaluate(&request(), 100).unwrap().verdict,
        DecisionVerdict::Allow
    );
    let runtime = PdpRuntime::from_snapshot(snapshot(
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
    let runtime = PdpRuntime::from_snapshot(snapshot(
        r#"
forbid(principal, action == SecCore::Action::"file.read", resource)
when { resource.path == "/work/secrets" || resource.path like "/work/secrets/*" };
"#,
    ))
    .unwrap();
    let decision = runtime.evaluate(&request(), 100).unwrap();
    assert_eq!(
        decision.verdict,
        DecisionVerdict::Allow,
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
    publication.baseline = source(&format!(
        r#"{COMMAND}
permit(principal, action == SecCore::Action::"file.read", resource)
when {{ resource.path == "/usr/bin/python3" || resource.path == "/work" }};
"#
    ));
    let decision = PdpRuntime::from_snapshot(publication)
        .unwrap()
        .evaluate(&request(), 100)
        .unwrap();
    assert_eq!(decision.verdict, DecisionVerdict::Deny);
    assert!(decision.reason.contains("directory-only"));
}

#[test]
fn missing_policy_boundary_facts_defer_without_a_grant() {
    let runtime = PdpRuntime::from_snapshot(snapshot(
        r#"
forbid(principal, action == SecCore::Action::"file.read", resource)
when { resource.path == "/work/private" || resource.path like "/work/private/*" };
"#,
    ))
    .unwrap();
    let decision = runtime.evaluate(&request(), 100).unwrap();
    assert_eq!(decision.verdict, DecisionVerdict::Defer);
    assert_eq!(decision.missing_paths, vec!["/work/private"]);
    assert!(decision.effective_permissions.is_none());
}

#[test]
fn dynamic_permits_cannot_remove_baseline_forbids() {
    let mut publication = snapshot(
        r#"
forbid(principal, action == SecCore::Action::"file.exec", resource);
"#,
    );
    publication.task = Some(source("permit(principal, action, resource);"));
    assert_eq!(
        PdpRuntime::from_snapshot(publication)
            .unwrap()
            .evaluate(&request(), 100)
            .unwrap()
            .verdict,
        DecisionVerdict::Deny
    );
}

#[test]
fn warning_and_approval_advice_are_not_erased_by_plain_permits() {
    let runtime = PdpRuntime::from_snapshot(snapshot(
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
    let mut input = request();
    input.capabilities.ask = false;
    assert_eq!(
        runtime.evaluate(&input, 100).unwrap().verdict,
        DecisionVerdict::Deny
    );
}

#[test]
fn transform_returns_final_authorization_and_preserves_original_approval_and_warning() {
    let runtime = PdpRuntime::from_snapshot(snapshot(r#"
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
    assert!(decision.continuation.is_none());
    assert!(decision.candidate_launch.is_none());
}

#[test]
fn transform_returns_modify_in_one_evaluation_but_cannot_bypass_final_forbid() {
    let transform = r#"
@advice("{\"verdict\":\"transform\",\"transform\":{\"path\":\"$target.args\",\"value\":[\"safe.py\"]}}")
permit(principal, action == SecCore::Action::"process.spawn", resource);
"#;
    let runtime = PdpRuntime::from_snapshot(snapshot(transform)).unwrap();
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
    assert!(decision.continuation.is_none());
    let mut publication = snapshot(transform);
    publication.task = Some(source(&format!(
        r#"{FILES}{COMMAND}
forbid(principal, action == SecCore::Action::"file.exec", resource)
when {{ context.input.args.contains({{index: 0, value: "safe.py"}}) }};
"#
    )));
    let denied = PdpRuntime::from_snapshot(publication)
        .unwrap()
        .evaluate(&input, 100)
        .unwrap();
    assert_eq!(denied.verdict, DecisionVerdict::Deny);
    assert!(denied.effective_permissions.is_none());
}

#[test]
fn transformed_absolute_program_can_use_prepared_facts() {
    let runtime = PdpRuntime::from_snapshot(snapshot(r#"
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
    assert!(decision.continuation.is_none());
}

#[test]
fn unresolved_program_defers_and_continuation_cannot_widen_original_permissions() {
    let runtime = PdpRuntime::from_snapshot(snapshot(r#"
@advice("{\"verdict\":\"transform\",\"transform\":{\"path\":\"$target.program\",\"value\":\"python-safe\"}}")
permit(principal, action == SecCore::Action::"process.spawn", resource);
"#)).unwrap();
    let mut input = request();
    let first = runtime.evaluate(&input, 100).unwrap();
    assert_eq!(first.verdict, DecisionVerdict::Defer, "{}", first.reason);
    assert!(first.effective_permissions.is_none());
    input.continuation = first.continuation;
    input.launch = first.candidate_launch.unwrap();
    assert!(input.launch.resolved_program.is_empty());
    input.launch.resolved_program = "/usr/bin/python3".into();
    input.request_id = "request-2".into();
    let mut widened = input.clone();
    widened.requested_permissions.network.mode = NetworkMode::Full;
    assert_eq!(
        runtime.evaluate(&widened, 101).unwrap().verdict,
        DecisionVerdict::Deny
    );
    let final_decision = runtime.evaluate(&input, 102).unwrap();
    assert_eq!(
        final_decision.verdict,
        DecisionVerdict::Modify,
        "{}",
        final_decision.reason
    );
    assert_eq!(
        runtime.evaluate(&input, 103).unwrap().verdict,
        DecisionVerdict::Deny
    );
}

#[test]
fn final_authorization_rejects_a_second_distinct_transform() {
    let runtime = PdpRuntime::from_snapshot(snapshot(r#"
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
        assert!(PdpRuntime::from_snapshot(snapshot(&text)).is_err());
    }
}

#[test]
fn full_network_is_never_silently_downgraded() {
    let runtime = PdpRuntime::from_snapshot(snapshot("")).unwrap();
    let mut input = request();
    input.requested_permissions.network.mode = NetworkMode::Full;
    assert_eq!(
        runtime.evaluate(&input, 100).unwrap().verdict,
        DecisionVerdict::Deny
    );
}

#[test]
fn noexec_requires_backend_support_and_cannot_cover_initial_program() {
    let runtime = PdpRuntime::from_snapshot(snapshot("")).unwrap();
    let mut input = request();
    input.requested_permissions.execution.deny_paths = vec!["/work".into()];
    input.capabilities.noexec = false;
    assert_eq!(
        runtime.evaluate(&input, 100).unwrap().verdict,
        DecisionVerdict::Deny
    );
    input.capabilities.noexec = true;
    input.requested_permissions.execution.deny_paths = vec!["/usr/bin/python3".into()];
    assert_eq!(
        runtime.evaluate(&input, 100).unwrap().verdict,
        DecisionVerdict::Deny
    );
}

#[test]
fn canonical_policy_denies_apply_to_every_declared_mount_alias() {
    let mut input = request();
    for path in ["/lib", "/usr/lib"] {
        input
            .requested_permissions
            .filesystem
            .rules
            .push(FilesystemRule {
                effect: RuleEffect::Allow,
                path: path.into(),
                rights: vec![FileRight::Read],
            });
        input.path_facts.push(PathFact {
            path: path.into(),
            canonical_path: "/usr/lib".into(),
            is_directory: true,
            device: 1,
            inode: 10,
        });
        input.path_facts.push(PathFact {
            path: format!("{path}/private"),
            canonical_path: "/usr/lib/private".into(),
            is_directory: true,
            device: 1,
            inode: 11,
        });
    }
    let mut publication = snapshot(
        r#"
permit(principal, action == SecCore::Action::"file.read", resource)
when { resource.path == "/usr/lib" || resource.path like "/usr/lib/*" };
forbid(principal, action == SecCore::Action::"file.read", resource)
when { resource.path == "/usr/lib/private" || resource.path like "/usr/lib/private/*" };
"#,
    );
    publication
        .environment_permissions
        .filesystem
        .rules
        .push(FilesystemRule {
            effect: RuleEffect::Allow,
            path: "/usr/lib".into(),
            rights: vec![FileRight::Read],
        });
    let decision = PdpRuntime::from_snapshot(publication)
        .unwrap()
        .evaluate(&input, 100)
        .unwrap();
    assert_eq!(
        decision.verdict,
        DecisionVerdict::Allow,
        "{}",
        decision.reason
    );
    let rules = decision.effective_permissions.unwrap().filesystem.rules;
    for path in ["/lib", "/usr/lib"] {
        assert!(
            rules
                .iter()
                .any(|rule| rule.path == path && rule.effect == RuleEffect::Allow)
        );
        assert!(
            rules
                .iter()
                .any(|rule| rule.path == format!("{path}/private")
                    && rule.effect == RuleEffect::Deny)
        );
    }
}

#[test]
fn supplied_alias_facts_cannot_choose_the_directory_representative() {
    let mut publication = snapshot("");
    publication.baseline = source(&format!(
        r#"{COMMAND}
permit(principal, action == SecCore::Action::"file.read", resource)
when {{ resource.path == "/usr/bin/python3" || resource.path == "/work" }};
"#
    ));
    let mut input = request();
    input.path_facts.push(PathFact {
        path: "/work/__asc_permission_region_0__".into(),
        canonical_path: "/usr/bin/python3".into(),
        is_directory: false,
        device: 1,
        inode: 1,
    });
    let decision = PdpRuntime::from_snapshot(publication)
        .unwrap()
        .evaluate(&input, 100)
        .unwrap();
    assert_eq!(decision.verdict, DecisionVerdict::Deny);
    assert!(decision.reason.contains("directory-only"));
}

#[test]
fn evaluation_errors_are_not_ignored_when_another_rule_permits() {
    let runtime = PdpRuntime::from_snapshot(snapshot(
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
    let runtime = PdpRuntime::from_snapshot(snapshot(
        r#"
forbid(principal, action == SecCore::Action::"process.spawn", resource)
when { context.input.args.contains({index: 0, value: "-c"}) };
"#,
    ))
    .unwrap();
    let mut input = request();
    input.launch.args = vec!["-c".into(), "print(1)".into()];
    assert_eq!(
        runtime.evaluate(&input, 100).unwrap().verdict,
        DecisionVerdict::Deny
    );
    input.launch.args = vec!["agent.py".into(), "-c".into()];
    assert_eq!(
        runtime.evaluate(&input, 100).unwrap().verdict,
        DecisionVerdict::Allow
    );
}

#[test]
fn executable_alias_does_not_bypass_file_invocation_forbid() {
    let runtime = PdpRuntime::from_snapshot(snapshot(
        r#"
forbid(principal, action == SecCore::Action::"file.exec", resource)
when { resource.path == "/usr/bin/python3" };
"#,
    ))
    .unwrap();
    let mut input = request();
    input.launch.resolved_program = "/bin/python3".into();
    input.path_facts.push(PathFact {
        path: "/bin/python3".into(),
        canonical_path: "/usr/bin/python3".into(),
        is_directory: false,
        device: 1,
        inode: 1,
    });
    assert_eq!(
        runtime.evaluate(&input, 100).unwrap().verdict,
        DecisionVerdict::Deny
    );
}

#[test]
fn full_network_requires_both_policy_and_environment_grants() {
    let mut publication = snapshot(
        r#"
permit(principal, action == SecCore::Action::"network.use", resource == SecCore::Network::"all");
"#,
    );
    publication.environment_permissions.network.mode = NetworkMode::Full;
    let mut input = request();
    input.requested_permissions.network.mode = NetworkMode::Full;
    let decision = PdpRuntime::from_snapshot(publication)
        .unwrap()
        .evaluate(&input, 100)
        .unwrap();
    assert_eq!(
        decision.verdict,
        DecisionVerdict::Allow,
        "{}",
        decision.reason
    );
    assert_eq!(
        decision.effective_permissions.unwrap().network.mode,
        NetworkMode::Full
    );
}

#[test]
fn conflicting_transform_requirements_fail_closed() {
    let runtime = PdpRuntime::from_snapshot(snapshot(r#"
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
fn canonical_launch_paths_can_use_declared_alias_grants() {
    let mut input = request();
    for (index, alias, canonical, directory, inode) in [
        (0, "/bin/python3", "/usr/bin/python3", false, 1),
        (1, "/workspace", "/work", true, 2),
    ] {
        input.requested_permissions.filesystem.rules[index].path = alias.into();
        input.path_facts.push(PathFact {
            path: alias.into(),
            canonical_path: canonical.into(),
            is_directory: directory,
            device: 1,
            inode,
        });
    }
    let decision = PdpRuntime::from_snapshot(snapshot(""))
        .unwrap()
        .evaluate(&input, 100)
        .unwrap();
    assert_eq!(
        decision.verdict,
        DecisionVerdict::Allow,
        "{}",
        decision.reason
    );
    let rules = decision.effective_permissions.unwrap().filesystem.rules;
    for alias in ["/bin/python3", "/workspace"] {
        assert!(rules.iter().any(|rule| {
            rule.path == alias
                && rule.effect == RuleEffect::Allow
                && rule.rights.contains(&FileRight::Read)
        }));
    }
}

#[test]
fn removed_write_grants_do_not_leave_approval_or_warning_requirements() {
    let runtime = PdpRuntime::from_snapshot(snapshot(
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
    let mut input = request();
    input.capabilities.ask = false;
    let decision = runtime.evaluate(&input, 100).unwrap();
    assert_eq!(
        decision.verdict,
        DecisionVerdict::Allow,
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
fn transformed_cwd_accepts_only_its_reprepared_directory_identity() {
    let runtime = PdpRuntime::from_snapshot(snapshot(
        r#"
@advice("{\"verdict\":\"transform\",\"transform\":{\"path\":\"$target.cwd\",\"value\":\"/work/link\"}}")
permit(principal, action == SecCore::Action::"process.spawn", resource);
"#,
    ))
    .unwrap();
    let mut input = request();
    let first = runtime.evaluate(&input, 100).unwrap();
    assert_eq!(first.verdict, DecisionVerdict::Defer, "{}", first.reason);
    input.continuation = first.continuation;
    input.launch = first.candidate_launch.unwrap();
    input.launch.resolved_program = "/usr/bin/python3".into();
    input.launch.cwd = "/work/real".into();
    input.request_id = "request-2".into();
    input.path_facts.push(PathFact {
        path: "/work/real".into(),
        canonical_path: "/work/real".into(),
        is_directory: true,
        device: 1,
        inode: 4,
    });
    let missing_alias = runtime.evaluate(&input, 101).unwrap();
    assert_eq!(missing_alias.verdict, DecisionVerdict::Deny);
    assert!(missing_alias.reason.contains("continuation does not match"));
    let mut alias = PathFact {
        path: "/work/link".into(),
        canonical_path: "/work/other".into(),
        is_directory: true,
        device: 1,
        inode: 4,
    };
    input.path_facts.push(alias.clone());
    assert_eq!(
        runtime.evaluate(&input, 102).unwrap().verdict,
        DecisionVerdict::Deny
    );
    input.path_facts.pop();
    alias.canonical_path = "/work/real".into();
    alias.is_directory = false;
    input.path_facts.push(alias.clone());
    assert_eq!(
        runtime.evaluate(&input, 103).unwrap().verdict,
        DecisionVerdict::Deny
    );
    input.path_facts.pop();
    alias.is_directory = true;
    input.path_facts.push(alias);
    let decision = runtime.evaluate(&input, 104).unwrap();
    assert_eq!(
        decision.verdict,
        DecisionVerdict::Modify,
        "{}",
        decision.reason
    );
    assert_eq!(decision.launch.cwd, "/work/real");
}

#[test]
fn known_transformed_cwd_is_authorized_without_repreparation() {
    let runtime = PdpRuntime::from_snapshot(snapshot(r#"
@advice("{\"verdict\":\"transform\",\"transform\":{\"path\":\"$target.cwd\",\"value\":\"/work/secrets\"}}")
permit(principal, action == SecCore::Action::"process.spawn", resource);
"#)).unwrap();
    let mut input = request();
    input.launch.program = "/usr/bin/python3".into();
    let decision = runtime.evaluate(&input, 100).unwrap();
    assert_eq!(
        decision.verdict,
        DecisionVerdict::Modify,
        "{}",
        decision.reason
    );
    assert_eq!(decision.launch.cwd, "/work/secrets");
    assert!(decision.continuation.is_none());
}

#[test]
fn prepared_cwd_alias_and_canonical_identity_return_modify_without_defer() {
    let runtime = PdpRuntime::from_snapshot(snapshot(r#"
@advice("{\"verdict\":\"transform\",\"transform\":{\"path\":\"$target.cwd\",\"value\":\"/work/link\"}}")
permit(principal, action == SecCore::Action::"process.spawn", resource);
"#)).unwrap();
    let mut input = request();
    input.launch.program = "/usr/bin/python3".into();
    for path in ["/work/link", "/work/real"] {
        input.path_facts.push(PathFact {
            path: path.into(),
            canonical_path: "/work/real".into(),
            is_directory: true,
            device: 1,
            inode: 4,
        });
    }
    let decision = runtime.evaluate(&input, 100).unwrap();
    assert_eq!(
        decision.verdict,
        DecisionVerdict::Modify,
        "{}",
        decision.reason
    );
    assert_eq!(decision.launch.cwd, "/work/real");
    assert!(decision.continuation.is_none());
    assert!(decision.missing_paths.is_empty());
}
