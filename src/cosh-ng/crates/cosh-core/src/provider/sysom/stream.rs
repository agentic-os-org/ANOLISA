//! Cumulative SSE state machine for the SysOM stream format.
//!
//! SysOM frames are cumulative snapshots rather than deltas, so reassembly is
//! stateful: text and tool arguments must extend what was already seen, tool
//! identity must stay stable across frames, and a failure frame must be
//! summarized without echoing its payload. Keeping that machine here leaves
//! `sysom.rs` to transport, signing, and metadata concerns.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::StreamExt;
use serde_json::Value;

use super::super::{GenerateEvent, GenerateStream, MAX_TOOL_CALL_INDEX};
use super::hex_sha256;

/// Byte stream feeding the SSE state machine. Errors are pre-stringified so the
/// state machine is independent of the transport.
pub(super) type SysomByteStream =
    Pin<Box<dyn futures::Stream<Item = Result<Vec<u8>, String>> + Send>>;

/// Bounded SysOM stream failure.
///
/// Variants carry counts only. The raw SSE payload, assistant text, and tool
/// arguments may contain session content, so they are never embedded.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SysomStreamError {
    /// A frame's bytes were not valid UTF-8, even once fully reassembled.
    InvalidUtf8 {
        byte_offset: usize,
        block_bytes: usize,
    },
    /// A single SSE block exceeded [`MAX_SSE_BLOCK_BYTES`], terminated or not.
    BlockTooLarge { buffered_bytes: usize, limit: usize },
    /// An `OK` frame's `data:` payload was not valid JSON.
    MalformedJson,
    /// An `OK` frame's payload parsed but its root was not a JSON object.
    RootWrongType,
    /// `choices` was present but not an array of choice objects.
    ChoicesWrongType,
    /// `choices[0].message` was present but not an object.
    MessageWrongType,
    /// `message.tool_use` was present but not an array.
    ToolUseWrongType,
    /// `choices[0].message.content` was present but not a string.
    ContentWrongType,
    /// Cumulative assistant text stopped extending its previous snapshot.
    ContentRewritten {
        previous_bytes: usize,
        new_bytes: usize,
    },
    /// A `tool_use` entry was not an object.
    ToolEntryWrongType { position: usize },
    /// `index` was present but not an integer within [`MAX_TOOL_CALL_INDEX`].
    ToolIndexInvalid { position: usize },
    /// `id` or `function.name` was present but not a string.
    ToolIdentityWrongType { index: u32 },
    /// The frame that first revealed a tool carried no usable `id`/`function.name`.
    ToolIdentityMissing { index: u32 },
    /// A later frame reported a different `id` or `function.name` for the index.
    ToolIdentityChanged { index: u32 },
    /// `function.arguments` was present but not a string.
    ArgumentsWrongType { index: u32 },
    /// Cumulative tool arguments stopped extending their previous snapshot.
    ArgumentsRewritten {
        index: u32,
        previous_bytes: usize,
        new_bytes: usize,
    },
}

