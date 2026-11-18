//! Wire-level control for the outbound write path of the Shell ACP client.
//!
//! The peer finishes the handshake, consumes a bounded prefix of the prompt so
//! the write is provably under way, then stops draining stdin and stays alive.
//! A client loop stuck inside a blocking write can never reach its cancel
//! check, so the run must still converge inside the existing cancel grace.
//! Only this write entry is covered: no Core SDK queue and no other queue.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use cosh_shell::adapter::{AcpAgentAdapter, AdapterInstance, AgentRunPoll};
use cosh_shell::types::{AgentEvent, CoshApprovalMode};

use super::acp_entry::{agent_script, make_request, nonce};

/// Large enough that no default pipe capacity can hold it, small enough to stay
/// a legal single frame. Linux and macOS both default to at most 64 KiB.
const PROMPT_BYTES: usize = 900_000;
/// Independent oracle for the documented single-frame limit. The prompt leaves
/// 8192 bytes of framing headroom below it and stays far above the 64 KiB pipe.
const ORACLE_MAX_FRAME_BYTES: usize = 1024 * 1024;
const _: () = assert!(PROMPT_BYTES + 8192 <= ORACLE_MAX_FRAME_BYTES);
const _: () = assert!(PROMPT_BYTES > 64 * 1024);
/// Independent oracle for the documented cancel grace, plus scheduling margin.
const ORACLE_CANCEL_BOUND: Duration = Duration::from_secs(10);
/// Observation window: four times the grace, so a stuck run is recorded as stuck
/// instead of being reported as a slow success.
const OBSERVE_BOUND: Duration = Duration::from_secs(20);
/// Prefix the peer consumes to prove the prompt write started.
const PROMPT_PREFIX: usize = 4096;

struct StalledPeer {
    script: PathBuf,
    phase: PathBuf,
    pidfile: PathBuf,
}

