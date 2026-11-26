//! Measurement harness for the Core ACP outbound path after the SDK accepts a frame.
//!
//! The agent side uses the production `agent(runtime, close)` builder and the same
//! `Lines` + `bounded_lines` wiring as `bounded_stdio`, with a duplex byte pipe
//! standing in for stdout/stdin and a counter placed after the sink flush. The
//! client is a raw JSON-RPC peer so it can stop reading on demand.
//!
//! Acceptance is gated on a checkable fact, never on a sleep: `drive_prompt`
//! awaits the `forward_events` task and only then calls `clear_turn`, so the
//! session turn slot going from occupied to empty proves the local 256-slot
//! channel was drained and every projected notification already returned from
//! `send_notification`. The window starts only after that transition, and only
//! `session/update` notifications are accounted as backlog; the terminal
//! response is sent after the transition and is reported separately.
//!
//! The accepted total equals the handed total only for a turn that ends
//! normally. A fatal capacity close discards whatever the SDK had not yet handed
//! to the sink, so for those runs the handed count is a lower bound and no
//! backlog decomposition is reported.
//!
//! This harness asserts measurement identities and losslessness only. Choosing
//! a numeric backlog bound is a separate design decision.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema::{
    ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest, TextContent,
};
use agent_client_protocol::{ConnectTo, Error, Lines};
use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use super::transport::{bounded_lines, CloseWatch, InFlight};
use super::{agent, AcpRuntime, CLOSE_GRACE, MAX_INBOUND_FRAME_BYTES, MAX_INFLIGHT_FRAMES};
use crate::config::CoreConfig;
use crate::provider::{
    ContentGenerator, GenerateConfig, GenerateEvent, GenerateStream, Message, ToolDeclaration,
};

const PIPE_BYTES: usize = 64 * 1024;
const WINDOW: Duration = Duration::from_millis(1500);
const SLOW_STEP: Duration = Duration::from_millis(100);
const DRAIN_BOUND: Duration = Duration::from_secs(120);
const TURN_BOUND: Duration = Duration::from_secs(60);
const UPDATE_METHOD: &str = "session/update";

/// Emits an exact number of equal-size text deltas, then ends the message.
struct FloodProvider {
    deltas: usize,
    payload: String,
    pace: Option<Duration>,
    start: Arc<Notify>,
    emitted: Arc<AtomicUsize>,
    finished: Arc<AtomicBool>,
}

#[async_trait]
impl ContentGenerator for FloodProvider {
    async fn generate(
        &self,
        _messages: &[Message],
        _tools: &[ToolDeclaration],
        _config: &GenerateConfig,
    ) -> Result<GenerateStream, String> {
        let total = self.deltas;
        let payload = self.payload.clone();
        let pace = self.pace;
        let start = Arc::clone(&self.start);
        let emitted = Arc::clone(&self.emitted);
        let finished = Arc::clone(&self.finished);
        Ok(Box::pin(futures::stream::unfold(0usize, move |index| {
            let payload = payload.clone();
            let start = Arc::clone(&start);
            let emitted = Arc::clone(&emitted);
            let finished = Arc::clone(&finished);
            async move {
                if index == 0 {
                    // The test releases this only after it observed turn=Some,
                    // so a fast turn cannot complete before the first sample.
                    start.notified().await;
                }
                if index < total {
                    if let Some(step) = pace {
                        tokio::time::sleep(step).await;
                    }
                    emitted.fetch_add(1, Ordering::SeqCst);
                    Some((GenerateEvent::TextDelta(payload), index + 1))
                } else if index == total {
                    // The kernel stops pulling at MessageEnd, so the flag must
                    // be set while yielding it, not on a later poll.
                    finished.store(true, Ordering::SeqCst);
                    Some((GenerateEvent::MessageEnd, index + 1))
                } else {
                    None
                }
            }
        })))
    }

    fn cancel(&self) {}
}

/// Lets one provider instance serve every session the factory creates.
struct SharedFlood(Arc<FloodProvider>);

#[async_trait]
impl ContentGenerator for SharedFlood {
    async fn generate(
        &self,
        messages: &[Message],
        tools: &[ToolDeclaration],
        config: &GenerateConfig,
    ) -> Result<GenerateStream, String> {
        self.0.generate(messages, tools, config).await
    }

    fn cancel(&self) {
        self.0.cancel();
    }
}

/// `handed` is counted before the write so it can never lag a client read;
/// `flushed` is counted after it, which makes the in-flight frame visible.
#[derive(Default, Clone, Copy)]
struct SinkCount {
    handed: Count,
    flushed: Count,
}

