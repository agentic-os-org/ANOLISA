//! Wire-level observation of the Shell UI event queue under a stopped consumer.
//!
//! The inbound bound is a transport claim only. These tests drive the production
//! entry with a consumer that never polls while the peer produces, so the queue
//! measured here is the one between the adapter thread and the runtime. No limit
//! is frozen: the counts record the current unbounded behaviour as the
//! counterexample the M05 decision needs.
//!
//! Measurement boundary: the peer's phase marker proves production finished, not
//! that the adapter finished delivering. Totals are therefore labelled as drained
//! volume and the zero-wait prefix is only the volume received inside the window.
//! The inventory evidence is a delivery barrier the ordered client loop answers
//! after forwarding every earlier frame, read from the peer side while the UI
//! queue stays untouched: the owned notification when a foreground ask exists,
//! otherwise the method-not-found reply to an unsupported method.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use cosh_shell::adapter::{
    AcpAgentAdapter, AdapterInstance, AgentRunHandle, AgentRunPoll, ApprovalDecision,
    ApprovalResponse,
};
use cosh_shell::types::{AgentEvent, CoshApprovalMode};

use super::acp_entry::{agent_script, make_request, nonce};

const EXECUTE: &str = "_cosh/shell/v1/execute";
/// The Shell writes this before it raises the ask, from the same ordered loop
/// that forwards updates, so receiving it proves every earlier frame was handed
/// to the UI channel. It is the inventory barrier, independent of UI consumption.
const OWNED: &str = "_cosh/shell/v1/owned";
/// Delivery barrier for a peer with no foreground ask. The ordered client loop
/// answers an unsupported method only after it forwarded every earlier frame,
/// so the reply proves the flood is queued without raising an ask.
const BARRIER: &str = concat!(
    r#"{"jsonrpc":"2.0","id":77,"method":"fs/read_text_file","#,
    r#""params":{"sessionId":"host-session","path":"/tmp/cosh-ui-barrier"}}"#,
);
const BARRIER_ID: u64 = 77;
const GROWTH_SMALL: usize = 2_000;
const GROWTH_LARGE: usize = 8_000;
const APPROVAL_FRAMES: usize = 2_000;
const CANCEL_FRAMES: usize = 4_000;
const RATE_FRAMES: usize = 20_000;
/// Frames per pace slice. A slice stays far below the transport queue cap, so
/// the client loop absorbs it during the pause and the UI queue is what grows.
const PACE_EVERY: usize = 200;
const DRAIN_BOUND: Duration = Duration::from_secs(60);
/// Mirrors the product UI_EVENT_CAP in adapter/mod.rs; the bound oracle pins it.
const UI_CAP: usize = 4_096;

/// These cases flood real pipes. Running them concurrently starves both the
/// scripted peers and the adapter threads on a small host, so they take turns.
/// This serializes the observation cases, not their assertions or bounds.
static OBSERVATION_LOCK: Mutex<()> = Mutex::new(());

fn observation_guard() -> MutexGuard<'static, ()> {
    OBSERVATION_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One legal bounded `session/update` notification per line.
const UPDATE_FRAME: &str = concat!(
    r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"host-session","#,
    r#""update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"x"}}}}"#,
);

struct UiPeer {
    script: PathBuf,
    gate: PathBuf,
    phase: PathBuf,
    marker: PathBuf,
    half: PathBuf,
    asked: PathBuf,
    capture: PathBuf,
    pidfile: PathBuf,
}

impl UiPeer {
    fn cleanup(&self) {
        for path in [
            &self.script,
            &self.gate,
            &self.phase,
            &self.marker,
            &self.half,
            &self.asked,
            &self.capture,
            &self.pidfile,
        ] {
            let _ = fs::remove_file(path);
        }
    }
}

