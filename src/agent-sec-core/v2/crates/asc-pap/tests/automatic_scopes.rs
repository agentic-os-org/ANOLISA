//! Exercise the same PAP planning pass and `SQLite` admission transaction as the daemon.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, mpsc};

use asc_foundation_types::{ResourceId, Revision};
use asc_pap::{PapError, PapRepository, PapService, ScopeDiscovery, ScopeDiscoverySeed};
use asc_policy_repository_sqlite::SqlitePolicyRepository;
use asc_policy_types::authoring::PolicySource;
use asc_policy_types::binding::BindingStatus;
use asc_policy_types::cedar::{CedarPolicySource, CedarSchemaReference};
use asc_policy_types::process_discovery::ProcessIdentity;
use asc_policy_types::scope::ProcessMatcher;

const SELECTORS: &[asc_policy_cedar::deployment::DeploymentSelector] = &[
    asc_policy_translator_agentsight::selection::SELECTOR,
    asc_gateway_plan::selection::SELECTOR,
];

struct Discovery;
impl ScopeDiscovery for Discovery {
    fn start(&self, _: &ScopeDiscoverySeed) -> Result<(), PapError> {
        Ok(())
    }
    fn stop(&self, _: &ResourceId) -> Result<(), PapError> {
        Ok(())
    }
}

fn file(path: &str) -> PolicySource {
    PolicySource::Cedar(CedarPolicySource {
        schema: CedarSchemaReference {
            schema_id: ResourceId::new(asc_policy_cedar::schema::SCHEMA_ID).unwrap(),
            revision: Revision::new(3).unwrap(),
        },
        text: format!(
            r#"forbid(principal is SecCore::Subject, action == SecCore::Action::"file.read",resource is SecCore::File)
        when {{principal has agentType && principal.agentType == "qoder" && principal has ownerUid && principal.ownerUid == 1000}}
        when {{resource.path == "{path}"}};"#
        ),
    })
}

