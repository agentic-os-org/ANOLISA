//! Bounded per-Skill debounce; notifications schedule existing service commands, never new policy.

use super::{SkillFsError, resolver::Resolver};
use asc_action_runtime::{ActionRuntime, ExecutionControl, Finalizer};
use asc_action_types::{ActionAttribution, ActionId, CallerIdentity, Correlation};
use asc_capability_skill_guard::command::GuardCommand;
use asc_capability_skill_guard::executor::{
    SkillGuardAuditProjector, SkillGuardExecutor, SkillGuardRequest,
};
use asc_capability_skill_guard::{SkillGuardService, SkillIdentity};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct Queue {
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
    resolver: Arc<Resolver>,
    service: Arc<SkillGuardService>,
    finalizer: Finalizer,
) -> Result<JoinHandle<()>, SkillFsError> {
    Ok(thread::Builder::new()
        .name("skillguard-notify".into())
        .spawn(move || {
            let _lifetime = WorkerLifetime(queue.clone());
            let runtime = ActionRuntime::new(
                ActionId::SkillGuard,
                SkillGuardExecutor::new(service),
                SkillGuardAuditProjector,
                finalizer,
            );
            while let Some((identity, peer)) = queue.next() {
                let attribution = ActionAttribution {
                    caller: peer,
                    correlation: Correlation::default(),
                };
                let mut errors = Vec::new();
                // Activation must still run when scanning fails or returns a no-op.
                for command in [
                    GuardCommand::Scan {
                        skill_dir: Some(identity.clone()),
                        all: false,
                        skill_dirs: Vec::new(),
                        scanners: None,
                        force: false,
                    },
                    GuardCommand::Activate {
                        skill_dir: identity.clone(),
                    },
                ] {
                    let deadline = Instant::now() + Duration::from_secs(30);
                    let prepared = resolver.resolve(&identity, deadline);
                    let (roots, preparation_error) = match prepared {
                        Ok(root) => (vec![root], None),
                        Err(error) => (
                            Vec::new(),
                            Some(asc_capability_skill_guard::GuardError::Integrity(
                                error.to_string(),
                            )),
                        ),
                    };
                    let request = SkillGuardRequest {
                        command,
                        roots,
                        caller_uid: peer.uid,
                        preparation_error,
                    };
                    loop {
                        let outcome = runtime.invoke(
                            &ExecutionControl {
                                deadline,
                                cancelled: false,
                            },
                            &attribution,
                            &request,
                        );
                        if outcome.error_type == "Busy"
                            && Instant::now() + Duration::from_millis(100) < deadline
                        {
                            thread::sleep(Duration::from_millis(100));
                            continue;
                        }
                        if !outcome.success || outcome.data["output"]["activationPending"] == true {
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
                    eprintln!(
                        "agent-sec-daemon: SkillFS background processing failed for {}: {error}",
                        identity.path().display()
                    );
                }
                queue.finish(error);
            }
        })?)
}

#[cfg(test)]
mod tests {
    use super::*;

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
