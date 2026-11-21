//! Real subprocess pipe exit observations for `cosh-core --acp`.
//!
//! A duplex fixture cannot prove production exit: the real transport uses
//! Tokio's blocking stdin/stdout threads and `main` calls `std::process::exit`
//! only after the connection driver returns. These cases drive the real binary
//! over real pipes and separate three facts: the tail frames actually written,
//! the process actually exiting, and what a stopped reader does to both.
//!
//! Oracle:
//! - Reading peer: stdin EOF drains the accepted tail and the process exits 0.
//! - Stopped reader: resuming inside the close budget still drains the whole
//!   tail and exits 0. A permanent stop ends the process inside that budget and
//!   declares the tail incomplete instead of staying resident.
//! - Vanished peer: closing both ends must end the process rather than leak it.

mod common;

use std::process::Stdio;
use std::time::{Duration, Instant};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema::{
    ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest, TextContent,
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const EXIT_TIMEOUT: Duration = Duration::from_secs(30);
/// Residency observation window for the stopped-reader case. Stays below
/// CLOSE_GRACE so the read resumes inside the frozen close budget.
const STOP_READ_OBSERVATION: Duration = Duration::from_secs(1);
/// Mirrors the production post-EOF close budget. Asserting the bound here means
/// a change to the production value fails this oracle instead of silently
/// widening the promise.
const CLOSE_GRACE: Duration = Duration::from_secs(5);
/// Scheduling slack allowed on top of the budget when asserting the exit.
const EXIT_SLACK: Duration = Duration::from_secs(3);
/// Marker the agent must print when it ends a connection with an unflushed tail.
const INCOMPLETE_MARKER: &str = "tail is incomplete";
/// Large enough to exceed the pipe capacity, small enough to stay under the
/// kernel's emergency context threshold so the turn actually runs.
const PROMPT_BYTES: usize = 80_000;
/// Small enough that the whole turn output fits inside the pipe buffer, so a
/// stopped reader cannot block the writer. Used as the control case.
const SMALL_PROMPT_BYTES: usize = 2_000;

fn mock_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("temp home");
    let config_dir = home.path().join(".copilot-shell");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        "[ai]\nactive_provider = \"mock\"\n\n[ai.providers.mock]\ntype = \"mock\"\n",
    )
    .unwrap();
    home
}

struct Peer {
    stdin: Option<ChildStdin>,
    out: BufReader<ChildStdout>,
    pending: Vec<u8>,
    next_id: u64,
}

impl Peer {
    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id + 1;
        self.next_id = id;
        self.write_frame(&serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params,
        }))
        .await;
        loop {
            let value = self.next_json().await;
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return value;
            }
        }
    }

    async fn write_frame(&mut self, frame: &Value) {
        let mut line = frame.to_string();
        line.push('\n');
        let stdin = self.stdin.as_mut().expect("stdin still open");
        stdin
            .write_all(line.as_bytes())
            .await
            .expect("write request frame");
        stdin.flush().await.expect("flush request frame");
    }

    async fn next_line(&mut self) -> Option<String> {
        let mut buf = std::mem::take(&mut self.pending);
        let read = tokio::time::timeout(REQUEST_TIMEOUT, self.out.read_until(b'\n', &mut buf))
            .await
            .expect("timed out waiting for a frame")
            .expect("read stdout");
        if read == 0 && buf.is_empty() {
            return None;
        }
        if buf.last() == Some(&b'\n') {
            buf.pop();
        }
        Some(String::from_utf8(buf).expect("frame is UTF-8"))
    }

    async fn next_json(&mut self) -> Value {
        let line = self
            .next_line()
            .await
            .expect("stdout closed before the expected frame");
        serde_json::from_str(&line).expect("frame is JSON")
    }

    /// Drains stdout to EOF, returning the echoed text, frame count and the
    /// prompt terminal response.
    async fn drain_to_terminal(&mut self) -> (String, usize, Value) {
        let mut text = String::new();
        let mut frames = 0usize;
        let mut terminal = None;
        while let Some(line) = self.next_line().await {
            frames += 1;
            let value: Value = serde_json::from_str(&line).expect("frame is JSON");
            if value.get("id").and_then(Value::as_u64) == Some(99) {
                terminal = Some(value.clone());
            }
            if let Some(chunk) = value
                .pointer("/params/update/content/text")
                .and_then(Value::as_str)
            {
                text.push_str(chunk);
            }
        }
        (
            text,
            frames,
            terminal.expect("the terminal response must arrive"),
        )
    }

    /// Consumes exactly one output byte. This proves the agent started writing
    /// without draining the pipe, so the writer is left blocked on a full pipe.
    async fn probe_one_byte(&mut self) {
        let mut byte = [0u8; 1];
        tokio::time::timeout(REQUEST_TIMEOUT, self.out.read_exact(&mut byte))
            .await
            .expect("timed out waiting for the first output byte")
            .expect("read the first output byte");
        self.pending.push(byte[0]);
    }
}