#[test]
fn automatic_file_rule_without_agent_type_reports_its_statement_without_binding() {
    for add_targeted_rule in [false, true] {
        let repo = Arc::new(SqlitePolicyRepository::temporary().unwrap());
        let pap = PapService::new(repo)
            .with_scope_discovery(Arc::new(Discovery))
            .with_automatic_scopes(
                Ok(BTreeMap::from([(
                    "qoder".into(),
                    ProcessMatcher::Name {
                        process_name: "qoder".into(),
                    },
                )])),
                SELECTORS,
                || {},
            );
        let PolicySource::Cedar(mut policy) = file("/work/B") else {
            unreachable!()
        };
        let subjectless = r#"
            forbid(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
            when { resource.path == "/work/A" &&
                   context.input.operation == SecCore::WriteOperation::"delete" };"#;
        policy.text = if add_targeted_rule {
            format!("{subjectless}\n{}", policy.text)
        } else {
            subjectless.into()
        };
        let saved = pap
            .create_policy("file restrictions", &PolicySource::Cedar(policy))
            .unwrap();
        pap.recover_scopes().unwrap();
        let details = pap
            .policy_details(&saved.policy_id, saved.revision)
            .unwrap();
        let diagnostic = details
            .automatic_diagnostics
            .iter()
            .find(|entry| entry.state == "unassigned")
            .unwrap();
        assert_eq!(
            diagnostic.backend,
            asc_policy_types::policy::DeploymentBackend::ActPlane
        );
        assert_eq!(diagnostic.statement_ids, ["policy0".to_owned()].into());
        assert_eq!(diagnostic.scope_id, None);
        assert!(
            diagnostic
                .reason
                .as_deref()
                .unwrap()
                .contains("requires an exact principal.agentType")
        );
        let scopes = pap.list_scopes(10, 0).unwrap().items;
        assert_eq!(scopes.len(), usize::from(add_targeted_rule));
        if let Some(scope) = scopes.first() {
            let selected = scope.policy_snapshots[0].deployment.as_ref().unwrap();
            assert_eq!(selected.statement_ids, ["policy1".to_owned()].into());
        }
    }
}

#[test]
fn running_instance_follows_current_revision_and_new_policy_waits_for_fresh_uid() {
    let repo = Arc::new(SqlitePolicyRepository::temporary().unwrap());
    let (wake, notifications) = mpsc::sync_channel(1);
    let pap = PapService::new(repo.clone())
        .with_scope_discovery(Arc::new(Discovery))
        .with_automatic_scopes(
            Ok(BTreeMap::from([(
                "qoder".into(),
                ProcessMatcher::Name {
                    process_name: "qoder".into(),
                },
            )])),
            SELECTORS,
            move || {
                let _ = wake.try_send(());
            },
        );
    let a = pap.create_policy("file A", &file("/work/A")).unwrap();
    notifications.try_recv().unwrap();
    pap.recover_scopes().unwrap();
    let scope = pap.list_scopes(10, 0).unwrap().items.pop().unwrap();
    assert!(pap.delete_scope(&scope.scope_id).is_err());
    let instance = ProcessIdentity {
        boot_id: "boot1".into(),
        pid_namespace: "pid:[1]".into(),
        pid: 31,
        start_time: 42,
    };
    let selected = std::slice::from_ref(&instance);
    let first = repo.automatic_scope_generation(&scope.scope_id).unwrap();
    assert_eq!(first, Some(1));
    assert!(
        repo.sync_observed_scope_instances(&scope.scope_id, selected, &BTreeMap::new(), first)
            .unwrap()
            .is_empty()
    );
    let verified = BTreeMap::from([(instance.clone(), 1000)]);
    let original = repo
        .sync_observed_scope_instances(&scope.scope_id, selected, &verified, first)
        .unwrap()
        .pop()
        .unwrap()
        .binding
        .spec;
    let revised = pap
        .update_policy(&a.policy_id, "file A", &file("/work/B"))
        .unwrap();
    let b = pap.create_policy("file B", &file("/work/C")).unwrap();
    pap.sweep_deleting_scopes().unwrap();
    let current = repo.automatic_scope_generation(&scope.scope_id).unwrap();
    assert_eq!(current, Some(2));
    assert_eq!(
        repo.sync_observed_scope_instances(&scope.scope_id, selected, &verified, first),
        Err(PapError::Conflict)
    );
    let updated = repo
        .sync_observed_scope_instances(&scope.scope_id, selected, &BTreeMap::new(), current)
        .unwrap()
        .pop()
        .unwrap()
        .binding
        .spec;
    assert_eq!(updated.binding_id, original.binding_id);
    assert_eq!(updated.binding_revision.get(), 2);
    assert_eq!(updated.policy.revision, revised.revision);
    assert_eq!(pap.list_bindings(10, 0).unwrap().total, 1);
    let new = repo
        .sync_observed_scope_instances(&scope.scope_id, selected, &verified, current)
        .unwrap()
        .pop()
        .unwrap()
        .binding
        .spec;
    assert_eq!(new.policy.policy_id, b.policy_id);
    pap.delete_policy_revision(&a.policy_id, revised.revision)
        .unwrap();
    pap.sweep_deleting_scopes().unwrap();
    let remaining = repo.get_scope(&scope.scope_id).unwrap();
    assert_eq!(remaining.policy_snapshots.len(), 1);
    let after_delete = repo.automatic_scope_generation(&scope.scope_id).unwrap();
    let retired = repo
        .sync_observed_scope_instances(&scope.scope_id, selected, &verified, after_delete)
        .unwrap();
    assert_eq!(retired.len(), 1);
    assert_eq!(retired[0].binding.spec.binding_id, original.binding_id);
    assert_eq!(
        retired[0].binding.status.phase,
        BindingStatus::PendingDelete
    );
}

#[test]
fn broken_mappings_keep_admitted_scopes_pending_without_starting_discovery() {
    let repo = Arc::new(SqlitePolicyRepository::temporary().unwrap());
    let ready = PapService::new(repo.clone())
        .with_scope_discovery(Arc::new(Discovery))
        .with_automatic_scopes(
            Ok(BTreeMap::from([(
                "qoder".into(),
                ProcessMatcher::Name {
                    process_name: "qoder".into(),
                },
            )])),
            SELECTORS,
            || {},
        );
    let policy = ready.create_policy("Qoder file", &file("/work/A")).unwrap();
    ready.recover_scopes().unwrap();
    let admitted = ready.list_scopes(10, 0).unwrap().items.pop().unwrap();

    let reason = "invalid agent-types.json";
    let paused = PapService::new(repo).with_automatic_scopes(Err(reason.into()), SELECTORS, || {});
    paused.recover_scopes().unwrap();
    paused.sweep_deleting_scopes().unwrap();
    assert_eq!(paused.get_scope(&admitted.scope_id).unwrap(), admitted);
    assert!(
        paused
            .retry_scope(&admitted.scope_id)
            .unwrap_err()
            .to_string()
            .contains(reason)
    );
    let details = paused
        .policy_details(&policy.policy_id, policy.revision)
        .unwrap();
    assert_eq!(details.automatic_diagnostics.len(), 1);
    assert_eq!(
        details.automatic_diagnostics[0].reason.as_deref(),
        Some(reason)
    );
    paused
        .create_policy("unrelated write", &file("/work/B"))
        .unwrap();
}

#[test]
fn a_policy_without_a_mapping_is_saved_and_recovers_when_the_integration_is_configured() {
    let repo = Arc::new(SqlitePolicyRepository::temporary().unwrap());
    let pap = PapService::new(repo.clone())
        .with_scope_discovery(Arc::new(Discovery))
        .with_automatic_scopes(Ok(BTreeMap::new()), SELECTORS, || {});
    let policy = pap.create_policy("qoder", &file("/work/A")).unwrap();
    pap.recover_scopes().unwrap();
    assert!(pap.list_scopes(10, 0).unwrap().items.is_empty());
    assert!(
        pap.policy_details(&policy.policy_id, policy.revision)
            .unwrap()
            .automatic_diagnostics
            .iter()
            .any(|d| d.policy_id == policy.policy_id
                && d.reason.as_ref().unwrap().contains("no process matcher"))
    );
    let restarted = PapService::new(repo)
        .with_scope_discovery(Arc::new(Discovery))
        .with_automatic_scopes(
            Ok(BTreeMap::from([(
                "qoder".into(),
                ProcessMatcher::Name {
                    process_name: "qoder".into(),
                },
            )])),
            SELECTORS,
            || {},
        );
    restarted.recover_scopes().unwrap();
    restarted.sweep_deleting_scopes().unwrap();
    assert_eq!(restarted.list_scopes(10, 0).unwrap().total, 1);
    restarted
        .delete_policy_revision(&policy.policy_id, policy.revision)
        .unwrap();
    restarted.recover_scopes().unwrap();
    assert!(restarted.list_scopes(10, 0).unwrap().items.is_empty());
}

struct Gateway {
    repo: Arc<SqlitePolicyRepository>,
    rejected: std::sync::atomic::AtomicBool,
    revoked: Mutex<Vec<ResourceId>>,
}
impl asc_pap::CredentialRegistration for Gateway {
    fn register(
        &self,
        source: &CedarPolicySource,
    ) -> Result<Vec<asc_policy_types::policy::CredentialBinding>, PapError> {
        Ok(asc_policy_cedar::http::compile(source)
            .unwrap()
            .into_iter()
            .map(|r| asc_policy_types::policy::CredentialBinding {
                pair_id: format!("pair-{}", r.rule_id),
                rule_id: r.rule_id,
            })
            .collect())
    }
}
use asc_policy_types::scope::PreparedScope;
impl asc_pap::ScopeExecution for Gateway {
    fn publish_automatic(
        &self,
        updates: &asc_pap::ScopePublication,
        sources: &[(ResourceId, Revision)],
    ) -> Result<(), PapError> {
        if self.rejected.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(PapError::Unavailable);
        }
        self.repo.replace_automatic_scopes(updates, sources)
    }
    fn validate(&self, _: &PreparedScope) -> Result<(), PapError> {
        Ok(())
    }
    fn prepare(&self, _: &PreparedScope) -> Result<(), PapError> {
        Ok(())
    }
    fn restore(&self, _: &PreparedScope) -> Result<(), PapError> {
        Ok(())
    }
    fn revoke(&self, scope: &PreparedScope) -> Result<(), PapError> {
        self.revoked.lock().unwrap().push(scope.scope_id.clone());
        Ok(())
    }
    fn release(&self, _: &ResourceId) -> Result<(), PapError> {
        Ok(())
    }
}

