//! Public runtime handle for the persistent cosh-core service.

use std::io::{BufWriter, Write};
use std::process::ChildStdin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::Value;

use crate::types::CoshApprovalMode;

use super::super::cosh_core::{mark_recovery_failure, SessionResumeAttempt, SessionRuntimeState};
use super::super::cosh_core_registry::{
    extension_mutation_requires_reload, registry_timeout, RegistryQueryError,
};
use super::super::{
    AdapterError, AgentRunHandle, ApprovalChannelMessage, AuthResponse, PreparedInvocation,
    ProviderCancellationArtifactStore,
};
use super::command::{RegistryCommand, RunCommand, ServiceCommand};
use super::process::{control_request, terminate_marked, ProcessBindings};

const CANCEL_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum AdmissionState {
    #[default]
    Idle,
    Run {
        queued: bool,
    },
    Registry,
}

impl AdmissionState {
    pub(super) fn finish_run(&mut self) {
        *self = match *self {
            Self::Run { queued: true } => Self::Run { queued: false },
            Self::Run { queued: false } => Self::Idle,
            state => state,
        };
    }
}

// A live registry command transfers ownership to the service instead.
struct ShortRegistryReservation(Arc<Mutex<AdmissionState>>);

impl Drop for ShortRegistryReservation {
    fn drop(&mut self) {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if *state == AdmissionState::Registry {
            *state = AdmissionState::Idle;
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct PersistentCoshCoreRuntime {
    pub(super) command_tx: Mutex<Option<mpsc::Sender<ServiceCommand>>>,
    pub(super) live: Arc<AtomicBool>,
    pub(super) admission: Arc<Mutex<AdmissionState>>,
    pub(super) cancel_pending: Arc<AtomicBool>,
    pub(super) reload_pending: Arc<AtomicBool>,
    /// Snapshot of the currently live process: expected-stop flag and pid are
    /// kept under one lock so Drop and forced cancellation always terminate
    /// the same process, even when a new process starts during the cancel
    /// grace window.
    pub(super) current_process: Arc<Mutex<ProcessBindings>>,
    pub(super) request_counter: AtomicU64,
    pub(super) active_stdin: Arc<Mutex<Option<Arc<Mutex<BufWriter<ChildStdin>>>>>>,
}

/// Terminate the process currently bound to the runtime, using the matching
/// expected-stop flag and pid captured under a single lock. This prevents a
/// direct-kill path from marking one process and killing another when the
/// bound process changes between inspection and termination.
pub(super) fn terminate_current_process(current_process: &Arc<Mutex<ProcessBindings>>) {
    let (expected_stop, pid) = current_process
        .lock()
        .ok()
        .map(|current| (current.expected_stop.clone(), current.child_pid))
        .unwrap_or((None, None));
    terminate_marked(expected_stop, pid);
}

impl Drop for PersistentCoshCoreRuntime {
    fn drop(&mut self) {
        // Grab the matching expected-stop flag and pid under one lock so the
        // kill path targets exactly the process that was live when Drop ran.
        terminate_current_process(&self.current_process);
        if let Ok(current) = self.command_tx.get_mut() {
            if let Some(sender) = current.take() {
                let _ = sender.send(ServiceCommand::Shutdown);
            }
        }
    }
}

impl PersistentCoshCoreRuntime {
    pub(in crate::adapter) fn start_run(
        &self,
        run_id: String,
        prepared: PreparedInvocation,
        raw_user_input: Option<String>,
        mode: CoshApprovalMode,
        session_state: Arc<Mutex<SessionRuntimeState>>,
        session_scope: String,
        resume_attempt: SessionResumeAttempt,
    ) -> AgentRunHandle {
        let (event_tx, event_rx) = mpsc::channel();
        let (approval_tx, approval_rx) = mpsc::channel();
        let (auth_tx, auth_rx) = mpsc::channel();
        let (answer_confirmation_tx, answer_confirmation_rx) = mpsc::channel();
        let pending_session = Arc::new(Mutex::new(None));
        let cancellation_artifacts = ProviderCancellationArtifactStore::default();
        let control_capabilities = Arc::new(Mutex::new(
            super::super::control_protocol::ControlProtocolCapabilities::default(),
        ));
        let cancelled = Arc::new(AtomicBool::new(false));
        let run_done = Arc::new(AtomicBool::new(false));

        let mut admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let previous = match *admission {
            AdmissionState::Idle => {
                *admission = AdmissionState::Run { queued: false };
                Some(AdmissionState::Idle)
            }
            AdmissionState::Run { queued: false }
                if self
                    .cancel_pending
                    .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok() =>
            {
                *admission = AdmissionState::Run { queued: true };
                Some(AdmissionState::Run { queued: false })
            }
            _ => None,
        };
        if previous.is_none() {
            let _ = mark_recovery_failure(
                &session_state,
                &resume_attempt,
                "cosh-core runtime is already processing a request",
            );
            let _ = event_tx.send(Err(AdapterError {
                message: "cosh-core runtime is already processing a request".to_string(),
            }));
            return AgentRunHandle {
                receiver: event_rx,
                cancel: Arc::new(|| {}),
                approval_sender: Some(approval_tx),
                question_answer_confirmation: None,
                auth_sender: Some(auth_tx),
                control_capabilities,
                pending_provider_session: Some(pending_session),
                cancellation_artifacts,
            };
        }

        let cancel_flag = Arc::clone(&cancelled);
        let cancel_done = Arc::clone(&run_done);
        let cancel_stdin = Arc::clone(&self.active_stdin);
        let cancel_process = Arc::clone(&self.current_process);
        let cancel_pending = Arc::clone(&self.cancel_pending);
        let cancel = Arc::new(move || {
            cancel_pending.store(true, Ordering::SeqCst);
            cancel_flag.store(true, Ordering::SeqCst);
            if let Some(stdin) = cancel_stdin.lock().ok().and_then(|current| current.clone()) {
                if let Ok(mut writer) = stdin.lock() {
                    let message = control_request("interrupt", "interrupt-1", Value::Null);
                    let _ = writeln!(writer, "{message}");
                    let _ = writer.flush();
                }
            }
            let done = Arc::clone(&cancel_done);
            let process = Arc::clone(&cancel_process);
            thread::spawn(move || {
                thread::sleep(CANCEL_GRACE);
                if !done.load(Ordering::SeqCst) {
                    // Snapshot the live process at termination time, not at
                    // cancel request time, so a process that starts during the
                    // grace window is terminated with its own flag.
                    terminate_current_process(&process);
                }
            });
        });

        let command = RunCommand {
            run_id,
            prepared,
            raw_user_input,
            mode,
            session_state,
            session_scope,
            resume_attempt,
            event_tx,
            internal_response_tx: approval_tx.clone(),
            approval_rx: Some(approval_rx),
            auth_rx: Some(auth_rx),
            answer_confirmation_tx,
            pending_session: Arc::clone(&pending_session),
            cancellation_artifacts: cancellation_artifacts.clone(),
            control_capabilities: Arc::clone(&control_capabilities),
            cancelled,
            run_done,
        };
        let service_error_tx = command.event_tx.clone();
        let recovery_state = Arc::clone(&command.session_state);
        let recovery_attempt = command.resume_attempt.clone();
        match self.sender().and_then(|sender| {
            sender
                .send(ServiceCommand::Run(command))
                .map_err(|_| AdapterError {
                    message: "cosh-core runtime service stopped".to_string(),
                })
        }) {
            Ok(()) => {}
            Err(error) => {
                if let Some(previous) = previous {
                    *admission = previous;
                }
                let _ = mark_recovery_failure(&recovery_state, &recovery_attempt, &error.message);
                let _ = service_error_tx.send(Err(error));
            }
        }

        AgentRunHandle {
            receiver: event_rx,
            cancel,
            approval_sender: Some(approval_tx),
            question_answer_confirmation: Some(answer_confirmation_rx),
            auth_sender: Some(auth_tx),
            control_capabilities,
            pending_provider_session: Some(pending_session),
            cancellation_artifacts,
        }
    }

    pub(crate) fn registry_activation_query(
        &self,
        params: Value,
        short: impl FnOnce() -> Result<Value, RegistryQueryError>,
    ) -> Result<Value, RegistryQueryError> {
        let mut admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if *admission != AdmissionState::Idle {
            return Err(RegistryQueryError::Response {
                message: "cosh-core runtime is busy; retry provider activation when idle".into(),
                code: Some("runtime_busy".into()),
            });
        }
        *admission = AdmissionState::Registry;
        if self.live.load(Ordering::SeqCst) {
            let receiver = match self.enqueue_registry("auth", "activate", params) {
                Ok(receiver) => receiver,
                Err(error) => {
                    *admission = AdmissionState::Idle;
                    return Err(error);
                }
            };
            drop(admission);
            return Self::receive_registry(receiver, registry_timeout("auth", "activate"));
        }
        drop(admission);
        let _reservation = ShortRegistryReservation(Arc::clone(&self.admission));
        short()
    }

    pub(crate) fn live_registry_query(
        &self,
        domain: &str,
        action: &str,
        params: Value,
    ) -> Option<Result<Value, RegistryQueryError>> {
        let mut admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !self.live.load(Ordering::SeqCst) || *admission != AdmissionState::Idle {
            return None;
        }
        *admission = AdmissionState::Registry;
        let receiver = match self.enqueue_registry(domain, action, params) {
            Ok(receiver) => receiver,
            Err(error) => {
                *admission = AdmissionState::Idle;
                return Some(Err(error));
            }
        };
        drop(admission);
        Some(Self::receive_registry(
            receiver,
            registry_timeout(domain, action),
        ))
    }

    fn enqueue_registry(
        &self,
        domain: &str,
        action: &str,
        params: Value,
    ) -> Result<mpsc::Receiver<Result<Value, RegistryQueryError>>, RegistryQueryError> {
        let request_id = format!(
            "live-reg-{}-{}",
            std::process::id(),
            self.request_counter.fetch_add(1, Ordering::SeqCst)
        );
        let (response_tx, response_rx) = mpsc::channel();
        let command = RegistryCommand {
            request_id,
            domain: domain.to_string(),
            action: action.to_string(),
            params,
            response_tx,
        };
        self.sender()
            .map_err(|error| RegistryQueryError::Transport(error.message))?
            .send(ServiceCommand::Registry(command))
            .map_err(|_| {
                RegistryQueryError::Transport("cosh-core runtime service stopped".into())
            })?;
        Ok(response_rx)
    }

    fn receive_registry(
        receiver: mpsc::Receiver<Result<Value, RegistryQueryError>>,
        timeout: Duration,
    ) -> Result<Value, RegistryQueryError> {
        receiver.recv_timeout(timeout).unwrap_or_else(|_| {
            Err(RegistryQueryError::Transport(
                "live registry query timed out".into(),
            ))
        })
    }

    pub(crate) fn note_external_mutation(&self, domain: &str, action: &str) {
        if self.live.load(Ordering::SeqCst) && extension_mutation_requires_reload(domain, action) {
            self.reload_pending.store(true, Ordering::SeqCst);
        }
    }

    fn sender(&self) -> Result<mpsc::Sender<ServiceCommand>, AdapterError> {
        let mut current = self.command_tx.lock().map_err(|_| AdapterError {
            message: "cosh-core runtime lock poisoned".to_string(),
        })?;
        if let Some(sender) = current.as_ref() {
            return Ok(sender.clone());
        }
        let (sender, receiver) = mpsc::channel();
        let live = Arc::clone(&self.live);
        let admission = Arc::clone(&self.admission);
        let cancel_pending = Arc::clone(&self.cancel_pending);
        let reload_pending = Arc::clone(&self.reload_pending);
        let current_process = Arc::clone(&self.current_process);
        let active_stdin = Arc::clone(&self.active_stdin);
        thread::spawn(move || {
            super::service_loop(
                receiver,
                live,
                admission,
                cancel_pending,
                reload_pending,
                current_process,
                active_stdin,
            );
        });
        *current = Some(sender.clone());
        Ok(sender)
    }
}

#[cfg(test)]
mod provider_activation_tests {
    use super::*;
    use crate::adapter::CoshCoreAdapter;

    #[test]
    fn auth_activate_before_runtime_keeps_short_registry_path() {
        let adapter = CoshCoreAdapter::new("/nonexistent-core-activation", false);
        let error = adapter
            .registry_query_classified("auth", "activate", Value::Null)
            .unwrap_err()
            .into_message();
        assert!(error.contains("spawn"), "{error}");
    }

    #[test]
    fn auth_activate_never_uses_short_process_while_runtime_is_busy_or_starting() {
        for live in [false, true] {
            let adapter = CoshCoreAdapter::new("/nonexistent-core-activation", false);
            adapter.runtime.live.store(live, Ordering::SeqCst);
            *adapter.runtime.admission.lock().unwrap() = AdmissionState::Run { queued: false };
            let error = adapter
                .registry_query_classified("auth", "activate", Value::Null)
                .unwrap_err()
                .into_message();
            assert!(error.contains("busy"), "live={live}: {error}");
            assert!(!error.contains("spawn"));
        }
    }
    fn run(runtime: &PersistentCoshCoreRuntime) -> AgentRunHandle {
        runtime.start_run(
            "admission-test".into(),
            PreparedInvocation {
                program: "/nonexistent-admission-core".into(),
                args: Vec::new(),
                prompt: "test".into(),
            },
            None,
            CoshApprovalMode::Auto,
            Arc::new(Mutex::new(SessionRuntimeState::default())),
            "/tmp".into(),
            SessionResumeAttempt::Fresh { generation: 0 },
        )
    }

    #[test]
    fn failed_run_enqueue_restores_only_its_reservation() {
        for previous in [AdmissionState::Idle, AdmissionState::Run { queued: false }] {
            let runtime = PersistentCoshCoreRuntime::default();
            let (sender, receiver) = mpsc::channel();
            drop(receiver);
            *runtime.command_tx.lock().unwrap() = Some(sender);
            *runtime.admission.lock().unwrap() = previous;
            runtime
                .cancel_pending
                .store(previous != AdmissionState::Idle, Ordering::SeqCst);
            let handle = run(&runtime);
            let error = handle
                .poll_event_timeout(Duration::from_secs(1))
                .err()
                .expect("closed sender must reject the run");
            assert!(error.message.contains("service stopped"), "{error:?}");
            assert_eq!(*runtime.admission.lock().unwrap(), previous);
        }
    }

    #[test]
    fn cancelled_successor_retains_run_admission_until_it_finishes() {
        let runtime = PersistentCoshCoreRuntime::default();
        let (sender, receiver) = mpsc::channel();
        *runtime.command_tx.lock().unwrap() = Some(sender);
        *runtime.admission.lock().unwrap() = AdmissionState::Run { queued: false };
        runtime.cancel_pending.store(true, Ordering::SeqCst);
        let _successor = run(&runtime);
        let _queued_command = receiver.try_recv().expect("successor must be enqueued");
        assert_eq!(
            *runtime.admission.lock().unwrap(),
            AdmissionState::Run { queued: true }
        );
        let rejected = run(&runtime);
        assert!(rejected.poll_event_timeout(Duration::from_secs(1)).is_err());
        runtime.admission.lock().unwrap().finish_run();
        assert_eq!(
            *runtime.admission.lock().unwrap(),
            AdmissionState::Run { queued: false }
        );
        assert!(runtime
            .registry_activation_query(Value::Null, || panic!(
                "registry must not steal queued run admission"
            ))
            .is_err());
        runtime.admission.lock().unwrap().finish_run();
        assert_eq!(*runtime.admission.lock().unwrap(), AdmissionState::Idle);
    }

    #[test]
    fn short_activation_releases_admission_after_error_and_panic() {
        let runtime = PersistentCoshCoreRuntime::default();
        let result = runtime.registry_activation_query(Value::Null, || {
            assert_eq!(
                *runtime.admission.try_lock().unwrap(),
                AdmissionState::Registry
            );
            Err(RegistryQueryError::Transport("test failure".into()))
        });
        assert!(result.is_err());
        assert_eq!(*runtime.admission.lock().unwrap(), AdmissionState::Idle);
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.registry_activation_query(Value::Null, || panic!("short process panicked"))
        }));
        assert!(panic.is_err());
        assert_eq!(*runtime.admission.lock().unwrap(), AdmissionState::Idle);
    }

