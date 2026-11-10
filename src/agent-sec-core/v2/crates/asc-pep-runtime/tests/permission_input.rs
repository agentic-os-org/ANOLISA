//! Compatibility and coarse Cedar projection, including real PDP request preparation.

use std::collections::BTreeMap;

use asc_daemon_protocol::authorization::*;
use asc_pep_runtime::{PermissionInput, PermissionProvider};
use serde_json::json;

fn launch() -> LaunchSpec {
    LaunchSpec {
        program: "/usr/bin/true".into(),
        resolved_program: "/usr/bin/true".into(),
        args: vec![],
        cwd: "/".into(),
        env: BTreeMap::new(),
    }
}

fn resolve(text: &str) -> OsPermissionSet {
    PermissionInput::parse(text)
        .unwrap()
        .resolve(&launch())
        .unwrap()
}

fn allows(permissions: &OsPermissionSet, path: &str, right: FileRight) -> bool {
    let applicable: Vec<_> = permissions
        .filesystem
        .rules
        .iter()
        .filter(|rule| {
            rule.path == "/" || path == rule.path || path.starts_with(&format!("{}/", rule.path))
        })
        .collect();
    !applicable.iter().any(|rule| {
        rule.effect == RuleEffect::Deny
            && (rule.rights.contains(&right) || rule.rights.contains(&FileRight::Read))
    }) && applicable.iter().any(|rule| {
        rule.effect == RuleEffect::Allow
            && (rule.rights.contains(&right)
                || right == FileRight::Read && rule.rights.contains(&FileRight::Write))
    })
}

#[test]
fn json_permissions_keep_order_denies_network_and_noexec() {
    let json = json!({
        "version": 1, "filesystem": {"default": "deny", "rules": [
            {"effect": "allow", "path": "/work", "rights": ["write", "read"]},
            {"effect": "deny", "path": "/work/private", "rights": ["read"]}
        ]}, "execution": {"deny_paths": ["/work"]}, "network": {"mode": "full"}
    });
    let expected: OsPermissionSet = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(resolve(&json.to_string()), expected);
    let duplicate = json
        .to_string()
        .replacen("\"version\":1", "\"version\":1,\"version\":1", 1);
    assert!(PermissionInput::parse(&duplicate).is_err());
    for bad in [
        "{broken",
        r#"{"version":1,"filesystem":{},"execution":{},"network":{}}"#,
    ] {
        assert!(PermissionInput::parse(bad).is_err());
    }
    let mut unknown = json;
    unknown["unexpected"] = json!(true);
    assert!(PermissionInput::parse(&unknown.to_string()).is_err());
}

const TREE: &str = r#"
permit(principal, action == SecCore::Action::"file.read", resource);
permit(principal, action == SecCore::Action::"file.write", resource)
when { resource.path == "/work" || resource.path like "/work/*" };
forbid(principal, action == SecCore::Action::"file.read", resource)
when { resource.path == "/work/private" || resource.path like "/work/private/*" };
"#;

#[test]
fn cedar_regions_preserve_exclusions_and_directory_boundaries() {
    let permissions = resolve(TREE);
    assert!(allows(&permissions, "/usr/lib/libc.so", FileRight::Read));
    assert!(allows(&permissions, "/work/new-file", FileRight::Write));
    assert!(!allows(&permissions, "/work/private/key", FileRight::Read));
    assert!(!allows(&permissions, "/work/private/key", FileRight::Write));
    assert!(!allows(
        &permissions,
        "/work-other/report",
        FileRight::Write
    ));
    assert_eq!(permissions.network.mode, NetworkMode::Off);
    assert!(permissions.execution.deny_paths.is_empty());
    let envelope = json!({"language":"cedar", "content":{
        "schema":{"schemaId":"seccore.system","revision":1}, "text":TREE
    }});
    assert_eq!(resolve(&envelope.to_string()), permissions);
}

#[test]
fn permitted_mutations_map_to_write_even_with_delete_forbidden() {
    for operation in ["write", "create", "rename", "setattr"] {
        let permissions = resolve(&format!(
            r#"
permit(principal, action == SecCore::Action::"file.write", resource)
when {{ (resource.path == "/work" || resource.path like "/work/*")
  && context.input.operation == SecCore::WriteOperation::"{operation}" }};
forbid(principal, action == SecCore::Action::"file.write", resource)
when {{ context.input.operation == SecCore::WriteOperation::"delete" }};
"#
        ));
        assert!(allows(&permissions, "/work/report", FileRight::Write));
        assert!(allows(&permissions, "/work/report", FileRight::Read));
        assert!(!allows(&permissions, "/other", FileRight::Write));
    }
}

