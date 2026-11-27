//! Exercise candidate publication against the real PAP, credential store and manifest.
use super::*;
use asc_pap::ScopeExecution as _;
use asc_policy_types::cedar::CedarPolicySource;
use asc_policy_types::policy::{CredentialBinding, DeploymentBackend, DeploymentSelection};

fn source(fixture: &Fixture, revision: u32, text: &str) -> PreparedPolicy {
    named_source(fixture, "qoder", revision, text)
}
fn named_source(fixture: &Fixture, id: &str, revision: u32, text: &str) -> PreparedPolicy {
    source_with_pair(fixture, id, revision, text, &fixture.pair_id())
}
fn source_with_pair(
    fixture: &Fixture,
    id: &str,
    revision: u32,
    text: &str,
    pair: &CredentialId,
) -> PreparedPolicy {
    let source = CedarPolicySource {
        schema: asc_policy_cedar::schema::embedded_reference(),
        text: text.into(),
    };
    let registrations = asc_policy_cedar::http::compile(&source).unwrap();
    let policy = PreparedPolicy {
        policy_id: ResourceId::new(id).unwrap(),
        policy_name: "Qoder".into(),
        revision: Revision::new(revision).unwrap(),
        template: PolicySource::Cedar(source),
        deployment: None,
        credential_bindings: registrations
            .into_iter()
            .map(|rule| CredentialBinding {
                rule_id: rule.rule_id,
                pair_id: pair.as_str().into(),
            })
            .collect(),
    };
    fixture.repository.put_policy(&policy).unwrap()
}

fn candidate(
    policy: &PreparedPolicy,
    scope_id: &str,
    uid: u32,
    statements: &[&str],
) -> PreparedScope {
    let mut selected = policy.clone();
    selected.deployment = Some(DeploymentSelection {
        fragments: std::collections::BTreeMap::new(),
        backend: DeploymentBackend::Gateway,
        statement_ids: statements.iter().map(|id| (*id).to_owned()).collect(),
    });
    PreparedScope {
        scope_id: ResourceId::new(scope_id).unwrap(),
        selector: ScopeSelector::Process {
            matcher: ProcessMatcher::Name {
                process_name: "qoder".into(),
            },
        },
        owner_uid: Some(uid),
        policy_snapshots: vec![selected],
        status: ScopeStatus::Active,
    }
}

fn publish(
    service: &GatewayService,
    expected: Option<&PreparedScope>,
    candidate: &PreparedScope,
    others: &[(Option<&PreparedScope>, Option<&PreparedScope>)],
) -> Result<(), GatewayError> {
    let sources = service
        .repository
        .all_current_policies()
        .unwrap()
        .into_iter()
        .filter(asc_pap::is_automatic_source)
        .map(|p| (p.policy_id, p.revision))
        .collect::<Vec<_>>();
    let mut updates = asc_pap::ScopePublication::from([(
        candidate.scope_id.clone(),
        asc_pap::ScopeUpdate {
            expected: expected.cloned(),
            candidate: Some(candidate.clone()),
        },
    )]);
    for &(old, next) in others {
        let id = next.or(old).unwrap().scope_id.clone();
        assert!(
            updates
                .insert(
                    id,
                    asc_pap::ScopeUpdate {
                        expected: old.cloned(),
                        candidate: next.cloned()
                    }
                )
                .is_none()
        );
    }
    service.publish_candidate(&updates, &sources)
}

// Inspect the actual file named by the manifest that the proxy acknowledged.
fn published(fixture: &Fixture) -> Vec<asc_gateway_plan::Plan> {
    let dir = &fixture.config.runtime_dir;
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("active.json")).unwrap()).unwrap();
    manifest["listeners"]
        .as_array()
        .unwrap()
        .iter()
        .map(|listener| {
            serde_json::from_slice(&fs::read(dir.join(listener["plan"].as_str().unwrap())).unwrap())
                .unwrap()
        })
        .collect()
}

const QODER: &str = include_str!("../../../../fixtures/policy/qoder-v3.cedar");