impl StalledPeer {
    fn cleanup(&self) {
        // Test-side fallback only. Killing the peer here is never evidence that
        // the product converged; the product reclaim is measured separately.
        if let Ok(text) = fs::read_to_string(&self.pidfile) {
            let pid = text.trim();
            if !pid.is_empty() {
                let _ = Command::new("kill")
                    .arg("-9")
                    .arg(format!("-{pid}"))
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
        for path in [&self.script, &self.phase, &self.pidfile] {
            let _ = fs::remove_file(path);
        }
    }
}

/// A peer that answers the handshake, reads exactly one bounded prefix of the
/// prompt, then never reads stdin again while staying alive.
fn stalled_peer(name: &str) -> StalledPeer {
    let stem = format!(
        "cosh-acp-outbound-{name}-{}-{}",
        std::process::id(),
        nonce()
    );
    let peer = StalledPeer {
        script: std::env::temp_dir().join(format!("{stem}.sh")),
        phase: std::env::temp_dir().join(format!("{stem}.phase")),
        pidfile: std::env::temp_dir().join(format!("{stem}.pid")),
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
head -c {prefix} > /dev/null
printf 'prompt-prefix-read\n' > '{phase}'
while :; do sleep 1; done
"#,
        pidfile = peer.pidfile.display(),
        phase = peer.phase.display(),
        prefix = PROMPT_PREFIX,
    );
    let script = agent_script(name, &body);
    fs::rename(&script, &peer.script).expect("place stalled fixture");
    peer
}

fn wait_for_file(path: &Path, bound: Duration) -> bool {
    let deadline = Instant::now() + bound;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// `kill -0` cannot tell a zombie from a live process, so the state is read.
fn peer_state(pidfile: &Path) -> String {
    let Ok(text) = fs::read_to_string(pidfile) else {
        return String::new();
    };
    let pid = text.trim().to_string();
    if pid.is_empty() {
        return String::new();
    }
    let output = Command::new("ps")
        .arg("-o")
        .arg("state=")
        .arg("-p")
        .arg(&pid)
        .output();
    match output {
        Ok(output) => String::from_utf8_lossy(&output.stdout).trim().to_string(),
        Err(_) => String::new(),
    }
}

fn peer_alive(pidfile: &Path) -> bool {
    let state = peer_state(pidfile);
    !state.is_empty() && !state.starts_with('Z')
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

/// A cancel requested while the prompt write is blocked must still end inside
/// the existing grace, and the product must reclaim the peer itself.
#[test]
fn a_cancel_during_a_blocked_prompt_write_converges_inside_the_grace() {
    let peer = stalled_peer("cancel");
    let mut request = make_request("acp-outbound-cancel");
    request.user_input = Some("x".repeat(PROMPT_BYTES));
    // The codec enforces the single-frame limit itself, so a write that provably
    // started is also proof that the frame it carries was accepted as legal.
    eprintln!("OUTBOUND-CANCEL user_input_bytes={PROMPT_BYTES}");

    let mut acp = AcpAgentAdapter::new(peer.script.to_string_lossy().into_owned());
    acp.idle_timeout = Duration::from_secs(20);
    let handle = AdapterInstance::Acp(acp.with_model_call(true))
        .start_cancellable(request, CoshApprovalMode::Auto);

    // The prefix can only be read after the client wrote it, so this file is
    // proof that the write phase started, not a timing assumption.
    let write_started = wait_for_file(&peer.phase, Duration::from_secs(30));
    eprintln!("OUTBOUND-CANCEL write_started={write_started}");

    let started = Instant::now();
    handle.cancel();
    let mut terminal = String::from("none");
    let deadline = Instant::now() + OBSERVE_BOUND;
    while Instant::now() < deadline {
        match handle.poll_event_timeout(Duration::from_millis(200)) {
            Ok(AgentRunPoll::Event(AgentEvent::AgentCancelled { .. })) => {
                terminal = "cancelled".to_string();
                break;
            }
            Ok(AgentRunPoll::Event(AgentEvent::AgentCompleted { .. })) => {
                terminal = "completed".to_string();
                break;
            }
            Ok(AgentRunPoll::Event(_)) | Ok(AgentRunPoll::Timeout) => continue,
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
    let elapsed = started.elapsed();
    let state_at_terminal = peer_state(&peer.pidfile);
    eprintln!(
        "OUTBOUND-CANCEL elapsed_ms={} terminal={terminal} peer_state={state_at_terminal}",
        elapsed.as_millis()
    );
    drop(handle);
    let reclaimed = wait_for_reclaim(&peer.pidfile, Duration::from_secs(10));
    eprintln!("OUTBOUND-CANCEL peer_reclaimed_by_product={reclaimed}");
    peer.cleanup();

    assert!(write_started, "the prompt write must provably have started");
    assert_eq!(
        terminal, "cancelled",
        "no complete prompt frame reached the peer, so the cancel must end as a cancellation, got {terminal}"
    );
    assert!(
        elapsed < ORACLE_CANCEL_BOUND,
        "the cancel must converge inside the existing grace, took {elapsed:?}"
    );
    assert!(
        reclaimed,
        "the product must reclaim the stalled peer without test-side help"
    );
}

/// Bytes the peer drains per step, and the pause between steps. The period stays
/// below the client's poll slice, so every poll sees progress and a cancel check
/// that only runs on a full slice without progress is never reached. Linux only
/// signals POLLOUT once PIPE_BUF (4096) bytes are free while macOS signals at
/// 512, so the step size is per platform; the rate stays low enough on both that
/// the frame is still being written when the post-cancel budget expires.
#[cfg(target_os = "linux")]
const DRIBBLE_BYTES: usize = 4096;
#[cfg(target_os = "linux")]
const DRIBBLE_PAUSE: &str = "0.035";
#[cfg(not(target_os = "linux"))]
const DRIBBLE_BYTES: usize = 512;
#[cfg(not(target_os = "linux"))]
const DRIBBLE_PAUSE: &str = "0.001";

struct DribblePeer {
    script: PathBuf,
    phase: PathBuf,
    pidfile: PathBuf,
    capture: PathBuf,
}

impl DribblePeer {
    fn consumed(&self) -> u64 {
        fs::metadata(&self.capture)
            .map(|data| data.len())
            .unwrap_or(0)
    }

    fn cleanup(&self) {
        // Test-side fallback only, never evidence of product convergence.
        if let Ok(text) = fs::read_to_string(&self.pidfile) {
            let pid = text.trim();
            if !pid.is_empty() {
                let _ = Command::new("kill")
                    .arg("-9")
                    .arg(format!("-{pid}"))
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
        for path in [&self.script, &self.phase, &self.pidfile, &self.capture] {
            let _ = fs::remove_file(path);
        }
    }
}

/// A peer that completes the handshake, proves the prompt write started, then
/// keeps draining a small amount more often than the client's poll slice. The
/// frame is far larger than the window can drain, so the write never completes.
/// `dd` is used because BSD `head -c` consumes the whole pipe buffer instead of
/// the requested count, which would let the write finish and hide the bypass.
fn dribble_peer(name: &str) -> DribblePeer {
    let stem = format!("cosh-acp-dribble-{name}-{}-{}", std::process::id(), nonce());
    let peer = DribblePeer {
        script: std::env::temp_dir().join(format!("{stem}.sh")),
        phase: std::env::temp_dir().join(format!("{stem}.phase")),
        pidfile: std::env::temp_dir().join(format!("{stem}.pid")),
        capture: std::env::temp_dir().join(format!("{stem}.jsonl")),
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
dd bs={prefix} count=1 >> '{capture}' 2>/dev/null
printf 'prompt-prefix-read\n' > '{phase}'
while :; do
  dd bs={dribble} count=1 >> '{capture}' 2>/dev/null || break
  sleep {pause}
done
"#,
        pidfile = peer.pidfile.display(),
        phase = peer.phase.display(),
        capture = peer.capture.display(),
        prefix = PROMPT_PREFIX,
        dribble = DRIBBLE_BYTES,
        pause = DRIBBLE_PAUSE,
    );
    let script = agent_script(name, &body);
    fs::rename(&script, &peer.script).expect("place dribble fixture");
    peer
}

/// A cancel must bound the total time spent writing, not just one wait without
/// progress: a peer that keeps the pipe barely writable must not let the write
/// run past the existing cancel grace.
#[test]
fn a_cancel_during_a_slowly_progressing_prompt_write_converges_inside_the_grace() {
    let peer = dribble_peer("cancel");
    let mut request = make_request("acp-outbound-dribble-cancel");
    request.user_input = Some("x".repeat(PROMPT_BYTES));
    eprintln!("OUTBOUND-DRIBBLE user_input_bytes={PROMPT_BYTES}");

    let mut acp = AcpAgentAdapter::new(peer.script.to_string_lossy().into_owned());
    acp.idle_timeout = Duration::from_secs(60);
    let handle = AdapterInstance::Acp(acp.with_model_call(true))
        .start_cancellable(request, CoshApprovalMode::Auto);

    let write_started = wait_for_file(&peer.phase, Duration::from_secs(30));
    // Waiting for the first dribble is a fact, not a pause: it proves the peer
    // is draining in steps smaller than the frame instead of having finished it.
    let dribbling = {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if peer.consumed() > PROMPT_PREFIX as u64 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        peer.consumed() > PROMPT_PREFIX as u64
    };
    eprintln!("OUTBOUND-DRIBBLE write_started={write_started} dribbling={dribbling}");

    let started = Instant::now();
    handle.cancel();
    let consumed_at_cancel = peer.consumed();
    let mut terminal = String::from("none");
    let deadline = Instant::now() + OBSERVE_BOUND;
    while Instant::now() < deadline {
        match handle.poll_event_timeout(Duration::from_millis(200)) {
            Ok(AgentRunPoll::Event(AgentEvent::AgentCancelled { .. })) => {
                terminal = "cancelled".to_string();
                break;
            }
            Ok(AgentRunPoll::Event(AgentEvent::AgentCompleted { .. })) => {
                terminal = "completed".to_string();
                break;
            }
            Ok(AgentRunPoll::Event(_)) | Ok(AgentRunPoll::Timeout) => continue,
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
    let elapsed = started.elapsed();
    let consumed_at_terminal = peer.consumed();
    eprintln!(
        "OUTBOUND-DRIBBLE elapsed_ms={} terminal={terminal} consumed_at_cancel={consumed_at_cancel} consumed_at_terminal={consumed_at_terminal}",
        elapsed.as_millis()
    );
    drop(handle);
    let reclaimed = wait_for_reclaim(&peer.pidfile, Duration::from_secs(10));
    eprintln!("OUTBOUND-DRIBBLE peer_reclaimed_by_product={reclaimed}");
    peer.cleanup();

    assert!(write_started, "the prompt write must provably have started");
    assert!(dribbling, "the peer must be draining in small steps");
    assert!(
        consumed_at_terminal > consumed_at_cancel,
        "the fixture must keep the write progressing after the cancel"
    );
    assert_eq!(
        terminal, "cancelled",
        "a slowly progressing write must still end as a cancellation, got {terminal}"
    );
    assert!(
        elapsed < ORACLE_CANCEL_BOUND,
        "the post-cancel write budget must not restart on progress, took {elapsed:?}"
    );
    assert!(
        reclaimed,
        "the product must reclaim the stalled peer without test-side help"
    );
}
