//! FFI interface for AgentSight — C API for other languages
//!
//! Implements the **eventfd + read** model described in `docs/design-docs/c-ffi-api.md`:
//! AgentSight runs a background pipeline thread; completed events are pushed
//! into an `mpsc` channel and the caller is notified via `eventfd`.
//! The caller consumes events by calling `agentsight_read()` with callbacks.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::analyzer::HttpRecord;
use crate::config::AgentsightConfig;
use crate::genai::semantic::LLMCall;
use crate::unified::AgentSight;
use agentsight_enforcement_protocol::{
    EnforcementStateEvent, EventIdentity, SecurityEvent, SecurityEventKind,
};
use uuid::Uuid;

// ===========================================================================
// Internal FFI event types (shared with unified.rs via crate::ffi)
// ===========================================================================

/// Internal event for FFI communication between pipeline and consumer.
///
/// Mutually exclusive: an HTTP exchange either becomes `Llm` (if recognised
/// as an LLM API call) or `Https` (otherwise).  See §5 in c-ffi-api.md.
pub(crate) enum FfiEvent {
    /// Raw HTTP exchange, plus the agent name resolved by the pipeline. The name
    /// is passed in because it comes from `AgentSight`'s pid → agent_name cache,
    /// which this layer cannot reach; resolving it here would miss processes that
    /// have already exited (the cache outlives them).
    Https(HttpRecord, Option<String>),
    Llm(LLMCall),
    Security(SecurityEvent),
}

/// Commands sent from the FFI caller thread to the background pipeline thread.
enum ProbeCommand {
    AddCgroup(u64),
    RemoveCgroup(u64),
}

/// Wraps an `mpsc::SyncSender<FfiEvent>` together with the `eventfd` descriptor
/// so that a single `.send()` call both enqueues the event and wakes up the
/// consumer's epoll/select loop.
///
/// Uses a bounded channel to prevent unbounded memory growth when the FFI
/// consumer does not call `agentsight_read()` frequently enough.
#[derive(Clone)]
pub(crate) struct FfiEventSender {
    tx: mpsc::SyncSender<FfiEvent>,
    eventfd: i32,
    enable_raw_https: bool,
    security_drops: Arc<SecurityDropState>,
}

#[derive(Default)]
struct SecurityDropState {
    count: AtomicU64,
    last_identity: Mutex<Option<EventIdentity>>,
}

impl SecurityDropState {
    fn record(&self, event: &SecurityEvent) {
        let mut identity = self
            .last_identity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *identity = Some(event.identity.clone());
        self.count.fetch_add(1, Ordering::Release);
    }

    fn take_event(&self) -> Option<SecurityEvent> {
        let dropped_events = self.count.swap(0, Ordering::AcqRel);
        if dropped_events == 0 {
            return None;
        }
        let identity = self
            .last_identity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64;
        Some(SecurityEvent {
            event_id: Uuid::new_v4(),
            occurred_at_ns: now,
            observed_at_ns: now,
            identity,
            kind: SecurityEventKind::EnforcementState(EnforcementStateEvent {
                policy_id: None,
                policy_revision: None,
                code: "ffi_queue_evidence_loss".into(),
                ready: true,
                message: "normalized security events were dropped because the FFI consumer queue was full".into(),
                dropped_events: Some(dropped_events),
            }),
        })
    }
}

impl FfiEventSender {
    pub fn send(&self, event: FfiEvent) {
        // Bounded channel: if the consumer cannot keep up, drop the newest event
        // rather than blocking the background pipeline or growing memory without
        // limit.  This matches the `DropNewest` policy used for probe events.
        match self.tx.try_send(event) {
            Ok(()) => {
                // Write 1 to the eventfd counter to wake up the consumer.
                let val: u64 = 1;
                unsafe {
                    libc::write(self.eventfd, &val as *const u64 as *const c_void, 8);
                }
            }
            Err(mpsc::TrySendError::Full(dropped)) => {
                if let FfiEvent::Security(event) = &dropped {
                    self.security_drops.record(event);
                }
                log::warn!(
                    "FFI event channel full; dropping event because consumer is not reading fast enough"
                );
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                // Receiver gone; nothing to do.
            }
        }
    }

    /// Enqueue a raw HTTP exchange, resolving its agent name only if the consumer
    /// actually asked for raw HTTPS.
    ///
    /// `agent_name` is a closure rather than a value because resolving it costs two
    /// `/proc` reads plus rule matching on a cache miss — and non-agent processes, which
    /// is exactly what produces non-LLM traffic, always miss. Raw HTTPS is off by
    /// default, so taking a resolved value here would burn that per event only to drop
    /// it one line later. Keeping the gate ahead of the closure also keeps it in one
    /// place instead of duplicating the condition at the call site.
    pub fn send_https(&self, record: &HttpRecord, agent_name: impl FnOnce() -> Option<String>) {
        if !self.enable_raw_https {
            return;
        }
        self.send(FfiEvent::Https(record.clone(), agent_name()));
    }

    /// Enqueue an LLM call, lowercasing `agent_name` on the way in.
    ///
    /// The FFI already takes a clone here, so normalizing it costs nothing and keeps
    /// the pipeline's own copy (dashboard, SQLite, `/api/agent-names`) at its original
    /// config casing. Normalizing at this boundary rather than in each builder is what
    /// makes the `agentsight_read_v2` envelope self-consistent: its `Llm` arm
    /// serializes `LLMCall` verbatim, so a `Hermes` here would not group with the
    /// `hermes` the raw-HTTPS arm reports for the very same process.
    pub fn send_llm(&self, call: &LLMCall) {
        let mut call = call.clone();
        if let Some(name) = call.agent_name.as_mut() {
            *name = name.to_lowercase();
        }
        self.send(FfiEvent::Llm(call));
    }
}

// ===========================================================================
// Thread-local last-error storage
// ===========================================================================

thread_local! {
    static LAST_ERROR: std::cell::RefCell<Option<CString>> = const { std::cell::RefCell::new(None) };
}

fn set_last_error(msg: &str) {
    LAST_ERROR.with(|e| {
        *e.borrow_mut() = CString::new(msg.replace('\0', "")).ok();
    });
}

fn ffi_boundary(name: &str, operation: impl FnOnce() -> c_int) -> c_int {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)) {
        Ok(result) => result,
        Err(_) => {
            let message = format!("{name} panicked");
            set_last_error(&message);
            log::error!("{message}");
            -1
        }
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

/// Create a `CString` from a Rust `&str`, replacing interior NUL bytes.
fn safe_cstring(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap()
}

/// Copy a Rust string into a fixed-size `[c_char; N]` buffer (NUL-terminated).
///
/// At most `N - 1` bytes are copied; truncation happens at a UTF-8 char
/// boundary so the C side never sees invalid UTF-8.
fn copy_to_fixed_buf<const N: usize>(s: &str) -> [c_char; N] {
    let mut buf = [0 as c_char; N];
    let mut end = s.len().min(N - 1);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    for (i, &b) in s.as_bytes()[..end].iter().enumerate() {
        buf[i] = b as c_char;
    }
    buf
}

/// Copy a Rust string into a fixed-size `[c_char; 16]` buffer (NUL-terminated).
fn copy_process_name(name: &str) -> [c_char; 16] {
    copy_to_fixed_buf(name)
}

/// Drain the eventfd counter so that epoll won't re-trigger.
fn drain_eventfd(fd: i32) {
    let mut buf: u64 = 0;
    unsafe {
        libc::read(fd, &mut buf as *mut u64 as *mut c_void, 8);
    }
}

// ===========================================================================
// C-compatible data structures  (§1 of c-ffi-api.md)
// ===========================================================================

/// HTTP layer data — non-LLM HTTPS traffic produces this structure.
#[repr(C)]
pub struct AgentsightHttpsData {
    pub pid: i32,
    pub process_name: [c_char; 16],
    pub timestamp_ns: u64,
    pub duration_ns: u64,
    pub method: *const c_char,
    pub path: *const c_char,
    pub status_code: u16,
    pub is_sse: u8,
    pub request_headers: *const c_char,
    pub request_headers_len: u32,
    pub request_body: *const c_char,
    pub request_body_len: u32,
    pub response_headers: *const c_char,
    pub response_headers_len: u32,
    pub response_body: *const c_char,
    pub response_body_len: u32,
    /// Space-joined process command line (argv), truncated to 127 bytes.
    /// Empty string when the process has already exited.
    ///
    /// This field and the two below were appended to the tail so a consumer
    /// compiled against the older, shorter layout keeps working unchanged.
    pub cmdline: [c_char; 128],
    pub agent_name: *const c_char,
    pub container_id: *const c_char,
}

/// LLM semantic layer data — only when the HTTP traffic is recognised as
/// an LLM API call.
#[repr(C)]
pub struct AgentsightLLMData {
    pub response_id: *const c_char,
    pub conversation_id: *const c_char,
    pub session_id: *const c_char,
    pub pid: i32,
    pub process_name: [c_char; 16],
    /// Space-joined process command line (argv), truncated to 127 bytes.
    /// Empty string when the process has already exited.
    pub cmdline: [c_char; 128],
    pub agent_name: *const c_char,
    pub container_id: *const c_char,
    pub timestamp_ns: u64,
    pub duration_ns: u64,
    pub request_url: *const c_char,
    pub provider: *const c_char,
    pub model: *const c_char,
    pub status_code: u16,
    pub is_sse: u8,
    pub finish_reason: *const c_char,
    pub llm_usage: bool,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
    pub cache_creation_input_tokens: u32,
    pub cache_read_input_tokens: u32,
    pub request_messages: *const c_char,
    pub request_messages_len: u32,
    pub response_messages: *const c_char,
    pub response_messages_len: u32,
    pub tools: *const c_char,
    pub tools_len: u32,
    /// Incremental (latest-round) request messages: the same per-round
    /// increment stored in SQLite (`genai_events.input_messages`). System
    /// messages are dropped and everything from the last `user` message onward
    /// is kept (inclusive). JSON array of InputMessage.
    pub input_message_delta: *const c_char,
    pub input_message_delta_len: u32,
}

/// Stable discriminator for the versioned generic event envelope.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentsightEventType {
    Https = 1,
    Llm = 2,
    Security = 3,
}