#[derive(Default, Clone, Copy)]
struct Count {
    frames: u64,
    bytes: u64,
    updates: u64,
    update_bytes: u64,
    responses: u64,
}

impl Count {
    fn account(&mut self, line: &str) {
        let bytes = line.len() as u64 + 1;
        self.frames += 1;
        self.bytes += bytes;
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return;
        };
        match value.get("method").and_then(serde_json::Value::as_str) {
            Some(UPDATE_METHOD) => {
                self.updates += 1;
                self.update_bytes += bytes;
            }
            Some(_) => {}
            None => self.responses += 1,
        }
    }

    fn add(&mut self, other: &Count) {
        self.frames += other.frames;
        self.bytes += other.bytes;
        self.updates += other.updates;
        self.update_bytes += other.update_bytes;
        self.responses += other.responses;
    }
}

/// Counts each line around the production outgoing sink, so `handed` is counted
/// before the write and `flushed` after it while the close budget and the
/// in-flight confirmation stay the product's own code.
fn counting_sink(
    writer: WriteHalf<DuplexStream>,
    count: Arc<Mutex<SinkCount>>,
    close: CloseWatch,
    inflight: InFlight,
) -> impl futures::Sink<String, Error = std::io::Error> + Send + 'static {
    let inner = Box::pin(super::transport::outgoing_sink(writer, close, inflight));
    futures::sink::unfold(
        (inner, count),
        |(mut inner, count), line: String| async move {
            let mut delta = Count::default();
            delta.account(&line);
            count.lock().expect("sink count").handed.add(&delta);
            futures::SinkExt::send(&mut inner, line).await?;
            count.lock().expect("sink count").flushed.add(&delta);
            Ok::<_, std::io::Error>((inner, count))
        },
    )
}

struct RawClient {
    lines: tokio::io::Lines<BufReader<ReadHalf<DuplexStream>>>,
    writer: WriteHalf<DuplexStream>,
    seen: Count,
    last: Vec<String>,
}

impl RawClient {
    fn new(peer: DuplexStream) -> Self {
        let (read, writer) = tokio::io::split(peer);
        Self {
            lines: BufReader::new(read).lines(),
            writer,
            seen: Count::default(),
            last: Vec::new(),
        }
    }

    async fn send(&mut self, id: u64, method: &str, params: serde_json::Value) {
        let line = serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params,
        });
        let mut text = line.to_string();
        text.push('\n');
        self.writer
            .write_all(text.as_bytes())
            .await
            .expect("client write");
        self.writer.flush().await.expect("client flush");
    }

    /// Reads one line, accounting it; `None` means the peer closed.
    async fn step(&mut self) -> Option<String> {
        let line = self.lines.next_line().await.expect("client read")?;
        self.seen.account(&line);
        self.last.push(line.clone());
        if self.last.len() > 6 {
            self.last.remove(0);
        }
        Some(line)
    }

    async fn response(&mut self, id: u64) -> Option<serde_json::Value> {
        loop {
            let line = self.step().await?;
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if value.get("id").and_then(serde_json::Value::as_u64) == Some(id) {
                return Some(value);
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Stopped,
    Slow,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Slow => "slow",
        }
    }
}

struct Measured {
    handed_at_window: Count,
    flushed_at_window: Count,
    read_at_window: Count,
    handed_total: Count,
    flushed_total: Count,
    read_total: Count,
    turn_occupied_then_cleared: bool,
    provider_emitted: usize,
    terminal: Option<serde_json::Value>,
    server_result: String,
    inflight_depth_at_window: usize,
    inflight_depth_total: usize,
}

impl Measured {
    fn completed(&self) -> bool {
        self.stop_reason() == "end_turn"
    }

    fn stop_reason(&self) -> String {
        match &self.terminal {
            Some(value) => value["result"]["stopReason"]
                .as_str()
                .or_else(|| value["error"]["message"].as_str())
                .unwrap_or("unknown")
                .to_string(),
            None => "no terminal response before EOF".to_string(),
        }
    }
}

