//! A persisted route cannot silently redirect cleanup to a different `AgentSight` endpoint.
use asc_agentsight_client::AgentSightClientFactory;
use asc_pap::PapRepository;
use asc_pcp::{AttemptSchedule, BindingReconciler, Clock, TargetDeploymentClientFactory};
use asc_policy_repository::*;
use asc_policy_repository_sqlite::SqlitePolicyRepository;
use asc_policy_types::{
    binding::{BindingStatus, PreparedBinding},
    scope::{PreparedScope, ScopeStatus},
    target::{PreparedApply, Presence, TargetRef},
};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Now;
impl Clock for Now {
    fn now_ms(&self) -> u64 {
        0
    }
}

fn core(repo: Arc<SqlitePolicyRepository>, endpoint: &str, token: &Path) -> BindingReconciler {
    BindingReconciler::new(
        repo,
        Arc::new(|_: &PreparedBinding| panic!("cleanup must not translate")),
        BTreeMap::from([(
            "agentsight".into(),
            Arc::new(AgentSightClientFactory::new(endpoint, token))
                as Arc<dyn TargetDeploymentClientFactory>,
        )]),
        "agentsight".into(),
        Arc::new(Now),
        RetryPolicy {
            max_attempts: 2,
            base_delay_ms: 1,
            max_delay_ms: 2,
        },
    )
    .unwrap()
}

fn persist(repo: &SqlitePolicyRepository, target: TargetRef) -> BindingStateSnapshot {
    let fixture: PreparedBinding = serde_json::from_str(include_str!(
        "../../asc-policy-types/tests/fixtures/prepared-binding.json"
    ))
    .unwrap();
    repo.put_policy(&fixture.policy).unwrap();
    repo.put_scope(&PreparedScope {
        scope_id: fixture.scope.scope_id.clone(),
        selector: fixture.scope.selector,
        policy_snapshots: vec![fixture.policy],
        status: ScopeStatus::Active,
    })
    .unwrap();
    let intent = repo
        .sync_scope_instances(&fixture.scope.scope_id, &[fixture.scope.process])
        .unwrap()
        .remove(0);
    let saved = repo
        .get_binding_state(&intent.spec.binding_id)
        .unwrap()
        .unwrap();
    repo.compare_exchange_binding_state(
        &saved,
        &BindingStateWrite::patch(ReconciliationPatch {
            deployments: Some(vec![Deployment {
                target,
                revision: saved.binding.spec.binding_revision,
                presence: Presence::Unknown,
                last_confirmed: None,
            }]),
            ..Default::default()
        }),
    )
    .unwrap();
    repo.begin_scope_delete(&fixture.scope.scope_id).unwrap();
    repo.finish_scope_discovery(&fixture.scope.scope_id)
        .unwrap();
    repo.get_binding_state(&intent.spec.binding_id)
        .unwrap()
        .unwrap()
}

#[test]
fn reopened_cleanup_rejects_endpoint_change_and_retries_original_endpoint() {
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let path = dir.path().join("policy-state.db");
    let token = dir.path().join("token");
    std::fs::write(&token, "first-token").unwrap();
    let a = TcpListener::bind("127.0.0.1:0").unwrap();
    let b = TcpListener::bind("127.0.0.1:0").unwrap();
    a.set_nonblocking(true).unwrap();
    b.set_nonblocking(true).unwrap();
    let endpoint_a = format!("http://{}/api", a.local_addr().unwrap());
    let endpoint_b = format!("http://{}/api", b.local_addr().unwrap());
    let prepared: PreparedApply = serde_json::from_str(include_str!(
        "../../../fixtures/clients/agentsight/file-deletion/prepared-7.json"
    ))
    .unwrap();
    let mut target = prepared.target;
    let mut cleanup: serde_json::Value = serde_json::from_slice(&target.cleanup).unwrap();
    cleanup["endpoint"] = endpoint_a.clone().into();
    target.cleanup = serde_json::to_vec(&cleanup).unwrap();
    let repo = SqlitePolicyRepository::open(&path).unwrap();
    let before = persist(&repo, target.clone());
    drop(repo);
    let repo = Arc::new(SqlitePolicyRepository::open(&path).unwrap());
    let binding_id = &before.binding.spec.binding_id;
    core(repo.clone(), &endpoint_b, &token)
        .reconcile(binding_id, &mut AttemptSchedule::default())
        .unwrap();
    let rejected = repo.get_binding_state(binding_id).unwrap().unwrap();
    assert_eq!(rejected.binding.status.phase, BindingStatus::DeleteFailed);
    assert_eq!(
        rejected.binding.status.error.unwrap().code,
        "AGENTSIGHT_ENDPOINT_MISMATCH"
    );
    assert_eq!(rejected.deployments, before.deployments);
    for listener in [&a, &b] {
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
    std::fs::write(&token, "rotated-token").unwrap();
    repo.retry_scope(&before.binding.spec.scope.scope_id)
        .unwrap();
    let server = serve_delete(a, target);
    core(repo.clone(), &format!("{endpoint_a}/"), &token)
        .reconcile(binding_id, &mut AttemptSchedule::default())
        .unwrap();
    server.join().unwrap();
    assert!(repo.get_binding_state(binding_id).unwrap().is_none());
    assert_eq!(
        repo.get_scope(&before.binding.spec.scope.scope_id),
        Err(asc_pap::PapError::NotFound)
    );
    assert_eq!(
        b.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

fn serve_delete(listener: TcpListener, target: TargetRef) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
                result => panic!("missing cleanup: {result:?}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut reader = BufReader::new(&mut stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(
            line,
            format!(
                "DELETE /api/enforcement/bindings/{} HTTP/1.1\r\n",
                target.id
            )
        );
        let mut authorization = None;
        loop {
            line.clear();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            let (name, value) = line.trim().split_once(':').unwrap();
            if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.trim().to_owned());
            }
        }
        assert_eq!(authorization.as_deref(), Some("Bearer rotated-token"));
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
    })
}