async fn spawn() -> (tempfile::TempDir, tempfile::TempDir, Child, Peer) {
    let home = mock_home();
    let workspace = tempfile::tempdir().expect("temp workspace");
    let mut command: Command = common::cosh_core_command(home.path()).into();
    command
        .arg("--acp")
        .arg("--workspace")
        .arg(workspace.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().expect("spawn cosh-core --acp");
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let peer = Peer {
        stdin: Some(stdin),
        out: BufReader::new(stdout),
        pending: Vec::new(),
        next_id: 0,
    };
    (home, workspace, child, peer)
}

async fn handshake(peer: &mut Peer, workspace: &std::path::Path) -> String {
    let initialize = peer
        .request(
            "initialize",
            serde_json::to_value(InitializeRequest::new(ProtocolVersion::V1)).unwrap(),
        )
        .await;
    assert!(
        initialize.get("result").is_some(),
        "initialize must succeed: {initialize}"
    );
    let session = peer
        .request(
            "session/new",
            serde_json::to_value(NewSessionRequest::new(workspace.to_path_buf())).unwrap(),
        )
        .await;
    session
        .pointer("/result/sessionId")
        .and_then(Value::as_str)
        .expect("session/new returns a session id")
        .to_string()
}

fn prompt_frame(session_id: &str, bytes: usize) -> Value {
    let text = "x".repeat(bytes);
    let request = PromptRequest::new(
        agent_client_protocol::v1::schema::SessionId::new(session_id.to_string()),
        vec![ContentBlock::Text(TextContent::new(text))],
    );
    serde_json::json!({
        "jsonrpc": "2.0", "id": 99, "method": "session/prompt",
        "params": serde_json::to_value(request).unwrap(),
    })
}

/// Collects frames until the prompt response, returning the text and the count.
async fn drain_prompt(peer: &mut Peer) -> (String, usize, Value) {
    let mut text = String::new();
    let mut frames = 0usize;
    loop {
        let value = peer.next_json().await;
        frames += 1;
        if value.get("id").and_then(Value::as_u64) == Some(99) {
            return (text, frames, value);
        }
        if let Some(chunk) = value
            .pointer("/params/update/content/text")
            .and_then(Value::as_str)
        {
            text.push_str(chunk);
        }
    }
}

async fn wait_exit(child: &mut Child) -> Option<i32> {
    match tokio::time::timeout(EXIT_TIMEOUT, child.wait()).await {
        Ok(status) => status.expect("child status").code(),
        Err(_) => None,
    }
}

#[tokio::test]
async fn eof_while_reading_drains_the_tail_and_exits() {
    let (_home, workspace, mut child, mut peer) = spawn().await;
    let session_id = handshake(&mut peer, workspace.path()).await;
    peer.write_frame(&prompt_frame(&session_id, PROMPT_BYTES))
        .await;
    let (text, frames, response) = drain_prompt(&mut peer).await;
    assert_eq!(
        response
            .pointer("/result/stopReason")
            .and_then(Value::as_str),
        Some("end_turn"),
        "the turn must end normally: {response}"
    );
    assert!(
        text.len() >= PROMPT_BYTES,
        "the whole echoed tail must be written, got {} bytes",
        text.len()
    );

    drop(peer.stdin.take());
    let mut tail = 0usize;
    while let Ok(Some(_)) = tokio::time::timeout(EXIT_TIMEOUT, peer.next_line()).await {
        tail += 1;
    }
    let code = wait_exit(&mut child).await;
    println!(
        "EXIT-READ frames={frames} tail_after_eof={tail} text_bytes={} exit={code:?}",
        text.len()
    );
    assert_eq!(code, Some(0), "a reading peer must see a clean exit");
}

#[tokio::test]
async fn a_stopped_reader_is_observed_and_resuming_still_drains_and_exits() {
    let (_home, workspace, mut child, mut peer) = spawn().await;
    let session_id = handshake(&mut peer, workspace.path()).await;
    peer.write_frame(&prompt_frame(&session_id, PROMPT_BYTES))
        .await;
    peer.probe_one_byte().await;

    // EOF arrives while the writer is blocked on a full pipe and nobody reads.
    // The window stays inside the close budget, so resuming here must still
    // recover every accepted byte.
    drop(peer.stdin.take());
    let started = Instant::now();
    let mut exited_early = None;
    while started.elapsed() < STOP_READ_OBSERVATION {
        if let Some(status) = child.try_wait().expect("try_wait") {
            exited_early = Some(status.code());
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    println!(
        "EXIT-STOPPED observation_ms={} exited_without_read={exited_early:?}",
        started.elapsed().as_millis()
    );

    let (text, frames, response) = drain_prompt(&mut peer).await;
    let code = wait_exit(&mut child).await;
    println!(
        "EXIT-RESUME frames={frames} text_bytes={} exit={code:?}",
        text.len()
    );
    assert_eq!(
        response
            .pointer("/result/stopReason")
            .and_then(Value::as_str),
        Some("end_turn"),
        "resuming the read must still deliver the terminal response: {response}"
    );
    assert!(
        text.len() >= PROMPT_BYTES,
        "resuming the read must recover the whole tail, got {} bytes",
        text.len()
    );
    assert_eq!(code, Some(0), "resuming the read must reach a clean exit");
}

#[tokio::test]
async fn a_vanished_peer_closes_both_ends_and_the_agent_exits() {
    let (_home, workspace, mut child, mut peer) = spawn().await;
    let session_id = handshake(&mut peer, workspace.path()).await;
    peer.write_frame(&prompt_frame(&session_id, PROMPT_BYTES))
        .await;

    // The peer process is gone: neither end is held any more.
    let mut stderr = child.stderr.take().expect("piped stderr");
    drop(peer.stdin.take());
    drop(peer.out);
    let code = wait_exit(&mut child).await;
    let mut reason = String::new();
    let _ = tokio::time::timeout(EXIT_TIMEOUT, stderr.read_to_string(&mut reason)).await;
    let reason: String = reason.chars().take(300).collect();
    println!("EXIT-VANISHED exit={code:?} stderr={reason:?}");
    assert!(
        code.is_some(),
        "a vanished peer must not leave the agent resident"
    );
}

#[tokio::test]
async fn a_small_turn_with_a_stopped_reader_still_exits_after_eof() {
    let (_home, workspace, mut child, mut peer) = spawn().await;
    let session_id = handshake(&mut peer, workspace.path()).await;
    peer.write_frame(&prompt_frame(&session_id, SMALL_PROMPT_BYTES))
        .await;

    // Control for the stopped-reader case: the same EOF with an output that
    // fits in the pipe buffer. Any residency above must therefore come from the
    // blocked write, not from EOF handling on its own.
    drop(peer.stdin.take());
    let code = wait_exit(&mut child).await;

    let mut text = String::new();
    let mut frames = 0usize;
    let mut terminal = false;
    while let Ok(Some(line)) = tokio::time::timeout(EXIT_TIMEOUT, peer.next_line()).await {
        frames += 1;
        let value: Value = serde_json::from_str(&line).expect("frame is JSON");
        if value.get("id").and_then(Value::as_u64) == Some(99) {
            terminal = true;
        }
        if let Some(chunk) = value
            .pointer("/params/update/content/text")
            .and_then(Value::as_str)
        {
            text.push_str(chunk);
        }
    }
    println!(
        "EXIT-SMALL-STOPPED exit={code:?} frames={frames} terminal_seen={terminal} text_bytes={}",
        text.len()
    );
    assert!(
        code.is_some(),
        "a small turn must not stay resident after stdin EOF"
    );
}

/// Measures how long a *reading* peer needs to drain the tail after stdin EOF.
///
/// M07 may only bound the permanent-stop-read case with a grace safely above
/// this baseline, so the number is measured rather than chosen. Two readers are
/// measured: one that drains as fast as the pipe allows, and one throttled to a
/// fixed rate, because a grace sized only for the fast reader would truncate a
/// slow but healthy consumer. The writer is left blocked on a full pipe before
/// EOF; the small payload is the control that fits inside the pipe buffer.
///
/// The mock provider's context window rejects prompts far above PROMPT_BYTES,
/// so the measured range is bounded by the fixture, not by the transport.
#[tokio::test]
async fn post_eof_drain_duration_baseline_for_a_reading_peer() {
    const REPEATS: usize = 3;
    for bytes in [SMALL_PROMPT_BYTES, PROMPT_BYTES] {
        for round in 1..=REPEATS {
            let (_home, workspace, mut child, mut peer) = spawn().await;
            let session_id = handshake(&mut peer, workspace.path()).await;
            peer.write_frame(&prompt_frame(&session_id, bytes)).await;
            peer.probe_one_byte().await;
            drop(peer.stdin.take());
            let started = Instant::now();
            let (text, frames, terminal) = peer.drain_to_terminal().await;
            let drain_ms = started.elapsed().as_millis();
            let code = wait_exit(&mut child).await;
            println!(
                "DRAIN-BASELINE reader=fast prompt_bytes={bytes} round={round} \
                 drain_ms={drain_ms} frames_after_eof={frames} text_bytes={} exit={code:?}",
                text.len()
            );
            assert_eq!(code, Some(0), "a reading peer must see a clean exit");
            assert_eq!(
                terminal
                    .pointer("/result/stopReason")
                    .and_then(Value::as_str),
                Some("end_turn"),
                "the turn must end normally: {terminal}"
            );
            assert!(
                text.len() >= bytes,
                "the whole tail must be written, got {} of {bytes}",
                text.len()
            );
        }
    }

    // A throttled reader that consumes raw bytes in bounded chunks, so the pipe
    // really is drained slowly and the writer has to wait for each chunk. 4096
    // bytes per 50 ms is about 80 KB/s: far slower than the pipe, still progress.
    const CHUNK: usize = 4096;
    const PAUSE: Duration = Duration::from_millis(50);
    for round in 1..=REPEATS {
        let (_home, workspace, mut child, mut peer) = spawn().await;
        let session_id = handshake(&mut peer, workspace.path()).await;
        peer.write_frame(&prompt_frame(&session_id, PROMPT_BYTES))
            .await;
        peer.probe_one_byte().await;
        drop(peer.stdin.take());
        let started = Instant::now();
        let mut raw = std::mem::take(&mut peer.pending);
        let mut buf = [0u8; CHUNK];
        loop {
            let read = tokio::time::timeout(EXIT_TIMEOUT, peer.out.read(&mut buf))
                .await
                .expect("timed out draining the throttled tail")
                .expect("read stdout");
            if read == 0 {
                break;
            }
            raw.extend_from_slice(&buf[..read]);
            tokio::time::sleep(PAUSE).await;
        }
        let drain_ms = started.elapsed().as_millis();
        let mut text = String::new();
        let mut frames = 0usize;
        let mut terminal = None;
        for line in String::from_utf8(raw).expect("tail is UTF-8").lines() {
            if line.trim().is_empty() {
                continue;
            }
            frames += 1;
            let value: Value = serde_json::from_str(line).expect("frame is JSON");
            if value.get("id").and_then(Value::as_u64) == Some(99) {
                terminal = Some(value.clone());
            }
            if let Some(chunk) = value
                .pointer("/params/update/content/text")
                .and_then(Value::as_str)
            {
                text.push_str(chunk);
            }
        }
        let code = wait_exit(&mut child).await;
        println!(
            "DRAIN-BASELINE reader=throttled prompt_bytes={PROMPT_BYTES} round={round} \
             drain_ms={drain_ms} frames_after_eof={frames} text_bytes={} exit={code:?}",
            text.len()
        );
        assert_eq!(
            code,
            Some(0),
            "a throttled reader must still see a clean exit"
        );
        let terminal = terminal.expect("the terminal response must still arrive");
        assert_eq!(
            terminal
                .pointer("/result/stopReason")
                .and_then(Value::as_str),
            Some("end_turn"),
            "the turn must end normally: {terminal}"
        );
        assert!(text.len() >= PROMPT_BYTES, "the whole tail must be written");
    }
}

/// What actually reached the peer, parsed from the raw bytes it consumed.
#[derive(Default)]
struct ObservedTail {
    raw_bytes: usize,
    frames: usize,
    text_bytes: usize,
    terminal: bool,
    partial: usize,
}

/// Consumes raw bytes to EOF, pacing each chunk when `pause` is non-zero, then
/// reports what arrived. A trailing partial frame is counted instead of parsed:
/// a bounded close can cut a frame mid-write.
async fn observe_tail(peer: &mut Peer, pause: Duration) -> ObservedTail {
    let mut raw = std::mem::take(&mut peer.pending);
    let mut buf = [0u8; 4096];
    loop {
        let read = tokio::time::timeout(EXIT_TIMEOUT, peer.out.read(&mut buf))
            .await
            .expect("timed out draining the tail")
            .expect("read stdout");
        if read == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..read]);
        if !pause.is_zero() {
            tokio::time::sleep(pause).await;
        }
    }
    let mut tail = ObservedTail {
        raw_bytes: raw.len(),
        ..Default::default()
    };
    let text = String::from_utf8_lossy(&raw);
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(value) => {
                tail.frames += 1;
                if value.get("id").and_then(Value::as_u64) == Some(99) {
                    tail.terminal = true;
                }
                if let Some(chunk) = value
                    .pointer("/params/update/content/text")
                    .and_then(Value::as_str)
                {
                    tail.text_bytes += chunk.len();
                }
            }
            Err(_) => tail.partial += 1,
        }
    }
    tail
}

