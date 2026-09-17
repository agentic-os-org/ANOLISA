//! One bounded `SkillSec` worker for startup scans and authenticated `SkillFS` notifications.

use crate::skillfs::SkillFsError;
use asc_action_runtime::ExecutionControl;
use asc_action_types::{CallerIdentity, SkillIdentity, SkillSecCommand};
use asc_capability_skill_sec::SkillSecError;
use asc_daemon_core::{ActionService, PeerCredentials};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Daemon-owned worker shared by ordinary startup discovery and optional `SkillFS` notification.
#[derive(Default)]
pub struct SkillWorker {
    pub(crate) queue: Arc<Queue>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl SkillWorker {
    /// Starts once after recovery. Discovery and scanning never block socket admission.
    ///
    /// # Errors
    /// Rejects repeated startup, poisoned state and thread creation failures.
    pub fn start(
        &self,
        application: Arc<ActionService>,
        discover: impl FnOnce() -> Result<Vec<SkillIdentity>, SkillSecError> + Send + 'static,
    ) -> Result<(), SkillFsError> {
        let mut worker = self
            .worker
            .lock()
            .map_err(|_| SkillFsError::Invalid("worker state poisoned"))?;
        if worker.is_some() {
            return Err(SkillFsError::Invalid("worker already started"));
        }
        *worker = Some(start(self.queue.clone(), application, discover)?);
        Ok(())
    }

    /// Stops admission and joins the current bounded operation before sinks close.
    ///
    /// # Errors
    /// Reports poisoned state or worker panic.
    pub fn shutdown(&self) -> Result<(), SkillFsError> {
        self.queue.stop();
        if let Some(worker) = self
            .worker
            .lock()
            .map_err(|_| SkillFsError::Invalid("worker state poisoned"))?
            .take()
        {
            worker
                .join()
                .map_err(|_| SkillFsError::Invalid("worker failed"))?;
        }
        Ok(())
    }
}

impl Drop for SkillWorker {
    fn drop(&mut self) {
        self.queue.stop();
    }
}

#[derive(Default)]
pub(crate) struct Queue {
    state: Mutex<State>,
    changed: Condvar,
}
#[derive(Default)]
struct State {
    pending: BTreeMap<SkillIdentity, Entry>,
    startup: VecDeque<(SkillIdentity, CallerIdentity)>,
    stopping: bool,
    running: bool,
    processed: u64,
    failed: u64,
    last_error: Option<String>,
}
struct Entry {
    first: Instant,
    due: Instant,
    peer: CallerIdentity,
}

struct WorkerLifetime(Arc<Queue>);

impl Drop for WorkerLifetime {
    fn drop(&mut self) {
        // An unwinding worker must stop admission rather than leave a healthy, undrained queue.
        if let Ok(mut state) = self.0.state.lock() {
            if !state.stopping {
                state.last_error = Some("worker stopped unexpectedly; restart daemon".into());
            }
            state.stopping = true;
            state.running = false;
        }
        self.0.changed.notify_all();
    }
}

impl Queue {
    pub fn enqueue(
        &self,
        identity: SkillIdentity,
        peer: CallerIdentity,
    ) -> Result<bool, SkillFsError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| SkillFsError::Unavailable("notify queue poisoned"))?;
        if state.stopping {
            return Err(SkillFsError::Unavailable("notify worker is stopping"));
        }
        let now = Instant::now();
        let newly_queued = !state.pending.contains_key(&identity);
        if let Some(entry) = state.pending.get_mut(&identity) {
            entry.due =
                (now + Duration::from_millis(500)).min(entry.first + Duration::from_secs(2));
            entry.peer = peer;
        } else {
            if state.pending.len() >= 256 {
                return Err(SkillFsError::Unavailable(
                    "notify queue is full; reconcile later",
                ));
            }
            state.pending.insert(
                identity,
                Entry {
                    first: now,
                    due: now + Duration::from_millis(500),
                    peer,
                },
            );
        }
        self.changed.notify_one();
        Ok(newly_queued)
    }

    pub fn schedule_startup(&self, identities: Vec<SkillIdentity>) -> Result<(), SkillFsError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| SkillFsError::Invalid("worker state poisoned"))?;
        if state.stopping {
            return Err(SkillFsError::Invalid("worker is stopping"));
        }
        let caller = CallerIdentity {
            uid: rustix::process::geteuid().as_raw(),
            gid: rustix::process::getegid().as_raw(),
            pid: std::process::id(),
        };
        state.startup = identities
            .into_iter()
            .map(|identity| (identity, caller))
            .collect();
        self.changed.notify_one();
        Ok(())
    }

    pub fn status(&self) -> Value {
        self.state.lock().map_or_else(|_| json!({"healthy":false,"error":"worker state poisoned"}),|s| json!({"healthy":!s.stopping,"queued":s.pending.len()+s.startup.len(),"running":s.running,"processed":s.processed,"failed":s.failed,"lastError":s.last_error}))
    }

    pub fn stop(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.stopping = true;
        }
        self.changed.notify_all();
    }

    fn next(&self) -> Option<(SkillIdentity, CallerIdentity)> {
        let mut state = self.state.lock().ok()?;
        loop {
            if state.stopping {
                return None;
            }
            if let Some(entry) = state.startup.pop_front() {
                state.running = true;
                return Some(entry);
            }
            if let Some((identity, entry)) = state.pending.iter().min_by_key(|(_, e)| e.due) {
                if let Some(wait) = entry.due.checked_duration_since(Instant::now()) {
                    state = self.changed.wait_timeout(state, wait).ok()?.0;
                } else {
                    let identity = identity.clone();
                    let entry = state.pending.remove(&identity)?;
                    state.running = true;
                    return Some((identity, entry.peer));
                }
            } else {
                state = self.changed.wait(state).ok()?;
            }
        }
    }

    fn finish(&self, error: Option<String>) {
        if let Ok(mut state) = self.state.lock() {
            state.running = false;
            state.processed = state.processed.saturating_add(1);
            if error.is_some() {
                state.failed = state.failed.saturating_add(1);
            }
            state.last_error = error;
        }
    }
}