async fn await_turn(runtime: &AcpRuntime, want_busy: bool) -> bool {
    let deadline = Instant::now() + TURN_BOUND;
    loop {
        {
            let guard = runtime.lock_session();
            let busy = guard.as_ref().is_some_and(|session| session.turn.is_some());
            if busy == want_busy {
                return true;
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Observes turn=Some, releases the gated provider, then observes turn=None.
/// `drive_prompt` clears the turn only after `forward_events` completed, so the
/// transition proves every projected notification returned from send_notification.
async fn await_forward_complete(runtime: &AcpRuntime, start: &Notify) -> bool {
    if !await_turn(runtime, true).await {
        return false;
    }
    start.notify_one();
    await_turn(runtime, false).await
}

async fn measure(
    deltas: usize,
    payload_bytes: usize,
    mode: Mode,
    pace: Option<Duration>,
) -> Measured {
    let finished = Arc::new(AtomicBool::new(false));
    let emitted = Arc::new(AtomicUsize::new(0));
    let start = Arc::new(Notify::new());
    let provider = Arc::new(FloodProvider {
        deltas,
        payload: "x".repeat(payload_bytes),
        pace,
        start: Arc::clone(&start),
        emitted: Arc::clone(&emitted),
        finished: Arc::clone(&finished),
    });
    let runtime = Arc::new(AcpRuntime::new(
        CoreConfig::default(),
        Arc::new(move |_| Box::new(SharedFlood(Arc::clone(&provider)))),
    ));
    let observer = Arc::clone(&runtime);

    let (agent_io, peer) = tokio::io::duplex(PIPE_BYTES);
    let (agent_read, agent_write) = tokio::io::split(agent_io);
    let written = Arc::new(Mutex::new(SinkCount::default()));
    let close = CloseWatch::new(CLOSE_GRACE);
    let inflight = observer.inflight.clone();
    let sink = counting_sink(
        agent_write,
        Arc::clone(&written),
        close.clone(),
        inflight.clone(),
    );
    let incoming = bounded_lines(BufReader::new(agent_read), MAX_INBOUND_FRAME_BYTES);
    let server: JoinHandle<Result<(), Error>> = tokio::spawn(async move {
        agent(runtime, close)
            .connect_to(Lines::new(Box::pin(sink), Box::pin(incoming)))
            .await
    });

    let mut client = RawClient::new(peer);
    client
        .send(
            1,
            "initialize",
            serde_json::to_value(InitializeRequest::new(ProtocolVersion::V1)).expect("initialize"),
        )
        .await;
    assert!(client.response(1).await.is_some(), "initialize response");
    client
        .send(
            2,
            "session/new",
            serde_json::to_value(NewSessionRequest::new(std::env::temp_dir()))
                .expect("new session"),
        )
        .await;
    let session = client.response(2).await.expect("session/new response");
    let session_id = session["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();
    client
        .send(
            3,
            "session/prompt",
            serde_json::to_value(PromptRequest::new(
                agent_client_protocol::v1::schema::SessionId::new(session_id),
                vec![ContentBlock::Text(TextContent::new("flood"))],
            ))
            .expect("prompt"),
        )
        .await;

    let turn_occupied_then_cleared = await_forward_complete(&observer, &start).await;
    if !turn_occupied_then_cleared {
        println!(
            "BACKLOG-DIAG turn slot never completed; provider_finished={} written={:?} read={:?} server_finished={}",
            finished.load(Ordering::SeqCst),
            written.lock().expect("sink count").handed.frames,
            client.seen.frames,
            server.is_finished()
        );
        for line in &client.last {
            println!("BACKLOG-DIAG last line: {line}");
        }
        panic!("the turn slot must be occupied and then cleared before the window");
    }

    let window_end = Instant::now() + WINDOW;
    match mode {
        Mode::Stopped => tokio::time::sleep(WINDOW).await,
        Mode::Slow => {
            while Instant::now() < window_end {
                tokio::time::sleep(SLOW_STEP).await;
                let _ = tokio::time::timeout(SLOW_STEP, client.step()).await;
            }
        }
    }
    let read_at_window = client.seen;
    let at_window = *written.lock().expect("sink count");
    let inflight_depth_at_window = inflight.depth();

    let terminal = tokio::time::timeout(DRAIN_BOUND, client.response(3))
        .await
        .expect("the drain must not exceed its bound");
    // The terminal response is the turn's last frame and `handed` is counted
    // before the write, so reading it proves every frame was already handed.
    // No EOF drain is needed to make the totals complete.
    let read_total = client.seen;
    let total = *written.lock().expect("sink count");
    drop(client);
    let server_result = match tokio::time::timeout(DRAIN_BOUND, server).await {
        Ok(joined) => format!(
            "{:?}",
            joined.map(|inner| inner.map_err(|error| error.to_string()))
        ),
        Err(_) => "server task did not finish inside the bound".to_string(),
    };
    let inflight_depth_total = inflight.depth();

    Measured {
        handed_at_window: at_window.handed,
        flushed_at_window: at_window.flushed,
        read_at_window,
        handed_total: total.handed,
        flushed_total: total.flushed,
        read_total,
        turn_occupied_then_cleared,
        provider_emitted: emitted.load(Ordering::SeqCst),
        terminal,
        server_result,
        inflight_depth_at_window,
        inflight_depth_total,
    }
}

fn report(tier: &str, deltas: usize, payload_bytes: usize, mode: Mode, m: &Measured) {
    let accepted_not_handed = m.handed_total.updates - m.handed_at_window.updates;
    let accepted_not_handed_bytes = m.handed_total.update_bytes - m.handed_at_window.update_bytes;
    let handed_not_read = m.handed_at_window.updates - m.read_at_window.updates;
    let handed_not_read_bytes = m.handed_at_window.update_bytes - m.read_at_window.update_bytes;
    println!(
        "BACKLOG tier={tier} mode={} deltas={deltas} payload_bytes={payload_bytes} pipe_bytes={PIPE_BYTES}",
        mode.label()
    );
    println!(
        "BACKLOG turn_occupied_then_cleared_before_window={} stop_reason={}",
        m.turn_occupied_then_cleared,
        m.stop_reason()
    );
    println!("BACKLOG server_result={}", m.server_result);
    println!("BACKLOG provider_emitted={}", m.provider_emitted);
    println!(
        "BACKLOG updates_total={} update_bytes_total={} responses_total={} frames_total={} bytes_total={}",
        m.handed_total.updates, m.handed_total.update_bytes, m.handed_total.responses,
        m.handed_total.frames, m.handed_total.bytes
    );
    println!(
        "BACKLOG window_handed_updates={} window_handed_update_bytes={} window_flushed_updates={} window_read_updates={} window_read_update_bytes={}",
        m.handed_at_window.updates, m.handed_at_window.update_bytes,
        m.flushed_at_window.updates, m.read_at_window.updates, m.read_at_window.update_bytes
    );
    println!(
        "BACKLOG window_handed_responses={} window_read_responses={}",
        m.handed_at_window.responses, m.read_at_window.responses
    );
    println!(
        "BACKLOG inflight_updates_at_window={}",
        m.handed_at_window.updates - m.flushed_at_window.updates
    );
    println!(
        "BACKLOG product_inflight_depth_at_window={} product_inflight_depth_total={} cap={MAX_INFLIGHT_FRAMES}",
        m.inflight_depth_at_window, m.inflight_depth_total
    );
    if !m.completed() {
        println!(
            "BACKLOG accepted_total=unknown_after_fatal_close handed_updates_is_a_lower_bound={}",
            m.handed_total.updates
        );
        return;
    }
    println!(
        "BACKLOG accepted_not_handed_updates={accepted_not_handed} accepted_not_handed_update_bytes={accepted_not_handed_bytes}"
    );
    println!(
        "BACKLOG handed_not_read_updates={handed_not_read} handed_not_read_update_bytes={handed_not_read_bytes}"
    );
    println!(
        "BACKLOG accepted_not_read_updates={} accepted_not_read_update_bytes={}",
        accepted_not_handed + handed_not_read,
        accepted_not_handed_bytes + handed_not_read_bytes
    );
    println!(
        "BACKLOG verdict=sdk_queue_held_notifications_beyond_the_pipe:{}",
        accepted_not_handed > 0
    );
}

fn assert_identities(m: &Measured) {
    assert!(
        m.turn_occupied_then_cleared,
        "acceptance must be proven by the gated turn slot transition, not by a sleep"
    );
    // Only a normally completed turn keeps the accepted total equal to the
    // handed total; a fatal close discards the un-handed SDK queue.
    assert!(
        m.completed(),
        "the backlog controls must end normally: {}",
        m.stop_reason()
    );
    // Dropping the peer can tear the sink down before the last post-flush
    // increment, so at most one frame may be in flight between the two counts.
    assert!(m.flushed_total.frames <= m.handed_total.frames);
    assert!(m.handed_total.frames - m.flushed_total.frames <= 1);
    assert_eq!(
        m.read_total.frames, m.handed_total.frames,
        "every handed frame must reach the client"
    );
    assert_eq!(
        m.read_total.bytes, m.handed_total.bytes,
        "every handed byte must reach the client"
    );
    assert_eq!(m.read_total.updates, m.handed_total.updates);
    assert!(m.read_at_window.updates <= m.handed_at_window.updates);
    assert!(m.flushed_at_window.updates <= m.handed_at_window.updates);
    assert!(m.handed_at_window.updates <= m.handed_total.updates);
    assert!(m.read_at_window.update_bytes <= m.handed_at_window.update_bytes);
}

/// Discriminates the EventSink overflow branch without touching product budgets.
/// Case A crosses the 1 MiB line cap in the aggregated text; case B keeps the
/// aggregate far below the cap while emitting more deltas. The product reports
/// one unified capacity error for every branch, so only the observed volumes,
/// emitted counts and handed counts are recorded here.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_overflow_branch_is_discriminated_by_aggregate_size() {
    for (label, deltas, payload, round) in [
        ("over-cap", 4200usize, 256usize, 1u8),
        ("over-cap", 4200, 256, 2),
        ("under-cap", 5000, 16, 1),
        ("under-cap", 5000, 16, 2),
    ] {
        let m = measure(deltas, payload, Mode::Stopped, PACE).await;
        println!(
            "BACKLOG-BRACKET case={label} round={round} deltas={deltas} payload_bytes={payload} aggregated_text_bytes={} provider_emitted={} updates_handed={} update_bytes_handed={} terminal={}",
            deltas * payload,
            m.provider_emitted,
            m.handed_total.updates,
            m.handed_total.update_bytes,
            m.stop_reason()
        );
        println!(
            "BACKLOG-BRACKET accepted_total_equals_handed={} server_result={}",
            m.completed(),
            m.server_result
        );
    }
}

const PACE: Option<Duration> = Some(Duration::from_millis(1));

/// A burst producer outruns the local 256-slot channel, so the existing budget
/// ends the turn. Recorded separately from the paced regime below.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_burst_producer_hits_the_local_event_budget() {
    let m = measure(500, 256, Mode::Stopped, None).await;
    assert!(m.turn_occupied_then_cleared);
    report("burst", 500, 256, Mode::Stopped, &m);
}

/// Two output tiers against a client that stops reading after the handshake.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stopped_reader_leaves_accepted_notifications_unwritten() {
    for (tier, deltas) in [("small", 500usize), ("large", 4000usize)] {
        let m = measure(deltas, 64, Mode::Stopped, PACE).await;
        report(tier, deltas, 64, Mode::Stopped, &m);
        assert_identities(&m);
    }
}

/// The same two tiers against a client that reads one frame per slow step.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slowly_reading_client_leaves_accepted_notifications_unwritten() {
    for (tier, deltas) in [("small", 500usize), ("large", 4000usize)] {
        let m = measure(deltas, 64, Mode::Slow, PACE).await;
        report(tier, deltas, 64, Mode::Slow, &m);
        assert_identities(&m);
    }
}

/// A stopped reader with a flood above the in-flight bound must end through the
/// existing capacity path instead of growing the accepted-not-handed queue, and
/// the product gauge must stay inside the frozen bound. The paced producer stays
/// below the local channel budget on its own, so the trip is caused by the wire
/// bound rather than by a burst.
///
/// Control headroom is measured, not asserted: the fatal close discards the
/// queued terminal error, so the peer sees EOF without a stop reason. The
/// same-host A/B in section 57 shows the local-overflow path already behaved
/// this way before the bound existed, so it is registered as A35 rather than
/// claimed here.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stopped_reader_above_the_inflight_bound_ends_through_the_capacity_path() {
    let deltas = 8000usize;
    let m = measure(deltas, 64, Mode::Stopped, PACE).await;
    report("inflight-bound", deltas, 64, Mode::Stopped, &m);
    println!(
        "BACKLOG-BOUND terminal_delivered={} peer_updates={} emitted={} depth_at_window={} depth_total={} cap={MAX_INFLIGHT_FRAMES}",
        m.terminal.is_some(),
        m.read_total.updates,
        m.provider_emitted,
        m.inflight_depth_at_window,
        m.inflight_depth_total
    );
    assert!(m.turn_occupied_then_cleared);
    assert!(
        m.provider_emitted > MAX_INFLIGHT_FRAMES,
        "the flood must exceed the in-flight bound, emitted {}",
        m.provider_emitted
    );
    assert!(
        !m.completed(),
        "a stopped reader above the bound must not complete: {}",
        m.stop_reason()
    );
    assert!(
        m.server_result.contains("ACP event budget"),
        "the existing unified capacity error must be reused: {}",
        m.server_result
    );
    assert!(
        m.read_total.updates < m.provider_emitted as u64,
        "the peer must receive fewer updates than were emitted, read {} of {}",
        m.read_total.updates,
        m.provider_emitted
    );
    assert!(
        m.server_result.contains("Err"),
        "the connection must fail through the existing path: {}",
        m.server_result
    );
    assert!(
        m.inflight_depth_at_window == MAX_INFLIGHT_FRAMES,
        "the bound must actually be reached at the window, got {}",
        m.inflight_depth_at_window
    );
    assert!(
        m.inflight_depth_total <= MAX_INFLIGHT_FRAMES,
        "the product gauge must stay inside the bound at the end, got {}",
        m.inflight_depth_total
    );
}
