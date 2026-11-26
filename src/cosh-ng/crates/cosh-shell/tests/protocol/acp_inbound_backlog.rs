//! Wire-level control for the inbound backlog bound on the Shell ACP client.
//!
//! Unlike the transport component tests, these drive the production entry, so
//! the ask reaches the runtime through the real approval channel. The peer is
//! still a scripted fixture: no real PTY, provider or model call is involved.
//! The bound claimed here is the transport queue only, not the UI queue or the
//! Core SDK.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use cosh_shell::adapter::{
    AcpAgentAdapter, AdapterInstance, AgentRunPoll, ApprovalDecision, ApprovalResponse,
};
use cosh_shell::types::{AgentEvent, CoshApprovalMode};

use super::acp_entry::{agent_script, make_request, nonce};

const EXECUTE: &str = "_cosh/shell/v1/execute";
const FLOOD_FRAMES: usize = 20_000;

/// One legal bounded `session/update` notification per line.
const UPDATE_FRAME: &str = concat!(
    r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"host-session","#,
    r#""update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"x"}}}}"#,
);

struct FloodPeer {
    script: PathBuf,
    gate: PathBuf,
    phase: PathBuf,
    marker: PathBuf,
    capture: PathBuf,
    pidfile: PathBuf,
}

impl FloodPeer {
    fn cleanup(&self) {
        for path in [
            &self.script,
            &self.gate,
            &self.phase,
            &self.marker,
            &self.capture,
            &self.pidfile,
        ] {
            let _ = fs::remove_file(path);
        }
    }
}

/// A peer that completes the handshake, hands one foreground action to the
/// Shell, waits until the runtime really raised the ask, then floods legal
/// single-frame notifications. It records whether every write succeeded, so a
/// reader that stops is observable without inspecting any private queue.
fn flood_peer(name: &str, stop_at: usize) -> FloodPeer {
    let stem = format!("cosh-acp-backlog-{name}-{}-{}", std::process::id(), nonce());
    let peer = FloodPeer {
        script: std::env::temp_dir().join(format!("{stem}.sh")),
        gate: std::env::temp_dir().join(format!("{stem}.gate")),
        phase: std::env::temp_dir().join(format!("{stem}.phase")),
        marker: std::env::temp_dir().join(format!("{stem}.done")),
        capture: std::env::temp_dir().join(format!("{stem}.jsonl")),
        pidfile: std::env::temp_dir().join(format!("{stem}.pid")),
    };
    let execute = format!(
        r#"{{"jsonrpc":"2.0","id":7,"method":"{EXECUTE}","params":{{"sessionId":"host-session","toolCallId":"call-backlog","command":"echo hi","cwd":"/tmp","ptyInstance":"session-1","hookRequiresApproval":false}}}}"#
    );
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
printf '%s\n' '{execute}'
IFS= read -r owned
for _ in $(seq 1 600); do
  [ -f '{gate}' ] && break
  sleep 0.1
done
flood_ok=1
for i in $(seq 1 {frames}); do
  if [ "$i" = "{stop_at}" ]; then
    printf '{{"jsonrpc":"2.0","id":%s,"result":{{"stopReason":"end_turn"}}}}\n' "$prompt_id"
  fi
  printf '%s\n' '{frame}' || {{ flood_ok=0; break; }}
done
[ "$flood_ok" = 1 ] && printf 'flooded\n' > '{marker}'
printf 'phase\n' > '{phase}'
for _ in $(seq 1 4); do
  IFS= read -r -t 10 line || break
  printf '%s\n' "$line" >> '{capture}'
done
printf '{{"jsonrpc":"2.0","id":%s,"result":{{"stopReason":"cancelled"}}}}\n' "$prompt_id"
"#,
        pidfile = peer.pidfile.display(),
        gate = peer.gate.display(),
        execute = execute,
        frames = FLOOD_FRAMES,
        stop_at = stop_at,
        frame = UPDATE_FRAME,
        marker = peer.marker.display(),
        phase = peer.phase.display(),
        capture = peer.capture.display(),
    );
    let script = agent_script(name, &body);
    fs::rename(&script, &peer.script).expect("place flood fixture");
    peer
}

fn wait_for_file(path: &Path, bound: Duration) {
    let deadline = Instant::now() + bound;
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("{} never appeared within {bound:?}", path.display());
}