#[test]
fn program_predicates_project_to_process_tree_reads_without_adding_program_paths() {
    let permissions = resolve(
        r#"
permit(principal, action == SecCore::Action::"file.read", resource)
when { (resource.path == "/work" || resource.path like "/work/*")
  && context.input.program == "/usr/bin/cat" };
"#,
    );
    assert!(allows(&permissions, "/work/file", FileRight::Read));
    assert!(!allows(&permissions, "/usr/bin/cat", FileRight::Read));
    let forbidden = resolve(
        r#"
permit(principal, action == SecCore::Action::"file.read", resource)
when { context.input.program == "/usr/bin/cat" };
forbid(principal, action == SecCore::Action::"file.read", resource)
when { context.input.program == "/usr/bin/cat" };
"#,
    );
    assert!(forbidden.filesystem.rules.is_empty());
}

#[test]
fn explicit_denies_still_override_coarse_writes_and_network() {
    let permissions = resolve(
        r#"
permit(principal, action == SecCore::Action::"file.write", resource);
forbid(principal, action == SecCore::Action::"file.read", resource);
permit(principal, action == SecCore::Action::"network.use", resource);
forbid(principal, action == SecCore::Action::"network.use", resource);
"#,
    );
    assert!(permissions.filesystem.rules.is_empty());
    assert_eq!(permissions.network.mode, NetworkMode::Off);
    assert_eq!(
        resolve(r#"permit(principal, action == SecCore::Action::"network.use", resource);"#)
            .network
            .mode,
        NetworkMode::Full
    );
    assert!(
        resolve(r#"forbid(principal, action == SecCore::Action::"file.write", resource);"#)
            .filesystem
            .rules
            .is_empty()
    );
}

#[test]
fn complement_forbids_do_not_turn_into_root_denies_over_explicit_grants() {
    let permissions = resolve(
        r#"
permit(principal, action == SecCore::Action::"file.read", resource);
forbid(principal, action == SecCore::Action::"file.read", resource)
unless { resource.path == "/work" || resource.path like "/work/*" };
"#,
    );
    assert!(allows(&permissions, "/work/file", FileRight::Read));
    assert!(!allows(&permissions, "/outside", FileRight::Read));
}

#[test]
fn point_file_grants_do_not_become_directory_grants() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file");
    std::fs::write(&path, "content").unwrap();
    let policy = |path: &std::path::Path| {
        format!(
            "permit(principal, action == SecCore::Action::\"file.read\", resource) when {{ resource.path == {} }};",
            serde_json::to_string(path.to_str().unwrap()).unwrap()
        )
    };
    let input = PermissionInput::parse(&policy(&path)).unwrap();
    let permissions = input.resolve(&launch()).unwrap();
    assert_eq!(permissions.filesystem.rules.len(), 1);
    assert_eq!(permissions.filesystem.rules[0].path, path.to_str().unwrap());
    // Replacing the file before path capture must not turn its grant into a subtree grant.
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let facts = asc_pep_runtime::prepare_path_facts(&launch(), &permissions, &[]).unwrap();
    assert!(
        input
            .validate_path_facts(&launch(), &permissions, &facts)
            .is_err()
    );
    assert!(
        PermissionInput::parse(&policy(dir.path()))
            .unwrap()
            .resolve(&launch())
            .is_err()
    );
}

#[test]
fn unsupported_conditions_and_advice_are_not_discarded() {
    for text in [
        r#"permit(principal, action == SecCore::Action::"file.read", resource) when { resource.path like "/work/*.txt" };"#,
        r#"permit(principal, action == SecCore::Action::"file.write", resource) when { context.input has mode && context.input.mode == 420 };"#,
        r#"permit(principal, action == SecCore::Action::"file.read", resource) when { principal has agent_type && principal.agent_type == "cosh" };"#,
        r#"@advice("{\"verdict\":\"escalate\"}") permit(principal, action == SecCore::Action::"file.read", resource);"#,
        r#"permit(principal, action == SecCore::Action::"file.read", resource) when { resource.path == "/work/../etc" };"#,
        r#"permit(principal, action == SecCore::Action::"file.read", resource) when { resource.path == "relative" };"#,
        "not Cedar",
        "",
    ] {
        let parsed = PermissionInput::parse(text);
        assert!(
            parsed.is_err() || parsed.unwrap().resolve(&launch()).is_err(),
            "{text}"
        );
    }
    assert!(PermissionInput::parse(&" ".repeat(4 * 1024 * 1024 + 1)).is_err());
}

#[test]
fn invocation_conditions_check_only_the_initial_launch() {
    let policy = r#"
permit(principal, action == SecCore::Action::"file.read", resource);
permit(principal, action == SecCore::Action::"file.exec", resource)
when { resource.path == "/usr/bin/true" && context.input.arg_count == 0 };
"#;
    let input = PermissionInput::parse(policy).unwrap();
    assert!(input.resolve(&launch()).is_ok());
    let mut changed = launch();
    changed.args.push("unexpected".into());
    assert!(input.resolve(&changed).is_err());
}

#[test]
fn binding_preserves_alias_exclusions_before_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("key");
    let alias = dir.path().join("alias");
    std::fs::write(&target, "secret").unwrap();
    std::os::unix::fs::symlink(&target, &alias).unwrap();
    for (allowed, forbidden) in [(&alias, &target), (&target, &alias)] {
        let source = format!(
            r#"permit(principal, action == SecCore::Action::"file.read", resource)
when {{ [{}].contains(resource.path) }};
forbid(principal, action == SecCore::Action::"file.read", resource)
when {{ {} == resource.path }};"#,
            json!(allowed),
            json!(forbidden),
        );
        let input = PermissionInput::parse(&source).unwrap();
        let bound = input.prepare(&launch()).unwrap();
        assert!(bound.permissions.filesystem.rules.is_empty());
        assert!(bound.mount_aliases.is_empty());
        assert!(
            bound
                .path_facts
                .iter()
                .any(|fact| fact.path == alias.to_str().unwrap())
        );
        assert!(
            bound
                .path_facts
                .iter()
                .any(|fact| fact.path == target.to_str().unwrap())
        );
    }
}