impl std::fmt::Display for SysomStreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUtf8 {
                byte_offset,
                block_bytes,
            } => write!(
                f,
                "SysOM stream: frame payload was not valid UTF-8 (first invalid byte at offset \
                 {byte_offset} of {block_bytes})"
            ),
            Self::BlockTooLarge {
                buffered_bytes,
                limit,
            } => write!(
                f,
                "SysOM stream: event block of {buffered_bytes} bytes exceeds the {limit} byte \
                 limit"
            ),
            Self::MalformedJson => write!(f, "SysOM stream: frame payload was not valid JSON"),
            Self::RootWrongType => {
                write!(f, "SysOM stream: frame payload was not a JSON object")
            }
            Self::ChoicesWrongType => {
                write!(
                    f,
                    "SysOM stream: choices was not an array of choice objects"
                )
            }
            Self::MessageWrongType => {
                write!(f, "SysOM stream: choice message was not an object")
            }
            Self::ToolUseWrongType => {
                write!(f, "SysOM stream: tool_use was not an array")
            }
            Self::ContentWrongType => {
                write!(f, "SysOM stream: message content was not a string")
            }
            Self::ContentRewritten {
                previous_bytes,
                new_bytes,
            } => write!(
                f,
                "SysOM stream: cumulative content is not an extension of the previous frame \
                 (previous {previous_bytes} bytes, new {new_bytes} bytes)"
            ),
            Self::ToolEntryWrongType { position } => write!(
                f,
                "SysOM stream: tool_use entry at position {position} was not an object"
            ),
            Self::ToolIndexInvalid { position } => write!(
                f,
                "SysOM stream: tool_use entry at position {position} carried an index that is not \
                 an integer in 0..={MAX_TOOL_CALL_INDEX}"
            ),
            Self::ToolIdentityWrongType { index } => write!(
                f,
                "SysOM stream: tool {index} id or function name was not a string"
            ),
            Self::ToolIdentityMissing { index } => write!(
                f,
                "SysOM stream: tool {index} was first seen without an id and function name"
            ),
            Self::ToolIdentityChanged { index } => write!(
                f,
                "SysOM stream: tool {index} changed its id or function name between frames"
            ),
            Self::ArgumentsWrongType { index } => {
                write!(f, "SysOM stream: tool {index} arguments were not a string")
            }
            Self::ArgumentsRewritten {
                index,
                previous_bytes,
                new_bytes,
            } => write!(
                f,
                "SysOM stream: tool {index} cumulative arguments are not an extension of the \
                 previous frame (previous {previous_bytes} bytes, new {new_bytes} bytes)"
            ),
        }
    }
}

/// Per-tool cumulative state, keyed by the provider-reported tool index.
#[derive(Default)]
struct SseToolState {
    /// Provider `index`, or the array position when the provider omits it.
    index: u32,
    /// Tool call id from the frame that first revealed this tool. Later frames
    /// must repeat it, because Core has already bound the id to this slot.
    id: String,
    /// Function name from the frame that first revealed this tool.
    name: String,
    /// Last complete cumulative arguments snapshot for this tool.
    arguments: String,
    /// Whether `ToolCallEnd` has already been emitted.
    ended: bool,
}

#[derive(Default)]
struct SseParseState {
    /// Last complete cumulative assistant text; new snapshots must extend it.
    last_content: String,
    /// Tool state in first-seen order, addressed by provider index.
    tools: Vec<SseToolState>,
    /// Whether a `Failed` frame already ended the message; later frames are
    /// ignored.
    message_ended: bool,
    /// Set on every terminal path — EOF, parse failure, provider failure,
    /// transport error, cancellation. Once set, the stream stops reading and
    /// returns `None` after draining what is already queued.
    stream_ended: bool,
    /// Latest usage info (SysOM repeats it per frame, emitted once at EOF).
    latest_usage: Option<(u32, u32, u32, u32)>,
}

impl SseParseState {
    fn tool_mut(&mut self, index: u32) -> Option<&mut SseToolState> {
        self.tools.iter_mut().find(|tool| tool.index == index)
    }
}