/// Generic event envelope. The payload pointer is valid only during callback.
#[repr(C)]
pub struct AgentsightEvent {
    pub event_type: AgentsightEventType,
    pub schema_version: u16,
    pub timestamp_ns: u64,
    pub payload_json: *const c_char,
    pub payload_json_len: u32,
}

// ===========================================================================
// Opaque handles
// ===========================================================================

/// Configuration handle (created → configured → passed to `agentsight_new`).
/// cbindgen:no-export
pub type AgentsightConfigHandle = AgentsightConfig;

/// Main runtime handle.
/// cbindgen:no-export
pub struct AgentsightHandle {
    rx: mpsc::Receiver<FfiEvent>,
    /// Sender kept alive so the background thread's sends don't fail
    /// after start; taken (moved) into the thread in `agentsight_start`.
    tx: Option<mpsc::SyncSender<FfiEvent>>,
    eventfd: i32,
    running: Arc<AtomicBool>,
    security_drops: Arc<SecurityDropState>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Optional normalized security-event subscriber sharing the same FFI queue.
    security_thread: Option<std::thread::JoinHandle<()>>,
    /// Config stored until `agentsight_start()` moves it into the thread.
    config: Option<AgentsightConfig>,
    /// Channel for runtime probe control commands (e.g. dynamic cgroup filter updates).
    /// Created in `agentsight_start()`; the receiver end is moved into the background thread.
    probe_cmd_tx: Option<mpsc::Sender<ProbeCommand>>,
}

// ===========================================================================
// Callback type aliases
// ===========================================================================

type HttpsCallbackFn = Option<unsafe extern "C" fn(*const AgentsightHttpsData, *mut c_void)>;
type LlmCallbackFn = Option<unsafe extern "C" fn(*const AgentsightLLMData, *mut c_void)>;
type EventCallbackFn = Option<unsafe extern "C" fn(*const AgentsightEvent, *mut c_void)>;

/// Flag for `agentsight_read()`: block until at least one event is available.
pub const AGENTSIGHT_READ_BLOCK: c_int = 1;

// ===========================================================================
// Temporary data holders (keep C strings and byte buffers alive during callbacks)
// ===========================================================================

struct HttpsDataHolder {
    c_data: AgentsightHttpsData,
    _method: CString,
    _path: CString,
    _req_headers: Vec<u8>,
    _req_body: Option<Vec<u8>>,
    _resp_headers: Vec<u8>,
    _resp_body: Option<Vec<u8>>,
    _agent_name: Option<CString>,
    _container_id: Option<CString>,
}

struct LlmDataHolder {
    c_data: AgentsightLLMData,
    _response_id: Option<CString>,
    _conversation_id: Option<CString>,
    _session_id: Option<CString>,
    _agent_name: Option<CString>,
    _container_id: Option<CString>,
    _request_url: CString,
    _provider: CString,
    _model: CString,
    _finish_reason: Option<CString>,
    _req_messages: CString,
    _resp_messages: CString,
    _tools: CString,
    _input_message_delta: CString,
}

struct EventDataHolder {
    c_data: AgentsightEvent,
    _payload_json: Vec<u8>,
}

/// Process attribution for a raw HTTP exchange, resolved from the pid.
///
/// Both consumer-facing shapes report these — the typed `AgentsightHttpsData` and
/// the generic `AgentsightEvent` JSON envelope — so the resolution lives here once
/// rather than being reimplemented per shape.
struct HttpsProcessMeta {
    /// Space-joined argv; empty once the process has exited.
    cmdline: String,
    container_id: Option<String>,
    /// Lowercased at the FFI boundary so C consumers observe a consistent value
    /// regardless of config-file casing (same as `build_llm_data`).
    agent_name: Option<String>,
}

fn resolve_https_process_meta(pid: u32, agent_name: Option<&str>) -> HttpsProcessMeta {
    HttpsProcessMeta {
        cmdline: crate::discovery::scanner::read_cmdline(pid).join(" "),
        container_id: crate::container::extract_container_id_cached(pid),
        agent_name: agent_name.map(str::to_lowercase),
    }
}

/// JSON payload for `AgentsightEventType::Https`: the `HttpRecord` fields plus the
/// same process attribution the typed path carries. Without this wrapper the generic
/// envelope would be strictly poorer than the typed one, because `HttpRecord` itself
/// has no agent_name / cmdline / container_id field — whereas the `Llm` arm gets them
/// for free from `LLMCall`. Absent values are omitted rather than serialized as null.
#[derive(serde::Serialize)]
struct HttpsEventPayload<'a> {
    #[serde(flatten)]
    record: &'a HttpRecord,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cmdline: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    container_id: Option<&'a str>,
}

fn build_event_data(event: &FfiEvent) -> Result<EventDataHolder, serde_json::Error> {
    let (event_type, timestamp_ns, payload_json) = match event {
        FfiEvent::Https(record, agent_name) => {
            // Re-resolved rather than shared with build_https_data: the two only ever
            // run for the same event while a consumer is mid-migration and has both
            // the generic and the typed callback registered. read_cmdline is one small
            // /proc read and container id lookups are cached, so paying twice in that
            // transitional window is cheaper than threading the meta through both
            // dispatch paths.
            let meta = resolve_https_process_meta(record.pid, agent_name.as_deref());
            let payload = HttpsEventPayload {
                record,
                agent_name: meta.agent_name.as_deref(),
                cmdline: Some(meta.cmdline.as_str()).filter(|s| !s.is_empty()),
                container_id: meta.container_id.as_deref(),
            };
            (
                AgentsightEventType::Https,
                record.timestamp_ns,
                serde_json::to_vec(&payload)?,
            )
        }
        FfiEvent::Llm(call) => (
            AgentsightEventType::Llm,
            call.start_timestamp_ns,
            serde_json::to_vec(call)?,
        ),
        FfiEvent::Security(event) => (
            AgentsightEventType::Security,
            event.occurred_at_ns,
            serde_json::to_vec(event)?,
        ),
    };
    let payload_json_len = payload_json.len().min(u32::MAX as usize) as u32;
    let c_data = AgentsightEvent {
        event_type,
        schema_version: 1,
        timestamp_ns,
        payload_json: payload_json.as_ptr().cast(),
        payload_json_len,
    };
    Ok(EventDataHolder {
        c_data,
        _payload_json: payload_json,
    })
}

fn build_https_data(record: &HttpRecord, agent_name: Option<&str>) -> HttpsDataHolder {
    let method = safe_cstring(&record.method);
    let path = safe_cstring(&record.path);
    let req_headers = record.request_headers.as_bytes().to_vec();
    let req_body = record.request_body.as_ref().map(|b| b.as_bytes().to_vec());
    let resp_headers = record.response_headers.as_bytes().to_vec();
    let resp_body = record.response_body.as_ref().map(|b| b.as_bytes().to_vec());

    // Process metadata, resolved the same way as the LLM path (`build_llm_data`) so
    // both event kinds report identical values for the same pid, and shared with the
    // generic JSON envelope via `resolve_https_process_meta`.
    let meta = resolve_https_process_meta(record.pid, agent_name);
    let agent_name = meta.agent_name.as_deref().map(safe_cstring);
    let container_id = meta.container_id.as_deref().map(safe_cstring);
    let cmdline = copy_to_fixed_buf::<128>(&meta.cmdline);

    let c_data = AgentsightHttpsData {
        pid: record.pid as i32,
        // Process name = the *process* comm (`<procfs root>/<pid>/comm`), not
        // the SSL event's per-event thread comm (which may be a worker-thread
        // name such as "HTTP client"). Falls back to the event comm when the
        // entry is gone.
        process_name: copy_process_name(
            &crate::discovery::scanner::read_comm(record.pid)
                .unwrap_or_else(|| record.comm.clone()),
        ),
        timestamp_ns: record.timestamp_ns,
        duration_ns: record.duration_ns,
        method: method.as_ptr(),
        path: path.as_ptr(),
        status_code: record.status_code,
        is_sse: record.is_sse as u8,
        request_headers: req_headers.as_ptr().cast(),
        request_headers_len: req_headers.len() as u32,
        request_body: req_body.as_ref().map_or(ptr::null(), |s| s.as_ptr().cast()),
        request_body_len: req_body.as_ref().map_or(0, |s| s.len() as u32),
        response_headers: resp_headers.as_ptr().cast(),
        response_headers_len: resp_headers.len() as u32,
        response_body: resp_body
            .as_ref()
            .map_or(ptr::null(), |s| s.as_ptr().cast()),
        response_body_len: resp_body.as_ref().map_or(0, |s| s.len() as u32),
        cmdline,
        agent_name: agent_name.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
        container_id: container_id.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
    };

    HttpsDataHolder {
        c_data,
        _method: method,
        _path: path,
        _req_headers: req_headers,
        _req_body: req_body,
        _resp_headers: resp_headers,
        _resp_body: resp_body,
        _agent_name: agent_name,
        _container_id: container_id,
    }
}