#[test]
fn directory_and_intermediate_links_share_canonical_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("real");
    let alias = dir.path().join("alias");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("key"), "secret").unwrap();
    std::fs::write(target.join("public"), "public").unwrap();
    std::os::unix::fs::symlink(&target, &alias).unwrap();
    let source = format!(
        r#"permit(principal, action == SecCore::Action::"file.read", resource)
when {{ resource.path == {alias} || resource.path like {pattern} }};
permit(principal, action == SecCore::Action::"file.read", resource)
when {{ resource.path == {public} }};
forbid(principal, action == SecCore::Action::"file.read", resource)
when {{ resource.path == {key} }};"#,
        alias = json!(alias),
        pattern = json!(format!("{}/*", alias.display())),
        key = json!(alias.join("key")),
        public = json!(alias.join("public")),
    );
    let bound = PermissionInput::parse(&source)
        .unwrap()
        .prepare(&launch())
        .unwrap();
    assert!(allows(
        &bound.permissions,
        target.join("public").to_str().unwrap(),
        FileRight::Read
    ));
    assert!(!allows(
        &bound.permissions,
        target.join("key").to_str().unwrap(),
        FileRight::Read
    ));
    assert_eq!(bound.mount_aliases, vec![alias.to_str().unwrap()]);
    assert!(
        bound
            .permissions
            .filesystem
            .rules
            .iter()
            .all(|rule| !rule.path.starts_with(alias.to_str().unwrap()))
    );
}

#[test]
fn hardlink_exclusions_are_rejected_before_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("key");
    let alias = dir.path().join("hardlink");
    std::fs::write(&target, "secret").unwrap();
    std::fs::hard_link(&target, &alias).unwrap();
    let source = format!(
        r#"
permit(principal, action == SecCore::Action::"file.read", resource)
when {{ resource.path == {} }};
forbid(principal, action == SecCore::Action::"file.read", resource)
when {{ resource.path == {} }};"#,
        json!(alias),
        json!(target)
    );
    let error = PermissionInput::parse(&source)
        .unwrap()
        .prepare(&launch())
        .unwrap_err();
    assert!(
        error.to_string().contains("hardlinked permission path"),
        "{error}"
    );
}

