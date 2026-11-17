//! Shared mixed fixture through both production Clients, with controlled host and HTTP ports.

use super::*;
use asc_agentsight_client::{
    AgentSightClient, AgentSightHttpMethod, AgentSightHttpRequest, AgentSightHttpResponse,
    AgentSightTransport, AgentSightTransportError, ProcessIdentityError, ProcessIdentityResolver,
};
use asc_pcp::{AttemptSchedule, BindingReconciler, Clock, Disposition, RetryPolicy};
use asc_policy_target_contracts::TargetDeploymentClientFactory;
use asc_policy_translator_agentsight::AgentSightTranslator;
use asc_policy_types::target::TranslationOutcome;
use serde_json::{Value, json};

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../../fixtures/reconciliation/mixed-file-network.json"
    ))
    .unwrap()
}

struct Identity(CanonicalProcess);
impl ProcessIdentityResolver for Identity {
    fn process_start_time(&self, pid: i32) -> Result<u64, ProcessIdentityError> {
        assert_eq!(u32::try_from(pid).unwrap(), self.0.pid);
        Ok(self.0.start_time)
    }
    fn boot_id(&self) -> Result<String, ProcessIdentityError> {
        Ok(self.0.boot_id.clone())
    }
    fn pid_namespace(&self, pid: i32) -> Result<String, ProcessIdentityError> {
        assert_eq!(u32::try_from(pid).unwrap(), self.0.pid);
        Ok(self.0.pid_namespace.clone())
    }
}

struct Transport {
    requests: Arc<Mutex<Vec<AgentSightHttpRequest>>>,
    host: FakeHost,
}
impl AgentSightTransport for Transport {
    fn send(
        &self,
        request: &AgentSightHttpRequest,
    ) -> Result<AgentSightHttpResponse, AgentSightTransportError> {
        self.requests.lock().unwrap().push(request.clone());
        let (status, body) = match request.method {
            AgentSightHttpMethod::Get => (
                200,
                json!({"ready":true,"backend":"actplane","capabilities":{
                    "file_delete_guard":false,"static_mount_topology":true,"file_entry_delete_protection":true
                }}),
            ),
            AgentSightHttpMethod::Post => {
                assert!(
                    self.host.log().moves.is_empty(),
                    "file request must precede Gateway attachment"
                );
                let request: Value =
                    serde_json::from_slice(request.body.as_ref().unwrap()).unwrap();
                assert_eq!(
                    request["file_protection_policy"],
                    fixture()["expectedFileProtection"]
                );
                assert!(request.get("policy_dsl").is_none());
                (
                    200,
                    json!({"request":request,"state":"enforced","domain_id":41}),
                )
            }
            AgentSightHttpMethod::Delete => {
                return Ok(AgentSightHttpResponse {
                    status: 204,
                    body: vec![],
                });
            }
        };
        Ok(AgentSightHttpResponse {
            status,
            body: serde_json::to_vec(&body).unwrap(),
        })
    }
}

struct TestClock;
impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        0
    }
}

fn factory(client: Arc<dyn TargetDeploymentClient>) -> Arc<dyn TargetDeploymentClientFactory> {
    Arc::new(move || Ok(client.clone()))
}

fn prepare(fixture: &Fixture) -> (Arc<GatewayService>, PreparedScope, PreparedBinding) {
    fixture.key(REAL);
    let mut seed: PreparedBinding =
        serde_json::from_value(self::fixture()["binding"].clone()).unwrap();
    seed.policy.credential_bindings[0].pair_id = fixture.pair_id().as_str().into();
    fixture.repository.put_policy(&seed.policy).unwrap();
    let scope = fixture
        .repository
        .put_scope(&PreparedScope {
            scope_id: seed.scope.scope_id.clone(),
            selector: seed.scope.selector.clone(),
            policy_snapshots: vec![seed.policy],
            owner_uid: Some(UID),
            status: ScopeStatus::Active,
        })
        .unwrap();
    let service = fixture.service();
    service.prepare_scope(&scope).unwrap();
    service.enable().unwrap();
    fixture.host.log().fence = Some((
        seed.scope.process.boot_id.clone(),
        seed.scope.process.pid_namespace.clone(),
    ));
    let mut process = TestProcess::new(0);
    process.start = seed.scope.process.start_time;
    fixture
        .host
        .log()
        .processes
        .insert(seed.scope.process.pid, process);
    let binding = fixture
        .repository
        .sync_scope_instances(&scope.scope_id, &[seed.scope.process])
        .unwrap()
        .remove(0)
        .spec
        .clone();
    (service, scope, binding)
}

