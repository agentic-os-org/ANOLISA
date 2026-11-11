mod compat;
mod io;
mod worker;
use io::read_frame;
pub(super) use io::write_frame;
use std::time::{Duration, Instant};

use crate::config::guide::{ClaimDecision, ClaimRequest, GuideStore, InviteKind, RuntimeState};
use crate::insight::guidance::{
    self, BashEvent, CommandIdentity, Decision, ExecutionScope, FailureKind, SkipReason,
};

const MAX_BYTES: usize = 8192;
const VERSION: &str = "2";
#[derive(Debug)]
enum Request {
    Compat {
        proof: CompatibilityProof,
        budget_ms: u64,
    },
    Advise {
        proof: CompatibilityProof,
        budget_ms: u64,
        event: BashEvent,
        host: String,
        session: String,
        generation: String,
    },
}

impl Request {
    fn proof(&self) -> &CompatibilityProof {
        match self {
            Self::Compat { proof, .. } | Self::Advise { proof, .. } => proof,
        }
    }

    fn budget_ms(&self) -> u64 {
        match self {
            Self::Compat { budget_ms, .. } | Self::Advise { budget_ms, .. } => *budget_ms,
        }
    }
}

#[derive(Debug, Clone)]
struct CompatibilityProof {
    phase: Phase,
    registration_snapshot: String,
    deadline_boottime_us: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Registration,
    Activation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Status {
    Show,
    Skip,
    Disable,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Response {
    decision: Status,
    reason: String,
    generation: String,
    decision_id: String,
    runtime_directory: String,
    text: String,
    cache_dependencies: Vec<String>,
}

impl Response {
    fn new(decision: Status, reason: &'static str) -> Self {
        Self {
            decision,
            reason: reason.to_string(),
            generation: String::new(),
            decision_id: String::new(),
            runtime_directory: String::new(),
            text: String::new(),
            cache_dependencies: Vec::new(),
        }
    }
}

fn decode(operation: &str, bytes: &[u8]) -> Result<Request, ()> {
    if bytes.len() > MAX_BYTES {
        return Err(());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ())?;
    let text = text.strip_suffix('\0').ok_or(())?;
    let fields: Vec<_> = text.split('\0').collect();
    if !matches!((operation, fields.len()), ("compat", 6) | ("advise", 15))
        || fields[0] != VERSION
        || fields[1] != "external-bash"
    {
        return Err(());
    }
    let phase = match fields[2] {
        "registration" => Phase::Registration,
        "activation" => Phase::Activation,
        _ => return Err(()),
    };
    let proof = CompatibilityProof {
        phase,
        registration_snapshot: fields[3].to_string(),
        deadline_boottime_us: fields[4].parse().map_err(|_| ())?,
    };
    if proof.deadline_boottime_us == 0
        || match phase {
            Phase::Registration => !proof.registration_snapshot.is_empty(),
            Phase::Activation => {
                proof.registration_snapshot.len() != 64
                    || !proof
                        .registration_snapshot
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
            }
        }
    {
        return Err(());
    }
    let budget_ms: u64 = fields[5].parse().map_err(|_| ())?;
    if budget_ms == 0 || budget_ms > if operation == "compat" { 100 } else { 50 } {
        return Err(());
    }
    if operation == "compat" {
        return Ok(Request::Compat { proof, budget_ms });
    }
    let scope = match fields[10] {
        "observed_command_fragment" => ExecutionScope::ObservedCommandFragment,
        "unknown" => ExecutionScope::Unknown,
        _ => return Err(()),
    };
    let identity = match fields[11] {
        "external_unshadowed" => CommandIdentity::ExternalUnshadowed,
        "alias" => CommandIdentity::Alias,
        "function" => CommandIdentity::Function,
        "unknown" => CommandIdentity::Unknown,
        _ => return Err(()),
    };
    let status = fields[7].parse::<u8>().map_err(|_| ())? as u16;
    let event = BashEvent {
        expression: fields[6].to_string(),
        status,
        cwd: fields[8].to_string(),
        observed_at_ms: fields[9].parse().map_err(|_| ())?,
        scope,
        identity,
    };
    Ok(Request::Advise {
        proof,
        budget_ms,
        event,
        host: fields[12].to_string(),
        session: fields[13].to_string(),
        generation: fields[14].to_string(),
    })
}

fn evaluate(
    request: Request,
    now_ms: u64,
    guide: Option<(&GuideStore, &RuntimeState)>,
    render: impl FnOnce(&BashEvent, FailureKind, &str) -> Option<String>,
) -> Response {
    let Some((store, runtime)) = guide else {
        return Response::new(Status::Disable, "state_unavailable");
    };
    let snapshot = match store.snapshot(Some(runtime)) {
        Ok(snapshot) => snapshot,
        Err(_) => return Response::new(Status::Disable, "state_unavailable"),
    };
    if !snapshot.preferences.guide_enabled
        || !matches!(runtime.read(), Ok(Some(state)) if state.enabled)
    {
        return Response::new(Status::Disable, "guidance_off");
    }
    let Some(runtime_directory) = runtime
        .directory()
        .to_str()
        .filter(|value| !value.chars().any(char::is_control))
    else {
        return Response::new(Status::Disable, "state_unavailable");
    };
    let Request::Advise {
        proof,
        event,
        host,
        session,
        generation,
        ..
    } = request
    else {
        let mut response = Response::new(Status::Show, "compatible");
        response.generation = snapshot.generation;
        response.runtime_directory = runtime_directory.to_string();
        return response;
    };
    if proof.phase != Phase::Activation
        || event.observed_at_ms > now_ms
        || now_ms - event.observed_at_ms > 30_000
    {
        return Response::new(Status::Skip, "invalid_event_time_or_phase");
    }
    if snapshot.generation != generation {
        return Response::new(Status::Disable, "stale_generation");
    }
    let kind = match guidance::decide(&event, &snapshot) {
        Decision::SuggestOnly(kind) => kind,
        Decision::Skip(reason) => {
            let (status, reason, dependencies) = match reason {
                SkipReason::Disabled => (Status::Disable, "guidance_off", vec![]),
                SkipReason::UnsupportedCommand => (
                    Status::Skip,
                    "unsupported_command",
                    vec!["expression", "status"],
                ),
                SkipReason::UnprovenScope => (Status::Skip, "unproven_scope", vec!["scope"]),
                SkipReason::UnprovenIdentity => {
                    (Status::Skip, "unproven_identity", vec!["identity"])
                }
                SkipReason::InvalidEvidence => (Status::Skip, "invalid_evidence", vec![]),
            };
            let mut response = Response::new(status, reason);
            response.cache_dependencies = dependencies.into_iter().map(str::to_owned).collect();
            return response;
        }
    };
    let context_id = uuid::Uuid::new_v4().simple().to_string();
    let Some(text) = render(&event, kind, &context_id).filter(|text| {
        !text.is_empty()
            && text.len() <= 4096
            && !text
                .chars()
                .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
    }) else {
        return Response::new(Status::Disable, "presentation_unavailable");
    };
    use sha2::{Digest, Sha256};
    let problem = format!(
        "{:x}",
        Sha256::digest(format!("{}\0{}", event.expression, event.cwd).as_bytes())
    );
    let claim = match store.claim_failure_context(
        runtime,
        &snapshot,
        ClaimRequest {
            kind: InviteKind::Failure,
            host: &host,
            problem: &problem,
            session: &session,
        },
        &context_id,
        &event,
        now_ms,
    ) {
        Ok(ClaimDecision::Claimed(claim)) => claim,
        Ok(ClaimDecision::Disabled) => return Response::new(Status::Disable, "guidance_off"),
        Ok(ClaimDecision::Suppressed) => return Response::new(Status::Skip, "suppressed"),
        Ok(ClaimDecision::Stale) => return Response::new(Status::Disable, "stale_generation"),
        Err(_) => return Response::new(Status::Disable, "state_unavailable"),
    };
    if store.validate_claim(Some(runtime), &claim) != Ok(true) {
        return Response::new(Status::Disable, "stale_generation");
    }
    let mut response = Response::new(Status::Show, "suggest_only");
    response.generation = claim.generation;
    response.decision_id = context_id;
    response.runtime_directory = runtime_directory.to_string();
    response.text = text;
    response
}

fn encode(response: &Response) -> Vec<u8> {
    let status = match response.decision {
        Status::Show => "SHOW",
        Status::Skip => "SKIP",
        Status::Disable => "DISABLE",
    };
    let dependencies = response.cache_dependencies.join(",");
    let mut bytes = [
        VERSION,
        status,
        &response.reason,
        &response.generation,
        &response.decision_id,
        &dependencies,
        &response.runtime_directory,
        &response.text,
    ]
    .join("\0")
    .into_bytes();
    bytes.push(0);
    if bytes.len() > MAX_BYTES {
        return encode(&Response::new(Status::Disable, "response_too_large"));
    }
    bytes
}

pub(crate) fn run_cli(operation: &str) -> i32 {
    if operation == "--__compat-worker-v2" {
        return worker::run();
    }
    let started = Instant::now();
    let maximum_ms = match operation {
        "compat" => 100,
        "advise" => 50,
        _ => return 2,
    };
    let bootstrap = started + Duration::from_millis(maximum_ms);
    let mut deadline = bootstrap;
    let response = match read_frame(nix::libc::STDIN_FILENO, bootstrap) {
        Ok(bytes) => match decode(operation, &bytes) {
            Ok(request) => {
                let budget = Duration::from_millis(request.budget_ms());
                let before = Instant::now();
                match crate::diagnostics::health::runtime::bash_budget_remaining(
                    request.proof().deadline_boottime_us,
                    budget,
                ) {
                    Some(remaining) => {
                        deadline = (started + budget).min(before + remaining);
                        worker::evaluate(operation, &bytes, deadline)
                    }
                    None => Response::new(Status::Disable, "deadline_exceeded"),
                }
            }
            Err(()) => Response::new(Status::Disable, "invalid_request"),
        },
        Err(_) => Response::new(Status::Disable, "invalid_frame"),
    };
    i32::from(write_frame(nix::libc::STDOUT_FILENO, &encode(&response), deadline).is_err())
}

#[cfg(test)]
#[path = "shell_integration/tests.rs"]
mod tests;