#[test]
fn pap_size_and_scope_limits_reject_candidates_before_changing_the_live_gateway() {
    let fixture = Fixture::new();
    fixture.key(REAL);
    let pair = fixture.pair_id();
    let service = fixture.service();
    let serving = grant_for(&fixture, "original", 1, UID, &pair, "serving");
    publish(&service, None, &serving, &[]).unwrap();
    service.enable().unwrap();
    ready_binding(
        &fixture,
        &GatewayBindingClient::new(service.clone()),
        &serving,
        77,
        &[],
    );
    let current_rules = fixture.rules();
    assert!(current_rules.contains("counter redirect"));
    let current_manifest = fs::read(fixture.config.runtime_dir.join("active.json")).unwrap();
    let check_rejected = |next: &PreparedScope, reason: &str| {
        assert!(
            publish(&service, None, next, &[])
                .unwrap_err()
                .to_string()
                .contains(reason)
        );
        assert!(fixture.host.log().active);
        assert_eq!(fixture.rules(), current_rules);
        assert_eq!(
            fs::read(fixture.config.runtime_dir.join("active.json")).unwrap(),
            current_manifest
        );
        assert!(matches!(
            fixture.repository.get_scope(&next.scope_id),
            Err(asc_pap::PapError::NotFound)
        ));
    };
    let mut too_big = grant_for(&fixture, "large-first", 1, UID, &pair, "too-big");
    for i in 0..5 {
        let name = format!("large-{i}");
        let padded = QODER.replace("api.github.com", &format!("{name}.example.com"))
            + "\n//"
            + &"x".repeat(210 * 1024);
        let snapshot = named_source(&fixture, &name, 1, &padded);
        too_big
            .policy_snapshots
            .extend(candidate(&snapshot, "too-big", UID, &["policy1"]).policy_snapshots);
    }
    assert!(serde_json::to_vec(&too_big).unwrap().len() > 1024 * 1024);
    check_rejected(&too_big, "exceeds 1 MiB");

    let manual = fixture.scope("manual-seed", 1);
    for i in 0..30 {
        let mut scope = manual.clone();
        scope.scope_id = ResourceId::new(format!("manual-{i}")).unwrap();
        fixture.repository.put_scope(&scope).unwrap();
    }
    let overflow = grant_for(&fixture, "overflow", 1, UID, &pair, "overflow");
    check_rejected(&overflow, "32 active Scopes");
}

#[test]
fn rejected_candidate_keeps_the_serving_rules_until_restart_or_valid_publication() {
    let fixture = Fixture::new();
    fixture.key(REAL);
    let service = fixture.service();
    let get = QODER.replace("== 1000", &format!("== {UID}"));
    let first = candidate(
        &source(&fixture, 1, &get),
        "automatic-qoder",
        UID,
        &["policy1"],
    );
    publish(&service, None, &first, &[]).unwrap();
    service.enable().unwrap();
    let client = GatewayBindingClient::new(service.clone());
    ready_binding(&fixture, &client, &first, 77, &[]);
    let serving = published(&fixture);
    assert!(matches!(
        service.status(UID).scopes[0].automatic,
        Some(crate::AutomaticPublication::Serving {
            can_inject: true,
            ..
        })
    ));
    assert_eq!(
        serving[0].rules[0].matcher.methods,
        [asc_gateway_plan::Method::Get]
    );
    let denied = format!(
        "{get}\nforbid(principal,action,resource is SecCore::HttpEndpoint) when {{ resource.host == \"api.github.com\" }};"
    );
    let invalid = candidate(
        &source(&fixture, 2, &denied),
        "automatic-qoder",
        UID,
        &["policy1", "policy2"],
    );
    assert!(publish(&service, Some(&first), &invalid, &[]).is_err());
    ready_binding(&fixture, &client, &first, 78, &[77]);
    assert_eq!(published(&fixture), serving);
    assert_eq!(fixture.rules().matches("counter redirect").count(), 2);
    assert_eq!(
        fixture.repository.get_scope(&first.scope_id).unwrap(),
        first
    );
    drop(client);
    drop(service);
    let restarted = fixture.service();
    restarted.recover().unwrap();
    assert!(published(&fixture).is_empty());
    assert_eq!(
        restarted.status(UID).scopes[0].automatic,
        Some(crate::AutomaticPublication::Unavailable)
    );
    assert_eq!(fixture.rules().matches("counter redirect").count(), 0);
    assert!(publish(&restarted, Some(&first), &invalid, &[]).is_err());
    assert!(published(&fixture).is_empty());
    let post = get.replace(
        "context.input.method == \"GET\"",
        "[\"GET\", \"POST\"].contains(context.input.method)",
    );
    let corrected = candidate(
        &source(&fixture, 3, &post),
        "automatic-qoder",
        UID,
        &["policy1"],
    );
    publish(&restarted, Some(&first), &corrected, &[]).unwrap();
    assert_eq!(
        published(&fixture)[0].rules[0].matcher.methods,
        [
            asc_gateway_plan::Method::Get,
            asc_gateway_plan::Method::Post
        ]
    );
    assert_eq!(fixture.rules().matches("counter redirect").count(), 0);
    // 78 execed into another executable while the daemon was offline. Its pidfd,
    // cgroup and UID still match, but discovery now selects only the original 77.
    let generation = fixture
        .repository
        .automatic_scope_generation(&first.scope_id)
        .unwrap()
        .unwrap();
    assert!(
        restarted
            .resume_observed(&first.scope_id, generation + 1, &[process(78)])
            .is_err()
    );
    assert_eq!(fixture.rules().matches("counter redirect").count(), 0);
    restarted
        .resume_observed(&first.scope_id, generation, &[process(77)])
        .unwrap();
    assert_eq!(fixture.rules().matches("counter redirect").count(), 1);
    assert!(fixture.rules().contains("-77-"));
    assert!(!fixture.rules().contains("-78-"));
    assert!(publish(&restarted, Some(&first), &invalid, &[]).is_err());
    assert_eq!(
        fixture.repository.get_scope(&first.scope_id).unwrap(),
        corrected
    );
}