fn credential(rule: &str, uid: u32) -> PolicySource {
    let PolicySource::Cedar(mut source) = file("/unused") else {
        unreachable!()
    };
    source.text = format!(
        r#"@id("{rule}") @modify("inject_credential")
        @real_token_file("/secure/{rule}-real") @fake_token_file("/secure/{rule}-fake")
        @inject_header("Authorization") @inject_format("auto") @response_rewrite("true")
        permit(principal is SecCore::Subject,action == SecCore::Action::"network.send",resource is SecCore::HttpEndpoint)
        when {{principal has agentType && principal.agentType == "qoder" && principal has ownerUid && principal.ownerUid == {uid}}}
        when {{resource.host == "{rule}.example.com" && context.input.scheme == "https"}};"#
    );
    PolicySource::Cedar(source)
}

#[test]
fn rejected_initial_gateway_candidate_can_retry_and_deleted_grants_are_withdrawn_on_rejection() {
    use std::sync::atomic::Ordering;
    let repo = Arc::new(SqlitePolicyRepository::temporary().unwrap());
    let gateway = Arc::new(Gateway {
        repo: repo.clone(),
        rejected: true.into(),
        revoked: Mutex::default(),
    });
    let pap = PapService::new(repo.clone())
        .with_credential_registration(gateway.clone())
        .with_scope_execution(gateway.clone())
        .with_scope_discovery(Arc::new(Discovery))
        .with_automatic_scopes(
            Ok(BTreeMap::from([(
                "qoder".into(),
                ProcessMatcher::Name {
                    process_name: "qoder".into(),
                },
            )])),
            SELECTORS,
            || {},
        );
    let a = pap.create_policy("A", &credential("one", 1000)).unwrap();
    pap.recover_scopes().unwrap();
    let failed = pap
        .policy_details(&a.policy_id, a.revision)
        .unwrap()
        .automatic_diagnostics
        .pop()
        .unwrap();
    assert_eq!(failed.state, "failed");
    let id = failed.scope_id.unwrap();
    assert!(id.as_str().len() <= 63);
    assert_eq!(repo.get_scope(&id), Err(PapError::NotFound));
    assert!(pap.scope_for_retry(&id).unwrap().uses_credentials());
    gateway.rejected.store(false, Ordering::Relaxed);
    pap.retry_scope(&id).unwrap();
    let b = pap.create_policy("B", &credential("two", 1000)).unwrap();
    pap.sweep_deleting_scopes().unwrap();
    assert_eq!(repo.get_scope(&id).unwrap().policy_snapshots.len(), 2);
    gateway.rejected.store(true, Ordering::Relaxed);
    pap.update_policy(&b.policy_id, "B", &credential("renamed", 1000))
        .unwrap();
    pap.sweep_deleting_scopes().unwrap();
    assert!(gateway.revoked.lock().unwrap().is_empty());
    pap.delete_policy_revision(&a.policy_id, a.revision)
        .unwrap();
    pap.sweep_deleting_scopes().unwrap();
    assert_eq!(*gateway.revoked.lock().unwrap(), vec![id.clone()]);
    gateway.rejected.store(false, Ordering::Relaxed);
    let repaired = pap.retry_scope(&id).unwrap();
    assert_eq!(repaired.policy_snapshots.len(), 1);
    assert_eq!(repaired.policy_snapshots[0].policy_id, b.policy_id);
}