/// Waits for the real process exit inside the frozen budget plus slack.
async fn wait_exit_bounded(child: &mut Child) -> Option<i32> {
    match tokio::time::timeout(CLOSE_GRACE + EXIT_SLACK, child.wait()).await {
        Ok(status) => status.expect("child status").code(),
        Err(_) => None,
    }
}

async fn stderr_text(child: &mut Child) -> String {
    let mut stderr = child.stderr.take().expect("piped stderr");
    let mut text = String::new();
    let _ = tokio::time::timeout(EXIT_TIMEOUT, stderr.read_to_string(&mut text)).await;
    text
}

/// A peer that never reads again must not keep the agent resident: the process
/// ends inside the close budget, the exit is a failure, and the unflushed tail is
/// declared rather than reported as a clean close.
#[tokio::test]
async fn a_permanently_stopped_reader_ends_inside_the_close_budget() {
    let (_home, workspace, mut child, mut peer) = spawn().await;
    let session_id = handshake(&mut peer, workspace.path()).await;
    peer.write_frame(&prompt_frame(&session_id, PROMPT_BYTES))
        .await;
    peer.probe_one_byte().await;
    drop(peer.stdin.take());

    let started = Instant::now();
    let code = wait_exit_bounded(&mut child).await;
    let elapsed = started.elapsed();
    let tail = observe_tail(&mut peer, Duration::ZERO).await;
    let stderr = stderr_text(&mut child).await;
    println!(
        "EXIT-BOUNDED exit={code:?} elapsed_ms={} raw_bytes={} frames={} text_bytes={} \
         terminal={} partial={} stderr={:?}",
        elapsed.as_millis(),
        tail.raw_bytes,
        tail.frames,
        tail.text_bytes,
        tail.terminal,
        tail.partial,
        stderr
    );
    assert!(
        code.is_some(),
        "a permanently stopped reader must not keep the agent resident"
    );
    assert!(
        elapsed <= CLOSE_GRACE + EXIT_SLACK,
        "the exit must land inside the close budget, took {elapsed:?}"
    );
    assert!(
        elapsed >= CLOSE_GRACE,
        "the budget must run before the close, exited after {elapsed:?}"
    );
    assert_ne!(
        code,
        Some(0),
        "an unflushed tail must not be reported as a clean close"
    );
    assert!(
        tail.raw_bytes > 0,
        "the budget must allow the drain to progress before it ends"
    );
    assert!(
        !tail.terminal,
        "the terminal response must not be delivered to a stopped reader"
    );
    assert!(
        stderr.contains(INCOMPLETE_MARKER),
        "the incomplete tail must be declared: {stderr}"
    );
}