#[test]
fn moving_a_grant_between_owners_preserves_the_credential_and_removes_the_old_listener() {
    let fixture = Fixture::new();
    fixture.key(REAL);
    let service = fixture.service();
    let first = candidate(
        &source(&fixture, 1, &QODER.replace("== 1000", &format!("== {UID}"))),
        "qoder-first",
        UID,
        &["policy1"],
    );
    publish(&service, None, &first, &[]).unwrap();
    service.enable().unwrap();
    let moved = candidate(
        &source(
            &fixture,
            2,
            &QODER.replace("== 1000", &format!("== {}", UID + 1)),
        ),
        "qoder-next",
        UID + 1,
        &["policy1"],
    );
    publish(&service, None, &moved, &[(Some(&first), None)]).unwrap();
    let plans = published(&fixture);
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].scope_id.as_str(), moved.scope_id.as_str());
    assert_eq!(plans[0].credentials, vec![fixture.pair_id()]);
    assert_eq!(
        fixture
            .repository
            .get_scope(&first.scope_id)
            .unwrap()
            .status,
        ScopeStatus::Deleting
    );
    assert_eq!(
        fixture.repository.get_scope(&moved.scope_id).unwrap(),
        moved
    );
}

#[test]
fn transferring_one_policy_preserves_the_other_grant_in_a_shared_listener() {
    let fixture = Fixture::new();
    fixture.key(REAL);
    let service = fixture.service();
    let a = named_source(
        &fixture,
        "a",
        1,
        &QODER.replace("== 1000", &format!("== {UID}")),
    );
    let b = named_source(
        &fixture,
        "b",
        1,
        &QODER
            .replace("api.github.com", "storage.example.com")
            .replace("== 1000", &format!("== {UID}")),
    );
    let remaining = candidate(&b, "shared", UID, &["policy1"]);
    let mut shared = candidate(&a, "shared", UID, &["policy1"]);
    shared
        .policy_snapshots
        .extend_from_slice(&remaining.policy_snapshots);
    publish(&service, None, &shared, &[]).unwrap();
    service.enable().unwrap();
    let moved = candidate(
        &named_source(
            &fixture,
            "a",
            2,
            &QODER.replace("== 1000", &format!("== {}", UID + 1)),
        ),
        "new-owner",
        UID + 1,
        &["policy1"],
    );
    publish(&service, None, &moved, &[(Some(&shared), Some(&remaining))]).unwrap();
    let plans = published(&fixture);
    assert_eq!(plans.len(), 2);
    for (scope, expected_host) in [
        (&remaining, "storage.example.com"),
        (&moved, "api.github.com"),
    ] {
        let plan = plans
            .iter()
            .find(|plan| plan.scope_id.as_str() == scope.scope_id.as_str())
            .unwrap();
        assert_eq!(plan.rules.len(), 1);
        assert_eq!(plan.rules[0].matcher.host, expected_host);
        assert_eq!(plan.credentials, vec![fixture.pair_id()]);
        assert_eq!(
            &fixture.repository.get_scope(&scope.scope_id).unwrap(),
            scope
        );
    }
}

// Create independent registered pairs so retiring either one is observable.
fn second_pair(fixture: &Fixture) -> CredentialId {
    CredentialStore::open(&fixture.config.store_dir)
        .unwrap()
        .register_pairs(vec![PairInput {
            real: Secret::new(OTHER_REAL.to_owned()).unwrap(),
            fake: "Y".repeat(40),
        }])
        .unwrap()
        .remove(0)
}

