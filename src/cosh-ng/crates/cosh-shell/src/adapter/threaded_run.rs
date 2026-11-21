//! Threaded adapter run driver shared by the process and ACP entries.
//!
//! Spawns the run on a worker thread, meters the ACP UI queue admission and
//! delivers one cancellation fact per turn. Kept beside the adapter hub so the
//! run handle's private fields stay reachable without widening visibility.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use crate::types::{AgentEvent, AgentRequest};

use super::{
    is_ui_data_event, AdapterError, AdapterInstance, AgentAdapter, AgentRunHandle,
    ControlProtocolCapabilities, ProviderCancellationArtifactStore,
};

pub(super) fn start_threaded_adapter_run(
    adapter: AdapterInstance,
    request: AgentRequest,
) -> AgentRunHandle {
    start_threaded_adapter_run_with_flag(adapter, request, Arc::new(AtomicBool::new(false)), None)
}

pub(super) fn start_threaded_adapter_run_with_flag(
    adapter: AdapterInstance,
    request: AgentRequest,
    cancelled: Arc<AtomicBool>,
    ui_cap: Option<usize>,
) -> AgentRunHandle {
    let (sender, receiver) = mpsc::channel();
    let ui_inflight = ui_cap.map(|_| Arc::new(AtomicUsize::new(0)));
    let sink_gauge = ui_inflight.clone();
    let cancel_flag = Arc::clone(&cancelled);
    let cancel = Arc::new(move || {
        cancel_flag.store(true, Ordering::SeqCst);
    });
    // The ACP peer asks the Shell to own foreground actions, so it needs the
    // same approval channel the process providers use; without it the runtime
    // could never deliver a terminal fact back to the extension.
    // The host-shell extension carries the same terminal fact the control
    // protocol calls a host-executed shell result, so an ACP run owns that
    // capability as soon as its foreground authority is wired. Without it the
    // runtime stops the run at approval time and the authority waits forever.
    let (adapter, approval_sender, control_capabilities) = match adapter {
        AdapterInstance::Acp(acp) => {
            let (approval_tx, approval_rx) = mpsc::channel();
            (
                AdapterInstance::Acp(acp.with_host_channels(sender.clone(), approval_rx)),
                Some(approval_tx),
                ControlProtocolCapabilities {
                    can_handle_host_executed_shell_tool_result: true,
                    ..ControlProtocolCapabilities::default()
                },
            )
        }
        other => (other, None, ControlProtocolCapabilities::default()),
    };

    thread::spawn(move || {
        let mut sent_cancelled = false;
        let result = adapter.run_stream(&request, &mut |event| {
            if cancelled.load(Ordering::SeqCst) {
                // One turn owes one cancellation fact. The ACP session keeps
                // forwarding frames it already read while cancelled, so sending
                // per event would turn every in-flight frame into a terminal.
                if !sent_cancelled {
                    sent_cancelled = true;
                    let _ = sender.send(Ok(AgentEvent::AgentCancelled {
                        run_id: request.id.clone(),
                        reason: "user requested cancellation".to_string(),
                    }));
                }
                return Err(AdapterError {
                    message: "agent run cancelled".to_string(),
                });
            }
            if let (Some(gauge), Some(cap)) = (&sink_gauge, ui_cap) {
                if is_ui_data_event(&event) {
                    if gauge.load(Ordering::SeqCst) >= cap {
                        return Err(AdapterError {
                            message: format!(
                                "UI event queue exceeded the {cap}-event bound; closing the run"
                            ),
                        });
                    }
                    gauge.fetch_add(1, Ordering::SeqCst);
                }
            }
            sender.send(Ok(event)).map_err(|_| AdapterError {
                message: "agent event receiver dropped".to_string(),
            })
        });
        if let Err(err) = result {
            if !sent_cancelled {
                let _ = sender.send(Err(err));
            }
        }
    });

    AgentRunHandle {
        receiver,
        cancel,
        approval_sender,
        question_answer_confirmation: None,
        auth_sender: None,
        control_capabilities: Arc::new(Mutex::new(control_capabilities)),
        pending_provider_session: None,
        cancellation_artifacts: ProviderCancellationArtifactStore::default(),
        ui_inflight,
    }
}