/// Resuming after the budget is a different fact from resuming inside it: the
/// process is already gone and the tail is incomplete. Asserting both sides keeps
/// the budget from quietly becoming a promise of lossless delivery.
#[tokio::test]
async fn resuming_after_the_close_budget_finds_an_incomplete_tail() {
    let (_home, workspace, mut child, mut peer) = spawn().await;
    let session_id = handshake(&mut peer, workspace.path()).await;
    peer.write_frame(&prompt_frame(&session_id, PROMPT_BYTES))
        .await;
    peer.probe_one_byte().await;
    drop(peer.stdin.take());
    tokio::time::sleep(CLOSE_GRACE + Duration::from_secs(1)).await;

    let code = wait_exit_bounded(&mut child).await;
    let tail = observe_tail(&mut peer, Duration::ZERO).await;
    let stderr = stderr_text(&mut child).await;
    println!(
        "EXIT-LATE-RESUME exit={code:?} raw_bytes={} frames={} text_bytes={} terminal={} partial={}",
        tail.raw_bytes, tail.frames, tail.text_bytes, tail.terminal, tail.partial
    );
    assert_eq!(
        code,
        Some(1),
        "the budget must end the connection as a failure"
    );
    assert!(
        tail.raw_bytes > 0,
        "the budget must allow the drain to progress before it ends"
    );
    assert!(
        !tail.terminal,
        "the terminal response must not arrive after the budget"
    );
    assert!(
        tail.text_bytes < PROMPT_BYTES,
        "the tail must be incomplete, got {} bytes",
        tail.text_bytes
    );
    assert!(
        stderr.contains(INCOMPLETE_MARKER),
        "the incomplete tail must be declared: {stderr}"
    );
}