#[test]
fn moving_one_grant_from_a_shared_scope_keeps_the_other_grant_and_rejects_stale_snapshots() {
    use std::sync::atomic::Ordering;
    let repo = Arc::new(SqlitePolicyRepository::temporary().unwrap());
    let gateway = Arc::new(Gateway {
        repo: repo.clone(),
        rejected: false.into(),
        revoked: Mutex::default(),
    });
    let pap = PapService::new(repo.clone())
        .with_credential_registration(gateway.clone())
        .with_scope_execution(gateway.clone())
        .with_scope_discovery(Arc::new(Discovery))
        .with_automatic_scopes(
            Ok(BTreeMap::from([(
                "qoder".into(),
                ProcessMatcher::Name {
                    process_name: "qoder".into(),
                },
            )])),
            SELECTORS,
            || {},
        );
    let moved = pap.create_policy("A", &credential("one", 1000)).unwrap();
    let kept = pap.create_policy("B", &credential("two", 1000)).unwrap();
    pap.recover_scopes().unwrap();
    let original = pap.list_scopes(10, 0).unwrap().items.pop().unwrap();
    assert_eq!(original.policy_snapshots.len(), 2);
    pap.update_policy(&moved.policy_id, "A", &credential("one", 1001))
        .unwrap();
    gateway.rejected.store(true, Ordering::Relaxed);
    pap.sweep_deleting_scopes().unwrap();
    assert_eq!(repo.get_scope(&original.scope_id).unwrap(), original);
    assert_eq!(pap.list_scopes(10, 0).unwrap().total, 1);
    gateway.rejected.store(false, Ordering::Relaxed);
    pap.sweep_deleting_scopes().unwrap();
    let groups = pap.list_scopes(10, 0).unwrap().items;
    assert_eq!(groups.len(), 2);
    for group in &groups {
        assert_eq!(group.policy_snapshots.len(), 1);
        match group.owner_uid {
            Some(1000) => assert_eq!(group.policy_snapshots[0].policy_id, kept.policy_id),
            Some(1001) => assert_eq!(group.policy_snapshots[0].policy_id, moved.policy_id),
            actual => panic!("unexpected owner {actual:?}"),
        }
    }
    let stale_sources = vec![
        (moved.policy_id, moved.revision),
        (kept.policy_id, kept.revision),
    ];
    assert_eq!(
        repo.replace_automatic_scopes(
            &asc_pap::ScopePublication::from([(
                original.scope_id.clone(),
                asc_pap::ScopeUpdate {
                    expected: None,
                    candidate: Some(original.clone())
                }
            )]),
            &stale_sources
        ),
        Err(PapError::Conflict)
    );
}