/// A peer that completes the handshake and then produces legal notifications
/// without ever waiting for the runtime to render them. `gated` pauses halfway
/// so a cancel can be issued against a queue that is already backed up, and
/// `ask` hands one foreground action over after the flood so the review round
/// trip is measured with the flood still queued. `paced` throttles production to
/// a rate the client loop absorbs; without it the peer outruns the loop and the
/// inbound transport bound trips before the UI queue can grow.
fn ui_peer(name: &str, frames: usize, ask: bool, gated: bool, paced: bool) -> UiPeer {
    let stem = format!("cosh-acp-ui-{name}-{}-{}", std::process::id(), nonce());
    let peer = UiPeer {
        script: std::env::temp_dir().join(format!("{stem}.sh")),
        gate: std::env::temp_dir().join(format!("{stem}.gate")),
        phase: std::env::temp_dir().join(format!("{stem}.phase")),
        marker: std::env::temp_dir().join(format!("{stem}.done")),
        half: std::env::temp_dir().join(format!("{stem}.half")),
        asked: std::env::temp_dir().join(format!("{stem}.asked")),
        capture: std::env::temp_dir().join(format!("{stem}.jsonl")),
        pidfile: std::env::temp_dir().join(format!("{stem}.pid")),
    };
    let execute = format!(
        r#"{{"jsonrpc":"2.0","id":7,"method":"{EXECUTE}","params":{{"sessionId":"host-session","toolCallId":"call-ui","command":"echo hi","cwd":"/tmp","ptyInstance":"session-1","hookRequiresApproval":false}}}}"#
    );
    let half_at = if gated { frames / 2 + 1 } else { 0 };
    let pace = if paced {
        format!("if [ $((i % {PACE_EVERY})) -eq 0 ]; then sleep 0.01; fi\n  ")
    } else {
        String::new()
    };
    let ask_body = if ask {
        format!(
            r#"printf '%s\n' '{execute}'
/bin/echo asked > '{asked}'
for _ in $(seq 1 4); do
  IFS= read -r -t 15 line || break
  printf '%s\n' "$line" | /bin/cat >> '{capture}'
  case "$line" in *'"id":7'*) break ;; esac
done
"#,
            execute = execute,
            asked = peer.asked.display(),
            capture = peer.capture.display(),
        )
    } else {
        String::new()
    };
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
flood_ok=1
for i in $(seq 1 {frames}); do
  if [ "$i" = "{half_at}" ]; then
    printf '%s\n' '{barrier}'
    IFS= read -r -t 15 barrier_reply
    case "$barrier_reply" in
      *'"id":77'*) ;;
      *) echo "fixture: the delivery barrier reply never arrived" >&2; exit 1 ;;
    esac
    printf '%s\n' "$barrier_reply" | /bin/cat >> '{capture}'
    /bin/echo "$((i - 1))" > '{half}.tmp' && /bin/mv '{half}.tmp' '{half}'
    for _ in $(seq 1 30); do
      [ -f '{gate}' ] && break
      if IFS= read -r -t 1 line; then
        printf '%s\n' "$line" | /bin/cat >> '{capture}'
        case "$line" in
          *session/cancel*)
            printf '{{"jsonrpc":"2.0","id":%s,"result":{{"stopReason":"cancelled"}}}}\n' "$prompt_id"
            /bin/echo "phase $i" > '{phase}'
            exit 0
            ;;
        esac
      fi
    done
  fi
  {pace}printf '%s\n' '{frame}' || {{ flood_ok=0; break; }}
done
[ "$flood_ok" = 1 ] && /bin/echo flooded > '{marker}'
{ask_body}printf '{{"jsonrpc":"2.0","id":%s,"result":{{"stopReason":"end_turn"}}}}\n' "$prompt_id"
/bin/echo "phase $i" > '{phase}'
for _ in $(seq 1 2); do
  IFS= read -r -t 2 line || break
  printf '%s\n' "$line" | /bin/cat >> '{capture}'
done
"#,
        pidfile = peer.pidfile.display(),
        frames = frames,
        half_at = half_at,
        barrier = BARRIER,
        pace = pace,
        half = peer.half.display(),
        gate = peer.gate.display(),
        frame = UPDATE_FRAME,
        marker = peer.marker.display(),
        ask_body = ask_body,
        phase = peer.phase.display(),
        capture = peer.capture.display(),
    );
    let script = agent_script(name, &body);
    fs::rename(&script, &peer.script).expect("place ui fixture");
    peer
}

