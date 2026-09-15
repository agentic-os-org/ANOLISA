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

#[derive(Debug, Default)]
pub(crate) struct PersistentCoshCoreRuntime {
    pub(super) command_tx: Mutex<Option<mpsc::Sender<ServiceCommand>>>,
    pub(super) live: Arc<AtomicBool>,
    pub(super) busy: Arc<AtomicBool>,
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

        let acquired = self
            .busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        // A failed answer write cancels the owning turn and immediately starts
        // a fallback turn. Queue that one successor behind teardown; unrelated
        // concurrent starts still fail closed.
        let queued_after_cancel = !acquired
            && self
                .cancel_pending
                .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok();
        if !acquired && !queued_after_cancel {
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
                self.busy.store(false, Ordering::SeqCst);
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

    pub(crate) fn live_registry_query(
        &self,
        domain: &str,
        action: &str,
        params: Value,
    ) -> Option<Result<Value, RegistryQueryError>> {
        if !self.live.load(Ordering::SeqCst)
            || self
                .busy
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            return None;
        }
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
        let sent = self
            .sender()
            .map_err(|error| RegistryQueryError::Transport(error.message))
            .and_then(|sender| {
                sender.send(ServiceCommand::Registry(command)).map_err(|_| {
                    RegistryQueryError::Transport("cosh-core runtime service stopped".to_string())
                })
            });
        if let Err(error) = sent {
            self.busy.store(false, Ordering::SeqCst);
            return Some(Err(error));
        }
        let timeout = registry_timeout(domain, action);
        Some(response_rx.recv_timeout(timeout).unwrap_or_else(|_| {
            Err(RegistryQueryError::Transport(
                "live registry query timed out".to_string(),
            ))
        }))
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
        let busy = Arc::clone(&self.busy);
        let cancel_pending = Arc::clone(&self.cancel_pending);
        let reload_pending = Arc::clone(&self.reload_pending);
        let current_process = Arc::clone(&self.current_process);
        let active_stdin = Arc::clone(&self.active_stdin);
        thread::spawn(move || {
            super::service_loop(
                receiver,
                live,
                busy,
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
