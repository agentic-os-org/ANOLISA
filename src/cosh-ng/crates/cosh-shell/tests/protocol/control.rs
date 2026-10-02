pub(crate) use std::sync::{Arc, Mutex};
pub(crate) use std::thread;
pub(crate) use std::time::{Duration, Instant};

pub(crate) use cosh_shell::adapter::{
    AdapterInstance, AgentAdapter, ApprovalDecision, ApprovalResponse, FakeAgentAdapter,
    HostExecutedShellMetadata, HostExecutedShellResult,
};
pub(crate) use cosh_shell::types::{AgentEvent, CoshApprovalMode};

#[path = "../support/control_protocol.rs"]
mod support_control_protocol;

pub(crate) use support_control_protocol::{
    collect_events_until, make_adapter, make_cosh_core_adapter, make_qwen_adapter, make_request,
};

fn wait_for_session_id(
    session_state: &Arc<Mutex<Option<String>>>,
    expected: &str,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if session_state.lock().unwrap().as_deref() == Some(expected) {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

fn wait_for_cosh_core_session(
    session_state: &Arc<Mutex<cosh_shell::adapter::SessionRuntimeState>>,
    expected: &str,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if session_state.lock().unwrap().active_session_id() == Some(expected) {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

#[path = "control/approval_round_trip.rs"]
mod approval_round_trip;
#[path = "control/host_executed.rs"]
mod host_executed;
#[path = "control/initialize_wire.rs"]
mod initialize_wire;
#[path = "control/provider_modes.rs"]
mod provider_modes;