fn build_llm_data(call: &LLMCall) -> LlmDataHolder {
    let response_id = call.metadata.get("response_id").map(|s| safe_cstring(s));
    let conversation_id = call
        .metadata
        .get("conversation_id")
        .map(|s| safe_cstring(s));
    let session_id = call.metadata.get("session_id").map(|s| safe_cstring(s));
    // Normalize agent_name to lowercase at the FFI boundary so C consumers
    // observe a consistent value regardless of config-file casing.
    let agent_name = call
        .agent_name
        .as_ref()
        .map(|s| safe_cstring(&s.to_lowercase()));
    // Space-joined argv from <procfs root>/<pid>/cmdline; empty when the process
    // has already exited (read_cmdline returns an empty vec on error).
    let cmdline =
        copy_to_fixed_buf::<128>(&crate::discovery::scanner::read_cmdline(call.pid).join(" "));
    let container_id =
        crate::container::extract_container_id_cached(call.pid as u32).map(|s| safe_cstring(&s));

    // Construct request_url from metadata
    let server_addr = call
        .metadata
        .get("server.address")
        .cloned()
        .unwrap_or_default();
    let server_port = call
        .metadata
        .get("server.port")
        .cloned()
        .unwrap_or_default();
    let path = call.metadata.get("path").cloned().unwrap_or_default();
    let url = if server_port.is_empty() {
        format!("https://{server_addr}{path}")
    } else {
        format!("https://{server_addr}:{server_port}{path}")
    };
    let request_url = safe_cstring(&url);

    let provider = safe_cstring(&call.provider);
    let model = safe_cstring(&call.model);

    let status_code: u16 = call
        .metadata
        .get("status_code")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let is_sse: bool = call.metadata.get("is_sse").is_some_and(|s| s == "true");

    let finish_reason = call
        .response
        .messages
        .first()
        .and_then(|m| m.finish_reason.as_ref())
        .map(|s| safe_cstring(s));

    let (llm_usage, input_tokens, output_tokens, total_tokens, cache_creation, cache_read) =
        match &call.token_usage {
            Some(u) => (
                true,
                u.input_tokens,
                u.output_tokens,
                u.total_tokens,
                u.cache_creation_input_tokens.unwrap_or(0),
                u.cache_read_input_tokens.unwrap_or(0),
            ),
            None => (false, 0, 0, 0, 0, 0),
        };

    let req_messages_json = serde_json::to_string(&call.request.messages).unwrap_or_default();
    let resp_messages_json = serde_json::to_string(&call.response.messages).unwrap_or_default();
    let req_messages = safe_cstring(&req_messages_json);
    let resp_messages = safe_cstring(&resp_messages_json);

    // Incremental (latest-round) input messages: the same per-round increment
    // stored in SQLite (`genai_events.input_messages`). Drops system messages
    // and keeps everything from the last `user` message onward.
    let input_delta = crate::genai::semantic::latest_round_input_messages(&call.request.messages);
    let input_message_delta_json = serde_json::to_string(&input_delta).unwrap_or_default();
    let input_message_delta = safe_cstring(&input_message_delta_json);

    let tools_json = call
        .request
        .tools
        .as_ref()
        .map(|tools| serde_json::to_string(tools).unwrap_or_default())
        .unwrap_or_else(|| "[]".to_string());
    let tools = safe_cstring(&tools_json);

    let c_data = AgentsightLLMData {
        response_id: response_id.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
        conversation_id: conversation_id.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
        session_id: session_id.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
        pid: call.pid,
        process_name: copy_process_name(&call.process_name),
        cmdline,
        agent_name: agent_name.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
        container_id: container_id.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
        timestamp_ns: call.start_timestamp_ns,
        duration_ns: call.duration_ns,
        request_url: request_url.as_ptr(),
        provider: provider.as_ptr(),
        model: model.as_ptr(),
        status_code,
        is_sse: is_sse as u8,
        finish_reason: finish_reason.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
        llm_usage,
        input_tokens,
        output_tokens,
        total_tokens,
        cache_creation_input_tokens: cache_creation,
        cache_read_input_tokens: cache_read,
        request_messages: req_messages.as_ptr(),
        request_messages_len: req_messages_json.len() as u32,
        response_messages: resp_messages.as_ptr(),
        response_messages_len: resp_messages_json.len() as u32,
        tools: tools.as_ptr(),
        tools_len: tools_json.len() as u32,
        input_message_delta: input_message_delta.as_ptr(),
        input_message_delta_len: input_message_delta_json.len() as u32,
    };

    LlmDataHolder {
        c_data,
        _response_id: response_id,
        _conversation_id: conversation_id,
        _session_id: session_id,
        _agent_name: agent_name,
        _container_id: container_id,
        _request_url: request_url,
        _provider: provider,
        _model: model,
        _finish_reason: finish_reason,
        _req_messages: req_messages,
        _resp_messages: resp_messages,
        _tools: tools,
        _input_message_delta: input_message_delta,
    }
}

/// Dispatch a single FFI event to the appropriate callback.
///
/// # Safety
/// Caller must ensure the callback function pointers and user-data pointers
/// are valid for the duration of the call.
unsafe fn dispatch_event(
    event: FfiEvent,
    http_cb: HttpsCallbackFn,
    http_ud: *mut c_void,
    llm_cb: LlmCallbackFn,
    llm_ud: *mut c_void,
) {
    match event {
        FfiEvent::Https(record, agent_name) => {
            if let Some(cb) = http_cb {
                let holder = build_https_data(&record, agent_name.as_deref());
                unsafe { cb(&holder.c_data, http_ud) };
            }
        }
        FfiEvent::Llm(call) => {
            if let Some(cb) = llm_cb {
                let holder = build_llm_data(&call);
                unsafe { cb(&holder.c_data, llm_ud) };
            }
        }
        FfiEvent::Security(_) => {}
    }
}

/// Dispatches legacy typed callbacks and the versioned generic callback from
/// the same queue item. This lets existing consumers keep their LLM mapping
/// while incrementally adopting generic event types.
unsafe fn dispatch_event_v2(
    event: FfiEvent,
    http_cb: HttpsCallbackFn,
    http_ud: *mut c_void,
    llm_cb: LlmCallbackFn,
    llm_ud: *mut c_void,
    event_cb: EventCallbackFn,
    event_ud: *mut c_void,
) {
    if let Some(cb) = event_cb {
        match build_event_data(&event) {
            Ok(holder) => unsafe { cb(&holder.c_data, event_ud) },
            Err(error) => log::warn!("failed to serialize FFI event envelope: {error}"),
        }
    }
    unsafe { dispatch_event(event, http_cb, http_ud, llm_cb, llm_ud) };
}

// ===========================================================================
// C API functions  (§2 of c-ffi-api.md)
// ===========================================================================

// ---- Error handling ----

/// Return the last error description, or NULL if no error.
/// The pointer is valid until the next API call on the same thread.
#[unsafe(no_mangle)]
pub extern "C" fn agentsight_last_error() -> *const c_char {
    LAST_ERROR.with(|e| e.borrow().as_ref().map_or(ptr::null(), |s| s.as_ptr()))
}

// ---- Configuration ----

/// Create a new configuration with default values.
#[unsafe(no_mangle)]
pub extern "C" fn agentsight_config_new() -> *mut AgentsightConfigHandle {
    Box::into_raw(Box::new(AgentsightConfig::default()))
}

/// Set the verbose flag (0 = off, 1 = on).
///
/// # Safety
///
/// `cfg` / `h` must be a valid pointer returned by the corresponding
/// `_new()` function, or null (which is handled gracefully).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn agentsight_config_set_verbose(
    cfg: *mut AgentsightConfigHandle,
    verbose: c_int,
) {
    if !cfg.is_null() {
        unsafe { (*cfg).verbose = verbose != 0 };
    }
}

/// Set the log file path (NULL → stderr).
///
/// # Safety
///
/// `cfg` / `h` must be a valid pointer returned by the corresponding
/// `_new()` function, or null (which is handled gracefully).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn agentsight_config_set_log_path(
    cfg: *mut AgentsightConfigHandle,
    path: *const c_char,
) {
    if cfg.is_null() {
        return;
    }
    unsafe {
        (*cfg).log_path = if path.is_null() {
            None
        } else {
            Some(CStr::from_ptr(path).to_string_lossy().to_string())
        };
    }
}

/// Enable or disable raw HTTPS fallback events in FFI mode (0 = off, non-zero = on).
///
/// # Safety
///
/// `cfg` must be a valid pointer returned by `agentsight_config_new()`, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn agentsight_config_set_enable_raw_https(
    cfg: *mut AgentsightConfigHandle,
    enabled: c_int,
) {
    if !cfg.is_null() {
        unsafe { (*cfg).ffi_enable_raw_https = enabled != 0 };
    }
}

/// Enable or disable normalized security audit events in FFI mode.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn agentsight_config_set_enable_security_audit(
    cfg: *mut AgentsightConfigHandle,
    enabled: c_int,
) {
    if !cfg.is_null() {
        unsafe { (*cfg).ffi_enable_security_audit = enabled != 0 };
    }
}

/// Override the local enforcer socket used by the security audit subscriber.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn agentsight_config_set_enforcer_socket(
    cfg: *mut AgentsightConfigHandle,
    path: *const c_char,
) {
    if cfg.is_null() || path.is_null() {
        return;
    }
    let value = unsafe { CStr::from_ptr(path) }.to_string_lossy();
    if !value.is_empty() {
        unsafe { (*cfg).ffi_enforcer_socket = std::path::PathBuf::from(value.as_ref()) };
    }
}

/// Override the procfs mount point that pid lookups resolve through.
///
/// Pass a host procfs bind-mounted into the caller's container (e.g.
/// `"/logtail_host/proc"`) to observe processes in other pid namespaces without
/// joining them — the caller then needs no `hostPID`. Defaults to `/proc`.
///
/// The path is the caller's deployment detail, which is why it is passed in
/// rather than probed: a standalone install must keep reading its own `/proc`.
/// Must be set before `agentsight_start`, since probes bake a pid-namespace flag
/// derived from this root into their BPF objects at load time.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn agentsight_config_set_procfs_root(
    cfg: *mut AgentsightConfigHandle,
    path: *const c_char,
) {
    if cfg.is_null() || path.is_null() {
        return;
    }
    let value = unsafe { CStr::from_ptr(path) }.to_string_lossy();
    if !value.is_empty() {
        unsafe { (*cfg).procfs_root = std::path::PathBuf::from(value.as_ref()) };
    }
}