#[test]
fn gateway_selects_each_grants_own_uid_and_keeps_cross_policy_http_restrictions() {
    let repo = Arc::new(SqlitePolicyRepository::temporary().unwrap());
    let gateway = Arc::new(Gateway {
        repo: repo.clone(),
        rejected: false.into(),
        revoked: Mutex::default(),
    });
    let pap = PapService::new(repo)
        .with_credential_registration(gateway.clone())
        .with_scope_execution(gateway)
        .with_scope_discovery(Arc::new(Discovery))
        .with_automatic_scopes(
            Ok(BTreeMap::from([(
                "qoder".into(),
                ProcessMatcher::Name {
                    process_name: "qoder".into(),
                },
            )])),
            SELECTORS,
            || {},
        );
    let PolicySource::Cedar(mut mixed) = credential("github", 1000) else {
        unreachable!()
    };
    let PolicySource::Cedar(gitlab) = credential("gitlab", 1000) else {
        unreachable!()
    };
    mixed.text.push_str(&gitlab.text.replace(
        " && principal has ownerUid && principal.ownerUid == 1000",
        "",
    ));
    let PolicySource::Cedar(root) = credential("root", 0) else {
        unreachable!()
    };
    mixed.text.push_str(&root.text);
    mixed.text.push_str(
        r#"
        forbid(principal is SecCore::Subject, action == SecCore::Action::"process.spawn", resource)
        when {principal has agentType && principal.agentType == "qoder"};"#,
    );
    let policy = pap
        .create_policy("mixed", &PolicySource::Cedar(mixed))
        .unwrap();
    pap.recover_scopes().unwrap();
    let groups = pap.list_scopes(10, 0).unwrap().items;
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].owner_uid, Some(1000));
    let plan = asc_gateway_plan::compile_scope(&groups[0]).unwrap();
    assert_eq!(plan.rules.len(), 1);
    assert_eq!(plan.credentials[0].as_str(), "pair-github");
    let diagnostics = pap
        .policy_details(&policy.policy_id, policy.revision)
        .unwrap()
        .automatic_diagnostics;
    assert_eq!(
        diagnostics
            .iter()
            .filter(|d| d.state == "unassigned"
                && d.reason.as_deref().is_some_and(|m| m.contains("ownerUid")))
            .count(),
        2
    );

    let restriction = PolicySource::Cedar(CedarPolicySource {
        schema: asc_policy_cedar::schema::principal_reference(),
        text: r#"forbid(principal, action == SecCore::Action::"network.send", resource is SecCore::HttpEndpoint)
            when {principal has agentType && principal.agentType == "qoder"};"#.into(),
    });
    let extra = pap.create_policy("http restriction", &restriction).unwrap();
    pap.sweep_deleting_scopes().unwrap();
    // The fake publisher exposes PAP's candidate. The production HTTP compiler cannot
    // implement forbid yet, so it must reject this candidate before moving any traffic.
    let scope = pap.list_scopes(10, 0).unwrap().items.pop().unwrap();
    assert!(
        scope
            .policy_snapshots
            .iter()
            .any(|s| s.policy_id == extra.policy_id)
    );
    assert!(asc_gateway_plan::compile_scope(&scope).is_err());
}