    #[test]
    fn failed_live_registry_enqueue_restores_admission_without_short_fallback() {
        let runtime = PersistentCoshCoreRuntime::default();
        runtime.live.store(true, Ordering::SeqCst);
        let (sender, receiver) = mpsc::channel();
        drop(receiver);
        *runtime.command_tx.lock().unwrap() = Some(sender);
        assert!(runtime
            .registry_activation_query(Value::Null, || panic!(
                "live failure must not activate another process"
            ))
            .is_err());
        assert_eq!(*runtime.admission.lock().unwrap(), AdmissionState::Idle);
    }

    #[test]
    fn caller_timeout_does_not_release_service_registry_admission() {
        let runtime = PersistentCoshCoreRuntime::default();
        runtime.live.store(true, Ordering::SeqCst);
        let (sender, receiver) = mpsc::channel();
        *runtime.command_tx.lock().unwrap() = Some(sender);
        let result = runtime
            .live_registry_query("auth", "state", Value::Null)
            .unwrap();
        assert!(result.unwrap_err().into_message().contains("timed out"));
        assert!(matches!(
            receiver.try_recv(),
            Ok(ServiceCommand::Registry(_))
        ));
        assert_eq!(*runtime.admission.lock().unwrap(), AdmissionState::Registry);
        let rejected = run(&runtime);
        assert!(rejected.poll_event_timeout(Duration::from_secs(1)).is_err());
    }
}