/// Add a cmdline rule (allowlist or denylist).
/// * `rule` — NULL-terminated array of C strings (glob patterns).
/// * `agent_name` — agent name for allow=1; ignored for allow=0 (may be NULL).
/// * `allow` — 1 = whitelist (attach), 0 = blacklist (don't attach).
///
/// # Safety
///
/// `cfg` / `h` must be a valid pointer returned by the corresponding
/// `_new()` function, or null (which is handled gracefully).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn agentsight_config_add_cmdline_rule(
    cfg: *mut AgentsightConfigHandle,
    rule: *const *const c_char,
    agent_name: *const c_char,
    allow: c_int,
) {
    if cfg.is_null() || rule.is_null() {
        return;
    }
    let c = unsafe { &mut *cfg };

    // Collect patterns from NULL-terminated array
    let mut patterns = Vec::new();
    let mut i = 0usize;
    loop {
        let ptr = unsafe { *rule.add(i) };
        if ptr.is_null() {
            break;
        }
        let s = unsafe { CStr::from_ptr(ptr).to_string_lossy().to_string() };
        if !s.is_empty() {
            patterns.push(s);
        }
        i += 1;
    }

    if patterns.is_empty() {
        return;
    }

    let agent_name = if agent_name.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(agent_name).to_string_lossy().to_string() })
    };

    c.cmdline_rules.push(crate::config::CmdlineRule {
        patterns,
        agent_name,
        allow: allow != 0,
    });
}

/// Add an HTTPS rule (domain glob pattern for SSL/TLS probe attachment).
/// * `rule` — domain glob pattern (e.g. "*.openai.com").
///
/// # Safety
///
/// `cfg` / `h` must be a valid pointer returned by the corresponding
/// `_new()` function, or null (which is handled gracefully).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn agentsight_config_add_https(
    cfg: *mut AgentsightConfigHandle,
    rule: *const c_char,
) {
    if cfg.is_null() || rule.is_null() {
        return;
    }
    let c = unsafe { &mut *cfg };
    let s = unsafe { CStr::from_ptr(rule).to_string_lossy().to_string() };
    if !s.is_empty() {
        c.https_rules.push(crate::config::HttpsRule { pattern: s });
    }
}

/// Add an HTTP capture target for plain HTTP traffic (tcpsniff probe).
///
/// * `target` — string that is auto-detected:
///   - `":8080"`          → port-only endpoint
///   - `"10.0.0.1"`       → IP-only endpoint
///   - `"10.0.0.1:8080"`  → IP+port endpoint
///   - `"model-svc.default.svc"` → domain (DNS-resolved at runtime)
///
/// Returns 0 on success, <0 on error (call `agentsight_last_error()`).
///
/// # Safety
///
/// `cfg` / `h` must be a valid pointer returned by the corresponding
/// `_new()` function, or null (which is handled gracefully).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn agentsight_config_add_http(
    cfg: *mut AgentsightConfigHandle,
    target: *const c_char,
) -> c_int {
    if cfg.is_null() || target.is_null() {
        set_last_error("NULL config or target");
        return -1;
    }
    let c = unsafe { &mut *cfg };
    let s = unsafe { CStr::from_ptr(target).to_string_lossy().to_string() };
    if s.is_empty() {
        set_last_error("Empty HTTP target string");
        return -1;
    }
    match s.parse::<crate::config::TcpTarget>() {
        Ok(t) => c.http_targets.push(crate::config::HttpTarget::Endpoint(t)),
        Err(_) => c.http_targets.push(crate::config::HttpTarget::Domain(s)),
    }
    0
}

/// Load configuration from a JSON string. Rules are appended to existing ones.
/// Returns 0 on success, <0 on parse error.
///
/// # Safety
///
/// `cfg` / `h` must be a valid pointer returned by the corresponding
/// `_new()` function, or null (which is handled gracefully).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn agentsight_config_load_config(
    cfg: *mut AgentsightConfigHandle,
    json_str: *const c_char,
) -> c_int {
    if cfg.is_null() || json_str.is_null() {
        return -1;
    }
    let c = unsafe { &mut *cfg };
    let json = unsafe { CStr::from_ptr(json_str).to_string_lossy() };

    match c.load_from_json(&json) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(&e);
            -1
        }
    }
}

/// Free the configuration handle.
///
/// # Safety
///
/// `cfg` / `h` must be a valid pointer returned by the corresponding
/// `_new()` function, or null (which is handled gracefully).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn agentsight_config_free(cfg: *mut AgentsightConfigHandle) {
    if !cfg.is_null() {
        unsafe { drop(Box::from_raw(cfg)) };
    }
}

// ---- Lifecycle ----

/// Create a new AgentSight handle.  Does NOT start the pipeline yet.
/// Returns NULL on failure (call `agentsight_last_error()` for details).
///
/// # Safety
///
/// `cfg` / `h` must be a valid pointer returned by the corresponding
/// `_new()` function, or null (which is handled gracefully).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn agentsight_new(cfg: *mut AgentsightConfigHandle) -> *mut AgentsightHandle {
    // Create eventfd
    let efd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
    if efd < 0 {
        set_last_error("Failed to create eventfd");
        return ptr::null_mut();
    }

    let config = if cfg.is_null() {
        AgentsightConfig::default()
    } else {
        unsafe { (*cfg).clone() }
    };

    // Bounded FFI event channel sized from runtime_limits so that a slow
    // consumer cannot cause unbounded memory growth.
    let ffi_channel_capacity = config.runtime_limits.event_channel_capacity.max(1);
    let (tx, rx) = mpsc::sync_channel(ffi_channel_capacity);
    let running = Arc::new(AtomicBool::new(false));
    let security_drops = Arc::new(SecurityDropState::default());

    Box::into_raw(Box::new(AgentsightHandle {
        rx,
        tx: Some(tx),
        eventfd: efd,
        running,
        security_drops,
        thread: None,
        security_thread: None,
        config: Some(config),
        probe_cmd_tx: None,
    }))
}

/// Start the background pipeline thread.  Returns 0 on success, <0 on error.
///
/// # Safety
///
/// `cfg` / `h` must be a valid pointer returned by the corresponding
/// `_new()` function, or null (which is handled gracefully).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn agentsight_start(h: *mut AgentsightHandle) -> c_int {
    if h.is_null() {
        set_last_error("NULL handle");
        return -1;
    }
    let handle = unsafe { &mut *h };

    // Take config and sender (can only start once)
    let config = match handle.config.take() {
        Some(c) => c,
        None => {
            set_last_error("agentsight_start called more than once");
            return -1;
        }
    };
    let tx = match handle.tx.take() {
        Some(t) => t,
        None => {
            set_last_error("agentsight_start: sender already consumed");
            return -1;
        }
    };

    let running = handle.running.clone();
    running.store(true, Ordering::SeqCst);
    let eventfd = handle.eventfd;

    if config.ffi_enable_security_audit {
        let security_tx = tx.clone();
        let security_running = running.clone();
        let socket_path = config.ffi_enforcer_socket.clone();
        let security_drops = Arc::clone(&handle.security_drops);
        handle.security_thread = Some(std::thread::spawn(move || {
            ffi_security_background_thread(
                socket_path,
                security_tx,
                eventfd,
                security_running,
                security_drops,
            );
        }));
    }

    // Probe control channel: caller thread sends commands; background thread drains them.
    let (probe_cmd_tx, probe_cmd_rx) = mpsc::channel::<ProbeCommand>();
    handle.probe_cmd_tx = Some(probe_cmd_tx);

    handle.thread = Some(std::thread::spawn(move || {
        ffi_background_thread(config, tx, eventfd, running, probe_cmd_rx);
    }));

    0
}

fn ffi_security_background_thread(
    socket_path: std::path::PathBuf,
    tx: mpsc::SyncSender<FfiEvent>,
    eventfd: i32,
    running: Arc<AtomicBool>,
    security_drops: Arc<SecurityDropState>,
) {
    const MIN_RETRY: std::time::Duration = std::time::Duration::from_millis(250);
    const MAX_RETRY: std::time::Duration = std::time::Duration::from_secs(5);
    let sender = FfiEventSender {
        tx,
        eventfd,
        enable_raw_https: false,
        security_drops,
    };
    let client = crate::enforcement::EnforcementClient::new(&socket_path);
    let mut retry_delay = MIN_RETRY;
    while running.load(Ordering::SeqCst) {
        let mut subscription = match client.subscribe_security_events() {
            Ok(subscription) => subscription,
            Err(error) => {
                log::warn!(
                    "AgentSight security audit subscriber could not connect to {}: {error}",
                    socket_path.display()
                );
                sleep_while_running(running.as_ref(), retry_delay);
                retry_delay = (retry_delay * 2).min(MAX_RETRY);
                continue;
            }
        };
        retry_delay = MIN_RETRY;
        log::info!(
            "AgentSight security audit subscriber connected to {}",
            socket_path.display()
        );
        while running.load(Ordering::SeqCst) {
            match subscription.next_event() {
                Ok(Some(event)) => sender.send(FfiEvent::Security(event)),
                Ok(None) => {}
                Err(error) => {
                    log::warn!("AgentSight security audit subscription disconnected: {error}");
                    sleep_while_running(running.as_ref(), retry_delay);
                    retry_delay = (retry_delay * 2).min(MAX_RETRY);
                    break;
                }
            }
        }
    }
}