fn grant_for(
    fixture: &Fixture,
    name: &str,
    rev: u32,
    uid: u32,
    pair: &CredentialId,
    scope_id: &str,
) -> PreparedScope {
    let cedar = QODER
        .replace("api.github.com", &format!("{name}.example.com"))
        .replace("qoder-github", name)
        .replace("== 1000", &format!("== {uid}"));
    candidate(
        &source_with_pair(fixture, name, rev, &cedar, pair),
        scope_id,
        uid,
        &["policy1"],
    )
}

#[test]
fn splitting_one_listener_publishes_both_destinations_or_keeps_both_original_grants() {
    let fixture = Fixture::new();
    fixture.key(REAL);
    let a = fixture.pair_id();
    let b = second_pair(&fixture);
    let service = fixture.service();
    let mut shared = grant_for(&fixture, "github", 1, UID, &a, "shared");
    shared
        .policy_snapshots
        .extend(grant_for(&fixture, "gitlab", 1, UID, &b, "shared").policy_snapshots);
    publish(&service, None, &shared, &[]).unwrap();
    service.enable().unwrap();
    let old_plans = published(&fixture);
    let first = grant_for(&fixture, "github", 2, UID + 1, &a, "owner-a");
    let second = grant_for(&fixture, "gitlab", 2, UID + 2, &b, "owner-b");
    let mut invalid = second.clone();
    invalid.owner_uid = Some(0);
    assert!(
        publish(
            &service,
            None,
            &first,
            &[(Some(&shared), None), (None, Some(&invalid))]
        )
        .is_err()
    );
    assert_eq!(published(&fixture), old_plans);
    assert_eq!(
        fixture.repository.get_scope(&shared.scope_id).unwrap(),
        shared
    );
    assert!(matches!(
        fixture.repository.get_scope(&first.scope_id),
        Err(asc_pap::PapError::NotFound)
    ));
    publish(
        &service,
        None,
        &first,
        &[(Some(&shared), None), (None, Some(&second))],
    )
    .unwrap();
    let plans = published(&fixture);
    assert_eq!(plans.len(), 2);
    for (scope, key) in [(&first, a), (&second, b)] {
        let plan = plans
            .iter()
            .find(|p| p.scope_id.as_str() == scope.scope_id.as_str())
            .unwrap();
        assert_eq!(plan.credentials, vec![key]);
        assert_eq!(
            fixture.repository.get_scope(&scope.scope_id).unwrap(),
            *scope
        );
    }
    assert_eq!(
        fixture
            .repository
            .get_scope(&shared.scope_id)
            .unwrap()
            .status,
        ScopeStatus::Deleting
    );
}

#[test]
fn recovering_a_shared_automatic_scope_restores_only_its_live_credential() {
    let fixture = Fixture::new();
    fixture.key(REAL);
    let retired = fixture.pair_id();
    let live = second_pair(&fixture);
    let service = fixture.service();
    let mut shared = grant_for(&fixture, "github", 1, UID, &retired, "shared");
    shared
        .policy_snapshots
        .extend(grant_for(&fixture, "gitlab", 1, UID, &live, "shared").policy_snapshots);
    publish(&service, None, &shared, &[]).unwrap();
    service.enable().unwrap();
    service.remove_key(retired.as_str()).unwrap();
    let updated_b = source_with_pair(
        &fixture,
        "gitlab",
        2,
        &QODER
            .replace("qoder-github", "gitlab")
            .replace("== 1000", &format!("== {UID}"))
            .replace("api.github.com", "new.example.com"),
        &live,
    );
    let mut updated = shared.clone();
    updated.policy_snapshots[1] = candidate(&updated_b, "shared", UID, &["policy1"])
        .policy_snapshots
        .remove(0);
    publish(&service, Some(&shared), &updated, &[]).unwrap();
    let live_plan = published(&fixture);
    assert_eq!(live_plan.len(), 1);
    assert_eq!(live_plan[0].credentials, vec![live]);
    assert_eq!(live_plan[0].rules.len(), 1);
    assert_eq!(live_plan[0].rules[0].matcher.host, "new.example.com");
    drop(service);
    let restarted = fixture.service();
    restarted.recover().unwrap();
    assert!(published(&fixture).is_empty());
    publish(&restarted, Some(&updated), &updated, &[]).unwrap();
    assert_eq!(published(&fixture), live_plan);

    // Reusing the same retired pair for a new destination on the same Scope is a new grant.
    let changed_a = source_with_pair(
        &fixture,
        "github",
        2,
        &QODER
            .replace("qoder-github", "github")
            .replace("== 1000", &format!("== {UID}"))
            .replace("api.github.com", "unapproved.example.com"),
        &retired,
    );
    let mut rejected = updated.clone();
    rejected.policy_snapshots[0] = candidate(&changed_a, "shared", UID, &["policy1"])
        .policy_snapshots
        .remove(0);
    assert!(publish(&restarted, Some(&updated), &rejected, &[]).is_err());
    let mut new_target = rejected;
    new_target.scope_id = ResourceId::new("other").unwrap();
    assert!(publish(&restarted, None, &new_target, &[]).is_err());
    assert_eq!(published(&fixture), live_plan);
}

