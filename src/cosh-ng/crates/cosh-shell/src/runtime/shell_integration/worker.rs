use super::{CompatibilityProof, Phase, Request, Response, Status};
use crate::process::{read_worker_request, write_worker_reply, BoundedWorker};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const LIMIT: usize = 4096;

pub(super) fn evaluate(operation: &str, bytes: &[u8], deadline: Instant) -> Response {
    let unavailable = || Response::new(Status::Disable, "verification_unavailable");
    let mut request = operation.as_bytes().to_vec();
    request.push(0);
    request.extend_from_slice(bytes);
    if request.len() > LIMIT {
        return unavailable();
    }
    let Some(worker_deadline) = deadline.checked_sub(Duration::from_millis(3)) else {
        return unavailable();
    };
    if worker_deadline <= Instant::now() {
        return unavailable();
    }
    let Ok(exe) = std::env::current_exe() else {
        return unavailable();
    };
    let mut command = Command::new(exe);
    command.args(["shell-integration", "--__compat-worker-v2"]);
    let Ok(mut worker) = BoundedWorker::spawn(command, &request, worker_deadline, LIMIT) else {
        return unavailable();
    };
    loop {
        if let Some(result) = worker.poll_ready() {
            return result
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_else(unavailable);
        }
        if Instant::now() >= worker_deadline {
            worker.cancel();
            return unavailable();
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

pub(super) fn run() -> i32 {
    let Ok(bytes) = read_worker_request(LIMIT, Duration::from_millis(100)) else {
        return 1;
    };
    let Some(separator) = bytes.iter().position(|byte| *byte == 0) else {
        return 1;
    };
    let Ok(operation) = std::str::from_utf8(&bytes[..separator]) else {
        return 1;
    };
    let Ok(request) = super::decode(operation, &bytes[separator + 1..]) else {
        return 1;
    };
    let response = execute(request);
    let Ok(bytes) = serde_json::to_vec(&response) else {
        return 1;
    };
    i32::from(write_worker_reply(&bytes, LIMIT).is_err())
}

fn execute(request: Request) -> Response {
    let proof: CompatibilityProof = request.proof().clone();
    let Some(now_ms) = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
    else {
        return Response::new(Status::Disable, "clock_unavailable");
    };
    let Ok(snapshot) = super::compat::verify() else {
        return Response::new(Status::Disable, "incompatible_runtime");
    };
    let compat = matches!(request, Request::Compat { .. });
    if proof.phase == Phase::Activation && snapshot != proof.registration_snapshot {
        return Response::new(Status::Disable, "module_changed");
    }
    let Ok((store, runtime)) = crate::config::guide::open_user_state() else {
        return Response::new(Status::Disable, "state_unavailable");
    };
    if proof.phase == Phase::Registration && store.initialize_runtime(&runtime).is_err() {
        return Response::new(Status::Disable, "state_unavailable");
    }
    let mut response = super::evaluate(
        request,
        now_ms,
        Some((&store, &runtime)),
        crate::ui::guidance::render_failure,
    );
    if response.decision == Status::Show {
        let Ok(final_snapshot) = super::compat::verify() else {
            return Response::new(Status::Disable, "incompatible_runtime");
        };
        if final_snapshot != snapshot {
            return Response::new(Status::Disable, "module_changed");
        }
        if compat {
            response.decision_id = final_snapshot;
        }
    }
    if response.decision == Status::Show && response.reason == "suggest_only" {
        let styled = std::env::var_os("NO_COLOR").is_none()
            && std::env::var("TERM")
                .is_ok_and(|term| term.starts_with("xterm") || term.starts_with("screen"))
            && !std::env::var("COSH_SHELL_RENDER").is_ok_and(|mode| {
                mode.eq_ignore_ascii_case("plain") || mode.eq_ignore_ascii_case("text")
            });
        // Add only trusted SGR after the plain-data validation and claim checks.
        response.text = crate::ui::guidance::style_invitation(&response.text, styled);
    }
    response
}