fn sleep_while_running(running: &AtomicBool, duration: std::time::Duration) {
    let deadline = std::time::Instant::now() + duration;
    while running.load(Ordering::SeqCst) {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        std::thread::sleep(remaining.min(std::time::Duration::from_millis(100)));
    }
}

/// Background thread: creates AgentSight and runs the event loop.
///
/// This function creates AgentSight *inside* the thread to avoid `Send`
/// constraints on eBPF objects.
fn ffi_background_thread(
    config: AgentsightConfig,
    tx: mpsc::SyncSender<FfiEvent>,
    eventfd: i32,
    running: Arc<AtomicBool>,
    probe_cmd_rx: mpsc::Receiver<ProbeCommand>,
) {
    let sender = FfiEventSender {
        tx,
        eventfd,
        enable_raw_https: config.ffi_enable_raw_https,
        security_drops: Arc::new(SecurityDropState::default()),
    };

    let mut sight = match AgentSight::new(config) {
        Ok(s) => s,
        Err(e) => {
            log::error!("agentsight background thread: AgentSight::new failed: {e}");
            return;
        }
    };

    // Install FFI event sender on the AgentSight instance.
    sight.set_ffi_sender(sender);

    // Event loop controlled by the external running flag.
    while running.load(Ordering::SeqCst) {
        // Drain probe commands (non-blocking)
        while let Ok(cmd) = probe_cmd_rx.try_recv() {
            match cmd {
                ProbeCommand::AddCgroup(id) => {
                    if let Err(e) = sight.add_traced_cgroup(id) {
                        log::warn!("add_traced_cgroup({id}) failed: {e}");
                    } else {
                        log::info!("Added cgroup_id {id} to BPF filter");
                    }
                }
                ProbeCommand::RemoveCgroup(id) => {
                    if let Err(e) = sight.remove_traced_cgroup(id) {
                        log::warn!("remove_traced_cgroup({id}) failed: {e}");
                    } else {
                        log::info!("Removed cgroup_id {id} from BPF filter");
                    }
                }
            }
        }
        if sight.try_process().is_none() {
            // No event available — flush any timed-out pending GenAI events
            sight.flush_expired_pending_genai();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    // Shutdown: flush pending events.
    sight.shutdown();
}

/// Stop the background pipeline thread.  Returns 0 on success.
///
/// # Safety
///
/// `cfg` / `h` must be a valid pointer returned by the corresponding
/// `_new()` function, or null (which is handled gracefully).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn agentsight_stop(h: *mut AgentsightHandle) -> c_int {
    if h.is_null() {
        set_last_error("NULL handle");
        return -1;
    }
    let handle = unsafe { &mut *h };
    handle.running.store(false, Ordering::SeqCst);

    // Wait for background thread to finish.
    if let Some(th) = handle.thread.take() {
        let _ = th.join();
    }
    if let Some(th) = handle.security_thread.take() {
        let _ = th.join();
    }
    0
}

/// Free the handle.  Must be called after `agentsight_stop()`.
/// The eventfd is closed automatically via the `Drop` impl.
///
/// # Safety
///
/// `cfg` / `h` must be a valid pointer returned by the corresponding
/// `_new()` function, or null (which is handled gracefully).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn agentsight_free(h: *mut AgentsightHandle) {
    if !h.is_null() {
        unsafe { drop(Box::from_raw(h)) };
    }
}

impl Drop for AgentsightHandle {
    fn drop(&mut self) {
        // Close the eventfd managed by this handle.
        if self.eventfd >= 0 {
            unsafe { libc::close(self.eventfd) };
            self.eventfd = -1;
        }
        // Join the background thread if still running.
        if let Some(th) = self.thread.take() {
            self.running.store(false, Ordering::SeqCst);
            let _ = th.join();
        }
        if let Some(th) = self.security_thread.take() {
            self.running.store(false, Ordering::SeqCst);
            let _ = th.join();
        }
    }
}

/// Return a static version string (e.g. "0.2.2").
#[unsafe(no_mangle)]
pub extern "C" fn agentsight_version() -> *const c_char {
    static VERSION_CSTR: std::sync::OnceLock<CString> = std::sync::OnceLock::new();
    VERSION_CSTR
        .get_or_init(|| CString::new(env!("CARGO_PKG_VERSION")).unwrap())
        .as_ptr()
}

// ---- Event notification ----

/// Return the eventfd descriptor.  The caller may register it with
/// epoll/select.  Returns <0 if eventfd is not supported.
/// The fd is managed internally — the caller must NOT close it.
///
/// # Safety
///
/// `cfg` / `h` must be a valid pointer returned by the corresponding
/// `_new()` function, or null (which is handled gracefully).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn agentsight_get_eventfd(h: *mut AgentsightHandle) -> c_int {
    if h.is_null() {
        return -1;
    }
    unsafe { (*h).eventfd }
}

// ---- Data reading ----

/// Process available events via callbacks.
///
/// * `http_cb` / `llm_cb`: callbacks for HTTP / LLM events (NULL = ignore).
/// * `flags`: 0 = non-blocking, `AGENTSIGHT_READ_BLOCK` = block until ≥1 event.
///
/// Returns the number of events processed, 0 if none, <0 on error.
///
/// # Safety
///
/// `cfg` / `h` must be a valid pointer returned by the corresponding
/// `_new()` function, or null (which is handled gracefully).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn agentsight_read(
    h: *mut AgentsightHandle,
    http_cb: HttpsCallbackFn,
    http_ud: *mut c_void,
    llm_cb: LlmCallbackFn,
    llm_ud: *mut c_void,
    flags: c_int,
) -> c_int {
    if h.is_null() {
        return -1;
    }
    let handle = unsafe { &*h };
    let mut count: c_int = 0;

    // Blocking mode: wait for at least one event.
    if flags & AGENTSIGHT_READ_BLOCK != 0 {
        match handle.rx.recv() {
            Ok(event) => {
                // Clear notifications before draining the queue. A producer
                // racing after this read leaves eventfd readable, so its event
                // cannot be stranded without a future wakeup.
                drain_eventfd(handle.eventfd);
                unsafe { dispatch_event(event, http_cb, http_ud, llm_cb, llm_ud) };
                count += 1;
            }
            Err(_) => return -1,
        }
    } else {
        drain_eventfd(handle.eventfd);
    }

    // Non-blocking drain of remaining (or all) events.
    while let Ok(event) = handle.rx.try_recv() {
        unsafe { dispatch_event(event, http_cb, http_ud, llm_cb, llm_ud) };
        count += 1;
    }

    count
}

/// Process available events through both legacy typed callbacks and a generic,
/// versioned JSON envelope callback. All callbacks observe the same queue item.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn agentsight_read_v2(
    h: *mut AgentsightHandle,
    http_cb: HttpsCallbackFn,
    http_ud: *mut c_void,
    llm_cb: LlmCallbackFn,
    llm_ud: *mut c_void,
    event_cb: EventCallbackFn,
    event_ud: *mut c_void,
    flags: c_int,
) -> c_int {
    ffi_boundary("agentsight_read_v2", || unsafe {
        agentsight_read_v2_inner(
            h, http_cb, http_ud, llm_cb, llm_ud, event_cb, event_ud, flags,
        )
    })
}

unsafe fn agentsight_read_v2_inner(
    h: *mut AgentsightHandle,
    http_cb: HttpsCallbackFn,
    http_ud: *mut c_void,
    llm_cb: LlmCallbackFn,
    llm_ud: *mut c_void,
    event_cb: EventCallbackFn,
    event_ud: *mut c_void,
    flags: c_int,
) -> c_int {
    if h.is_null() {
        return -1;
    }
    let handle = unsafe { &*h };
    let mut count: c_int = 0;

    if flags & AGENTSIGHT_READ_BLOCK != 0 {
        match handle.rx.recv() {
            Ok(event) => {
                drain_eventfd(handle.eventfd);
                unsafe {
                    dispatch_event_v2(event, http_cb, http_ud, llm_cb, llm_ud, event_cb, event_ud)
                };
                count += 1;
            }
            Err(_) => return -1,
        }
    } else {
        drain_eventfd(handle.eventfd);
    }
    while let Ok(event) = handle.rx.try_recv() {
        unsafe { dispatch_event_v2(event, http_cb, http_ud, llm_cb, llm_ud, event_cb, event_ud) };
        count += 1;
    }
    if let Some(event) = handle.security_drops.take_event() {
        unsafe {
            dispatch_event_v2(
                FfiEvent::Security(event),
                http_cb,
                http_ud,
                llm_cb,
                llm_ud,
                event_cb,
                event_ud,
            )
        };
        count += 1;
    }
    count
}

// ---- Dynamic cgroup filter control ----

/// Add a cgroup inode ID to the BPF cgroup_filter map at runtime.
/// Returns 0 on success, -1 on failure (check agentsight_last_error()).
/// Requires agentsight_start() to have been called first.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn agentsight_add_traced_cgroup(
    h: *mut AgentsightHandle,
    cgroup_id: u64,
) -> c_int {
    if h.is_null() {
        set_last_error("null handle");
        return -1;
    }
    let handle = unsafe { &*h };
    match &handle.probe_cmd_tx {
        Some(tx) => {
            if tx.send(ProbeCommand::AddCgroup(cgroup_id)).is_err() {
                set_last_error("probe command channel closed");
                -1
            } else {
                0
            }
        }
        None => {
            set_last_error("handle not started or probe_cmd_tx not initialized");
            -1
        }
    }
}