/// Waits until the peer's own capture holds the barrier frame. This reads the
/// peer side only, so the UI queue stays untouched while the barrier is proven.
fn wait_for_barrier(capture: &Path, bound: Duration) {
    let deadline = Instant::now() + bound;
    while Instant::now() < deadline {
        if fs::read_to_string(capture)
            .unwrap_or_default()
            .contains(OWNED)
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the owned barrier never reached the peer within {bound:?}");
}

fn deny(handle: &AgentRunHandle, request_id: &str) {
    handle
        .respond_approval(ApprovalResponse {
            request_id: request_id.to_string(),
            tool_use_id: Some(request_id.to_string()),
            tool_input: None,
            decision: ApprovalDecision::Deny {
                message: "review denied".to_string(),
            },
        })
        .expect("approval delivery");
}

fn wait_for_file(path: &Path, bound: Duration) {
    let deadline = Instant::now() + bound;
    while Instant::now() < deadline {
        if path.metadata().map(|meta| meta.len() > 0).unwrap_or(false) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("{} never appeared within {bound:?}", path.display());
}

fn start_run(name: &str, peer: &UiPeer) -> AgentRunHandle {
    let mut acp = AcpAgentAdapter::new(peer.script.to_string_lossy().into_owned());
    acp.idle_timeout = Duration::from_secs(60);
    AdapterInstance::Acp(acp.with_model_call(true))
        .start_cancellable(make_request(name), CoshApprovalMode::Auto)
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

/// What the consumer finds once it finally reads. `drained_*` is the total
/// volume delivered over the whole run, not a snapshot: a peer that finished its
/// writes does not prove the adapter finished delivering, and frames may still
/// sit in the inbound transport. `prefix_*` is the no-wait drain prefix, the
/// volume received inside the observation window; the counting loop itself takes
/// time, so the adapter can enqueue more between reads and this is neither an
/// exact inventory nor a lower bound. The inventory evidence is the owned
/// barrier instead. Bytes are the serialized notification volume, not heap or RSS.
#[derive(Default)]
struct UiDrain {
    drained_events: usize,
    drained_bytes: usize,
    prefix_events: usize,
    prefix_bytes: usize,
    prefix_text_events: usize,
    text_events: usize,
    text_bytes: usize,
    asks: usize,
    status_events: usize,
    cancelled_events: usize,
    completed_events: usize,
    failed_events: usize,
    other_events: usize,
    terminal: String,
    elapsed_ms: u128,
}

fn drain_all(handle: &AgentRunHandle, bound: Duration) -> UiDrain {
    let started = Instant::now();
    let deadline = started + bound;
    let mut out = UiDrain {
        terminal: "none".to_string(),
        ..UiDrain::default()
    };
    // The first pass never waits, which makes it the no-wait drain prefix. The
    // loop still spends time per event, so delivery can continue between reads:
    // this is the volume received inside the window, not an inventory snapshot
    // and not a lower bound either.
    let mut probing = true;
    while Instant::now() < deadline {
        let wait = if probing {
            Duration::ZERO
        } else {
            Duration::from_millis(200)
        };
        match handle.poll_event_timeout(wait) {
            Ok(AgentRunPoll::Event(event)) => {
                let bytes = serde_json::to_string(&event)
                    .map(|text| text.len())
                    .unwrap_or(0);
                out.drained_events += 1;
                out.drained_bytes += bytes;
                if probing {
                    out.prefix_events += 1;
                    out.prefix_bytes += bytes;
                    if matches!(&event, AgentEvent::TextDelta { .. }) {
                        out.prefix_text_events += 1;
                    }
                }
                match &event {
                    AgentEvent::TextDelta { text, .. } => {
                        out.text_events += 1;
                        out.text_bytes += text.len();
                    }
                    AgentEvent::ToolPermissionRequest { .. } => out.asks += 1,
                    AgentEvent::StatusChanged { .. } => out.status_events += 1,
                    AgentEvent::AgentCompleted { .. } => {
                        out.completed_events += 1;
                        out.terminal = "completed".to_string();
                    }
                    AgentEvent::AgentCancelled { .. } => {
                        out.cancelled_events += 1;
                        out.terminal = "cancelled".to_string();
                    }
                    AgentEvent::AgentFailed { error, .. } => {
                        out.failed_events += 1;
                        out.terminal = format!("failed: {error}");
                    }
                    _ => out.other_events += 1,
                }
            }
            Ok(AgentRunPoll::Timeout) => probing = false,
            Ok(AgentRunPoll::Finished) => {
                if out.terminal == "none" {
                    out.terminal = "finished-without-a-terminal-fact".to_string();
                }
                break;
            }
            Err(failure) => {
                out.terminal = format!("failed: {}", failure.message);
                break;
            }
        }
    }
    out.elapsed_ms = started.elapsed().as_millis();
    out
}

/// A29 bound oracle. Below the cap a stopped consumer keeps every produced
/// frame queued and the run completes; above the cap the admission gauge fails
/// the run at the bound while the consumer is stopped, and the queued data
/// events equal the cap. Control events stay exempt so the ask still delivers.
#[test]
fn a_stopped_consumer_lets_the_ui_queue_grow_with_the_peer_output() {
    let _observe = observation_guard();
    let frames = GROWTH_SMALL;
    let name = format!("acp-ui-growth-{frames}");
    let peer = ui_peer(&name, frames, true, false, true);
    let handle = start_run(&name, &peer);
    wait_for_file(&peer.marker, Duration::from_secs(120));
    wait_for_barrier(&peer.capture, Duration::from_secs(60));
    deny(&handle, "call-ui");
    wait_for_file(&peer.phase, Duration::from_secs(60));
    let drain = drain_all(&handle, DRAIN_BOUND);
    eprintln!(
        "UI-GROWTH frames={frames} barrier_owned=true drained_events={} drained_bytes={} text_events={} text_bytes={} terminal={} drain_ms={}",
        drain.drained_events, drain.drained_bytes, drain.text_events, drain.text_bytes, drain.terminal, drain.elapsed_ms
    );
    drop(handle);
    let reclaimed = wait_for_reclaim(&peer.pidfile, Duration::from_secs(20));
    eprintln!("UI-GROWTH frames={frames} peer_reclaimed={reclaimed}");
    peer.cleanup();
    assert_eq!(
        drain.text_events, frames,
        "below the cap every frame stays queued"
    );
    assert_eq!(drain.text_bytes, frames, "one text byte per frame");
    assert_eq!(drain.terminal, "completed", "got {}", drain.terminal);
    assert_eq!(
        drain.asks, 1,
        "the ask must still be queued behind the flood"
    );
    assert!(reclaimed, "the peer must be reclaimed within a bound");

    let frames = GROWTH_LARGE;
    let name = format!("acp-ui-bound-{frames}");
    // No half pause: the peer floods past the cap continuously while the
    // consumer stays stopped, so the gauge fills and the adapter fails.
    let peer = ui_peer(&name, frames, false, false, true);
    let handle = start_run(&name, &peer);
    // The consumer stays stopped while the adapter reaches the cap and fails;
    // the paced flood crosses the cap well inside this settle window.
    std::thread::sleep(Duration::from_secs(5));
    let drain = drain_all(&handle, DRAIN_BOUND);
    eprintln!(
        "UI-BOUND frames={frames} drained_events={} text_events={} status_events={} terminal={} drain_ms={}",
        drain.drained_events, drain.text_events, drain.status_events, drain.terminal, drain.elapsed_ms
    );
    drop(handle);
    let reclaimed = wait_for_reclaim(&peer.pidfile, Duration::from_secs(20));
    eprintln!("UI-BOUND frames={frames} peer_reclaimed={reclaimed}");
    peer.cleanup();
    assert_eq!(
        drain.text_events, UI_CAP,
        "the queued data events must equal the cap"
    );
    assert!(
        drain.terminal.starts_with("failed"),
        "the run must fail at the bound, got {}",
        drain.terminal
    );
    assert!(
        drain.terminal.contains("UI event queue"),
        "the bound error must name the UI queue, got {}",
        drain.terminal
    );
    assert!(reclaimed, "the peer must be reclaimed within a bound");
}

/// The rate counterexample: an unpaced peer produces faster than the client loop
/// drains, so the inbound transport bound trips and the run fails while the UI
/// queue holds only part of the flood. Which regime a machine lands in is
/// recorded, and each regime carries its own invariant; the upstream bound
/// failing the run is not the UI queue being bounded.
#[test]
fn an_unpaced_peer_shows_the_transport_bound_trip_before_the_ui_queue_fills() {
    let _observe = observation_guard();
    let peer = ui_peer("ui-rate", RATE_FRAMES, false, false, false);
    let handle = start_run("acp-ui-rate", &peer);
    wait_for_file(&peer.phase, Duration::from_secs(120));
    let completed_flood = peer.marker.exists();
    let drain = drain_all(&handle, DRAIN_BOUND);
    eprintln!(
        "UI-RATE frames={RATE_FRAMES} completed_flood={completed_flood} drained_events={} drained_bytes={} prefix_events={} prefix_bytes={} text_events={} terminal={} drain_ms={}",
        drain.drained_events, drain.drained_bytes, drain.prefix_events, drain.prefix_bytes, drain.text_events, drain.terminal, drain.elapsed_ms
    );
    drop(handle);
    let reclaimed = wait_for_reclaim(&peer.pidfile, Duration::from_secs(20));
    eprintln!("UI-RATE peer_reclaimed={reclaimed}");
    peer.cleanup();
    if completed_flood {
        assert_eq!(drain.text_events, RATE_FRAMES);
        assert_eq!(drain.terminal, "completed", "got {}", drain.terminal);
    } else {
        assert!(
            drain.terminal.contains("backlog"),
            "a cut flood must fail on the inbound bound, got {}",
            drain.terminal
        );
        assert!(
            drain.text_events < RATE_FRAMES,
            "the UI queue must hold only part of the flood, got {}",
            drain.text_events
        );
    }
    assert!(reclaimed, "the peer must be reclaimed within a bound");
}

/// Cancelling against a backed-up queue must reach the peer and end the run
/// without the consumer reading a single queued event first. The half marker is
/// published only after the client answered the delivery barrier, so the queued
/// volume it names is delivered volume rather than peer production.
///
/// The barrier covers only the text forwarded before that reply. The session
/// writes the reply before it sinks the matching status event, so a cancel
/// landing in between replaces the status with the single cancellation fact.
/// The status count is therefore bounded by one rather than fixed at one, and
/// the drained total is decomposed exactly over the event kinds instead of
/// asserting a fixed extra count.
#[test]
fn cancelling_a_backed_up_run_does_not_wait_for_the_consumer() {
    let _observe = observation_guard();
    let peer = ui_peer("ui-cancel", CANCEL_FRAMES, false, true, true);
    let handle = start_run("acp-ui-cancel", &peer);
    wait_for_file(&peer.half, Duration::from_secs(120));
    let half_frames = fs::read_to_string(&peer.half).unwrap_or_default();
    eprintln!("UI-CANCEL half_frames={}", half_frames.trim());
    let started = Instant::now();
    handle.cancel();
    wait_for_file(&peer.phase, Duration::from_secs(30));
    let cancel_to_phase_ms = started.elapsed().as_millis();
    let drain = drain_all(&handle, DRAIN_BOUND);
    let barrier_reply = replies(&peer.capture)
        .iter()
        .find(|frame| frame["id"] == BARRIER_ID)
        .map(|frame| frame["error"]["code"].clone());
    eprintln!(
        "UI-CANCEL cancel_to_phase_ms={cancel_to_phase_ms} drained_events={} drained_bytes={} prefix_events={} prefix_bytes={} text_events={} status_events={} cancelled_events={} completed_events={} failed_events={} other_events={} terminal={} drain_ms={}",
        drain.drained_events, drain.drained_bytes, drain.prefix_events, drain.prefix_bytes, drain.text_events, drain.status_events, drain.cancelled_events, drain.completed_events, drain.failed_events, drain.other_events, drain.terminal, drain.elapsed_ms
    );
    eprintln!(
        "UI-CANCEL phase={:?} capture={:?}",
        fs::read_to_string(&peer.phase).unwrap_or_default(),
        fs::read_to_string(&peer.capture).unwrap_or_default()
    );
    drop(handle);
    let reclaimed = wait_for_reclaim(&peer.pidfile, Duration::from_secs(20));
    eprintln!("UI-CANCEL peer_reclaimed={reclaimed}");
    peer.cleanup();
    assert_eq!(
        barrier_reply,
        Some(serde_json::json!(-32601)),
        "the half marker must be published by a real delivery barrier reply"
    );
    assert_eq!(
        drain.text_events,
        CANCEL_FRAMES / 2,
        "the half flood must have stayed queued, got {}",
        drain.text_events
    );
    assert!(
        drain.status_events <= 1,
        "the barrier's unsupported-method fact is at most one status event, got {}",
        drain.status_events
    );
    assert_eq!(drain.terminal, "cancelled", "got {}", drain.terminal);
    assert_eq!(
        drain.cancelled_events, 1,
        "one cancellation fact, not one per queued frame"
    );
    assert_eq!(drain.completed_events, 0);
    assert_eq!(drain.failed_events, 0);
    assert_eq!(drain.other_events, 0);
    assert_eq!(drain.asks, 0);
    assert_eq!(
        drain.drained_events,
        drain.text_events + drain.status_events + drain.cancelled_events
    );
    assert!(
        cancel_to_phase_ms < 15_000,
        "the cancel must reach the peer without a drain, took {cancel_to_phase_ms}ms"
    );
    assert!(reclaimed, "the peer must be reclaimed within a bound");
}

/// The foreground review must complete while the flood is still queued: the ask
/// itself stays unread, so the answer provably did not depend on the consumer.
#[test]
fn a_pending_review_is_answered_while_the_ui_queue_stays_undrained() {
    let _observe = observation_guard();
    let peer = ui_peer("ui-approval", APPROVAL_FRAMES, true, false, true);
    let handle = start_run("acp-ui-approval", &peer);
    wait_for_file(&peer.marker, Duration::from_secs(120));
    wait_for_file(&peer.asked, Duration::from_secs(30));
    wait_for_barrier(&peer.capture, Duration::from_secs(60));
    let started = Instant::now();
    deny(&handle, "call-ui");
    wait_for_file(&peer.phase, Duration::from_secs(60));
    let reply_ms = started.elapsed().as_millis();
    eprintln!(
        "UI-APPROVAL capture_at_phase={:?}",
        fs::read_to_string(&peer.capture).unwrap_or_default()
    );
    let drain = drain_all(&handle, DRAIN_BOUND);
    let seen = replies(&peer.capture);
    let refusal = seen
        .iter()
        .find(|frame| frame["id"] == 7)
        .map(|frame| frame["error"]["code"].clone());
    eprintln!(
        "UI-APPROVAL reply_ms={reply_ms} barrier_owned=true execute_reply={refusal:?} drained_events={} drained_bytes={} prefix_events={} prefix_bytes={} text_events={} asks={} terminal={} drain_ms={}",
        drain.drained_events, drain.drained_bytes, drain.prefix_events, drain.prefix_bytes, drain.text_events, drain.asks, drain.terminal, drain.elapsed_ms
    );
    drop(handle);
    let reclaimed = wait_for_reclaim(&peer.pidfile, Duration::from_secs(20));
    eprintln!("UI-APPROVAL peer_reclaimed={reclaimed}");
    peer.cleanup();
    assert_eq!(
        refusal,
        Some(serde_json::json!(-32011)),
        "the review must refuse the action, got {refusal:?}"
    );
    assert_eq!(
        drain.asks, 1,
        "the ask must still have been queued when the answer was delivered"
    );
    assert_eq!(drain.text_events, APPROVAL_FRAMES);
    assert_eq!(drain.terminal, "completed", "got {}", drain.terminal);
    assert!(reclaimed, "the peer must be reclaimed within a bound");
}