/// Starts the production entry and opens the flood only after the real runtime
/// approval channel raised the ask, so the review is genuinely pending.
fn pending_review(name: &str, peer: &FloodPeer) -> cosh_shell::adapter::AgentRunHandle {
    let mut acp = AcpAgentAdapter::new(peer.script.to_string_lossy().into_owned());
    acp.idle_timeout = Duration::from_secs(20);
    let handle = AdapterInstance::Acp(acp.with_model_call(true))
        .start_cancellable(make_request(name), CoshApprovalMode::Auto);
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut saw_ask = false;
    while Instant::now() < deadline {
        match handle.next_event_timeout(Duration::from_millis(200)) {
            Ok(Some(AgentEvent::ToolPermissionRequest { request_id, .. })) => {
                assert_eq!(request_id, "call-backlog");
                saw_ask = true;
                break;
            }
            Ok(Some(_)) | Ok(None) => continue,
            Err(failure) => panic!("the run failed before the ask: {failure:?}"),
        }
    }
    assert!(saw_ask, "the foreground ask must reach the runtime");
    fs::write(&peer.gate, b"go").expect("open the flood gate");
    wait_for_file(&peer.phase, Duration::from_secs(90));
    handle
}

fn peer_alive(pidfile: &Path) -> bool {
    let Ok(text) = fs::read_to_string(pidfile) else {
        return false;
    };
    let pid = text.trim();
    !pid.is_empty()
        && Command::new("kill")
            .arg("-0")
            .arg(pid)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
}