/// Convert one SSE event block (the text before a blank-line terminator) into
/// zero or more incremental events.
///
/// SysOM frames are cumulative: a single frame can carry new assistant text, a
/// newly visible tool call, and that call's complete arguments at once. Emitting
/// only the first of those loses the arguments whenever no later frame repeats
/// the tool call, so every observation in the frame is returned in order:
/// `TextDelta`, then `ToolCallStart` / `ToolCallDelta` per tool.
///
/// # Errors
///
/// Returns a bounded [`SysomStreamError`] when the payload is not valid JSON,
/// carries wrong field types, or when a cumulative snapshot shrinks or is
/// rewritten — silently concatenating a rewritten snapshot would corrupt the
/// reassembled arguments.
fn parse_sysom_sse_events(
    block: &str,
    state: &mut SseParseState,
) -> Result<Vec<GenerateEvent>, SysomStreamError> {
    if state.message_ended {
        return Ok(Vec::new());
    }

    let mut event_type = String::new();
    // SSE allows one event to spread its payload over several `data:` lines,
    // joined by newlines. Keeping only the last line would truncate a
    // pretty-printed or split payload to its final fragment.
    let mut data_str = String::new();
    let mut has_data = false;

    for line in block.lines() {
        if let Some(val) = line.strip_prefix("event:") {
            event_type = val.trim().to_string();
        } else if let Some(val) = line.strip_prefix("data:") {
            if has_data {
                data_str.push('\n');
            }
            // Per the SSE spec only a single leading space is separator.
            data_str.push_str(val.strip_prefix(' ').unwrap_or(val));
            has_data = true;
        }
        // id: line is ignored
    }

    if event_type == "Failed" {
        state.message_ended = true;
        // A provider failure is terminal: EOF bookkeeping must not follow it
        // with a clean `MessageEnd` over a turn that failed.
        state.stream_ended = true;
        return Ok(vec![GenerateEvent::Error(format!(
            "SysOM stream failed: {}",
            failure_summary(&data_str)
        ))]);
    }

    if event_type != "OK" || data_str.trim().is_empty() {
        return Ok(Vec::new());
    }

    let data: Value =
        serde_json::from_str(&data_str).map_err(|_| SysomStreamError::MalformedJson)?;
    // A non-object root has no place for `choices` or `usage`; treating it as
    // "nothing in this frame" would let malformed provider output finish the
    // turn with a clean `MessageEnd`.
    if !data.is_object() {
        return Err(SysomStreamError::RootWrongType);
    }

    let mut events = Vec::new();
    // Absent or null containers mean "nothing in this frame", but a present
    // container of the wrong shape is a schema violation: skipping it would
    // drop the frame's text or tool calls while the turn still finishes
    // cleanly — the silent-loss class this parser exists to refuse.
    let message = match data.get("choices") {
        None | Some(Value::Null) => None,
        Some(Value::Array(choices)) => match choices.first() {
            None | Some(Value::Null) => None,
            Some(Value::Object(choice)) => match choice.get("message") {
                None | Some(Value::Null) => None,
                Some(message @ Value::Object(_)) => Some(message),
                Some(_) => return Err(SysomStreamError::MessageWrongType),
            },
            Some(_) => return Err(SysomStreamError::ChoicesWrongType),
        },
        Some(_) => return Err(SysomStreamError::ChoicesWrongType),
    };

    if let Some(message) = message {
        if let Some(delta) = take_content_delta(message, state)? {
            events.push(GenerateEvent::TextDelta(delta));
        }
        match message.get("tool_use") {
            None | Some(Value::Null) => {}
            Some(Value::Array(tool_use)) => collect_tool_events(tool_use, state, &mut events)?,
            Some(_) => return Err(SysomStreamError::ToolUseWrongType),
        }
    }

    if let Some(usage) = data.get("usage").and_then(|u| u.as_object()) {
        let prompt = usage
            .get("prompt_tokens")
            .or_else(|| usage.get("input_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let completion = usage
            .get("completion_tokens")
            .or_else(|| usage.get("output_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let total = usage
            .get("total_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let cached = crate::provider::extract_cached_tokens(usage);
        state.latest_usage = Some((prompt, completion, total, cached));
    }

    Ok(events)
}

/// Extract the newly appended assistant text from a cumulative frame.
fn take_content_delta(
    message: &Value,
    state: &mut SseParseState,
) -> Result<Option<String>, SysomStreamError> {
    // A frame that omits `content` (or sends null/empty) carries no snapshot:
    // treat it as "no change" instead of a rewrite, since providers may drop the
    // field once they switch to streaming tool calls.
    let content = match message.get("content") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(text)) if text.is_empty() => return Ok(None),
        Some(Value::String(text)) => text.as_str(),
        Some(_) => return Err(SysomStreamError::ContentWrongType),
    };

    let Some(delta) = content.strip_prefix(state.last_content.as_str()) else {
        return Err(SysomStreamError::ContentRewritten {
            previous_bytes: state.last_content.len(),
            new_bytes: content.len(),
        });
    };
    if delta.is_empty() {
        return Ok(None);
    }
    let delta = delta.to_string();
    state.last_content = content.to_string();
    Ok(Some(delta))
}

/// Read an optional tool identity field.
///
/// Absent, null, and blank are all "not reported in this frame"; a non-string is a
/// schema violation, because silently coercing it would invent an identity.
fn identity_field(value: Option<&Value>, index: u32) -> Result<Option<&str>, SysomStreamError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.trim().is_empty() => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.as_str())),
        Some(_) => Err(SysomStreamError::ToolIdentityWrongType { index }),
    }
}