fn reconciler(
    fixture: &Fixture,
    service: Arc<GatewayService>,
    binding: &PreparedBinding,
    requests: Arc<Mutex<Vec<AgentSightHttpRequest>>>,
) -> (BindingReconciler, Value) {
    let file = Arc::new(AgentSightClient::with_dependencies(
        Transport {
            requests,
            host: fixture.host.clone(),
        },
        Identity(binding.scope.process.clone()),
    ));
    let gateway = Arc::new(GatewayBindingClient::new(service));
    let mut expected = self::fixture()["expectedGatewayNetwork"].clone();
    expected["credentials"][0] = json!(fixture.pair_id());
    expected["rules"][0]["match"]["credentialId"] = json!(fixture.pair_id());
    let source = match &binding.policy.template {
        PolicySource::Cedar(source) => source,
        PolicySource::SecCore(_) => unreachable!(),
    };
    let syntax = asc_policy_cedar::pst(source).unwrap();
    let network =
        asc_policy_cedar::project_pst(&syntax, asc_policy_cedar::PolicyDomain::Network).unwrap();
    let network = asc_gateway_plan::compile_binding(asc_gateway_plan::GatewayBindingView {
        source: asc_gateway_plan::GatewayBindingSource::from_binding(binding),
        schema: &source.schema,
        credential_bindings: &binding.policy.credential_bindings,
        network: &network,
    })
    .unwrap();
    assert_eq!(serde_json::to_value(&network.network).unwrap(), expected);
    let files =
        asc_policy_cedar::project_pst(&syntax, asc_policy_cedar::PolicyDomain::File).unwrap();
    let TranslationOutcome::Translated(file_plan) = AgentSightTranslator
        .translate_cedar_pst(binding, &files)
        .unwrap()
    else {
        panic!("file view must translate")
    };
    let plans = vec![
        ("agentsight".into(), file_plan),
        (
            "gateway".into(),
            TargetBindingPlan {
                format: GATEWAY_BINDING_FORMAT.into(),
                content: serde_json::to_vec(&network).unwrap(),
            },
        ),
    ];
    let core = BindingReconciler::new(
        fixture.repository.clone(),
        Arc::new(|binding: &PreparedBinding| AgentSightTranslator.translate(binding)),
        BTreeMap::from([
            ("agentsight".into(), factory(file)),
            ("gateway".into(), factory(gateway)),
        ]),
        "agentsight".into(),
        Arc::new(TestClock),
        RetryPolicy {
            max_attempts: 1,
            base_delay_ms: 1,
            max_delay_ms: 1,
        },
    )
    .unwrap()
    .with_plan_builder(move |_| Ok(plans.clone()));
    (core, expected)
}

#[test]
fn mixed_fixture_delivers_both_clients_and_cleans_both_deployments() {
    let fixture = Fixture::new();
    let (service, scope, binding) = prepare(&fixture);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (core, expected) = reconciler(&fixture, service, &binding, requests.clone());
    let mut schedule = AttemptSchedule::default();
    assert_eq!(
        core.reconcile(&binding.binding_id, &mut schedule).unwrap(),
        Disposition::Completed
    );
    let state = fixture
        .repository
        .get_binding_state(&binding.binding_id)
        .unwrap()
        .unwrap();
    assert_eq!(state.binding.spec, binding);
    assert_eq!(state.binding.status.phase, BindingStatus::Ready);
    assert_eq!(state.deployments.len(), 2);
    assert!(
        state
            .deployments
            .iter()
            .all(|deployment| deployment.presence == asc_pcp::Presence::Present)
    );
    let manifest: Value =
        serde_json::from_slice(&fs::read(fixture.config.runtime_dir.join("active.json")).unwrap())
            .unwrap();
    let published: Value = serde_json::from_slice(
        &fs::read(
            fixture
                .config
                .runtime_dir
                .join(manifest["listeners"][0]["plan"].as_str().unwrap()),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(published, expected);
    assert!(fixture.rules().contains("counter redirect"));
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.method == AgentSightHttpMethod::Post)
            .count(),
        1
    );
    fixture
        .repository
        .sync_scope_instances(&scope.scope_id, &[])
        .unwrap();
    assert_eq!(
        core.reconcile(&binding.binding_id, &mut schedule).unwrap(),
        Disposition::Completed
    );
    assert!(
        fixture
            .repository
            .get_binding_state(&binding.binding_id)
            .unwrap()
            .is_none()
    );
    assert!(!fixture.rules().contains("counter redirect"));
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.method == AgentSightHttpMethod::Delete)
            .count(),
        1
    );
}