#[test]
fn actplane_only_receives_the_file_statement_from_a_mixed_resource_membership_policy() {
    let pap = PapService::new(Arc::new(SqlitePolicyRepository::temporary().unwrap()))
        .with_scope_discovery(Arc::new(Discovery))
        .with_automatic_scopes(
            Ok(BTreeMap::from([(
                "qoder".into(),
                ProcessMatcher::Name {
                    process_name: "qoder".into(),
                },
            )])),
            SELECTORS,
            || {},
        );
    let PolicySource::Cedar(mut policy) = file("/work/A") else {
        unreachable!()
    };
    policy.text.push_str(r#"
        forbid(principal, action, resource in SecCore::HttpEndpoint::"remote")
        when { principal has agentType && principal.agentType == "qoder" };
        forbid(principal, action, resource is SecCore::HttpEndpoint in SecCore::HttpEndpoint::"other")
        when { principal has agentType && principal.agentType == "qoder" };"#);
    pap.create_policy("mixed", &PolicySource::Cedar(policy))
        .unwrap();
    pap.recover_scopes().unwrap();
    let scopes = pap.list_scopes(10, 0).unwrap().items;
    assert_eq!(scopes.len(), 1);
    let selected = scopes[0].policy_snapshots[0].deployment.as_ref().unwrap();
    assert_eq!(
        selected.backend,
        asc_policy_types::policy::DeploymentBackend::ActPlane
    );
    assert_eq!(selected.statement_ids, ["policy0".to_owned()].into());
}

#[test]
fn registering_only_gateway_does_not_derive_file_scopes() {
    let repo = Arc::new(SqlitePolicyRepository::temporary().unwrap());
    let pap = PapService::new(repo)
        .with_scope_discovery(Arc::new(Discovery))
        .with_automatic_scopes(
            Ok(BTreeMap::from([(
                "qoder".into(),
                ProcessMatcher::Name {
                    process_name: "qoder".into(),
                },
            )])),
            &[asc_gateway_plan::selection::SELECTOR],
            || {},
        );
    pap.create_policy("file", &file("/work/A")).unwrap();
    pap.recover_scopes().unwrap();
    assert_eq!(pap.list_scopes(10, 0).unwrap().total, 0);
}

#[test]
fn splitting_a_shared_scope_preserves_both_grants_across_rejection_and_retry() {
    use std::sync::atomic::Ordering;
    let repo = Arc::new(SqlitePolicyRepository::temporary().unwrap());
    let gateway = Arc::new(Gateway {
        repo: repo.clone(),
        rejected: false.into(),
        revoked: Mutex::default(),
    });
    let pap = PapService::new(repo.clone())
        .with_credential_registration(gateway.clone())
        .with_scope_execution(gateway.clone())
        .with_scope_discovery(Arc::new(Discovery))
        .with_automatic_scopes(
            Ok(BTreeMap::from([(
                "qoder".into(),
                ProcessMatcher::Name {
                    process_name: "qoder".into(),
                },
            )])),
            SELECTORS,
            || {},
        );
    let a = pap.create_policy("A", &credential("one", 1000)).unwrap();
    let b = pap.create_policy("B", &credential("two", 1000)).unwrap();
    pap.recover_scopes().unwrap();
    let shared = pap.list_scopes(10, 0).unwrap().items.pop().unwrap();
    pap.update_policy(&a.policy_id, "A", &credential("one", 1001))
        .unwrap();
    pap.update_policy(&b.policy_id, "B", &credential("two", 1002))
        .unwrap();
    gateway.rejected.store(true, Ordering::Relaxed);
    pap.sweep_deleting_scopes().unwrap();
    assert_eq!(repo.get_scope(&shared.scope_id).unwrap(), shared);
    gateway.rejected.store(false, Ordering::Relaxed);
    pap.sweep_deleting_scopes().unwrap();
    let active: BTreeMap<_, _> = pap
        .list_scopes(10, 0)
        .unwrap()
        .items
        .into_iter()
        .filter(|s| s.status == asc_policy_types::scope::ScopeStatus::Active)
        .map(|s| {
            (
                s.owner_uid.unwrap(),
                s.policy_snapshots[0].policy_id.clone(),
            )
        })
        .collect();
    assert_eq!(
        active,
        BTreeMap::from([(1001, a.policy_id), (1002, b.policy_id)])
    );
}

#[test]
fn retry_finishes_old_automatic_deletion_before_reusing_its_scope_id() {
    use asc_policy_repository::{BindingStateRepository, BindingStateWrite, WriteResult};
    use asc_policy_types::scope::ScopeStatus;

    let (pap, repo) = automatic_file_service();
    let old = pap.create_policy("old", &file("/work/A")).unwrap();
    pap.sweep_deleting_scopes().unwrap();
    let id = pap.list_scopes(1, 0).unwrap().items.remove(0).scope_id;
    let process = ProcessIdentity {
        boot_id: "boot".into(),
        pid_namespace: "pid:[1]".into(),
        pid: 31,
        start_time: 42,
    };
    let binding = repo
        .sync_observed_scope_instances(
            &id,
            std::slice::from_ref(&process),
            &BTreeMap::from([(process.clone(), 1000)]),
            repo.automatic_scope_generation(&id).unwrap(),
        )
        .unwrap()
        .remove(0)
        .binding
        .spec
        .binding_id;
    pap.delete_policy_revision(&old.policy_id, old.revision)
        .unwrap();
    pap.sweep_deleting_scopes().unwrap();
    let pending = repo.get_binding_state(&binding).unwrap().unwrap();
    assert_eq!(pending.binding.status.phase, BindingStatus::PendingDelete);
    assert!(
        repo.fail_pending_binding(&(&pending).into(), asc_pap::EnqueueError::Full)
            .unwrap()
    );
    let replacement = pap.create_policy("replacement", &file("/work/B")).unwrap();
    pap.sweep_deleting_scopes().unwrap();
    assert_eq!(
        pap.get_binding(&binding).unwrap().status.phase,
        BindingStatus::DeleteFailed
    );

    let old_scope = pap.retry_scope(&id).unwrap();
    assert_eq!(old_scope.status, ScopeStatus::Deleting);
    assert_eq!(old_scope.policy_snapshots[0].policy_id, old.policy_id);
    let retried = repo.get_binding_state(&binding).unwrap().unwrap();
    assert_eq!(retried.binding.status.phase, BindingStatus::PendingDelete);
    pap.retry_scope(&id).unwrap();
    assert_eq!(repo.get_binding_state(&binding).unwrap().unwrap(), retried);

    // The worker acknowledges removal before the planner may reuse the deterministic ID.
    let mut deleting = retried.clone();
    deleting.binding.status = BindingStatus::Deleting.into();
    assert!(matches!(
        repo.compare_exchange_binding_state(&retried, &BindingStateWrite::new(deleting)),
        Ok(WriteResult::Applied(_))
    ));
    let deleting = repo.get_binding_state(&binding).unwrap().unwrap();
    assert!(matches!(
        repo.compare_exchange_binding_state(&deleting, &BindingStateWrite::delete()),
        Ok(WriteResult::Applied(_))
    ));
    assert_eq!(pap.get_scope(&id), Err(PapError::NotFound));
    pap.sweep_deleting_scopes().unwrap();
    let active = pap.get_scope(&id).unwrap();
    assert_eq!(active.status, ScopeStatus::Active);
    assert_eq!(active.policy_snapshots.len(), 1);
    assert_eq!(active.policy_snapshots[0].policy_id, replacement.policy_id);
    assert_eq!(pap.get_binding(&binding), Err(PapError::NotFound));
    assert_stale_deletion_leaves_replacement_untouched(&pap, &repo, &old_scope, &active, &process);
}

fn automatic_file_service() -> (
    PapService<SqlitePolicyRepository>,
    Arc<SqlitePolicyRepository>,
) {
    let repo = Arc::new(SqlitePolicyRepository::temporary().unwrap());
    let pap = PapService::new(repo.clone())
        .with_scope_discovery(Arc::new(Discovery))
        .with_automatic_scopes(
            Ok(BTreeMap::from([(
                "qoder".into(),
                ProcessMatcher::Name {
                    process_name: "qoder".into(),
                },
            )])),
            SELECTORS,
            || {},
        );
    (pap, repo)
}

fn assert_stale_deletion_leaves_replacement_untouched(
    pap: &PapService<SqlitePolicyRepository>,
    repo: &SqlitePolicyRepository,
    deleted: &PreparedScope,
    active: &PreparedScope,
    process: &ProcessIdentity,
) {
    use asc_policy_repository::BindingStateRepository;
    let id = &active.scope_id;
    let binding = repo
        .sync_observed_scope_instances(
            id,
            std::slice::from_ref(process),
            &BTreeMap::from([(process.clone(), 1000)]),
            repo.automatic_scope_generation(id).unwrap(),
        )
        .unwrap()
        .remove(0)
        .binding
        .spec
        .binding_id;
    let pending = repo.get_binding_state(&binding).unwrap().unwrap();
    assert!(
        repo.fail_pending_binding(&(&pending).into(), asc_pap::EnqueueError::Full)
            .unwrap()
    );
    let failed = repo.get_binding_state(&binding).unwrap().unwrap();
    assert_eq!(failed.binding.status.phase, BindingStatus::ApplyFailed);
    // Resume the operations after the same ID now names a different active assignment.
    assert_eq!(&pap.retry_scope_snapshot(deleted).unwrap(), deleted);
    assert!(pap.delete_scope_snapshot(deleted).unwrap().completed);
    assert_eq!(&pap.get_scope(id).unwrap(), active);
    assert_eq!(repo.get_binding_state(&binding).unwrap().unwrap(), failed);
}

#[test]
fn automatic_retry_pins_the_authorized_candidate_or_serving_snapshot() {
    let (pap, repo) = automatic_file_service();
    let original = pap
        .create_policy("original", &file("/work/original"))
        .unwrap();
    pap.sweep_deleting_scopes().unwrap();
    let old_active = pap.list_scopes(1, 0).unwrap().items.remove(0);
    let id = &old_active.scope_id;
    assert_eq!(
        PapService::new(repo).retry_scope(id),
        Err(PapError::Unavailable)
    );
    pap.delete_policy_revision(&original.policy_id, original.revision)
        .unwrap();
    pap.sweep_deleting_scopes().unwrap();
    assert_eq!(pap.get_scope(id), Err(PapError::NotFound));

    let replacement = pap
        .create_policy("replacement", &file("/work/one"))
        .unwrap();
    let unpublished = pap.scope_for_retry(id).unwrap();
    let second = pap
        .update_policy(&replacement.policy_id, "replacement", &file("/work/two"))
        .unwrap();
    assert_eq!(
        pap.retry_scope_snapshot(&unpublished),
        Err(PapError::Conflict)
    );
    assert_eq!(pap.get_scope(id), Err(PapError::NotFound));
    let serving = pap.retry_scope(id).unwrap();
    assert_eq!(serving.policy_snapshots[0].revision, second.revision);

    assert_eq!(
        pap.retry_scope_snapshot(&old_active),
        Err(PapError::Conflict)
    );
    assert_eq!(pap.get_scope(id).unwrap(), serving);
    let third = pap
        .update_policy(&replacement.policy_id, "replacement", &file("/work/three"))
        .unwrap();
    let applied = pap.retry_scope_snapshot(&serving).unwrap();
    assert_eq!(applied.policy_snapshots[0].revision, third.revision);
    assert_eq!(pap.get_scope(id).unwrap(), applied);
}