/// Append start/delta events for every tool call observed in a cumulative frame.
fn collect_tool_events(
    tool_use: &[Value],
    state: &mut SseParseState,
    events: &mut Vec<GenerateEvent>,
) -> Result<(), SysomStreamError> {
    for (position, entry) in tool_use.iter().enumerate() {
        if !entry.is_object() {
            return Err(SysomStreamError::ToolEntryWrongType { position });
        }
        // Provider index is authoritative; the array position is only a fallback
        // for providers that omit it entirely. A present but unusable index is a
        // hard error: falling back to the position would misroute deltas into a
        // slot the provider never named. The upper bound matters because Core
        // sizes its pending-call vector from this number.
        let index = match entry.get("index") {
            // An array longer than the protocol allows is itself out of range.
            None if position > MAX_TOOL_CALL_INDEX as usize => {
                return Err(SysomStreamError::ToolIndexInvalid { position })
            }
            None => position as u32,
            Some(value) => match value.as_u64() {
                Some(index) if index <= u64::from(MAX_TOOL_CALL_INDEX) => index as u32,
                _ => return Err(SysomStreamError::ToolIndexInvalid { position }),
            },
        };

        // As with `content`, a missing or empty arguments field is "no snapshot
        // in this frame" rather than a snapshot that shrank to nothing.
        let arguments = match entry.get("function").and_then(|f| f.get("arguments")) {
            None | Some(Value::Null) => None,
            Some(Value::String(args)) if args.is_empty() => None,
            Some(Value::String(args)) => Some(args.as_str()),
            Some(_) => return Err(SysomStreamError::ArgumentsWrongType { index }),
        };

        let id = identity_field(entry.get("id"), index)?;
        let name = identity_field(entry.get("function").and_then(|f| f.get("name")), index)?;

        match state.tool_mut(index) {
            // Core keys tool results by id and dispatches by name, so a slot that
            // starts without both would accumulate arguments it can never execute.
            None => {
                let (Some(id), Some(name)) = (id, name) else {
                    return Err(SysomStreamError::ToolIdentityMissing { index });
                };
                state.tools.push(SseToolState {
                    index,
                    id: id.to_string(),
                    name: name.to_string(),
                    ..SseToolState::default()
                });
                events.push(GenerateEvent::ToolCallStart {
                    index,
                    id: id.to_string(),
                    name: name.to_string(),
                });
            }
            // Later frames may omit the identity, but must not contradict it:
            // subsequent arguments still land in the slot opened above.
            Some(tool) => {
                let id_conflict = id.is_some_and(|id| id != tool.id);
                let name_conflict = name.is_some_and(|name| name != tool.name);
                if id_conflict || name_conflict {
                    return Err(SysomStreamError::ToolIdentityChanged { index });
                }
            }
        }

        let Some(arguments) = arguments else {
            continue;
        };
        // `tool_mut` was just ensured to exist for this index.
        let Some(tool) = state.tool_mut(index) else {
            continue;
        };
        let Some(delta) = arguments.strip_prefix(tool.arguments.as_str()) else {
            return Err(SysomStreamError::ArgumentsRewritten {
                index,
                previous_bytes: tool.arguments.len(),
                new_bytes: arguments.len(),
            });
        };
        if delta.is_empty() {
            continue;
        }
        let delta = delta.to_string();
        tool.arguments = arguments.to_string();
        events.push(GenerateEvent::ToolCallDelta {
            index,
            arguments_delta: delta,
        });
    }
    Ok(())
}