#[test]
fn deleted_grant_stays_withdrawn_when_another_scope_republishes() {
    let fixture = Fixture::new();
    fixture.key(REAL);
    let service = fixture.service();
    let a = named_source(
        &fixture,
        "a",
        1,
        &QODER.replace("== 1000", &format!("== {UID}")),
    );
    let b = named_source(
        &fixture,
        "b",
        1,
        &QODER
            .replace("api.github.com", "b.example.com")
            .replace("== 1000", &format!("== {UID}")),
    );
    let mut shared = candidate(&a, "shared", UID, &["policy1"]);
    shared
        .policy_snapshots
        .extend(candidate(&b, "shared", UID, &["policy1"]).policy_snapshots);
    publish(&service, None, &shared, &[]).unwrap();
    service.enable().unwrap();
    let client = GatewayBindingClient::new(service.clone());
    ready_binding(&fixture, &client, &shared, 77, &[]);
    let prohibition = named_source(
        &fixture,
        "c",
        1,
        r#"forbid(principal,action,resource is SecCore::HttpEndpoint) when { resource.host == "api.github.com" };"#,
    );
    let mut invalid = shared.clone();
    invalid
        .policy_snapshots
        .extend(candidate(&prohibition, "shared", UID, &["policy0"]).policy_snapshots);
    assert!(publish(&service, Some(&shared), &invalid, &[]).is_err());
    assert_eq!(published(&fixture)[0].rules.len(), 2);
    fixture
        .repository
        .delete_policy_revision(&a.policy_id, a.revision)
        .unwrap();
    invalid
        .policy_snapshots
        .retain(|p| p.policy_id != a.policy_id);
    assert!(publish(&service, Some(&shared), &invalid, &[]).is_err());
    service.revoke_scope(&shared).unwrap();
    assert!(published(&fixture).is_empty());
    let manual = fixture.scope("unrelated", 1);
    service.prepare_scope(&manual).unwrap();
    let plans = published(&fixture);
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].scope_id.as_str(), "unrelated");
    assert_eq!(fixture.rules().matches("counter redirect").count(), 0);
}

#[test]
fn listener_fault_withdraws_traffic_like_manual_publication_and_initial_auto_assignment_retries() {
    for automatic in [false, true] {
        let fixture = Fixture::new();
        fixture.key(REAL);
        let service = fixture.service();
        service.enable().unwrap();
        let scope = if automatic {
            candidate(
                &source(&fixture, 1, &QODER.replace("== 1000", &format!("== {UID}"))),
                "first-auto",
                UID,
                &["policy1"],
            )
        } else {
            fixture.scope("first-manual", 1)
        };
        fixture.host.log().failed_operation = Some("publish routes");
        let result = if automatic {
            publish(&service, None, &scope, &[])
        } else {
            service.prepare_scope(&scope)
        };
        assert!(result.is_err());
        assert!(!service.status(UID).ready);
        assert!(!fixture.host.log().active);
        assert_eq!(fixture.rules(), crate::nft::render_delete());
        if automatic {
            assert!(matches!(
                fixture.repository.get_scope(&scope.scope_id),
                Err(asc_pap::PapError::NotFound)
            ));
            assert!(
                service.status(UID).scopes.is_empty(),
                "the failed initial publication must release its unused listener port"
            );
            assert!(
                fixture.service().status(UID).scopes.is_empty(),
                "the reservation must also be removed from durable state"
            );
            {
                let mut host = fixture.host.log();
                host.failed_operation = Some("stop");
                host.owner_sensitive_on_start = true;
            }
            // The late owner check fails after startup. If the proxy cannot be
            // stopped, retain the reservation so recovery fences its old listener.
            assert!(publish(&service, None, &scope, &[]).is_err());
            assert_eq!(service.status(UID).scopes.len(), 1);
            assert_eq!(fixture.service().status(UID).scopes.len(), 1);
            fixture.host.log().owner_sensitive_on_start = false;
            drop(service);
            fixture.host.log().failed_operation = None;
            let restarted = fixture.service();
            restarted.recover().unwrap();
            publish(&restarted, None, &scope, &[]).unwrap();
            assert_eq!(published(&fixture)[0].credentials, vec![fixture.pair_id()]);
        }
    }
}