/// A consumer that progresses and then stalls past the budget is cut off and
/// declared incomplete. This is the frozen tradeoff: bounded exit and lossless
/// delivery cannot both hold for an arbitrarily slow peer, and earlier progress
/// must not restart the one-shot budget.
#[tokio::test]
async fn a_reader_that_stalls_past_the_close_budget_is_declared_incomplete() {
    let (_home, workspace, mut child, mut peer) = spawn().await;
    let session_id = handshake(&mut peer, workspace.path()).await;
    peer.write_frame(&prompt_frame(&session_id, PROMPT_BYTES))
        .await;
    peer.probe_one_byte().await;
    drop(peer.stdin.take());
    let started = Instant::now();

    // One real chunk of progress, then a stall longer than the budget. The
    // writer still has more than the pipe capacity left to write.
    let mut buf = [0u8; 4096];
    let read = tokio::time::timeout(EXIT_TIMEOUT, peer.out.read(&mut buf))
        .await
        .expect("timed out reading the first chunk")
        .expect("read stdout");
    assert!(read > 0, "the peer must consume a chunk before stalling");
    peer.pending.extend_from_slice(&buf[..read]);
    tokio::time::sleep(CLOSE_GRACE + Duration::from_secs(1)).await;

    let tail = observe_tail(&mut peer, Duration::ZERO).await;
    let elapsed = started.elapsed();
    let code = wait_exit_bounded(&mut child).await;
    let stderr = stderr_text(&mut child).await;
    println!(
        "EXIT-STALL-READER exit={code:?} elapsed_ms={} raw_bytes={} frames={} text_bytes={} \
         terminal={} partial={}",
        elapsed.as_millis(),
        tail.raw_bytes,
        tail.frames,
        tail.text_bytes,
        tail.terminal,
        tail.partial
    );
    assert_eq!(
        code,
        Some(1),
        "a consumer stalled past the budget must end as a failure"
    );
    assert!(
        elapsed >= CLOSE_GRACE,
        "progress must be allowed for the whole budget, took {elapsed:?}"
    );
    assert!(
        tail.raw_bytes > 0,
        "the budget must allow the drain to progress before it ends"
    );
    assert!(
        !tail.terminal,
        "the terminal response must not arrive after the budget"
    );
    assert!(
        tail.text_bytes < PROMPT_BYTES,
        "the tail must be incomplete, got {} bytes",
        tail.text_bytes
    );
    assert!(
        stderr.contains(INCOMPLETE_MARKER),
        "the incomplete tail must be declared: {stderr}"
    );
}

