use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use super::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ThreadCase {
    case_id: String,
    variant: String,
    block_at: String,
    initial: ReconcileRecord,
    calls: Vec<Call>,
    admission: Option<BindingView>,
    intermediate: ReconcileRecord,
    expected: Option<ReconcileRecord>,
    trace: Vec<String>,
    first: Disposition,
    second: Disposition,
}

struct Blocking {
    inner: Arc<Harness>,
    block_at: String,
    entered: Sender<()>,
    release: Mutex<Receiver<()>>,
}

impl Blocking {
    fn block(&self, point: &str) {
        if self.block_at == point {
            self.entered.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
    }
}

impl TargetDeploymentClient for Blocking {
    fn prepare_apply(&self, plan: &TargetBindingPlan) -> Result<PreparedApply, Failure> {
        self.inner.prepare_apply(plan)
    }
    fn create(&self, prepared: &PreparedApply) -> DeploymentReport {
        let report = self.inner.create(prepared);
        self.block("create");
        report
    }
    fn update(&self, previous: &[TargetRef], prepared: &PreparedApply) -> DeploymentReport {
        self.inner.update(previous, prepared)
    }
    fn delete(&self, targets: &[TargetRef]) -> DeploymentReport {
        self.inner.delete(targets)
    }
}

impl BindingStateRepository for Blocking {
    fn get_binding_state(
        &self,
        id: &ResourceId,
    ) -> Result<Option<BindingStateSnapshot>, StoreError> {
        self.inner.get_binding_state(id)
    }
    fn compare_exchange_binding_state(
        &self,
        expected: &BindingStateSnapshot,
        write: &BindingStateWrite,
    ) -> Result<WriteResult, StoreError> {
        if store::write_phase(expected, write) == "finish" {
            self.block("finish");
        }
        self.inner.compare_exchange_binding_state(expected, write)
    }
}

pub(super) fn run(objects: &BTreeMap<String, Value>) -> BTreeSet<String> {
    let raw = serde_json::from_str(include_str!(
        "../../../../fixtures/reconciliation/concurrency.json"
    ))
    .unwrap();
    let cases: Vec<ThreadCase> = serde_json::from_value(expand(raw, objects)).unwrap();
    let mut ids = BTreeSet::new();
    for case in cases {
        let key = format!("{}/{}", case.case_id, case.variant);
        assert!(ids.insert(key.clone()));
        run_case(case);
        eprintln!("PASS {key}");
    }
    ids
}

fn run_case(case: ThreadCase) {
    let id = case.initial.binding.spec.binding_id.clone();
    let harness = Arc::new(Harness {
        repository: ProcessLocalPapRepository::with_binding_states(vec![case.initial]).unwrap(),
        script: Mutex::new(Script {
            calls: case.calls.into(),
            ..Script::default()
        }),
        now: AtomicU64::new(0),
        id: id.clone(),
    });
    let (entered_tx, entered_rx) = channel();
    let (release_tx, release_rx) = channel();
    let blocking = Arc::new(Blocking {
        inner: harness.clone(),
        block_at: case.block_at.clone(),
        entered: entered_tx,
        release: Mutex::new(release_rx),
    });
    // The caller serializes attempts. Runtime tests verify queue ownership.
    let build = || {
        BindingReconciler::new(
            blocking.clone(),
            harness.clone(),
            BTreeMap::from([("test".into(), {
                let client = blocking.clone();
                Arc::new(move || Ok(client.clone() as Arc<dyn TargetDeploymentClient>))
                    as Arc<dyn crate::TargetDeploymentClientFactory>
            })]),
            "test".into(),
            harness.clone(),
            RetryPolicy {
                max_attempts: 3,
                base_delay_ms: 100,
                max_delay_ms: 150,
            },
        )
        .unwrap()
    };
    let first = build();
    let second = build();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        let first_id = id.clone();
        let mut first_task = tokio::task::spawn_blocking(move || {
            first.reconcile(&first_id, &mut crate::AttemptSchedule::default())
        });
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            harness.repository.read(&id).unwrap().unwrap(),
            case.intermediate
        );
        if let Some(desired) = &case.admission {
            let before = harness.repository.read(&id).unwrap().unwrap();
            assert!(
                harness
                    .repository
                    .compare_exchange_reconcile_intent(
                        &ExpectedBinding::from_binding(&before.binding),
                        desired
                    )
                    .unwrap()
            );
            harness.script.lock().unwrap().trace.push("admit".into());
        }
        if case.block_at == "create" {
            // Timeout/abort of an async waiter does not terminate an already
            // running blocking call. Retain its handle and join it explicitly.
            assert!(
                tokio::time::timeout(Duration::ZERO, &mut first_task)
                    .await
                    .is_err()
            );
            first_task.abort();
        }
        assert_eq!(
            harness
                .script
                .lock()
                .unwrap()
                .trace
                .iter()
                .filter(|e| e.as_str() == "claim")
                .count(),
            1
        );
        release_tx.send(()).unwrap();
        assert_eq!(first_task.await.unwrap().unwrap(), case.first);
        assert_eq!(
            second
                .reconcile(&id, &mut crate::AttemptSchedule::default())
                .unwrap(),
            case.second
        );
    });
    assert_eq!(harness.repository.read(&id).unwrap(), case.expected);
    let script = harness.script.lock().unwrap();
    assert!(script.calls.is_empty());
    assert_eq!(script.trace, case.trace);
}