#[test]
fn writable_ancestors_cannot_retarget_permission_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("key");
    let alias = dir.path().join("alias");
    std::fs::write(&target, "secret").unwrap();
    std::os::unix::fs::symlink(&target, &alias).unwrap();
    let source = format!(
        r#"permit(principal, action == SecCore::Action::"file.write", resource)
when {{ resource.path == {root} || resource.path like {pattern} }};
forbid(principal, action == SecCore::Action::"file.read", resource)
when {{ resource.path == {alias} }};"#,
        root = json!(dir.path()),
        pattern = json!(format!("{}/*", dir.path().display())),
        alias = json!(alias),
    );
    let error = PermissionInput::parse(&source)
        .unwrap()
        .prepare(&launch())
        .unwrap_err();
    assert!(error.to_string().contains("writable ancestor"), "{error}");
}

#[test]
fn initial_program_and_cwd_must_match_the_captured_paths() {
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("allowed");
    let cwd = dir.path().join("cwd");
    std::fs::copy("/usr/bin/true", &executable).unwrap();
    std::fs::create_dir(&cwd).unwrap();
    let prepared = asc_pep_runtime::prepare_launch(
        executable.to_str().unwrap(),
        vec![],
        &cwd,
        BTreeMap::new(),
    )
    .unwrap();
    let source = format!(
        r#"permit(principal, action == SecCore::Action::"file.read", resource);
permit(principal, action == SecCore::Action::"file.exec", resource)
when {{ resource.path == {} }};
forbid(principal, action == SecCore::Action::"file.exec", resource)
when {{ resource.path == "/usr/bin/false" }};"#,
        json!(executable),
    );
    let input = PermissionInput::parse(&source).unwrap();
    assert!(input.prepare(&prepared).is_ok());
    // Deliberately replace the object between launch resolution and provider preparation.
    std::fs::remove_file(&executable).unwrap();
    std::os::unix::fs::symlink("/usr/bin/false", &executable).unwrap();
    let error = input.prepare(&prepared).unwrap_err();
    assert!(error.to_string().contains("launch path changed"), "{error}");
    std::fs::remove_file(&executable).unwrap();
    std::fs::copy("/usr/bin/true", &executable).unwrap();
    std::fs::remove_dir(&cwd).unwrap();
    std::os::unix::fs::symlink(dir.path(), &cwd).unwrap();
    let error = input.prepare(&prepared).unwrap_err();
    assert!(error.to_string().contains("launch path changed"), "{error}");
}

#[test]
fn bound_alias_retargeting_is_rejected_before_helper_start() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    let alias = dir.path().join("alias");
    std::fs::write(&first, "first").unwrap();
    std::fs::write(&second, "second").unwrap();
    std::os::unix::fs::symlink(&first, &alias).unwrap();
    let input = PermissionInput::parse(&format!(
        r#"permit(principal, action == SecCore::Action::"file.read", resource) when {{ resource.path == {} }};"#, json!(alias),
    )).unwrap();
    let bound = input.prepare(&launch()).unwrap();
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&second, &alias).unwrap();
    let error = asc_pep_runtime::SandboxBackend::new(dir.path().join("absent-helper"))
        .execute_bound(&bound)
        .unwrap_err();
    assert!(
        error.to_string().contains("path identity changed"),
        "{error}"
    );
}

