//! Unapproved proposals from authenticated peers within bounded input budgets.
//! Execution permits remain held after connection timeouts until execution ends.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use asc_intent::{
    IntentError, PolicyError, ProposalContext, ProposalError, ProposalOutcome, TaskInput,
    propose_task,
};
use asc_model_client::provider::{ModelProvider, ProviderError};
use sha2::{Digest as _, Sha256};

use crate::PeerCredentials;

/// Proposal failure categories without sensitive input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IntentServiceError {
    /// Invalid task, lifetime or materials.
    #[error("invalid intent input")]
    InvalidArgument,
    /// Input budget or actual execution capacity is exhausted.
    #[error("intent capacity exhausted")]
    ResourceExhausted,
    /// Configuration is unavailable or the model returned no valid intent.
    #[error("intent service unavailable")]
    Unavailable,
    /// Cancellation, model deadline or candidate expiry.
    #[error("intent deadline exceeded")]
    DeadlineExceeded,
    /// Internal encoding or Cedar validation failed.
    #[error("intent compilation failed")]
    Internal,
}

/// Initial proposal input without caller-supplied identity or digests.
pub struct IntentInput {
    /// Original task text.
    pub task: String,
    /// Requested lifetime in seconds.
    pub ttl: u32,
}

/// Initial proposal use case shared by the daemon composition root.
/// Candidates and approval records are never persisted.
pub struct IntentService {
    provider: Arc<dyn ModelProvider>,
    model: String,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    executing: Mutex<BTreeSet<u32>>,
}

impl IntentService {
    /// Injects the configured model and trusted Unix-seconds clock without calling the model.
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        model: String,
        clock: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        Self {
            provider,
            model,
            clock: Arc::new(clock),
            executing: Mutex::new(BTreeSet::new()),
        }
    }

    /// Generates a candidate without approval or authorization.
    ///
    /// `request_id` must be server-generated and `peer` must come from authenticated transport.
    /// The cancellation callback reads live control state; a blocked provider retains its permit.
    ///
    /// # Errors
    /// Returns no candidate on invalid input, exhausted limits, cancellation, expiry,
    /// model failure or compilation failure.
    pub fn propose(
        &self,
        peer: PeerCredentials,
        request_id: &str,
        input: IntentInput,
        deadline: Instant,
        cancelled: impl Fn() -> bool,
    ) -> Result<ProposalOutcome, IntentServiceError> {
        let check = || {
            if Instant::now() >= deadline || cancelled() {
                Err(IntentServiceError::DeadlineExceeded)
            } else {
                Ok(())
            }
        };
        check()?;
        let _permit = self.acquire(peer.uid())?;
        let created_at = (self.clock)();
        if input.task.len() > 64 * 1024 {
            return Err(IntentServiceError::ResourceExhausted);
        }
        if input.task.trim().is_empty() || !(1..=1800).contains(&input.ttl) {
            return Err(IntentServiceError::InvalidArgument);
        }
        if self.model.trim().is_empty()
            || self.model.len() > 256
            || self.model.chars().any(char::is_control)
        {
            return Err(IntentServiceError::Unavailable);
        }
        let expires_at = created_at
            .checked_add(i64::from(input.ttl))
            .ok_or(IntentServiceError::Internal)?;
        let task_id = format!(
            "intent-{}-{:x}",
            peer.uid(),
            Sha256::digest(request_id.as_bytes())
        );
        let context = ProposalContext::new(task_id.clone(), created_at, input.ttl)
            .map_err(|_| IntentServiceError::Internal)?;
        let task = TaskInput {
            source_id: format!("source-{task_id}"),
            text: input.task,
        };
        check()?;
        let remaining = expires_at
            .checked_sub((self.clock)())
            .filter(|seconds| *seconds > 0)
            .ok_or(IntentServiceError::DeadlineExceeded)?;
        let model_deadline = deadline.min(Instant::now() + Duration::from_secs(30)).min(
            Instant::now()
                + Duration::from_secs(
                    u64::try_from(remaining).map_err(|_| IntentServiceError::DeadlineExceeded)?,
                ),
        );
        let result = propose_task(
            self.provider.as_ref(),
            &self.model,
            &task,
            &context,
            model_deadline,
        );
        check()?;
        if (self.clock)() >= expires_at {
            return Err(IntentServiceError::DeadlineExceeded);
        }
        let outcome = result.map_err(|error| {
            let reason = match &error {
                ProposalError::Intent(IntentError::InvalidDraft) => "invalid_draft_fields",
                ProposalError::Intent(IntentError::Provider(_)) => "provider_failure",
                ProposalError::Policy(_) => "cedar_validation",
                _ => "input_or_deadline",
            };
            tracing::warn!(target: "asc_process_diagnostic", reason, "intent proposal failed");
            proposal_error(&error)
        })?;
        let bytes = serde_json::to_vec(&outcome).map_err(|_| IntentServiceError::Internal)?;
        if bytes.len() > asc_intent::MAX_CANDIDATE_BYTES {
            return Err(IntentServiceError::ResourceExhausted);
        }
        check()?;
        if (self.clock)() >= expires_at {
            return Err(IntentServiceError::DeadlineExceeded);
        }
        Ok(outcome)
    }

    fn acquire(&self, uid: u32) -> Result<ExecutionPermit<'_>, IntentServiceError> {
        let mut executing = self
            .executing
            .lock()
            .map_err(|_| IntentServiceError::Unavailable)?;
        if executing.len() >= 4 || !executing.insert(uid) {
            return Err(IntentServiceError::ResourceExhausted);
        }
        Ok(ExecutionPermit {
            executing: &self.executing,
            uid,
        })
    }
}