/// Terminal events for a stream that reached EOF without an explicit failure:
/// close every open tool call, then report usage, then end the message.
fn sysom_eof_events(state: &mut SseParseState) -> Vec<GenerateEvent> {
    let mut events = Vec::new();
    for tool in &mut state.tools {
        if !tool.ended {
            tool.ended = true;
            events.push(GenerateEvent::ToolCallEnd { index: tool.index });
        }
    }
    if let Some((prompt_tokens, completion_tokens, total_tokens, cached_tokens)) =
        state.latest_usage.take()
    {
        events.push(GenerateEvent::Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cached_tokens,
        });
    }
    events.push(GenerateEvent::MessageEnd);
    state.stream_ended = true;
    events
}

/// Describe a provider failure payload without echoing any of it.
///
/// Even a short prefix of the raw `data:` field can hold credentials, prompt text,
/// or the question the user was about to be asked, so the summary carries only an
/// allowlisted error code, the payload size, and a truncated digest for support.
fn failure_summary(data: &str) -> String {
    let trimmed = data.trim();
    let bytes = trimmed.len();
    match forwardable_error_code(trimmed) {
        Some(code) => format!("provider error code {code} ({bytes} byte payload)"),
        None => format!(
            "no recognizable provider error code ({bytes} byte payload, sha256:{})",
            digest_prefix(trimmed.as_bytes())
        ),
    }
}

/// Provider error codes that may be reported verbatim.
///
/// This is an exact allowlist rather than a character filter, because a filter is
/// not a privacy boundary: `sk-live-abc123` and `10.0.0.7` are also short and
/// opaque, so a syntax rule would forward secrets that happen to arrive in a
/// `code` field. Extend this list only with codes documented by the provider.
const FORWARDABLE_ERROR_CODES: &[&str] = &[
    "AccessDenied",
    "Forbidden",
    "InternalError",
    "InvalidAccessKeyId",
    "InvalidApiKey",
    "InvalidParameter",
    "MissingParameter",
    "ModelNotFound",
    "QuotaExhausted",
    "RequestTimeout",
    "ServiceUnavailable",
    "Throttling",
    "Throttling.Api",
    "Throttling.User",
    "Unauthorized",
    "context_length_exceeded",
    "insufficient_quota",
    "invalid_request_error",
    "rate_limit_exceeded",
    "server_error",
];

/// Match a failure payload's error code against [`FORWARDABLE_ERROR_CODES`].
///
/// Returns the matched `&'static str`, never the payload's own bytes, so an
/// unknown or crafted code cannot reach the caller even in part.
fn forwardable_error_code(data: &str) -> Option<&'static str> {
    let value: Value = serde_json::from_str(data).ok()?;
    let candidate = ["code", "Code", "error_code", "errorCode"]
        .iter()
        .find_map(|key| value.get(*key).and_then(|v| v.as_str()))
        .or_else(|| {
            value
                .get("error")
                .and_then(|error| error.get("code"))
                .and_then(|v| v.as_str())
        })?;
    FORWARDABLE_ERROR_CODES
        .iter()
        .find(|known| known.eq_ignore_ascii_case(candidate))
        .copied()
}

/// First 16 hex chars of the payload digest: enough to correlate two reports of
/// the same failure, not reversible into the payload.
fn digest_prefix(bytes: &[u8]) -> String {
    hex_sha256(bytes).chars().take(16).collect()
}

