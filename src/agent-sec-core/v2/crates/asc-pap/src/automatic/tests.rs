//! Candidate fan-out retains one authored source and deploys valid groups independently.
use super::*;
use std::fmt::Write as _;

fn file_policy(id: &str, uids: std::ops::Range<u32>) -> PreparedPolicy {
    let mut text = format!("//{}\n", "x".repeat(190_000));
    for uid in uids {
        writeln!(text, r#"forbid(principal is SecCore::Subject, action == SecCore::Action::"file.read", resource is SecCore::File)
            when {{principal has agentType && principal.agentType == "qoder" && principal has ownerUid && principal.ownerUid == {uid}}}
            when {{resource.path == "/protected"}};"#).unwrap();
    }
    serde_json::from_value(serde_json::json!({
        "policyId": id, "revision": 1, "policyName":"test",
        "template":{"language":"cedar","content":{"schema":{"schemaId":"seccore.system","revision":3},"text":text}}
    })).unwrap()
}

#[test]
fn many_uid_targets_share_source_and_an_oversized_assignment_fails_independently() {
    let mut policies = vec![file_policy("all", 1000..1064)];
    for index in 0..5 {
        policies.push(file_policy(&format!("extra-{index}"), 1000..1001));
    }
    let mappings = BTreeMap::from([(
        "qoder".into(),
        ProcessMatcher::Name {
            process_name: "qoder".into(),
        },
    )]);
    let desired = derive(
        &policies,
        Ok(&mappings),
        &[asc_policy_translator_agentsight::selection::SELECTOR],
    )
    .unwrap();
    assert!(std::ptr::eq(desired.policies, policies.as_slice()));
    let publications = publications(&BTreeMap::new(), &desired).unwrap();
    assert_eq!(publications.len(), 64);
    assert!(publications.iter().all(|group| group.len() == 1));
    let mut admitted = 0;
    let mut rejected = 0;
    for group in publications {
        let [id] = group.as_slice() else {
            unreachable!()
        };
        let mut updates = ScopePublication::from([(
            id.clone(),
            ScopeUpdate {
                expected: None,
                candidate: None,
            },
        )]);
        let result = desired.prepare_publication(&mut updates);
        if desired.scopes[id].group.uid == Some(1000) {
            assert!(matches!(result, Err(PapError::InvalidScope(_))));
            assert!(updates[id].candidate.is_none());
            rejected += 1;
        } else {
            result.unwrap();
            assert_eq!(
                updates[id]
                    .candidate
                    .as_ref()
                    .unwrap()
                    .policy_snapshots
                    .len(),
                1
            );
            admitted += 1;
        }
    }
    assert_eq!((admitted, rejected), (63, 1));
}