/// Remove a cgroup inode ID from the BPF cgroup_filter map at runtime.
/// Returns 0 on success, -1 on failure (check agentsight_last_error()).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn agentsight_remove_traced_cgroup(
    h: *mut AgentsightHandle,
    cgroup_id: u64,
) -> c_int {
    if h.is_null() {
        set_last_error("null handle");
        return -1;
    }
    let handle = unsafe { &*h };
    match &handle.probe_cmd_tx {
        Some(tx) => {
            if tx.send(ProbeCommand::RemoveCgroup(cgroup_id)).is_err() {
                set_last_error("probe command channel closed");
                -1
            } else {
                0
            }
        }
        None => {
            set_last_error("handle not started or probe_cmd_tx not initialized");
            -1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentsight_enforcement_protocol::{Effect, EventIdentity, PolicyDecision, PolicyMode};
    use uuid::Uuid;

    fn new_cfg() -> AgentsightConfig {
        let mut cfg = AgentsightConfig::default();
        cfg.cmdline_rules.clear();
        cfg.https_rules.clear();
        cfg
    }

    #[test]
    fn test_load_json_basic() {
        let mut cfg = new_cfg();
        let json = r#"{"verbose":1,"log_path":"/tmp/test.log"}"#;
        assert!(cfg.load_from_json(json).is_ok());
        assert!(cfg.verbose);
        assert_eq!(cfg.log_path, Some("/tmp/test.log".to_string()));
    }

    #[test]
    fn test_load_json_cmdline_allow_and_deny() {
        let mut cfg = new_cfg();
        let json = r#"{
            "cmdline": {
                "allow": [
                    {"rule": ["node", "*claude*"], "agent_name": "Claude Code"}
                ],
                "deny": [
                    {"rule": ["node", "*webpack*"]}
                ]
            }
        }"#;
        assert!(cfg.load_from_json(json).is_ok());
        assert_eq!(cfg.cmdline_rules.len(), 2);
        assert!(cfg.cmdline_rules[0].allow);
        assert_eq!(
            cfg.cmdline_rules[0].agent_name,
            Some("Claude Code".to_string())
        );
        assert!(!cfg.cmdline_rules[1].allow);
        assert!(cfg.cmdline_rules[1].agent_name.is_none());
    }

    #[test]
    fn test_load_json_https_rules() {
        let mut cfg = new_cfg();
        let json = r#"{
            "https": [
                {"rule": ["*.openai.com", "*.anthropic.com"]}
            ]
        }"#;
        assert!(cfg.load_from_json(json).is_ok());
        assert_eq!(cfg.https_rules.len(), 2);
        assert_eq!(cfg.https_rules[0].pattern, "*.openai.com");
        assert_eq!(cfg.https_rules[1].pattern, "*.anthropic.com");
    }

    #[test]
    fn test_load_json_invalid() {
        let mut cfg = new_cfg();
        let json = r#"{ invalid json }"#;
        assert!(cfg.load_from_json(json).is_err());
    }

    #[test]
    fn test_load_json_appends_to_existing() {
        let mut cfg = new_cfg();
        // First load
        let json1 = r#"{"cmdline":{"allow":[{"rule":["node"],"agent_name":"Agent1"}]}}"#;
        assert!(cfg.load_from_json(json1).is_ok());
        assert_eq!(cfg.cmdline_rules.len(), 1);

        // Second load should append
        let json2 = r#"{"cmdline":{"allow":[{"rule":["python3"],"agent_name":"Agent2"}]}}"#;
        assert!(cfg.load_from_json(json2).is_ok());
        assert_eq!(cfg.cmdline_rules.len(), 2);
        assert_eq!(cfg.cmdline_rules[1].agent_name, Some("Agent2".to_string()));
    }

    #[test]
    fn test_load_json_empty_rule_skipped() {
        let mut cfg = new_cfg();
        let json = r#"{
            "cmdline": {
                "allow": [
                    {"rule": [], "agent_name": "Skipped"},
                    {"rule": ["node"], "agent_name": "Kept"}
                ]
            }
        }"#;
        assert!(cfg.load_from_json(json).is_ok());
        assert_eq!(cfg.cmdline_rules.len(), 1);
        assert_eq!(cfg.cmdline_rules[0].agent_name, Some("Kept".to_string()));
    }

    #[test]
    fn test_safe_cstring_replaces_nul() {
        let s = "hel\0lo";
        let c = safe_cstring(s);
        assert_eq!(c.to_str().unwrap(), "hello");
    }

    #[test]
    fn test_config_set_enable_raw_https() {
        let cfg = agentsight_config_new();
        assert!(!cfg.is_null());
        assert!(!unsafe { (*cfg).ffi_enable_raw_https });

        unsafe { agentsight_config_set_enable_raw_https(cfg, 1) };
        assert!(unsafe { (*cfg).ffi_enable_raw_https });

        unsafe { agentsight_config_set_enable_raw_https(cfg, 0) };
        assert!(!unsafe { (*cfg).ffi_enable_raw_https });

        unsafe {
            agentsight_config_set_enable_raw_https(ptr::null_mut(), 1);
            agentsight_config_free(cfg);
        }
    }

    #[test]
    #[allow(clippy::needless_range_loop)]
    fn test_copy_process_name_truncate() {
        let name = "very_long_process_name_that_exceeds_16";
        let buf = copy_process_name(name);
        assert_eq!(buf[15], 0); // NUL-terminated
        // First 15 chars should match
        for i in 0..15 {
            assert_eq!(buf[i] as u8, name.as_bytes()[i]);
        }
    }

    #[test]
    fn test_copy_to_fixed_buf_truncates_at_char_boundary() {
        fn as_bytes<const N: usize>(buf: &[c_char; N]) -> Vec<u8> {
            buf.iter()
                .take_while(|&&c| c != 0)
                .map(|&c| c as u8)
                .collect()
        }

        // ASCII longer than N-1: truncated with NUL termination.
        let buf = copy_to_fixed_buf::<8>("abcdefghij");
        assert_eq!(as_bytes(&buf), b"abcdefg");
        assert_eq!(buf[7], 0);

        // Multi-byte UTF-8: a char straddling the limit is dropped whole,
        // never split into invalid bytes. "a中b" = 1 + 3 + 1 bytes; N-1 = 3
        // would cut through 中, so only "a" is copied.
        let buf = copy_to_fixed_buf::<4>("a中b");
        assert_eq!(as_bytes(&buf), b"a");

        // Empty input yields an all-zero buffer.
        let buf = copy_to_fixed_buf::<128>("");
        assert!(buf.iter().all(|&c| c == 0));
    }

    // ─── build_llm_data tests ───────────────────────────────────────────────

    fn make_http_record(request_body: Option<String>, response_body: Option<String>) -> HttpRecord {
        HttpRecord {
            timestamp_ns: 1,
            pid: 1,
            comm: "test".to_string(),
            method: "POST".to_string(),
            path: "/raw".to_string(),
            status_code: 200,
            request_headers: "{\"host\":\"example.com\"}".to_string(),
            request_body,
            response_headers: "{\"content-type\":\"application/octet-stream\"}".to_string(),
            response_body,
            duration_ns: 1,
            first_output_timestamp_ns: None,
            is_sse: false,
            sse_event_count: 0,
        }
    }

    #[test]
    fn test_build_https_data_preserves_interior_nul_bytes() {
        let request_body = "a\0b\0c".to_string();
        let response_body = "\0x\0y\0".to_string();
        let request_headers = "{\0\"host\":\"example.com\"\0}".to_string();
        let response_headers = "{\0\"content-type\":\"application/octet-stream\"\0}".to_string();
        let mut record = make_http_record(Some(request_body.clone()), Some(response_body.clone()));
        record.request_headers = request_headers.clone();
        record.response_headers = response_headers.clone();
        let holder = build_https_data(&record, None);

        let copied_request_headers = unsafe {
            std::slice::from_raw_parts(
                holder.c_data.request_headers.cast::<u8>(),
                holder.c_data.request_headers_len as usize,
            )
        };
        let copied_request = unsafe {
            std::slice::from_raw_parts(
                holder.c_data.request_body.cast::<u8>(),
                holder.c_data.request_body_len as usize,
            )
        };
        let copied_response_headers = unsafe {
            std::slice::from_raw_parts(
                holder.c_data.response_headers.cast::<u8>(),
                holder.c_data.response_headers_len as usize,
            )
        };
        let copied_response = unsafe {
            std::slice::from_raw_parts(
                holder.c_data.response_body.cast::<u8>(),
                holder.c_data.response_body_len as usize,
            )
        };
        assert_eq!(copied_request_headers, request_headers.as_bytes());
        assert_eq!(copied_request, request_body.as_bytes());
        assert_eq!(copied_response_headers, response_headers.as_bytes());
        assert_eq!(copied_response, response_body.as_bytes());
    }

    #[test]
    fn test_disabled_https_sender_does_not_consume_channel_capacity() {
        let (tx, rx) = mpsc::sync_channel(1);
        let sender = FfiEventSender {
            tx,
            eventfd: -1,
            enable_raw_https: false,
            security_drops: Arc::new(SecurityDropState::default()),
        };
        sender.send_https(&make_http_record(None, None), || None);
        sender.send(FfiEvent::Llm(make_llm_call(None, 1)));
        assert!(matches!(rx.try_recv(), Ok(FfiEvent::Llm(_))));
    }

    #[test]
    fn test_disabled_https_sender_does_not_resolve_agent_name() {
        // Raw HTTPS is off by default while FFI mode is on, so this is the common case:
        // every non-LLM exchange reaches send_https. Resolving the agent name costs two
        // /proc reads plus rule matching on a cache miss — and non-agent processes always
        // miss — so the resolver must not run at all when the consumer opted out.
        let (tx, _rx) = mpsc::sync_channel(1);
        let sender = FfiEventSender {
            tx,
            eventfd: -1,
            enable_raw_https: false,
            security_drops: Arc::new(SecurityDropState::default()),
        };
        let resolved = std::cell::Cell::new(false);
        sender.send_https(&make_http_record(None, None), || {
            resolved.set(true);
            None
        });
        assert!(
            !resolved.get(),
            "agent name must not be resolved while raw HTTPS is disabled"
        );
    }

    #[test]
    fn test_enabled_https_sender_enqueues_event() {
        let (tx, rx) = mpsc::sync_channel(1);
        let sender = FfiEventSender {
            tx,
            eventfd: -1,
            enable_raw_https: true,
            security_drops: Arc::new(SecurityDropState::default()),
        };
        sender.send_https(&make_http_record(None, None), || Some("hermes".to_string()));
        assert!(matches!(
            rx.try_recv(),
            Ok(FfiEvent::Https(_, Some(name))) if name == "hermes"
        ));
    }

    #[test]
    fn test_build_https_data_cmdline_live_process() {
        // The current test process has a readable /proc/<pid>/cmdline, so the
        // raw path must fill it just like the LLM path does.
        let mut record = make_http_record(None, None);
        record.pid = std::process::id();
        let holder = build_https_data(&record, None);
        let end = holder
            .c_data
            .cmdline
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(128);
        assert!(end > 0, "cmdline should be non-empty for a live process");
        assert!(
            end < 128,
            "cmdline must be NUL-terminated within the buffer"
        );
    }

    #[test]
    fn test_build_https_data_cmdline_dead_pid_empty() {
        // A non-existent pid makes read_cmdline fail, yielding an empty buffer.
        let mut record = make_http_record(None, None);
        record.pid = u32::MAX;
        let holder = build_https_data(&record, None);
        assert_eq!(holder.c_data.cmdline[0], 0);
    }

    #[test]
    fn test_build_https_data_lowercases_agent_name() {
        // Matches build_llm_data: C consumers see one casing regardless of config.
        let holder = build_https_data(&make_http_record(None, None), Some("Claude Code"));
        assert!(!holder.c_data.agent_name.is_null());
        let name = unsafe { CStr::from_ptr(holder.c_data.agent_name) };
        assert_eq!(name.to_str().unwrap(), "claude code");
    }

    #[test]
    fn test_build_https_data_agent_name_none_is_null() {
        let holder = build_https_data(&make_http_record(None, None), None);
        assert!(holder.c_data.agent_name.is_null());
    }

    fn make_llm_call(agent_name: Option<&str>, pid: i32) -> LLMCall {
        use crate::genai::semantic::{LLMRequest, LLMResponse};
        LLMCall {
            call_id: "call-1".to_string(),
            start_timestamp_ns: 1_000_000_000,
            end_timestamp_ns: 2_000_000_000,
            duration_ns: 1_000_000_000,
            provider: "openai".to_string(),
            model: "gpt-4".to_string(),
            request: LLMRequest {
                messages: vec![],
                temperature: None,
                max_tokens: None,
                frequency_penalty: None,
                presence_penalty: None,
                top_p: None,
                top_k: None,
                seed: None,
                stop_sequences: None,
                stream: false,
                tools: None,
                raw_body: None,
            },
            response: LLMResponse {
                messages: vec![],
                streamed: false,
                raw_body: None,
            },
            token_usage: None,
            error: None,
            pid,
            process_name: "test".to_string(),
            agent_name: agent_name.map(str::to_string),
            metadata: std::collections::HashMap::new(),
        }
    }

    fn make_security_event() -> SecurityEvent {
        SecurityEvent::policy_decision(
            EventIdentity {
                binding_id: Uuid::nil(),
                agent_id: "agent-1".to_string(),
                agent_name: Some("claude".to_string()),
                session_id: Some("session-1".to_string()),
                conversation_id: None,
                tool_call_id: None,
                pid: 42,
                process_start_time: 7,
                ppid: Some(1),
                cgroup_id: Some(99),
                protocol_version: 1,
                enforcer_version: "test".to_string(),
                actplane_revision: "revision-1".to_string(),
            },
            PolicyDecision {
                policy_id: "credential-exfiltration".to_string(),
                policy_revision: 3,
                source_event_id: Uuid::nil(),
                sink_event_id: Uuid::nil(),
                mode: PolicyMode::Audit,
                requested_effect: Effect::Notify,
                blocked: false,
                killed: false,
                errno: None,
                risk_score: 85,
                reason: "credential reached an unknown public endpoint".to_string(),
            },
        )
    }

    #[test]
    fn test_build_event_data_wraps_security_event_as_versioned_json() {
        let event = make_security_event();
        let holder = build_event_data(&FfiEvent::Security(event.clone()))
            .expect("security event should serialize");

        assert_eq!(holder.c_data.event_type, AgentsightEventType::Security);
        assert_eq!(holder.c_data.schema_version, 1);
        assert_eq!(holder.c_data.timestamp_ns, event.occurred_at_ns);
        let payload = unsafe {
            std::slice::from_raw_parts(
                holder.c_data.payload_json.cast::<u8>(),
                holder.c_data.payload_json_len as usize,
            )
        };
        let decoded: SecurityEvent =
            serde_json::from_slice(payload).expect("payload should be a SecurityEvent");
        assert_eq!(decoded, event);
    }

    fn envelope_payload(holder: &EventDataHolder) -> serde_json::Value {
        let bytes = unsafe {
            std::slice::from_raw_parts(
                holder.c_data.payload_json.cast::<u8>(),
                holder.c_data.payload_json_len as usize,
            )
        };
        serde_json::from_slice(bytes).expect("payload should be a JSON object")
    }

    #[test]
    fn test_build_event_data_https_envelope_carries_process_metadata() {
        // The generic envelope must not be poorer than the typed AgentsightHttpsData:
        // agent_name / cmdline ride alongside the flattened HttpRecord fields.
        let mut record = make_http_record(None, None);
        record.pid = std::process::id();
        let holder = build_event_data(&FfiEvent::Https(record, Some("Hermes".to_string())))
            .expect("https event should serialize");

        assert_eq!(holder.c_data.event_type, AgentsightEventType::Https);
        assert_eq!(holder.c_data.schema_version, 1);
        let payload = envelope_payload(&holder);
        // Lowercased, matching the typed path and the gen_ai.agent.type convention.
        assert_eq!(payload["agent_name"], "hermes");
        // The current test process is alive, so cmdline resolves.
        assert!(
            payload["cmdline"].as_str().is_some_and(|s| !s.is_empty()),
            "cmdline should be present for a live process, got {:?}",
            payload["cmdline"]
        );
        // HttpRecord's own fields are still flattened in at the top level.
        assert_eq!(payload["method"], "POST");
        assert_eq!(payload["path"], "/raw");
    }

    #[test]
    fn test_v2_envelope_agent_name_casing_agrees_across_event_kinds() {
        // A v2 consumer groups raw HTTPS and LLM events for one process by agent_name,
        // so the two arms must not disagree on casing. Config rules ship camel-cased
        // ("Hermes", "Codex", "Cosh"), and the Llm arm serializes LLMCall verbatim —
        // hence the normalization happens at the sender, not in the builders.
        let (tx, rx) = mpsc::sync_channel(2);
        let sender = FfiEventSender {
            tx,
            eventfd: -1,
            enable_raw_https: true,
            security_drops: Arc::new(SecurityDropState::default()),
        };
        sender.send_llm(&make_llm_call(Some("Hermes"), 1));
        sender.send_https(&make_http_record(None, None), || Some("Hermes".to_string()));

        let llm_event = rx.try_recv().expect("llm event should be queued");
        let https_event = rx.try_recv().expect("https event should be queued");
        let llm_payload =
            envelope_payload(&build_event_data(&llm_event).expect("llm event should serialize"));
        let https_payload = envelope_payload(
            &build_event_data(&https_event).expect("https event should serialize"),
        );

        assert_eq!(llm_payload["agent_name"], "hermes");
        assert_eq!(https_payload["agent_name"], "hermes");
        assert_eq!(llm_payload["agent_name"], https_payload["agent_name"]);
    }

    #[test]
    fn test_build_event_data_https_envelope_omits_absent_metadata() {
        // Dead pid + no rule match: absent values are omitted, not serialized as null,
        // so a consumer sees "key missing" rather than a null it has to special-case.
        let mut record = make_http_record(None, None);
        record.pid = u32::MAX;
        let holder =
            build_event_data(&FfiEvent::Https(record, None)).expect("https event should serialize");

        let payload = envelope_payload(&holder);
        assert!(payload.get("agent_name").is_none());
        assert!(payload.get("cmdline").is_none());
        assert!(payload.get("container_id").is_none());
    }

    #[test]
    fn full_ffi_queue_emits_normalized_security_evidence_loss() {
        let (tx, _rx) = mpsc::sync_channel(1);
        let security_drops = Arc::new(SecurityDropState::default());
        let sender = FfiEventSender {
            tx,
            eventfd: -1,
            enable_raw_https: false,
            security_drops: Arc::clone(&security_drops),
        };
        sender.send(FfiEvent::Llm(make_llm_call(None, 1)));
        sender.send(FfiEvent::Security(make_security_event()));

        let loss = security_drops
            .take_event()
            .expect("queue loss should become a security event");
        let SecurityEventKind::EnforcementState(state) = loss.kind else {
            panic!("expected enforcement state");
        };
        assert_eq!(state.code, "ffi_queue_evidence_loss");
        assert_eq!(state.dropped_events, Some(1));
        assert!(state.ready);
        assert!(security_drops.take_event().is_none());
    }

    #[derive(Default)]
    struct CallbackCounts {
        llm: usize,
        generic_llm: usize,
        security: usize,
    }

    unsafe extern "C" fn count_llm(_: *const AgentsightLLMData, user_data: *mut c_void) {
        let counts = unsafe { &mut *user_data.cast::<CallbackCounts>() };
        counts.llm += 1;
    }

    unsafe extern "C" fn count_generic(event: *const AgentsightEvent, user_data: *mut c_void) {
        let counts = unsafe { &mut *user_data.cast::<CallbackCounts>() };
        match unsafe { (*event).event_type } {
            AgentsightEventType::Llm => counts.generic_llm += 1,
            AgentsightEventType::Security => counts.security += 1,
            AgentsightEventType::Https => {}
        }
    }

    #[test]
    fn test_dispatch_v2_keeps_legacy_llm_callback_and_emits_generic_events() {
        let mut counts = CallbackCounts::default();
        let user_data = (&mut counts as *mut CallbackCounts).cast::<c_void>();

        unsafe {
            dispatch_event_v2(
                FfiEvent::Llm(make_llm_call(None, 1)),
                None,
                ptr::null_mut(),
                Some(count_llm),
                user_data,
                Some(count_generic),
                user_data,
            );
            dispatch_event_v2(
                FfiEvent::Security(make_security_event()),
                None,
                ptr::null_mut(),
                Some(count_llm),
                user_data,
                Some(count_generic),
                user_data,
            );
        }

        assert_eq!(counts.llm, 1);
        assert_eq!(counts.generic_llm, 1);
        assert_eq!(counts.security, 1);
    }

    #[test]
    fn ffi_boundary_translates_panics_to_an_error_result() {
        let result = ffi_boundary("agentsight_read_v2", || -> c_int {
            panic!("test-only FFI panic")
        });

        assert_eq!(result, -1);
        let message = unsafe { CStr::from_ptr(agentsight_last_error()) }
            .to_str()
            .expect("last error should be valid UTF-8");
        assert!(message.contains("agentsight_read_v2 panicked"));
    }

    #[test]
    fn test_config_security_audit_is_opt_in_and_socket_is_configurable() {
        let cfg = agentsight_config_new();
        assert!(!cfg.is_null());
        assert!(!unsafe { (*cfg).ffi_enable_security_audit });

        let socket = CString::new("/tmp/agentsight-enforcer.sock").unwrap();
        unsafe {
            agentsight_config_set_enable_security_audit(cfg, 1);
            agentsight_config_set_enforcer_socket(cfg, socket.as_ptr());
        }
        assert!(unsafe { (*cfg).ffi_enable_security_audit });
        assert_eq!(
            unsafe { &(*cfg).ffi_enforcer_socket },
            &std::path::PathBuf::from("/tmp/agentsight-enforcer.sock")
        );

        unsafe { agentsight_config_free(cfg) };
    }

    #[test]
    fn test_config_procfs_root_is_configurable() {
        let cfg = agentsight_config_new();
        assert!(!cfg.is_null());
        assert_eq!(
            unsafe { &(*cfg).procfs_root },
            &std::path::PathBuf::from(crate::utils::procfs::DEFAULT_PROC_ROOT)
        );

        let root = CString::new("/logtail_host/proc").unwrap();
        unsafe { agentsight_config_set_procfs_root(cfg, root.as_ptr()) };
        assert_eq!(
            unsafe { &(*cfg).procfs_root },
            &std::path::PathBuf::from("/logtail_host/proc")
        );

        // An empty string keeps the previous value.
        let empty = CString::new("").unwrap();
        unsafe { agentsight_config_set_procfs_root(cfg, empty.as_ptr()) };
        assert_eq!(
            unsafe { &(*cfg).procfs_root },
            &std::path::PathBuf::from("/logtail_host/proc")
        );

        unsafe { agentsight_config_free(cfg) };

        // Null operands are no-ops, not crashes.
        let some_path = CString::new("/proc").unwrap();
        unsafe { agentsight_config_set_procfs_root(std::ptr::null_mut(), some_path.as_ptr()) };
        let cfg = agentsight_config_new();
        unsafe { agentsight_config_set_procfs_root(cfg, std::ptr::null()) };
        assert_eq!(
            unsafe { &(*cfg).procfs_root },
            &std::path::PathBuf::from(crate::utils::procfs::DEFAULT_PROC_ROOT)
        );
        unsafe { agentsight_config_free(cfg) };
    }

    #[test]
    fn test_security_subscriber_stops_cleanly_when_enforcer_is_unavailable() {
        let (tx, rx) = mpsc::sync_channel(1);
        let running = Arc::new(AtomicBool::new(true));
        let worker_running = running.clone();
        let socket = std::env::temp_dir().join(format!(
            "agentsight-missing-enforcer-{}.sock",
            std::process::id()
        ));
        let worker = std::thread::spawn(move || {
            ffi_security_background_thread(
                socket,
                tx,
                -1,
                worker_running,
                Arc::new(SecurityDropState::default()),
            );
        });
        std::thread::sleep(std::time::Duration::from_millis(20));
        running.store(false, Ordering::SeqCst);
        worker.join().expect("security subscriber should stop");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_build_llm_data_lowercases_agent_name() {
        let holder = build_llm_data(&make_llm_call(Some("Claude"), 1234));
        assert!(!holder.c_data.agent_name.is_null());
        let name = unsafe { CStr::from_ptr(holder.c_data.agent_name) };
        assert_eq!(name.to_str().unwrap(), "claude");
    }

    #[test]
    fn test_build_llm_data_agent_name_none_is_null() {
        let holder = build_llm_data(&make_llm_call(None, 1234));
        assert!(holder.c_data.agent_name.is_null());
    }

    #[test]
    fn test_build_llm_data_cmdline_live_process() {
        // The current test process has a readable /proc/<pid>/cmdline, so the
        // buffer must hold a non-empty NUL-terminated string.
        let holder = build_llm_data(&make_llm_call(None, std::process::id() as i32));
        let end = holder
            .c_data
            .cmdline
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(128);
        assert!(end > 0, "cmdline should be non-empty for a live process");
        assert!(
            end < 128,
            "cmdline must be NUL-terminated within the buffer"
        );
    }

    #[test]
    fn test_build_llm_data_cmdline_dead_pid_empty() {
        // A non-existent pid makes read_cmdline fail, yielding an empty buffer.
        let holder = build_llm_data(&make_llm_call(None, i32::MAX));
        assert_eq!(holder.c_data.cmdline[0], 0);
    }

    #[test]
    fn test_load_json_cmdline_allow() {
        let mut cfg = new_cfg();
        let json = r#"{
            "cmdline": {
                "allow": [
                    {"rule": ["*python*", "*hermes*"], "agent_name": "Hermes"},
                    {"rule": ["node*", "*copilot-shell*"], "agent_name": "Cosh"}
                ]
            }
        }"#;
        assert!(cfg.load_from_json(json).is_ok());
        assert_eq!(cfg.cmdline_rules.len(), 2);
        assert_eq!(cfg.cmdline_rules[0].agent_name, Some("Hermes".to_string()));
        assert!(cfg.cmdline_rules[0].allow);
        assert_eq!(cfg.cmdline_rules[1].agent_name, Some("Cosh".to_string()));
    }

    #[test]
    fn test_load_json_cmdline_with_deny() {
        let mut cfg = new_cfg();
        let json = r#"{
            "cmdline": {
                "allow": [{"rule": ["node", "*claude*"], "agent_name": "Claude Code"}],
                "deny": [{"rule": ["node", "*webpack*"]}]
            }
        }"#;
        assert!(cfg.load_from_json(json).is_ok());
        assert_eq!(cfg.cmdline_rules.len(), 2);
        assert!(cfg.cmdline_rules[0].allow);
        assert!(!cfg.cmdline_rules[1].allow);
    }

    #[test]
    fn test_load_json_all_fields() {
        let mut cfg = new_cfg();
        let json = r#"{
            "verbose": 1,
            "cmdline": {
                "allow": [{"rule": ["node", "*claude*"], "agent_name": "Claude Code"}]
            },
            "https": [{"rule": ["*.openai.com"]}]
        }"#;
        assert!(cfg.load_from_json(json).is_ok());
        assert!(cfg.verbose);
        assert_eq!(cfg.cmdline_rules.len(), 1);
        assert_eq!(cfg.https_rules.len(), 1);
    }

    #[test]
    fn test_load_json_http_endpoint() {
        let mut cfg = new_cfg();
        let json = r#"{
            "http": [
                {"rule": [":8080", "10.0.0.1:9090"]}
            ]
        }"#;
        assert!(cfg.load_from_json(json).is_ok());
        assert_eq!(cfg.http_targets.len(), 2);
        match &cfg.http_targets[0] {
            crate::config::HttpTarget::Endpoint(t) => {
                assert_eq!(t.ip, None);
                assert_eq!(t.port, Some(8080));
            }
            _ => panic!("expected Endpoint"),
        }
        match &cfg.http_targets[1] {
            crate::config::HttpTarget::Endpoint(t) => {
                assert_eq!(t.ip, Some(std::net::Ipv4Addr::new(10, 0, 0, 1)));
                assert_eq!(t.port, Some(9090));
            }
            _ => panic!("expected Endpoint"),
        }
    }

    #[test]
    fn test_load_json_http_domain() {
        let mut cfg = new_cfg();
        let json = r#"{
            "http": [
                {"rule": ["model-svc.default.svc", "*.internal.com"]}
            ]
        }"#;
        assert!(cfg.load_from_json(json).is_ok());
        assert_eq!(cfg.http_targets.len(), 2);
        match &cfg.http_targets[0] {
            crate::config::HttpTarget::Domain(d) => assert_eq!(d, "model-svc.default.svc"),
            _ => panic!("expected Domain"),
        }
        match &cfg.http_targets[1] {
            crate::config::HttpTarget::Domain(d) => assert_eq!(d, "*.internal.com"),
            _ => panic!("expected Domain"),
        }
    }
}