/// Drive the cumulative SSE state machine over a byte stream.
///
/// Events parsed from one frame are queued and drained before the next frame is
/// read, so a frame that produces several events never loses any of them.
pub(super) fn sysom_event_stream(
    bytes: SysomByteStream,
    cancelled: Arc<AtomicBool>,
) -> GenerateStream {
    let initial = SysomStreamState {
        bytes,
        buf: Vec::new(),
        scanned: 0,
        cancelled,
        parse: SseParseState::default(),
        pending: VecDeque::new(),
    };

    Box::pin(futures::stream::unfold(initial, |mut state| async move {
        loop {
            // Cancellation is terminal: report it once, drop anything still
            // queued, and end the stream instead of repeating `Cancelled` on
            // every subsequent poll.
            if state.cancelled.load(Ordering::SeqCst) && !state.parse.stream_ended {
                state.parse.stream_ended = true;
                state.pending.clear();
                return Some((GenerateEvent::Cancelled, state));
            }
            // Pending events queued before the stream ended (EOF terminal
            // events included) drain exactly once before `None`.
            if let Some(event) = state.pending.pop_front() {
                return Some((event, state));
            }
            if state.parse.stream_ended {
                return None;
            }

            if let Some((pos, delimiter)) = state.take_block_end() {
                // A terminator does not lift the ceiling: decoding an oversized
                // block would still copy an arbitrarily large payload into the
                // String, parse state, and event pipeline.
                if pos > MAX_SSE_BLOCK_BYTES {
                    state.parse.stream_ended = true;
                    let error = SysomStreamError::BlockTooLarge {
                        buffered_bytes: pos,
                        limit: MAX_SSE_BLOCK_BYTES,
                    };
                    return Some((GenerateEvent::Error(error.to_string()), state));
                }
                let event_block = match decode_block(&state.buf[..pos]) {
                    Ok(text) => text.to_string(),
                    Err(e) => {
                        state.parse.stream_ended = true;
                        return Some((GenerateEvent::Error(e.to_string()), state));
                    }
                };
                state.buf.drain(..pos + delimiter);

                if event_block.trim().is_empty() {
                    continue;
                }
                match parse_sysom_sse_events(&event_block, &mut state.parse) {
                    Ok(events) => state.pending.extend(events),
                    Err(e) => {
                        state.parse.stream_ended = true;
                        return Some((GenerateEvent::Error(e.to_string()), state));
                    }
                }
                continue;
            }

            // Reached only when the whole buffer holds no terminator, so every
            // buffered byte belongs to one unfinished block. A peer that never
            // sends a blank line would otherwise grow this buffer without bound.
            if state.buf.len() > MAX_SSE_BLOCK_BYTES {
                state.parse.stream_ended = true;
                let error = SysomStreamError::BlockTooLarge {
                    buffered_bytes: state.buf.len(),
                    limit: MAX_SSE_BLOCK_BYTES,
                };
                return Some((GenerateEvent::Error(error.to_string()), state));
            }

            match state.bytes.next().await {
                // Buffered as bytes, not text: HTTP chunk boundaries fall wherever
                // the network puts them, and decoding a chunk that ends mid
                // character would substitute U+FFFD inside otherwise valid JSON —
                // silently corrupting non-ASCII question text, paths, or arguments.
                Some(Ok(chunk)) => {
                    state.buf.extend_from_slice(&chunk);
                }
                Some(Err(e)) => {
                    // Transport failures are terminal too: nothing after a
                    // broken connection can be trusted to extend the turn.
                    state.parse.stream_ended = true;
                    return Some((GenerateEvent::Error(format!("stream error: {e}")), state));
                }
                None => {
                    // Flush a trailing frame that arrived without its blank line.
                    let trailing = match decode_block(&state.buf) {
                        Ok(text) => text.to_string(),
                        Err(e) => {
                            state.parse.stream_ended = true;
                            return Some((GenerateEvent::Error(e.to_string()), state));
                        }
                    };
                    state.buf.clear();
                    if !trailing.trim().is_empty() {
                        let event_block = trailing.trim().to_string();
                        match parse_sysom_sse_events(&event_block, &mut state.parse) {
                            Ok(events) => state.pending.extend(events),
                            Err(e) => {
                                state.parse.stream_ended = true;
                                return Some((GenerateEvent::Error(e.to_string()), state));
                            }
                        }
                    }
                    // A trailing `Failed` frame already ended the stream: EOF
                    // bookkeeping must not queue a successful `MessageEnd`
                    // right after its error.
                    if !state.parse.stream_ended {
                        let terminal = sysom_eof_events(&mut state.parse);
                        state.pending.extend(terminal);
                    }
                    continue;
                }
            }
        }
    }))
}