pub(super) fn start(
    queue: Arc<Queue>,
    application: Arc<ActionService>,
    discover: impl FnOnce() -> Result<Vec<SkillIdentity>, SkillSecError> + Send + 'static,
) -> Result<JoinHandle<()>, SkillFsError> {
    Ok(thread::Builder::new()
        .name("skillsec-worker".into())
        .spawn(move || {
            let _lifetime = WorkerLifetime(queue.clone());
            if let Ok(identities) = discover() {
                if queue.schedule_startup(identities).is_err() {
                    return;
                }
            } else {
                tracing::warn!("SkillSec startup discovery failed");
                if let Ok(mut state) = queue.state.lock() {
                    state.last_error = Some("startup discovery failed".into());
                }
            }
            while let Some((identity, caller)) = queue.next() {
                crate::skill_task_scope("notify", || {
                    let peer = PeerCredentials::new(caller.uid, caller.gid, caller.pid);
                    let mut errors = Vec::new();
                    // Activation must still run when scanning fails or returns a no-op.
                    for command in [
                        SkillSecCommand::Scan {
                            skill_dir: Some(identity.clone()),
                            all: false,
                            skill_dirs: Vec::new(),
                            scanners: None,
                            force: false,
                        },
                        SkillSecCommand::Activate {
                            skill_dir: identity.clone(),
                        },
                    ] {
                        let deadline = Instant::now() + Duration::from_secs(30);
                        loop {
                            let Ok(outcome) = application.skill_sec(
                                peer,
                                &ExecutionControl {
                                    deadline,
                                    cancelled: false,
                                },
                                command.clone(),
                            ) else {
                                errors.push("InternalExecutionError".into());
                                break;
                            };
                            if outcome.error_type == "Busy"
                                && Instant::now() + Duration::from_millis(100) < deadline
                            {
                                thread::sleep(Duration::from_millis(100));
                                continue;
                            }
                            if !outcome.success
                                || outcome.data["output"]["activationPending"] == true
                            {
                                errors.push(if outcome.error_type.is_empty() {
                                    "activationPending".into()
                                } else {
                                    outcome.error_type
                                });
                            }
                            break;
                        }
                    }
                    let error = (!errors.is_empty()).then(|| errors.join(", "));
                    if let Some(error) = &error {
                        tracing::warn!(error_type = error, "SkillSec background processing failed");
                    }
                    queue.finish(error);
                });
            }
        })?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use asc_action_runtime::{
        ActionRuntime, CapabilityExecutor, SecurityEventSink,
        testing::{audit_finalizer, discarding_finalizer},
    };
    use asc_action_types::{ActionId, ActionOutcome, SkillSecRequest};
    use asc_capability_code_scan::{CodeScanAuditProjector, CodeScanExecutor};
    use asc_capability_prompt_scan::{
        CachingScannerProvider, PromptScanAuditProjector, PromptScanExecutor, PromptScanWarmup,
    };
    use asc_capability_skill_sec::executor::SkillSecAuditProjector;
    use asc_security_events::SecurityEvent;

    fn application(
        executor: impl CapabilityExecutor<Request = SkillSecRequest> + 'static,
        sink: Arc<dyn SecurityEventSink>,
    ) -> Arc<ActionService> {
        let finalizer = audit_finalizer(sink);
        Arc::new(
            ActionService::new(
                ActionRuntime::new(
                    ActionId::CodeScan,
                    CodeScanExecutor,
                    CodeScanAuditProjector,
                    finalizer.clone(),
                ),
                ActionRuntime::new(
                    ActionId::PiiScan,
                    asc_capability_pii_scan::PiiScanExecutor::new(Arc::new(
                        asc_capability_pii_scan::PiiRuleSet::builtin().unwrap(),
                    )),
                    asc_capability_pii_scan::PiiAuditProjector,
                    finalizer.clone(),
                ),
                ActionRuntime::new(
                    ActionId::PromptScan,
                    PromptScanExecutor::default(),
                    PromptScanAuditProjector,
                    discarding_finalizer(),
                ),
                PromptScanWarmup::new(Arc::new(CachingScannerProvider::default())),
            )
            .with_skill_sec(ActionRuntime::new(
                ActionId::SkillSec,
                executor,
                SkillSecAuditProjector,
                finalizer,
            )),
        )
    }

    #[test]
    fn failed_scan_still_activates_and_worker_continues_with_audited_busy_retries() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Scripted {
            calls: Arc<AtomicUsize>,
            observed: Arc<Mutex<Vec<(String, bool)>>>,
        }
        impl CapabilityExecutor for Scripted {
            type Request = SkillSecRequest;
            fn execute(&self, _: &ExecutionControl, request: &SkillSecRequest) -> ActionOutcome {
                let invocation = self.calls.fetch_add(1, Ordering::SeqCst);
                let context = asc_observability::snapshot();
                self.observed.lock().unwrap().push((
                    request.command.name().to_owned(),
                    context.compatibility.trace_id.is_none() && context.agent.is_empty(),
                ));
                assert_ne!(invocation, 0, "PRIVATE_INTERNAL_FAILURE");
                ActionOutcome {
                    success: invocation != 2,
                    exit_code: i64::from(invocation == 2),
                    error: None,
                    error_type: if invocation == 2 {
                        "Busy".into()
                    } else {
                        String::new()
                    },
                    data: serde_json::Map::from_iter([("output".into(), json!({"status":"pass"}))]),
                }
            }
        }
        #[derive(Default)]
        struct Events(Mutex<Vec<SecurityEvent>>);
        impl SecurityEventSink for Events {
            fn write(&self, event: &SecurityEvent) {
                self.0.lock().unwrap().push(event.clone());
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let events = Arc::new(Events::default());
        let observed = Arc::new(Mutex::new(Vec::new()));
        let application = application(
            Scripted {
                calls: calls.clone(),
                observed: observed.clone(),
            },
            events.clone(),
        );
        let queue = Arc::new(Queue::default());
        let startup = vec![
            SkillIdentity::new("/fixture/first").unwrap(),
            SkillIdentity::new("/fixture/second").unwrap(),
        ];
        let context = asc_observability::bind_trace_context_input(
            &asc_observability::Context::new(),
            &json!({"trace_id":"unrelated_rpc", "agent_name":"unrelated_agent"}),
        )
        .unwrap();
        let _parent = context.attach();
        let worker = start(queue.clone(), application, move || Ok(startup)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while queue.status()["processed"] != 2 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let status = queue.status();
        queue.stop();
        worker.join().unwrap();
        assert_eq!(status["processed"], 2);
        assert_eq!(status["failed"], 1);
        assert_eq!(status["healthy"], true);
        assert_eq!(calls.load(Ordering::SeqCst), 5);
        let observed = observed.lock().unwrap();
        assert_eq!(
            observed
                .iter()
                .map(|(command, _)| command.as_str())
                .collect::<Vec<_>>(),
            ["scan", "activate", "scan", "scan", "activate"],
        );
        assert!(observed.iter().all(|(_, clean_context)| *clean_context));
        let events = events.0.lock().unwrap();
        assert_eq!(events.len(), 5);
        assert!(
            events
                .iter()
                .all(|event| event.uid == rustix::process::geteuid().as_raw()
                    && event.pid == std::process::id())
        );
        assert!(events.iter().all(|event| event.trace_id.is_empty()));
        assert!(
            !serde_json::to_string(&*events)
                .unwrap()
                .contains("PRIVATE_INTERNAL_FAILURE")
        );
    }

    #[test]
    fn startup_returns_before_scanning_finishes_and_shutdown_drains_only_current_task() {
        use std::sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc,
        };

        struct Paused {
            started: mpsc::Sender<()>,
            release: Mutex<mpsc::Receiver<()>>,
        }
        impl CapabilityExecutor for Paused {
            type Request = SkillSecRequest;
            fn execute(&self, _: &ExecutionControl, request: &SkillSecRequest) -> ActionOutcome {
                if request.command.name() == "scan" {
                    self.started.send(()).unwrap();
                    self.release
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap();
                }
                ActionOutcome {
                    success: true,
                    exit_code: 0,
                    error: None,
                    error_type: String::new(),
                    data: serde_json::Map::from_iter([("output".into(), json!({"status":"pass"}))]),
                }
            }
        }
        #[derive(Default)]
        struct Output {
            closed: AtomicBool,
            writes: AtomicUsize,
        }
        impl SecurityEventSink for Output {
            fn write(&self, _: &SecurityEvent) {
                assert!(!self.closed.load(Ordering::SeqCst));
                self.writes.fetch_add(1, Ordering::SeqCst);
            }
        }
        let (started, entered) = mpsc::channel();
        let (release, proceed) = mpsc::channel();
        let output = Arc::new(Output::default());
        let application = application(
            Paused {
                started,
                release: Mutex::new(proceed),
            },
            output.clone(),
        );
        let owner = SkillWorker::default();
        let queue = owner.queue.clone();
        let second = SkillIdentity::new("/fixture/second").unwrap();
        queue
            .schedule_startup(vec![
                SkillIdentity::new("/fixture/first").unwrap(),
                second.clone(),
            ])
            .unwrap();
        let startup = queue
            .state
            .lock()
            .unwrap()
            .startup
            .iter()
            .map(|(id, _)| id.clone())
            .collect();
        owner.start(application, move || Ok(startup)).unwrap();
        let entered = entered.recv_timeout(Duration::from_secs(5));
        queue.stop();
        release.send(()).unwrap();
        owner.shutdown().unwrap();
        entered.unwrap();
        assert_eq!(output.writes.load(Ordering::SeqCst), 2);
        assert_eq!(queue.status()["processed"], 1);
        assert_eq!(queue.status()["queued"], 1);
        assert_eq!(queue.status()["running"], false);
        output.closed.store(true, Ordering::SeqCst);
        let caller = CallerIdentity {
            uid: 1001,
            gid: 1002,
            pid: 1003,
        };
        assert!(queue.enqueue(second.clone(), caller).is_err());
        let restarted = Queue::default();
        restarted.schedule_startup(vec![second.clone()]).unwrap();
        assert_eq!(restarted.next().unwrap().0, second);
        restarted.stop();
    }

    #[test]
    fn unwinding_worker_closes_admission_and_reports_failed_health() {
        let queue = Arc::new(Queue::default());
        let worker_queue = queue.clone();
        let worker = thread::spawn(move || {
            let _lifetime = WorkerLifetime(worker_queue.clone());
            worker_queue.state.lock().unwrap().running = true;
            panic!("synthetic worker failure");
        });
        assert!(worker.join().is_err());
        assert_eq!(queue.status()["healthy"], false);
        assert_eq!(queue.status()["running"], false);
        assert!(
            queue.status()["lastError"]
                .as_str()
                .unwrap()
                .contains("restart")
        );
        assert!(matches!(
            queue.enqueue(
                SkillIdentity::new("/fixture/demo").unwrap(),
                CallerIdentity {
                    uid: 0,
                    gid: 0,
                    pid: 1
                }
            ),
            Err(SkillFsError::Unavailable(_))
        ));
    }
}
