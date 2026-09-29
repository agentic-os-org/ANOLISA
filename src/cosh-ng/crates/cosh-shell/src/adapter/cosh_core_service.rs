//! Long-lived cosh-core JSONL process shared by Agent turns and registry requests.

mod command;
mod control;
mod process;
mod question;
mod registry;
mod run;
mod runtime;

use std::io::BufWriter;
use std::process::ChildStdin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::types::AgentEvent;

use super::claude::send_agent_event;
use super::cosh_core::mark_recovery_failure;
use super::AdapterError;
use command::{RegistryCommand, RunCommand, ServiceCommand};
use process::{
    control_request, reset_process, send_json, stop_process, PersistentProcess, ProcessBindings,
};
use registry::{execute_registry, log_registry_transport_failure};
use run::run_turn;

pub(crate) use runtime::PersistentCoshCoreRuntime;

const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

fn service_loop(
    receiver: mpsc::Receiver<ServiceCommand>,
    live: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
    cancel_pending: Arc<AtomicBool>,
    reload_pending: Arc<AtomicBool>,
    current_process: Arc<Mutex<ProcessBindings>>,
    active_stdin: Arc<Mutex<Option<Arc<Mutex<BufWriter<ChildStdin>>>>>>,
) {
    let mut process = None;
    while let Ok(command) = receiver.recv() {
        match command {
            ServiceCommand::Run(mut command) => {
                busy.store(true, Ordering::SeqCst);
                let result = run_turn(
                    &mut process,
                    &mut command,
                    &live,
                    &reload_pending,
                    &current_process,
                    &active_stdin,
                );
                match result {
                    Ok(reset_required) => {
                        if reset_required {
                            reset_process(&mut process, &live, &active_stdin, &current_process);
                        }
                    }
                    Err(error) => {
                        // Choke-point dual-write: every turn-level failure (spawn,
                        // protocol, stream errors) converges here; one warn covers
                        // the whole run_turn surface for post-mortem logs.
                        tracing::warn!(error = %error, "cosh-core turn failed; resetting process");
                        let _ = mark_recovery_failure(
                            &command.session_state,
                            &command.resume_attempt,
                            &error,
                        );
                        let _ = command.event_tx.send(Err(AdapterError { message: error }));
                        reset_process(&mut process, &live, &active_stdin, &current_process);
                    }
                }
                command.run_done.store(true, Ordering::SeqCst);
                cancel_pending.store(false, Ordering::SeqCst);
                busy.store(false, Ordering::SeqCst);
            }
            ServiceCommand::Registry(command) => {
                let result = match process.as_mut() {
                    Some(process) => execute_registry(process, &command),
                    None => Err(super::cosh_core_registry::RegistryQueryError::Transport(
                        "live cosh-core process is unavailable".to_string(),
                    )),
                };
                if log_registry_transport_failure(&result, &command) {
                    reset_process(&mut process, &live, &active_stdin, &current_process);
                }
                let _ = command.response_tx.send(result);
                busy.store(false, Ordering::SeqCst);
            }
            ServiceCommand::Shutdown => {
                if let Some(process) = process.as_mut() {
                    process.expected_stop.store(true, Ordering::SeqCst);
                    let _ = send_json(
                        &process.stdin,
                        &control_request("shutdown", "shutdown-1", Value::Null),
                    );
                    stop_process(&mut process.child, SHUTDOWN_GRACE);
                }
                if let Ok(mut current) = current_process.lock() {
                    *current = ProcessBindings::default();
                }
                break;
            }
        }
    }
    live.store(false, Ordering::SeqCst);
}