/// Largest single SSE block accepted, with or without its terminator.
///
/// Frames are cumulative snapshots, so a long answer legitimately produces large
/// frames; the limit only needs to sit far above that to stop a peer from
/// growing the buffer — or a decoded block — without bound.
const MAX_SSE_BLOCK_BYTES: usize = 4 * 1024 * 1024;

/// Decode one complete SSE block.
///
/// Decoding happens per block rather than per chunk, so a multi-byte character
/// split across two HTTP chunks is reassembled before it is interpreted.
///
/// # Errors
///
/// Returns [`SysomStreamError::InvalidUtf8`] when the reassembled block is still
/// not valid UTF-8. Lossy decoding is not an option: a replacement character
/// inside a JSON string leaves the frame syntactically valid, so corrupted
/// question text, paths, or tool arguments would reach execution looking clean.
fn decode_block(bytes: &[u8]) -> Result<&str, SysomStreamError> {
    std::str::from_utf8(bytes).map_err(|error| SysomStreamError::InvalidUtf8 {
        byte_offset: error.valid_up_to(),
        block_bytes: bytes.len(),
    })
}

struct SysomStreamState {
    bytes: SysomByteStream,
    /// Raw, still-undecoded bytes: see [`decode_block`].
    buf: Vec<u8>,
    /// How much of `buf` is already known to hold no block terminator.
    scanned: usize,
    cancelled: Arc<AtomicBool>,
    parse: SseParseState,
    /// Events parsed from the current frame that still need to be yielded.
    pending: VecDeque<GenerateEvent>,
}

impl SysomStreamState {
    /// Offset and byte length of the delimiter terminating the first buffered
    /// block, if any.
    ///
    /// SSE lines end in LF or CRLF, so a block terminator is any two
    /// consecutive line endings: `\n\n`, `\n\r\n`, `\r\n\n`, or `\r\n\r\n`.
    ///
    /// Resumes from the last scan position so each byte is examined a bounded
    /// number of times no matter how the provider chunks the stream; without
    /// that, a long unterminated block would be rescanned from the start per
    /// chunk.
    fn take_block_end(&mut self) -> Option<(usize, usize)> {
        for position in self.scanned..self.buf.len() {
            let Some(first) = line_end_at(&self.buf, position) else {
                continue;
            };
            let Some(second) = line_end_at(&self.buf, position + first) else {
                continue;
            };
            // The caller consumes through the terminator, so the next scan
            // starts at the beginning of what remains.
            self.scanned = 0;
            return Some((position, first + second));
        }
        // Keep the final bytes in view: the longest delimiter is four bytes,
        // so up to three of them may already be buffered while the rest is
        // still in flight in the next chunk.
        self.scanned = self.buf.len().saturating_sub(3);
        None
    }
}

/// Byte length of the LF or CRLF line ending starting at `position`, if any.
///
/// A trailing `\r` alone is not a line ending: the `\n` completing it may
/// still be in flight, so it stays buffered until the next chunk decides.
fn line_end_at(buf: &[u8], position: usize) -> Option<usize> {
    match buf.get(position)? {
        b'\n' => Some(1),
        b'\r' if buf.get(position + 1) == Some(&b'\n') => Some(2),
        _ => None,
    }
}

#[cfg(test)]
#[path = "stream/tests.rs"]
mod tests;