/// Spawns an ACP peer whose mock provider emits one tool-dispatch turn then a
/// final turn, with an explicit agent turn budget.
async fn spawn_toolturn(max_turns: u32) -> (tempfile::TempDir, tempfile::TempDir, Child, Peer) {
    let home = tempfile::tempdir().expect("temp home");
    let config_dir = home.path().join(".copilot-shell");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        format!(
            "[ai]\nactive_provider = \"mock\"\n\n[ai.providers.mock]\ntype = \"mock\"\nmodel = \"mock-toolturn\"\n\n[agent]\nmax_turns = {max_turns}\n"
        ),
    )
    .unwrap();
    let workspace = tempfile::tempdir().expect("temp workspace");
    let mut command: Command = common::cosh_core_command(home.path()).into();
    command
        .arg("--acp")
        .arg("--workspace")
        .arg(workspace.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().expect("spawn cosh-core --acp");
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let peer = Peer {
        stdin: Some(stdin),
        out: BufReader::new(stdout),
        pending: Vec::new(),
        next_id: 0,
    };
    (home, workspace, child, peer)
}

#[tokio::test]
async fn acp_turn_budget_stops_at_max_turns_and_reports_it() {
    // A one-turn budget exhausts on the tool-dispatch turn before the final
    // text turn, so the ACP terminal must report the turn budget, not end_turn.
    let (_home, workspace, mut child, mut peer) = spawn_toolturn(1).await;
    let session_id = handshake(&mut peer, workspace.path()).await;
    peer.write_frame(&prompt_frame(&session_id, 16)).await;
    let (_text, _frames, response) = drain_prompt(&mut peer).await;
    assert_eq!(
        response
            .pointer("/result/stopReason")
            .and_then(Value::as_str),
        Some("max_turn_requests"),
        "a spent turn budget must be reported as max_turn_requests: {response}"
    );
    drop(peer.stdin.take());
    let _ = wait_exit(&mut child).await;

    // With headroom the same provider reaches its final turn and ends normally.
    let (_home2, workspace2, mut child2, mut peer2) = spawn_toolturn(5).await;
    let session_id2 = handshake(&mut peer2, workspace2.path()).await;
    peer2.write_frame(&prompt_frame(&session_id2, 16)).await;
    let (_text2, _frames2, response2) = drain_prompt(&mut peer2).await;
    assert_eq!(
        response2
            .pointer("/result/stopReason")
            .and_then(Value::as_str),
        Some("end_turn"),
        "with budget headroom the turn must end normally: {response2}"
    );
    drop(peer2.stdin.take());
    let _ = wait_exit(&mut child2).await;
}