fn wait_for_reclaim(pidfile: &Path, bound: Duration) -> bool {
    let deadline = Instant::now() + bound;
    while Instant::now() < deadline {
        if !peer_alive(pidfile) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn replies(capture: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(capture)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Releasing the review lets the client loop drain again, so the overload must
/// surface as an explicit run failure instead of a clean stop.
#[test]
fn a_flooded_peer_fails_the_run_when_the_pending_review_is_released() {
    let peer = flood_peer("release", 0);
    let handle = pending_review("acp-backlog-release", &peer);
    let completed = peer.marker.exists();
    eprintln!("BACKLOG-RELEASE peer_completed_flood={completed}");

    handle
        .respond_approval(ApprovalResponse {
            request_id: "call-backlog".to_string(),
            tool_use_id: Some("call-backlog".to_string()),
            tool_input: None,
            decision: ApprovalDecision::Deny {
                message: "review denied".to_string(),
            },
        })
        .expect("approval delivery");
    let started = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut terminal = String::from("none");
    while Instant::now() < deadline {
        match handle.poll_event_timeout(Duration::from_millis(200)) {
            Ok(AgentRunPoll::Event(event)) => match event {
                AgentEvent::AgentCompleted { .. } => {
                    terminal = "completed".to_string();
                    break;
                }
                AgentEvent::AgentCancelled { .. } => {
                    terminal = "cancelled".to_string();
                    break;
                }
                _ => continue,
            },
            Ok(AgentRunPoll::Timeout) => continue,
            Ok(AgentRunPoll::Finished) => {
                terminal = "finished-without-a-terminal-fact".to_string();
                break;
            }
            Err(failure) => {
                terminal = format!("failed: {}", failure.message);
                break;
            }
        }
    }
    eprintln!(
        "BACKLOG-RELEASE elapsed_ms={} terminal={terminal}",
        started.elapsed().as_millis()
    );
    let seen = replies(&peer.capture);
    let refusal = seen
        .iter()
        .find(|frame| frame["id"] == 7)
        .map(|frame| frame["error"]["code"].clone());
    eprintln!("BACKLOG-RELEASE execute_reply={refusal:?}");
    drop(handle);
    let reclaimed = wait_for_reclaim(&peer.pidfile, Duration::from_secs(15));
    eprintln!("BACKLOG-RELEASE peer_reclaimed={reclaimed}");
    peer.cleanup();

    assert!(
        !completed,
        "a bounded reader must stop the peer from completing the flood"
    );
    assert!(
        terminal.contains("backlog"),
        "the overload must fail the run explicitly, got {terminal}"
    );
    assert_eq!(
        refusal,
        Some(serde_json::json!(-32011)),
        "the released review must refuse the action, got {refusal:?}"
    );
    assert!(reclaimed, "the peer must be reclaimed within a bound");
}

/// Cancelling the pending review must still end within a bound and reclaim the
/// peer, even though the queue is full and the reader already stopped.
#[test]
fn a_flooded_peer_is_cancelled_and_reclaimed_within_a_bound() {
    let peer = flood_peer("cancel", 0);
    let handle = pending_review("acp-backlog-cancel", &peer);
    let completed = peer.marker.exists();
    eprintln!("BACKLOG-CANCEL peer_completed_flood={completed}");

    let started = Instant::now();
    handle.cancel();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut terminal = String::from("none");
    while Instant::now() < deadline {
        match handle.poll_event_timeout(Duration::from_millis(200)) {
            Ok(AgentRunPoll::Event(AgentEvent::AgentCancelled { .. })) => {
                terminal = "cancelled".to_string();
            }
            Ok(AgentRunPoll::Event(_)) | Ok(AgentRunPoll::Timeout) => continue,
            Ok(AgentRunPoll::Finished) => {
                if terminal == "none" {
                    terminal = "finished-without-a-terminal-fact".to_string();
                }
                break;
            }
            Err(failure) => {
                terminal = format!("failed: {}", failure.message);
                break;
            }
        }
    }
    let elapsed = started.elapsed();
    eprintln!(
        "BACKLOG-CANCEL elapsed_ms={} terminal={terminal}",
        elapsed.as_millis()
    );
    drop(handle);
    let reclaimed = wait_for_reclaim(&peer.pidfile, Duration::from_secs(15));
    eprintln!("BACKLOG-CANCEL peer_reclaimed={reclaimed}");
    peer.cleanup();

    assert!(
        !completed,
        "a bounded reader must stop the peer from completing the flood"
    );
    assert_eq!(
        terminal, "cancelled",
        "the cancel must end as a cancellation"
    );
    assert!(
        elapsed < Duration::from_secs(15),
        "the cancel must end within a bound, took {elapsed:?}"
    );
    assert!(reclaimed, "the peer must be reclaimed within a bound");
}

/// A stop reply that entered the queue before the overload must not report a
/// normal end: the stream was cut short, so the failure outranks every frame
/// already waiting behind the pending review.
#[test]
fn a_stop_queued_before_the_overload_still_fails_the_run() {
    let peer = flood_peer("stop-first", 100);
    let handle = pending_review("acp-backlog-stop-first", &peer);
    let completed = peer.marker.exists();
    eprintln!("BACKLOG-STOP-FIRST peer_completed_flood={completed}");

    handle
        .respond_approval(ApprovalResponse {
            request_id: "call-backlog".to_string(),
            tool_use_id: Some("call-backlog".to_string()),
            tool_input: None,
            decision: ApprovalDecision::Deny {
                message: "review denied".to_string(),
            },
        })
        .expect("approval delivery");
    let started = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut terminal = String::from("none");
    while Instant::now() < deadline {
        match handle.poll_event_timeout(Duration::from_millis(200)) {
            Ok(AgentRunPoll::Event(event)) => match event {
                AgentEvent::AgentCompleted { .. } => {
                    terminal = "completed".to_string();
                    break;
                }
                AgentEvent::AgentCancelled { .. } => {
                    terminal = "cancelled".to_string();
                    break;
                }
                _ => continue,
            },
            Ok(AgentRunPoll::Timeout) => continue,
            Ok(AgentRunPoll::Finished) => {
                terminal = "finished-without-a-terminal-fact".to_string();
                break;
            }
            Err(failure) => {
                terminal = format!("failed: {}", failure.message);
                break;
            }
        }
    }
    eprintln!(
        "BACKLOG-STOP-FIRST elapsed_ms={} terminal={terminal}",
        started.elapsed().as_millis()
    );
    drop(handle);
    let reclaimed = wait_for_reclaim(&peer.pidfile, Duration::from_secs(15));
    peer.cleanup();

    assert!(!completed, "the bounded reader must still stop the flood");
    assert!(
        terminal.contains("backlog"),
        "a queued stop must not hide the overload, got {terminal}"
    );
    assert!(reclaimed, "the peer must be reclaimed within a bound");
}
