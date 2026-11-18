//! Deterministic oracle for the uniqueness of the ACP cancellation terminal fact.
//!
//! The peer emits its late frames only after it has read `session/cancel`, so the
//! frames are provably in flight after the cancel flag is set. The consumer first
//! observes the early frames, which is the delivery barrier: no marker timing is
//! involved and the case does not depend on host load.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use cosh_shell::adapter::{AcpAgentAdapter, AdapterInstance, AgentRunHandle, AgentRunPoll};
use cosh_shell::types::{AgentEvent, CoshApprovalMode};

use super::acp_entry::{agent_script, make_request, nonce};

const FRAME: &str = concat!(
    r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"host-session","#,
    r#""update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"x"}}}}"#,
);
const EARLY: usize = 3;
const LATE: usize = 5;
const BOUND: Duration = Duration::from_secs(30);

/// Peer that sends `early` frames, waits for the cancel, then sends `late` more
/// frames before acknowledging the cancellation.
fn peer(name: &str, early: usize, late: usize) -> (std::path::PathBuf, std::path::PathBuf) {
    let pidfile = std::env::temp_dir().join(format!("cosh-acp-cancel-{name}-{}.pid", nonce()));
    let body = format!(
        r#"
trap '' PIPE
trap '' TERM
printf '%s\n' "$$" > '{pidfile}'
req_id() {{ printf '%s' "$1" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2; }}
IFS= read -r init
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"protocolVersion":1,"agentCapabilities":{{}}}}}}\n' "$(req_id "$init")"
IFS= read -r new_session
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"sessionId":"host-session"}}}}\n' "$(req_id "$new_session")"
IFS= read -r prompt
prompt_id=$(req_id "$prompt")
i=0
while [ "$i" -lt {early} ]; do
  printf '%s\n' '{frame}'
  i=$((i + 1))
done
for _ in $(seq 1 60); do
  if IFS= read -r -t 1 line; then
    case "$line" in
      *session/cancel*)
        j=0
        while [ "$j" -lt {late} ]; do
          printf '%s\n' '{frame}'
          j=$((j + 1))
        done
        printf '{{"jsonrpc":"2.0","id":%s,"result":{{"stopReason":"cancelled"}}}}\n' "$prompt_id"
        exit 0
        ;;
    esac
  fi
done
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"stopReason":"end_turn"}}}}\n' "$prompt_id"
"#,
        pidfile = pidfile.display(),
        early = early,
        late = late,
        frame = FRAME,
    );
    (agent_script(name, &body), pidfile)
}

fn start(name: &str, script: &Path) -> AgentRunHandle {
    let mut acp = AcpAgentAdapter::new(script.to_string_lossy().into_owned());
    acp.idle_timeout = Duration::from_secs(60);
    AdapterInstance::Acp(acp.with_model_call(true))
        .start_cancellable(make_request(name), CoshApprovalMode::Auto)
}

struct Outcome {
    text_events: usize,
    cancelled_events: usize,
    other_events: usize,
    terminal: String,
    last_is_cancelled: bool,
}

/// Consumes exactly `early` text events, which proves they were delivered, then
/// cancels and drains the rest of the run.
fn cancel_after_delivery(handle: &AgentRunHandle, early: usize) -> Outcome {
    let mut out = Outcome {
        text_events: 0,
        cancelled_events: 0,
        other_events: 0,
        terminal: "none".to_string(),
        last_is_cancelled: false,
    };
    let deadline = Instant::now() + BOUND;
    let mut cancelled = false;
    while Instant::now() < deadline {
        match handle.poll_event_timeout(Duration::from_millis(200)) {
            Ok(AgentRunPoll::Event(event)) => {
                eprintln!("CANCEL-EVENT {event:?}");
                out.last_is_cancelled = matches!(event, AgentEvent::AgentCancelled { .. });
                match event {
                    AgentEvent::TextDelta { .. } => out.text_events += 1,
                    AgentEvent::AgentCancelled { .. } => {
                        out.cancelled_events += 1;
                        out.terminal = "cancelled".to_string();
                    }
                    AgentEvent::AgentCompleted { .. } => out.terminal = "completed".to_string(),
                    AgentEvent::AgentFailed { error, .. } => {
                        out.terminal = format!("failed: {error}");
                    }
                    _ => out.other_events += 1,
                }
                if !cancelled && out.text_events >= early {
                    handle.cancel();
                    cancelled = true;
                }
            }
            Ok(AgentRunPoll::Timeout) => {
                if !cancelled && out.text_events >= early {
                    handle.cancel();
                    cancelled = true;
                }
            }
            Ok(AgentRunPoll::Finished) => break,
            Err(failure) => {
                out.terminal = format!("failed: {}", failure.message);
                break;
            }
        }
    }
    assert!(
        cancelled,
        "the early frames must be delivered before cancelling"
    );
    out
}

fn reclaimed(pidfile: &Path) -> bool {
    let Ok(text) = fs::read_to_string(pidfile) else {
        return false;
    };
    let pid = text.trim().to_string();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let alive = Command::new("kill")
            .args(["-0", &pid])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if !alive {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn run_case(name: &str, early: usize, late: usize) -> Outcome {
    let (script, pidfile) = peer(name, early, late);
    let handle = start(&format!("acp-cancel-{name}"), &script);
    let out = cancel_after_delivery(&handle, early);
    drop(handle);
    let gone = reclaimed(&pidfile);
    let _ = fs::remove_file(&script);
    let _ = fs::remove_file(&pidfile);
    assert!(gone, "the peer must be reclaimed within a bound");
    out
}

#[test]
fn a_cancel_with_frames_still_in_flight_yields_one_cancellation_fact() {
    let out = run_case("late-frames", EARLY, LATE);
    assert_eq!(out.terminal, "cancelled", "got {}", out.terminal);
    assert_eq!(
        out.text_events, EARLY,
        "only the frames delivered before the cancel reach the UI as text"
    );
    assert_eq!(
        out.cancelled_events, 1,
        "one cancellation fact for the turn, not one per in-flight frame"
    );
    assert!(
        out.last_is_cancelled,
        "the cancellation fact must end the run"
    );
}

#[test]
fn a_cancel_acknowledged_without_late_frames_yields_one_cancellation_fact() {
    let out = run_case("no-late-frames", EARLY, 0);
    assert_eq!(out.terminal, "cancelled", "got {}", out.terminal);
    assert_eq!(out.text_events, EARLY);
    assert_eq!(out.cancelled_events, 1, "got {}", out.cancelled_events);
    assert!(
        out.last_is_cancelled,
        "the cancellation fact must end the run"
    );
}