#[test]
fn program_region_and_source_budgets_apply_before_representative_evaluation() {
    let expected =
        resolve(r#"permit(principal, action == SecCore::Action::"file.read", resource);"#);
    // Including the extra program class, 126 entries cost 16,256 regions; 127 cost 16,512.
    for (count, expected_error) in [
        (126, None),
        (127, Some("region budget")),
        (150, Some("region budget")),
        (16_385, Some("source exceeds")),
    ] {
        let programs: Vec<_> = (0..count)
            .map(|index| format!("/__asc_permission_input_{index}__"))
            .collect();
        let source = format!(
            r#"permit(principal, action == SecCore::Action::"file.read", resource)
when {{ {}.contains(context.input.program) }};"#,
            json!(programs),
        );
        assert!(source.len() < 4 * 1024 * 1024);
        let result = PermissionInput::parse(&source).unwrap().resolve(&launch());
        if let Some(expected_error) = expected_error {
            let error = result.unwrap_err();
            assert!(
                error.to_string().contains(expected_error),
                "{count}: {error}"
            );
        } else {
            assert_eq!(result.unwrap(), expected);
        }
    }
}

#[test]
fn path_capture_rejects_oversized_literals_and_expansion_before_filesystem_access() {
    let policy = |paths: &[String]| {
        format!(
            r#"permit(principal, action == SecCore::Action::"file.read", resource)
when {{ {}.contains(resource.path) }};"#,
            json!(paths)
        )
    };
    for paths in [
        vec![format!("/{}", "a".repeat(4095))],
        vec!["/a".repeat(2048)],
    ] {
        let error = PermissionInput::parse(&policy(&paths))
            .unwrap()
            .prepare(&launch())
            .unwrap_err();
        assert!(
            error.to_string().contains("path exceeds 4095 bytes"),
            "{error}"
        );
    }
    // Each literal is within the path limit; their distinct ancestors exceed
    // the aggregate budget. No giant source or corresponding directories are needed.
    let paths = [
        format!("/one{}", "/a".repeat(2045)),
        format!("/two{}", "/a".repeat(2045)),
    ];
    assert!(paths.iter().all(|path| path.len() <= 4095));
    let error = PermissionInput::parse(&policy(&paths))
        .unwrap()
        .prepare(&launch())
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("capture exceeds its byte budget"),
        "{error}"
    );
}

#[cfg(target_os = "linux")]
struct Client {
    pdp: asc_pdp_runtime::PdpRuntime,
    requests: std::cell::RefCell<Vec<AuthorizationRequest>>,
}

#[cfg(target_os = "linux")]
impl Client {
    fn new(baseline: String) -> Self {
        Self {
            pdp: asc_pdp_runtime::PdpRuntime::from_snapshot(asc_pdp_runtime::PolicySnapshot {
                version: "test-policy-1".into(),
                baseline: asc_policy_types::cedar::CedarPolicySource {
                    schema: asc_policy_cedar::schema::embedded_reference(),
                    text: baseline,
                },
                task: None,
                environment_permissions: resolve("permit(principal, action, resource);"),
                ttl_seconds: 60,
            })
            .unwrap(),
            requests: std::cell::RefCell::new(vec![]),
        }
    }
}

#[cfg(target_os = "linux")]
impl asc_pep_runtime::AuthorizationClient for Client {
    fn evaluate(
        &self,
        request: &AuthorizationRequest,
    ) -> Result<Decision, asc_pep_runtime::PepError> {
        self.requests.borrow_mut().push(request.clone());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.pdp
            .evaluate(request, now)
            .map_err(|error| asc_pep_runtime::PepError::Authorization(error.to_string()))
    }