struct ExecutionPermit<'a> {
    executing: &'a Mutex<BTreeSet<u32>>,
    uid: u32,
}

impl Drop for ExecutionPermit<'_> {
    fn drop(&mut self) {
        if let Ok(mut executing) = self.executing.lock() {
            executing.remove(&self.uid);
        }
    }
}

fn proposal_error(error: &ProposalError) -> IntentServiceError {
    match error {
        ProposalError::DeadlineExceeded
        | ProposalError::Intent(
            IntentError::DeadlineExceeded | IntentError::Provider(ProviderError::DeadlineExceeded),
        ) => IntentServiceError::DeadlineExceeded,
        ProposalError::Intent(IntentError::InvalidInput) => IntentServiceError::InvalidArgument,
        ProposalError::Intent(_) => IntentServiceError::Unavailable,
        ProposalError::Policy(PolicyError::TooLarge) => IntentServiceError::ResourceExhausted,
        ProposalError::InvalidContext | ProposalError::Policy(_) => IntentServiceError::Internal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asc_model_client::provider::FakeProvider;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    fn service() -> IntentService {
        IntentService::new(
            Arc::new(FakeProvider::new("{}".into())),
            "fixture".into(),
            || 2_000_000_000,
        )
    }

    fn input(ttl: u32) -> IntentInput {
        IntentInput {
            task: "Read /work/report.csv".into(),
            ttl,
        }
    }

    struct Blocked {
        entered: std::sync::mpsc::Sender<()>,
        release: Arc<(Mutex<bool>, std::sync::Condvar)>,
    }
    impl ModelProvider for Blocked {
        fn complete(&self, _: &str, _: &str, _: &str, _: Instant) -> Result<String, ProviderError> {
            self.entered.send(()).unwrap();
            let (lock, wake) = &*self.release;
            let (released, _) = wake
                .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(5), |released| {
                    !*released
                })
                .unwrap();
            assert!(*released, "blocked fixture watchdog");
            Err(ProviderError::Transport)
        }
    }

    #[test]
    fn cancelled_executions_keep_per_uid_and_global_capacity_until_provider_returns() {
        let (entered, started) = std::sync::mpsc::channel();
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let service = IntentService::new(
            Arc::new(Blocked {
                entered,
                release: Arc::clone(&release),
            }),
            "fixture".into(),
            || 2_000_000_000,
        );
        std::thread::scope(|threads| {
            let jobs: Vec<_> = (0..4)
                .map(|uid| {
                    let service = &service;
                    let cancelled = &cancelled;
                    threads.spawn(move || {
                        service.propose(
                            PeerCredentials::new(uid, uid, 1),
                            "blocked",
                            input(1),
                            Instant::now() + Duration::from_secs(5),
                            || cancelled.load(std::sync::atomic::Ordering::Relaxed),
                        )
                    })
                })
                .collect();
            for _ in 0..4 {
                started.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
            for uid in [0, 1, 8] {
                assert!(matches!(
                    service.propose(
                        PeerCredentials::new(uid, uid, 2),
                        "next",
                        input(1),
                        Instant::now() + Duration::from_secs(5),
                        || false
                    ),
                    Err(IntentServiceError::ResourceExhausted)
                ));
            }
            *release.0.lock().unwrap() = true;
            release.1.notify_all();
            for job in jobs {
                assert!(matches!(
                    job.join().unwrap(),
                    Err(IntentServiceError::DeadlineExceeded)
                ));
            }
        });
        assert!(service.executing.lock().unwrap().is_empty());
        assert!(service.acquire(0).is_ok());
    }

    #[test]
    fn invalid_input_expiry_and_provider_errors_leave_no_capacity_or_candidate() {
        let service = service();
        for ttl in [0, 1801] {
            assert!(matches!(
                service.propose(
                    PeerCredentials::new(0, 0, 1),
                    "r",
                    input(ttl),
                    Instant::now() + Duration::from_secs(1),
                    || false
                ),
                Err(IntentServiceError::InvalidArgument)
            ));
        }
        assert!(matches!(
            service.propose(
                PeerCredentials::new(0, 0, 1),
                "r",
                input(1),
                Instant::now(),
                || false
            ),
            Err(IntentServiceError::DeadlineExceeded)
        ));
        assert!(matches!(
            service.propose(
                PeerCredentials::new(0, 0, 1),
                "r",
                input(1),
                Instant::now() + Duration::from_secs(1),
                || false
            ),
            Err(IntentServiceError::Unavailable)
        ));
        assert!(service.executing.lock().unwrap().is_empty());
        assert_eq!(
            proposal_error(&ProposalError::Policy(PolicyError::TooLarge)),
            IntentServiceError::ResourceExhausted
        );
        let clock = std::sync::atomic::AtomicI64::new(2_000_000_000);
        let expiring = IntentService::new(
            Arc::new(FakeProvider::new("{}".into())),
            "fixture".into(),
            move || clock.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        );
        assert!(matches!(
            expiring.propose(
                PeerCredentials::new(0, 0, 1),
                "r",
                input(1),
                Instant::now() + Duration::from_secs(5),
                || false
            ),
            Err(IntentServiceError::DeadlineExceeded)
        ));
    }

    #[test]
    fn execution_limits_include_root_and_release_only_on_drop() {
        let service = service();
        let permits: Vec<_> = (0..4).map(|uid| service.acquire(uid).unwrap()).collect();
        assert!(matches!(
            service.acquire(0),
            Err(IntentServiceError::ResourceExhausted)
        ));
        assert!(matches!(
            service.acquire(5),
            Err(IntentServiceError::ResourceExhausted)
        ));
        drop(permits);
        assert!(service.acquire(0).is_ok());
        assert!(service.executing.lock().unwrap().is_empty());
    }

    #[derive(Default)]
    struct ReadReply(std::sync::atomic::AtomicUsize);
    impl ModelProvider for ReadReply {
        fn complete(
            &self,
            _: &str,
            _: &str,
            input: &str,
            _: Instant,
        ) -> Result<String, ProviderError> {
            use serde_json::json;
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let input: serde_json::Value = serde_json::from_str(input).unwrap();
            assert_eq!(input["task"]["text"], "Read /work/report.csv");
            assert!(input.get("paths").is_none());
            let policy = "permit(principal, action == SecCore::Action::\"file.read\", resource is SecCore::File) when { resource.path == \"/work/report.csv\" };";
            Ok(json!({"policy":policy,"unresolved":[]}).to_string())
        }
    }

    #[test]
    fn completed_candidates_are_discarded_when_expired_before_return() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for expire_on in [None, Some(2), Some(3)] {
            let provider = Arc::new(ReadReply::default());
            let reads = AtomicUsize::new(0);
            let service = IntentService::new(provider.clone(), "fixture".into(), move || {
                let read = reads.fetch_add(1, Ordering::Relaxed);
                2_000_000_000 + i64::from(expire_on.is_some_and(|limit| read >= limit))
            });
            let input = input(1);
            let result = service.propose(
                PeerCredentials::new(0, 0, 1),
                "expiry",
                input,
                Instant::now() + Duration::from_secs(5),
                || false,
            );
            if expire_on.is_none() {
                assert!(matches!(result, Ok(ProposalOutcome::Candidate { .. })));
            } else {
                assert!(matches!(result, Err(IntentServiceError::DeadlineExceeded)));
            }
            assert_eq!(provider.0.load(Ordering::Relaxed), 1);
            assert!(service.executing.lock().unwrap().is_empty());
        }
    }

    struct Panicking;
    impl ModelProvider for Panicking {
        fn complete(&self, _: &str, _: &str, _: &str, _: Instant) -> Result<String, ProviderError> {
            panic!("fixture provider unwind");
        }
    }

    #[test]
    fn unwind_releases_execution_but_poisoned_accounting_stays_closed() {
        let service = IntentService::new(Arc::new(Panicking), "fixture".into(), || 2_000_000_000);
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                let _ = service.propose(
                    PeerCredentials::new(0, 0, 1),
                    "panic",
                    input(1800),
                    Instant::now() + Duration::from_secs(5),
                    || false,
                );
            }))
            .is_err()
        );
        assert!(service.acquire(0).is_ok());
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _lock = service.executing.lock().unwrap();
            panic!("fixture poison");
        }));
        assert!(matches!(
            service.acquire(0),
            Err(IntentServiceError::Unavailable)
        ));
    }
}