    fn report(&self, _: &ExecutionReceipt) -> Result<ReceiptAck, asc_pep_runtime::PepError> {
        panic!("authorization-only preparation must not report execution");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn fresh_cedar_sources_reach_the_pdp_as_permissions_and_remain_subject_to_policy() {
    use asc_pep_runtime::{PepRuntime, SandboxBackend, prepare_launch};
    let dir = tempfile::tempdir().unwrap();
    let private = dir.path().join("private");
    std::fs::create_dir(&private).unwrap();
    let path = serde_json::to_string(dir.path().to_str().unwrap()).unwrap();
    let subtree = serde_json::to_string(&format!("{}/*", dir.path().display())).unwrap();
    let source = format!(
        r#"
permit(principal, action == SecCore::Action::"file.read", resource);
permit(principal, action == SecCore::Action::"file.write", resource)
when {{ resource.path == {path} || resource.path like {subtree} }};
"#
    );
    let baseline = format!(
        r#"
permit(principal, action, resource);
forbid(principal, action == SecCore::Action::"network.use", resource);
forbid(principal, action == SecCore::Action::"file.write", resource)
when {{ resource.path == {} || resource.path like {} }};
"#,
        serde_json::to_string(private.to_str().unwrap()).unwrap(),
        serde_json::to_string(&format!("{}/*", private.display())).unwrap()
    );
    let client = Client::new(baseline);
    let principal = AgentPrincipal {
        agent_type: Some("cosh".into()),
    };
    let launch = prepare_launch("/usr/bin/true", vec![], dir.path(), BTreeMap::new()).unwrap();
    let mut runtime = PepRuntime::new(SandboxBackend::new(dir.path().join("unused-helper")));
    let first = PermissionInput::parse(&source).unwrap();
    let (prepared, decision) = runtime
        .authorize(&principal, launch.clone(), &first, &client)
        .unwrap();
    assert_eq!(decision.verdict, DecisionVerdict::Modify);
    assert!(allows(
        &prepared.request.requested_permissions,
        private.to_str().unwrap(),
        FileRight::Write
    ));
    assert!(!allows(
        decision.effective_permissions.as_ref().unwrap(),
        private.to_str().unwrap(),
        FileRight::Write
    ));
    let second = PermissionInput::parse(
        r#"permit(principal, action == SecCore::Action::"file.read", resource);"#,
    )
    .unwrap();
    let (next, _) = runtime
        .authorize(&principal, launch.clone(), &second, &client)
        .unwrap();
    assert!(!allows(
        &next.request.requested_permissions,
        dir.path().to_str().unwrap(),
        FileRight::Write
    ));
    assert_ne!(prepared.request.request_id, next.request.request_id);
    assert_eq!(client.requests.borrow().len(), 2);
    assert_ne!(
        client.requests.borrow()[0].requested_permissions,
        client.requests.borrow()[1].requested_permissions
    );
    assert!(
        serde_json::to_value(&prepared.request)
            .unwrap()
            .get("policy")
            .is_none()
    );
    let network = PermissionInput::parse(
        r#"
permit(principal, action == SecCore::Action::"file.read", resource);
permit(principal, action == SecCore::Action::"network.use", resource);
"#,
    )
    .unwrap();
    let (_, denied) = runtime
        .authorize(&principal, launch, &network, &client)
        .unwrap();
    assert_eq!(denied.verdict, DecisionVerdict::Deny);
    assert!(denied.reason.contains("network.use"), "{}", denied.reason);
    assert!(denied.effective_permissions.is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn dynamic_pdp_requests_use_bound_canonical_exclusions() {
    use asc_pep_runtime::{PepRuntime, SandboxBackend, prepare_launch};
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("real");
    let alias = dir.path().join("alias");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("key"), "secret").unwrap();
    std::os::unix::fs::symlink(&target, &alias).unwrap();
    let source = format!(
        r#"
permit(principal, action == SecCore::Action::"file.read", resource);
forbid(principal, action == SecCore::Action::"file.read", resource)
when {{ resource.path == {} }};"#,
        json!(alias.join("key"))
    );
    let client = Client::new("permit(principal, action, resource);".into());
    let principal = AgentPrincipal {
        agent_type: Some("cosh".into()),
    };
    let launch = prepare_launch("/usr/bin/true", vec![], dir.path(), BTreeMap::new()).unwrap();
    let input = PermissionInput::parse(&source).unwrap();
    let mut runtime = PepRuntime::new(SandboxBackend::new(dir.path().join("unused-helper")));
    runtime
        .authorize(&principal, launch.clone(), &input, &client)
        .unwrap();
    let requests = client.requests.borrow();
    assert_eq!(requests.len(), 1);
    let permissions = &requests[0].requested_permissions;
    assert!(!allows(
        permissions,
        target.join("key").to_str().unwrap(),
        FileRight::Read
    ));
    assert!(
        permissions
            .filesystem
            .rules
            .iter()
            .any(|rule| rule.effect == RuleEffect::Deny
                && rule.path == target.join("key").to_str().unwrap())
    );
    assert!(
        permissions
            .filesystem
            .rules
            .iter()
            .all(|rule| !rule.path.starts_with(alias.to_str().unwrap()))
    );
    drop(requests);
    // A separate request reproduces the originally omitted exclusion: the target
    // has no lexical grant, but its alias does. It must never reach the PDP as allowed.
    let source = format!(
        r#"
permit(principal, action == SecCore::Action::"file.read", resource)
when {{ resource.path == {} }};
forbid(principal, action == SecCore::Action::"file.read", resource)
when {{ resource.path == {} }};"#,
        json!(alias.join("key")),
        json!(target.join("key"))
    );
    let input = PermissionInput::parse(&source).unwrap();
    runtime
        .authorize(&principal, launch, &input, &client)
        .unwrap();
    let requests = client.requests.borrow();
    assert_eq!(requests.len(), 2);
    assert!(!allows(
        &requests[1].requested_permissions,
        target.join("key").to_str().unwrap(),
        FileRight::Read
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn directory_exclusions_survive_pdp_compaction_and_check_hardlinked_descendants() {
    use asc_pep_runtime::{PepRuntime, SandboxBackend, prepare_launch};
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    let private = dir.path().join("private");
    let cwd = dir.path().join("cwd");
    for path in [&work, &private, &cwd] {
        std::fs::create_dir(path).unwrap();
    }
    std::fs::write(private.join("key"), "secret").unwrap();
    let helper = dir.path().join("helper");
    std::fs::write(
        &helper,
        "#!/bin/sh\necho '{\"strict_filesystem\":true,\"noexec\":true,\"network_off\":true}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tree = |path: &std::path::Path| {
        format!(
            "(resource.path == {} || resource.path like {})",
            json!(path),
            json!(format!("{}/*", path.display()))
        )
    };
    let principal = AgentPrincipal {
        agent_type: Some("cosh".into()),
    };
    let launch = prepare_launch("/usr/bin/true", vec![], &cwd, BTreeMap::new()).unwrap();
    let mut runtime = PepRuntime::new(SandboxBackend::new(helper));
    let client = Client::new("permit(principal, action, resource);".into());
    for (action, right) in [
        ("file.read", FileRight::Read),
        ("file.write", FileRight::Write),
    ] {
        for exclusion in [
            format!("resource.path == {}", json!(private)),
            format!(
                "resource.path like {}",
                json!(format!("{}/*", private.display()))
            ),
            tree(&private),
        ] {
            let source = format!(
                r#"permit(principal, action == SecCore::Action::"file.read", resource)
when {{ {} || {} || {} }};
permit(principal, action == SecCore::Action::"file.write", resource) when {{ {} }};
forbid(principal, action == SecCore::Action::"{action}", resource) when {{ {} }};"#,
                tree(std::path::Path::new("/usr")),
                tree(&cwd),
                tree(&work),
                tree(&work),
                exclusion
            );
            let envelope = json!({"language":"cedar", "content":{
                "schema":{"schemaId":"seccore.system","revision":1}, "text":source
            }});
            for text in [source.clone(), envelope.to_string()] {
                let input = PermissionInput::parse(&text).unwrap();
                std::fs::hard_link(private.join("key"), work.join("key")).unwrap();
                let bound = input.prepare(&launch).unwrap();
                let error = linux_sandbox::runtime::check_mount_plan(&bound).expect_err(&source);
                assert!(error.to_string().contains("hardlink"), "{error}");
                let (prepared, decision) = runtime
                    .authorize(&principal, launch.clone(), &input, &client)
                    .unwrap();
                let json =
                    PermissionInput::parse(&serde_json::to_string(&bound.permissions).unwrap())
                        .unwrap();
                let (json_prepared, json_decision) = runtime
                    .authorize(&principal, launch.clone(), &json, &client)
                    .unwrap();
                assert_eq!(
                    prepared.request.requested_permissions,
                    json_prepared.request.requested_permissions
                );
                assert_eq!(
                    decision.effective_permissions,
                    json_decision.effective_permissions
                );
                for permissions in [
                    &prepared.request.requested_permissions,
                    decision.effective_permissions.as_ref().unwrap(),
                ] {
                    assert!(
                        permissions
                            .filesystem
                            .rules
                            .iter()
                            .any(|rule| rule.effect == RuleEffect::Deny
                                && rule.path == private.to_str().unwrap()
                                && rule.rights.contains(&right))
                    );
                }
                let error = runtime.prepare_execution(&prepared, &decision).unwrap_err();
                assert!(error.to_string().contains("hardlink"), "{error}");
                std::fs::remove_file(work.join("key")).unwrap();
                runtime.prepare_execution(&prepared, &decision).unwrap();
                // A later hardlink must still be caught at execution preparation.
                std::fs::hard_link(private.join("key"), work.join("key")).unwrap();
                let error = runtime.prepare_execution(&prepared, &decision).unwrap_err();
                assert!(error.to_string().contains("hardlink"), "{error}");
                let mut dropped = decision.clone();
                dropped
                    .effective_permissions
                    .as_mut()
                    .unwrap()
                    .filesystem
                    .rules
                    .retain(|rule| rule.effect != RuleEffect::Deny);
                let error = runtime.prepare_execution(&prepared, &dropped).unwrap_err();
                assert!(
                    error.to_string().contains("removed requested exclusion"),
                    "{error}"
                );
                std::fs::remove_file(work.join("key")).unwrap();
            }
        }
    }
}
