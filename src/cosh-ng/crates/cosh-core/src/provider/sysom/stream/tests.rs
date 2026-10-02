//! Cumulative SSE reassembly: event ordering, tool identity, bounded failures.

use super::*;
use std::pin::Pin;
use std::task::{Context, Poll};

const ASK_ARGS: &str = r#"{"question":"How should local changes be handled?"}"#;

/// Render events as compact strings so ordering assertions read as one list.
fn summarize(events: &[GenerateEvent]) -> Vec<String> {
    events
        .iter()
        .map(|event| match event {
            GenerateEvent::TextDelta(text) => format!("text:{text}"),
            GenerateEvent::ThinkingDelta(text) => format!("thinking:{text}"),
            GenerateEvent::ToolCallStart { index, id, name } => {
                format!("start:{index}:{id}:{name}")
            }
            GenerateEvent::ToolCallDelta {
                index,
                arguments_delta,
            } => format!("delta:{index}:{arguments_delta}"),
            GenerateEvent::ToolCallEnd { index } => format!("end:{index}"),
            GenerateEvent::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
                cached_tokens: _,
            } => format!("usage:{prompt_tokens}:{completion_tokens}:{total_tokens}"),
            GenerateEvent::MessageEnd => "message_end".to_string(),
            GenerateEvent::Cancelled => "cancelled".to_string(),
            GenerateEvent::Error(message) => format!("error:{message}"),
        })
        .collect()
}

fn frame(data: Value) -> String {
    format!("event: OK\ndata: {data}\n\n")
}

fn tool_entry(index: u32, id: &str, name: &str, arguments: &str) -> Value {
    serde_json::json!({
        "index": index,
        "id": id,
        "type": "function",
        "function": { "name": name, "arguments": arguments },
    })
}

fn message_frame(content: Option<&str>, tool_use: Vec<Value>) -> Value {
    let mut message = serde_json::Map::new();
    if let Some(content) = content {
        message.insert("content".to_string(), serde_json::json!(content));
    }
    if !tool_use.is_empty() {
        message.insert("tool_use".to_string(), serde_json::json!(tool_use));
    }
    serde_json::json!({ "choices": [{ "message": Value::Object(message) }] })
}

fn parse(block: &str, state: &mut SseParseState) -> Vec<String> {
    summarize(&parse_sysom_sse_events(block, state).expect("frame parses"))
}

fn parse_err(block: &str, state: &mut SseParseState) -> SysomStreamError {
    parse_sysom_sse_events(block, state).expect_err("frame must be rejected")
}

async fn collect_stream(frames: Vec<String>) -> Vec<String> {
    collect_byte_chunks(frames.into_iter().map(String::into_bytes).collect()).await
}

/// Feed the state machine raw chunks, so tests can place a chunk boundary
/// anywhere — including inside a multi-byte character.
async fn collect_byte_chunks(chunks: Vec<Vec<u8>>) -> Vec<String> {
    collect_stream_items(chunks.into_iter().map(Ok).collect(), false).await
}

/// Feed raw stream items — including transport errors — and collect to EOF,
/// optionally with the cancellation flag already raised.
async fn collect_stream_items(items: Vec<Result<Vec<u8>, String>>, cancelled: bool) -> Vec<String> {
    let bytes: SysomByteStream = Box::pin(futures::stream::iter(items));
    let stream = sysom_event_stream(bytes, Arc::new(AtomicBool::new(cancelled)));
    summarize(&stream.collect::<Vec<_>>().await)
}

/// The incident shape: the first frame that reveals the tool call already holds
/// complete arguments and no later frame repeats it.
#[tokio::test]
async fn first_tool_frame_with_complete_arguments_reaches_core() {
    let frames = vec![
        frame(message_frame(Some("Let me check with you first."), vec![])),
        frame(message_frame(
            Some("Let me check with you first."),
            vec![tool_entry(0, "call-1", "ask_user_question", ASK_ARGS)],
        )),
    ];

    assert_eq!(
        collect_stream(frames).await,
        vec![
            "text:Let me check with you first.".to_string(),
            "start:0:call-1:ask_user_question".to_string(),
            format!("delta:0:{ASK_ARGS}"),
            "end:0".to_string(),
            "message_end".to_string(),
        ]
    );
}

/// HTTP chunk boundaries land wherever the network puts them. Decoding per chunk
/// would insert U+FFFD inside a character that spans two chunks, corrupting
/// Chinese question text or paths while leaving the JSON syntactically valid.
#[tokio::test]
async fn multibyte_characters_survive_every_chunk_boundary() {
    let question = "是否保留本地改动？";
    let arguments = format!(r#"{{"question":"{question}","options":[{{"label":"保留"}}]}}"#);
    let block = frame(message_frame(
        Some("正在检查本地改动……"),
        vec![tool_entry(0, "call-1", "ask_user_question", &arguments)],
    ));

    let whole = collect_byte_chunks(vec![block.clone().into_bytes()]).await;
    assert!(
        whole.iter().any(|event| event.contains(question)),
        "baseline must carry the question text: {whole:?}"
    );

    let raw = block.as_bytes();
    for split in 1..raw.len() {
        let events = collect_byte_chunks(vec![raw[..split].to_vec(), raw[split..].to_vec()]).await;
        assert_eq!(
            events, whole,
            "chunk split at byte {split} changed the stream"
        );
        assert!(
            !events.iter().any(|event| event.contains('\u{fffd}')),
            "chunk split at byte {split} corrupted a character: {events:?}"
        );
    }
}

/// Reassembly cannot repair bytes the provider never encoded correctly. Replacing
/// them with U+FFFD would keep the JSON valid, so corrupted text or arguments
/// would reach execution looking clean — the frame must be refused instead.
#[tokio::test]
async fn invalid_utf8_is_refused_rather_than_repaired() {
    // The bad byte sits inside a JSON string, so the frame stays parseable.
    let cases: &[(&str, &[u8], &[u8])] = &[
        (
            "content",
            br#"event: OK
data: {"choices":[{"message":{"content":"local diff "#,
            br#""}}]}

"#,
        ),
        (
            "arguments",
            br#"event: OK
data: {"choices":[{"message":{"tool_use":[{"index":0,"id":"call-1","function":{"name":"ask_user_question","arguments":"{\"question\":\"keep "#,
            br#"?\"}"}}]}}]}

"#,
        ),
    ];

    for (field, head, tail) in cases {
        let mut raw = head.to_vec();
        // A lone continuation byte: invalid however the stream is chunked.
        raw.push(0xff);
        raw.extend_from_slice(tail);
        let block_bytes = raw.len() - 2;

        for chunks in [
            vec![raw.clone()],
            vec![raw[..head.len()].to_vec(), raw[head.len()..].to_vec()],
        ] {
            let events = collect_byte_chunks(chunks).await;
            assert_eq!(
                events,
                vec![format!(
                    "error:{}",
                    SysomStreamError::InvalidUtf8 {
                        byte_offset: head.len(),
                        block_bytes,
                    }
                )],
                "invalid {field} bytes must fail the stream"
            );
            assert!(
                !events.iter().any(|event| event.contains('\u{fffd}')),
                "invalid {field} bytes must not be repaired: {events:?}"
            );
        }
    }
}

/// Nothing forces the peer to ever send `\n\n`, so the buffer needs its own
/// ceiling; a single huge chunk and a long drip of small ones must both hit it.
#[tokio::test]
async fn unterminated_blocks_are_bounded() {
    let oversized = vec![b'x'; MAX_SSE_BLOCK_BYTES + 1];
    let events = collect_byte_chunks(vec![oversized]).await;
    assert_eq!(
        events,
        vec![format!(
            "error:{}",
            SysomStreamError::BlockTooLarge {
                buffered_bytes: MAX_SSE_BLOCK_BYTES + 1,
                limit: MAX_SSE_BLOCK_BYTES,
            }
        )]
    );

    let chunk_bytes = 64 * 1024;
    let drip = vec![vec![b'x'; chunk_bytes]; MAX_SSE_BLOCK_BYTES / chunk_bytes + 2];
    let events = collect_byte_chunks(drip).await;
    match events.as_slice() {
        [only] => {
            assert!(only.starts_with("error:"), "{only}");
            assert!(only.contains("exceeds the"), "{only}");
        }
        other => panic!("expected a single bounded error, got {other:?}"),
    }

    // A block just under the limit still parses: the ceiling is not a size cap on
    // legitimately large cumulative frames.
    let padding = "y".repeat(MAX_SSE_BLOCK_BYTES - 4096);
    let large = frame(message_frame(Some(&padding), vec![]));
    assert!(large.len() < MAX_SSE_BLOCK_BYTES, "fixture must fit");
    assert_eq!(
        collect_byte_chunks(vec![large.into_bytes()]).await,
        vec![format!("text:{padding}"), "message_end".to_string()]
    );
}

/// A terminator must not lift the ceiling: an oversized block followed by
/// `\n\n` in the same chunk would otherwise be decoded and copied whole.
#[tokio::test]
async fn oversized_terminated_block_is_rejected_before_decoding() {
    let mut chunk = vec![b'x'; MAX_SSE_BLOCK_BYTES + 1];
    chunk.extend_from_slice(b"\n\n");
    assert_eq!(
        collect_byte_chunks(vec![chunk]).await,
        vec![format!(
            "error:{}",
            SysomStreamError::BlockTooLarge {
                buffered_bytes: MAX_SSE_BLOCK_BYTES + 1,
                limit: MAX_SSE_BLOCK_BYTES,
            }
        )]
    );
}

/// The per-block ceiling must not reject a chunk that carries several small
/// blocks whose combined size exceeds the limit.
#[tokio::test]
async fn multiple_blocks_in_one_chunk_all_parse() {
    let chunk = format!(
        "{}{}",
        frame(message_frame(Some("hi"), vec![])),
        frame(message_frame(
            Some("hi"),
            vec![tool_entry(0, "call-1", "ask_user_question", ASK_ARGS)],
        ))
    );

    assert_eq!(
        collect_byte_chunks(vec![chunk.into_bytes()]).await,
        vec![
            "text:hi".to_string(),
            "start:0:call-1:ask_user_question".to_string(),
            format!("delta:0:{ASK_ARGS}"),
            "end:0".to_string(),
            "message_end".to_string(),
        ]
    );
}

/// SSE permits CRLF line endings; a CRLF stream must not buffer until EOF and
/// collapse its events, and the four-byte delimiter must survive any chunk
/// boundary — including every split inside the delimiter itself.
#[tokio::test]
async fn crlf_delimited_events_parse_at_every_chunk_boundary() {
    let raw = format!(
        "{}{}",
        frame(message_frame(Some("Checking."), vec![])),
        frame(message_frame(
            Some("Checking."),
            vec![tool_entry(0, "call-1", "ask_user_question", ASK_ARGS)],
        ))
    )
    .replace('\n', "\r\n")
    .into_bytes();
    let expected = vec![
        "text:Checking.".to_string(),
        "start:0:call-1:ask_user_question".to_string(),
        format!("delta:0:{ASK_ARGS}"),
        "end:0".to_string(),
        "message_end".to_string(),
    ];

    assert_eq!(collect_byte_chunks(vec![raw.clone()]).await, expected);
    for split in 1..raw.len() {
        assert_eq!(
            collect_byte_chunks(vec![raw[..split].to_vec(), raw[split..].to_vec()]).await,
            expected,
            "chunk split at byte {split} changed the CRLF stream"
        );
    }
}

#[test]
fn single_frame_keeps_text_start_and_delta_in_order() {
    let mut state = SseParseState::default();
    let block = frame(message_frame(
        Some("Checking."),
        vec![tool_entry(0, "call-1", "ask_user_question", ASK_ARGS)],
    ));

    assert_eq!(
        parse(&block, &mut state),
        vec![
            "text:Checking.".to_string(),
            "start:0:call-1:ask_user_question".to_string(),
            format!("delta:0:{ASK_ARGS}"),
        ]
    );
}

#[test]
fn growing_arguments_emit_only_the_new_suffix() {
    let mut state = SseParseState::default();
    let first = frame(message_frame(
        None,
        vec![tool_entry(
            0,
            "call-1",
            "ask_user_question",
            r#"{"question":"#,
        )],
    ));
    let second = frame(message_frame(
        None,
        vec![tool_entry(0, "call-1", "ask_user_question", ASK_ARGS)],
    ));

    assert_eq!(
        parse(&first, &mut state),
        vec![
            "start:0:call-1:ask_user_question".to_string(),
            r#"delta:0:{"question":"#.to_string(),
        ]
    );
    assert_eq!(
        parse(&second, &mut state),
        vec![r#"delta:0:"How should local changes be handled?"}"#.to_string()]
    );
}

#[test]
fn repeated_cumulative_frame_emits_nothing() {
    let mut state = SseParseState::default();
    let block = frame(message_frame(
        Some("Checking."),
        vec![tool_entry(0, "call-1", "ask_user_question", ASK_ARGS)],
    ));

    assert_eq!(parse(&block, &mut state).len(), 3);
    assert!(parse(&block, &mut state).is_empty());
    assert!(parse(&block, &mut state).is_empty());
}

#[test]
fn multiple_new_tool_calls_in_one_frame_are_all_preserved() {
    let mut state = SseParseState::default();
    let block = frame(message_frame(
        None,
        vec![
            tool_entry(0, "call-a", "read_file", r#"{"path":"a"}"#),
            tool_entry(1, "call-b", "ask_user_question", ASK_ARGS),
        ],
    ));

    assert_eq!(
        parse(&block, &mut state),
        vec![
            "start:0:call-a:read_file".to_string(),
            r#"delta:0:{"path":"a"}"#.to_string(),
            "start:1:call-b:ask_user_question".to_string(),
            format!("delta:1:{ASK_ARGS}"),
        ]
    );
}

/// Provider index is authoritative: a tool that appears at array position 0 but
/// reports index 3 must keep routing its deltas to index 3.
#[test]
fn provider_index_is_not_confused_with_array_position() {
    let mut state = SseParseState::default();
    let first = frame(message_frame(
        None,
        vec![tool_entry(3, "call-x", "ask_user_question", r#"{"q"#)],
    ));
    let second = frame(message_frame(
        None,
        vec![tool_entry(
            3,
            "call-x",
            "ask_user_question",
            r#"{"question":"hi"}"#,
        )],
    ));

    assert_eq!(
        parse(&first, &mut state),
        vec![
            "start:3:call-x:ask_user_question".to_string(),
            r#"delta:3:{"q"#.to_string(),
        ]
    );
    assert_eq!(
        parse(&second, &mut state),
        vec![r#"delta:3:uestion":"hi"}"#.to_string()]
    );
}

/// Providers that omit `index` fall back to the array position, and the fallback
/// must stay stable across frames.
#[test]
fn missing_index_falls_back_to_array_position() {
    let mut state = SseParseState::default();
    let entry = |arguments: &str| {
        serde_json::json!({
            "id": "call-1",
            "function": { "name": "ask_user_question", "arguments": arguments },
        })
    };
    let first = frame(message_frame(None, vec![entry(r#"{"que"#)]));
    let second = frame(message_frame(None, vec![entry(r#"{"question":"hi"}"#)]));

    assert_eq!(
        parse(&first, &mut state),
        vec![
            "start:0:call-1:ask_user_question".to_string(),
            r#"delta:0:{"que"#.to_string(),
        ]
    );
    assert_eq!(
        parse(&second, &mut state),
        vec![r#"delta:0:stion":"hi"}"#.to_string()]
    );
}

#[test]
fn shrinking_or_rewritten_arguments_fail_loudly() {
    let mut state = SseParseState::default();
    let full = frame(message_frame(
        None,
        vec![tool_entry(0, "call-1", "ask_user_question", ASK_ARGS)],
    ));
    assert_eq!(parse(&full, &mut state).len(), 2);

    let shrunk = frame(message_frame(
        None,
        vec![tool_entry(
            0,
            "call-1",
            "ask_user_question",
            r#"{"question":"#,
        )],
    ));
    assert_eq!(
        parse_err(&shrunk, &mut state),
        SysomStreamError::ArgumentsRewritten {
            index: 0,
            previous_bytes: ASK_ARGS.len(),
            new_bytes: r#"{"question":"#.len(),
        }
    );

    let rewritten = frame(message_frame(
        None,
        vec![tool_entry(
            0,
            "call-1",
            "ask_user_question",
            r#"{"other":"x"}"#,
        )],
    ));
    assert!(matches!(
        parse_err(&rewritten, &mut state),
        SysomStreamError::ArgumentsRewritten { index: 0, .. }
    ));
}

#[test]
fn rewritten_content_fails_loudly() {
    let mut state = SseParseState::default();
    assert_eq!(
        parse(
            &frame(message_frame(Some("hello world"), vec![])),
            &mut state
        ),
        vec!["text:hello world".to_string()]
    );
    assert_eq!(
        parse_err(&frame(message_frame(Some("hello"), vec![])), &mut state),
        SysomStreamError::ContentRewritten {
            previous_bytes: 11,
            new_bytes: 5,
        }
    );
}

#[test]
fn malformed_json_and_wrong_types_fail_loudly() {
    let mut state = SseParseState::default();
    assert_eq!(
        parse_err("event: OK\ndata: {\"choices\":", &mut state),
        SysomStreamError::MalformedJson
    );

    let mut state = SseParseState::default();
    let object_arguments = frame(serde_json::json!({
        "choices": [{ "message": { "tool_use": [{
            "index": 0,
            "id": "call-1",
            "function": { "name": "ask_user_question", "arguments": { "question": "hi" } },
        }] } }]
    }));
    assert_eq!(
        parse_err(&object_arguments, &mut state),
        SysomStreamError::ArgumentsWrongType { index: 0 }
    );

    let mut state = SseParseState::default();
    let numeric_content = frame(serde_json::json!({
        "choices": [{ "message": { "content": 7 } }]
    }));
    assert_eq!(
        parse_err(&numeric_content, &mut state),
        SysomStreamError::ContentWrongType
    );

    let mut state = SseParseState::default();
    let scalar_entry = frame(serde_json::json!({
        "choices": [{ "message": { "tool_use": ["ask_user_question"] } }]
    }));
    assert_eq!(
        parse_err(&scalar_entry, &mut state),
        SysomStreamError::ToolEntryWrongType { position: 0 }
    );
}

/// A present container of the wrong shape must fail instead of degrading to
/// "field absent": `tool_use: {…}` would otherwise drop the whole tool call
/// while the turn still finishes with an empty assistant response.
#[test]
fn wrong_type_containers_fail_loudly_and_null_containers_do_not() {
    let cases = [
        (
            frame(serde_json::json!({ "choices": {} })),
            SysomStreamError::ChoicesWrongType,
        ),
        (
            frame(serde_json::json!({ "choices": "done" })),
            SysomStreamError::ChoicesWrongType,
        ),
        (
            frame(serde_json::json!({ "choices": ["done"] })),
            SysomStreamError::ChoicesWrongType,
        ),
        (
            frame(serde_json::json!({ "choices": [{ "message": 7 }] })),
            SysomStreamError::MessageWrongType,
        ),
        (
            frame(serde_json::json!({ "choices": [{ "message": [] }] })),
            SysomStreamError::MessageWrongType,
        ),
        (
            frame(serde_json::json!({
                "choices": [{ "message": { "tool_use": { "id": "call-1" } } }]
            })),
            SysomStreamError::ToolUseWrongType,
        ),
        (
            frame(serde_json::json!({
                "choices": [{ "message": { "tool_use": "call-1" } }]
            })),
            SysomStreamError::ToolUseWrongType,
        ),
    ];
    for (block, expected) in cases {
        let mut state = SseParseState::default();
        assert_eq!(parse_err(&block, &mut state), expected, "block: {block}");
    }

    // Absent, null, and empty containers stay "nothing in this frame".
    for data in [
        serde_json::json!({}),
        serde_json::json!({ "choices": null }),
        serde_json::json!({ "choices": [] }),
        serde_json::json!({ "choices": [null] }),
        serde_json::json!({ "choices": [{ "message": null }] }),
        serde_json::json!({ "choices": [{ "message": { "tool_use": null } }] }),
    ] {
        let mut state = SseParseState::default();
        assert!(
            parse(&frame(data.clone()), &mut state).is_empty(),
            "tolerated container shape must emit nothing: {data}"
        );
    }
}

/// End to end, a wrong-type container must terminate the stream with a bounded
/// error — never a clean `MessageEnd` over silently dropped content.
#[tokio::test]
async fn wrong_type_containers_never_end_the_message_successfully() {
    let cases = [
        (
            frame(serde_json::json!({ "choices": {} })),
            SysomStreamError::ChoicesWrongType,
        ),
        (
            frame(serde_json::json!({ "choices": [{ "message": 7 }] })),
            SysomStreamError::MessageWrongType,
        ),
        (
            frame(serde_json::json!({
                "choices": [{ "message": {
                    "tool_use": { "id": "call-1", "function": { "name": "shell" } },
                } }]
            })),
            SysomStreamError::ToolUseWrongType,
        ),
    ];
    for (block, expected) in cases {
        let events = collect_stream(vec![
            frame(message_frame(Some("hi"), vec![])),
            block.clone(),
            frame(message_frame(Some("hi there"), vec![])),
        ])
        .await;
        assert_eq!(
            events,
            vec!["text:hi".to_string(), format!("error:{expected}")],
            "block: {block}"
        );
        assert!(
            !events.iter().any(|event| event == "message_end"),
            "dropped content must not end as success: {events:?}"
        );
    }
}

/// A frame root that is not a JSON object has no place for `choices` or
/// `usage`; treating it as empty would let malformed provider output finish
/// the turn with a clean `MessageEnd`.
#[tokio::test]
async fn non_object_frame_roots_never_end_the_message_successfully() {
    for payload in ["null", "[]", "\"done\"", "7", "true"] {
        let events = collect_stream(vec![
            frame(message_frame(Some("hi"), vec![])),
            format!("event: OK\ndata: {payload}\n\n"),
            frame(message_frame(Some("hi there"), vec![])),
        ])
        .await;
        assert_eq!(
            events,
            vec![
                "text:hi".to_string(),
                format!("error:{}", SysomStreamError::RootWrongType),
            ],
            "payload {payload} must fail the stream without message_end"
        );
    }
}

/// Cancellation is terminal: exactly one `Cancelled`, then the stream ends
/// instead of repeating it on every poll or resuming queued events.
#[tokio::test]
async fn cancellation_yields_one_cancelled_event_then_ends() {
    let items = vec![Ok(frame(message_frame(Some("hi"), vec![])).into_bytes())];
    assert_eq!(
        collect_stream_items(items, true).await,
        vec!["cancelled".to_string()]
    );
}

/// A transport failure is terminal: no later chunk can be trusted to extend
/// the turn, and EOF bookkeeping must not follow it with `MessageEnd`.
#[tokio::test]
async fn transport_error_ends_the_stream_without_message_end() {
    let items = vec![
        Ok(frame(message_frame(Some("hi"), vec![])).into_bytes()),
        Err("connection reset".to_string()),
        Ok(frame(message_frame(Some("hi there"), vec![])).into_bytes()),
    ];
    assert_eq!(
        collect_stream_items(items, false).await,
        vec![
            "text:hi".to_string(),
            "error:stream error: connection reset".to_string(),
        ]
    );
}

/// A provider `Failed` frame is terminal end to end: later frames are not
/// parsed and EOF must not report a successful `MessageEnd` for a failed turn.
#[tokio::test]
async fn provider_failure_ends_the_stream_without_message_end() {
    let events = collect_stream(vec![
        frame(message_frame(Some("hi"), vec![])),
        "event: Failed\ndata: {\"code\":\"Throttling\"}\n\n".to_string(),
        frame(message_frame(Some("hi there"), vec![])),
    ])
    .await;
    assert_eq!(
        events,
        vec![
            "text:hi".to_string(),
            "error:SysOM stream failed: provider error code Throttling (21 byte payload)"
                .to_string(),
        ]
    );
}

/// A `Failed` frame flushed at EOF without its blank-line terminator is just
/// as terminal: the EOF bookkeeping that flushed it must not queue a
/// successful `MessageEnd` right after the error.
#[tokio::test]
async fn trailing_unterminated_failed_frame_ends_without_message_end() {
    let events = collect_stream(vec![
        frame(message_frame(Some("hi"), vec![])),
        "event: Failed\ndata: {\"code\":\"Throttling\"}".to_string(),
    ])
    .await;
    assert_eq!(
        events,
        vec![
            "text:hi".to_string(),
            "error:SysOM stream failed: provider error code Throttling (21 byte payload)"
                .to_string(),
        ]
    );
}

/// SSE permits one event to carry several `data:` lines joined by newlines;
/// keeping only the last line would truncate pretty-printed JSON to its final
/// fragment. The joined payload must survive LF and CRLF framing alike.
#[tokio::test]
async fn multiline_data_fields_are_joined_per_sse_semantics() {
    let pretty = serde_json::to_string_pretty(&message_frame(
        Some("Checking."),
        vec![tool_entry(0, "call-1", "ask_user_question", ASK_ARGS)],
    ))
    .expect("fixture serializes");
    assert!(pretty.contains('\n'), "fixture must span multiple lines");
    let data_lines: String = pretty
        .lines()
        .map(|line| format!("data: {line}\n"))
        .collect();
    let block = format!("event: OK\n{data_lines}\n");
    let expected = vec![
        "text:Checking.".to_string(),
        "start:0:call-1:ask_user_question".to_string(),
        format!("delta:0:{ASK_ARGS}"),
        "end:0".to_string(),
        "message_end".to_string(),
    ];

    assert_eq!(
        collect_byte_chunks(vec![block.clone().into_bytes()]).await,
        expected,
        "LF-framed multiline data must reassemble"
    );
    assert_eq!(
        collect_byte_chunks(vec![block.replace('\n', "\r\n").into_bytes()]).await,
        expected,
        "CRLF-framed multiline data must reassemble"
    );
}

/// A multiline `Failed` payload is joined before summarizing, and the summary
/// stays bounded: no fragment of the payload may leak.
#[test]
fn multiline_failed_payload_is_joined_and_stays_bounded() {
    let mut state = SseParseState::default();
    let block = "event: Failed\ndata: {\"message\":\ndata:  \"auth failed for sk-secret\"}\n";
    let events =
        parse_sysom_sse_events(block, &mut state).expect("failure frames are reported as events");

    match events.as_slice() {
        [GenerateEvent::Error(message)] => {
            assert!(!message.contains("sk-secret"), "payload leaked: {message}");
            // The size proves both lines were joined, not just the last kept.
            let joined = "{\"message\":\n \"auth failed for sk-secret\"}";
            assert!(
                message.contains(&format!("{} byte payload", joined.len())),
                "{message}"
            );
        }
        other => panic!("expected a single error event, got {other:?}"),
    }
    assert!(state.message_ended);
    assert!(state.stream_ended, "provider failure must end the stream");
}

#[tokio::test]
async fn malformed_frame_surfaces_a_bounded_stream_error() {
    let events = collect_stream(vec![
        frame(message_frame(Some("hi"), vec![])),
        "event: OK\ndata: {\"choices\":\n\n".to_string(),
        frame(message_frame(Some("hi there"), vec![])),
    ])
    .await;

    assert_eq!(
        events,
        vec![
            "text:hi".to_string(),
            format!("error:{}", SysomStreamError::MalformedJson),
        ]
    );
}

/// EOF must not swallow `MessageEnd` just because usage was reported.
#[tokio::test]
async fn eof_with_usage_keeps_usage_and_message_end() {
    let mut usage_frame = message_frame(Some("done"), vec![]);
    usage_frame["usage"] = serde_json::json!({
        "prompt_tokens": 11,
        "completion_tokens": 22,
        "total_tokens": 33,
    });

    assert_eq!(
        collect_stream(vec![frame(usage_frame)]).await,
        vec![
            "text:done".to_string(),
            "usage:11:22:33".to_string(),
            "message_end".to_string(),
        ]
    );
}

#[test]
fn sysom_extracts_cached_tokens_from_prompt_tokens_details() {
    let mut state = SseParseState::default();
    let mut usage_frame = message_frame(Some("done"), vec![]);
    usage_frame["usage"] = serde_json::json!({
        "prompt_tokens": 1000,
        "completion_tokens": 50,
        "total_tokens": 1050,
        "prompt_tokens_details": {
            "cached_tokens": 800
        }
    });

    let mut events = parse_sysom_sse_events(&frame(usage_frame), &mut state).expect("frame parses");
    events.extend(sysom_eof_events(&mut state));
    assert!(matches!(
        events.as_slice(),
        [
            GenerateEvent::TextDelta(text),
            GenerateEvent::Usage {
                prompt_tokens: 1000,
                completion_tokens: 50,
                total_tokens: 1050,
                cached_tokens: 800,
            },
            GenerateEvent::MessageEnd,
        ] if text == "done"
    ));
}

#[tokio::test]
async fn usage_only_final_frame_still_terminates_with_message_end() {
    let mut final_frame = message_frame(Some("hello"), vec![]);
    final_frame["usage"] = serde_json::json!({
        "prompt_tokens": 11,
        "completion_tokens": 7,
        "total_tokens": 18,
    });

    assert_eq!(
        collect_stream(vec![
            frame(message_frame(Some("hello"), vec![])),
            frame(final_frame),
        ])
        .await,
        vec![
            "text:hello".to_string(),
            "usage:11:7:18".to_string(),
            "message_end".to_string(),
        ]
    );
}

#[tokio::test]
async fn stream_without_usage_terminates_with_message_end() {
    assert_eq!(
        collect_stream(vec![frame(message_frame(Some("hi"), vec![]))]).await,
        vec!["text:hi".to_string(), "message_end".to_string()]
    );
}

#[tokio::test]
async fn unterminated_final_frame_is_flushed_before_message_end() {
    let mut final_frame = message_frame(Some("done"), vec![]);
    final_frame["usage"] = serde_json::json!({
        "prompt_tokens": 3,
        "completion_tokens": 1,
        "total_tokens": 4,
    });
    let frame = frame(final_frame);

    assert_eq!(
        collect_stream(vec![frame.trim_end().to_string()]).await,
        vec![
            "text:done".to_string(),
            "usage:3:1:4".to_string(),
            "message_end".to_string(),
        ]
    );
}

#[tokio::test]
async fn tool_call_with_final_usage_reaches_message_end() {
    let mut final_frame = message_frame(
        None,
        vec![tool_entry(0, "call_1", "read_file", r#"{"path":"/tmp/a"}"#)],
    );
    final_frame["usage"] = serde_json::json!({
        "prompt_tokens": 20,
        "completion_tokens": 9,
        "total_tokens": 29,
    });

    assert_eq!(
        collect_stream(vec![
            frame(message_frame(
                None,
                vec![tool_entry(0, "call_1", "read_file", "")],
            )),
            frame(final_frame),
        ])
        .await,
        vec![
            "start:0:call_1:read_file".to_string(),
            r#"delta:0:{"path":"/tmp/a"}"#.to_string(),
            "end:0".to_string(),
            "usage:20:9:29".to_string(),
            "message_end".to_string(),
        ]
    );
}

/// Byte stream that panics if polled again after reporting EOF.
struct PanicOnRepoll {
    chunks: std::vec::IntoIter<Vec<u8>>,
    exhausted: bool,
}

impl futures::Stream for PanicOnRepoll {
    type Item = Result<Vec<u8>, String>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        assert!(!self.exhausted, "byte stream polled after EOF");
        match self.chunks.next() {
            Some(chunk) => Poll::Ready(Some(Ok(chunk))),
            None => {
                self.exhausted = true;
                Poll::Ready(None)
            }
        }
    }
}

#[tokio::test]
async fn byte_stream_is_not_polled_after_eof() {
    let mut usage_frame = message_frame(Some("x"), vec![]);
    usage_frame["usage"] = serde_json::json!({
        "prompt_tokens": 1,
        "completion_tokens": 1,
        "total_tokens": 2,
    });
    let source: SysomByteStream = Box::pin(PanicOnRepoll {
        chunks: vec![frame(usage_frame).into_bytes()].into_iter(),
        exhausted: false,
    });

    let stream = sysom_event_stream(source, Arc::new(AtomicBool::new(false)));
    let events = summarize(&stream.collect::<Vec<_>>().await);

    assert_eq!(
        events,
        vec![
            "text:x".to_string(),
            "usage:1:1:2".to_string(),
            "message_end".to_string(),
        ]
    );
}

#[test]
fn eof_closes_started_tools_before_usage_and_message_end() {
    let mut state = SseParseState::default();
    let block = frame(message_frame(
        None,
        vec![
            tool_entry(0, "call-a", "read_file", r#"{"path":"a"}"#),
            tool_entry(1, "call-b", "ask_user_question", ASK_ARGS),
        ],
    ));
    assert_eq!(parse(&block, &mut state).len(), 4);
    state.latest_usage = Some((1, 2, 3, 0));

    assert_eq!(
        summarize(&sysom_eof_events(&mut state)),
        vec![
            "end:0".to_string(),
            "end:1".to_string(),
            "usage:1:2:3".to_string(),
            "message_end".to_string(),
        ]
    );
    assert!(state.stream_ended, "EOF must terminate the stream");
    assert!(
        summarize(&sysom_eof_events(&mut state)).eq(&["message_end".to_string()]),
        "tool ends and usage must not be emitted twice"
    );
}

#[test]
fn eof_usage_preserves_cached_tokens() {
    let mut state = SseParseState {
        latest_usage: Some((1, 2, 3, 42)),
        ..Default::default()
    };

    let events = sysom_eof_events(&mut state);
    assert!(matches!(
        events.as_slice(),
        [
            GenerateEvent::Usage {
                prompt_tokens: 1,
                completion_tokens: 2,
                total_tokens: 3,
                cached_tokens: 42,
            },
            GenerateEvent::MessageEnd,
        ]
    ));
}

/// The failure payload may hold credentials or the pending question, so no part of
/// it may reach the propagated error — only size, digest, or a whitelisted code.
#[test]
fn failed_event_reports_no_payload_and_ends_the_message() {
    let mut state = SseParseState::default();
    let secret = "sk-super-secret-token";
    let payload = format!("{{\"message\":\"auth failed for {secret}\"}}");
    let events = parse_sysom_sse_events(&format!("event: Failed\ndata: {payload}\n\n"), &mut state)
        .expect("failure frames are reported as events");

    match events.as_slice() {
        [GenerateEvent::Error(message)] => {
            assert!(!message.contains(secret), "payload leaked: {message}");
            assert!(
                !message.contains("auth failed"),
                "payload leaked: {message}"
            );
            assert!(
                message.contains(&format!("{} byte payload", payload.len())),
                "{message}"
            );
            assert!(message.contains("sha256:"), "{message}");
        }
        other => panic!("expected a single error event, got {other:?}"),
    }
    assert!(state.message_ended);
}

/// Only codes on the exact allowlist may leave the stream. A character or length
/// filter would also pass a short secret or an IP address that happens to arrive
/// in a `code` field, so those must degrade to size and digest.
#[test]
fn failed_event_forwards_only_allowlisted_provider_error_codes() {
    let code_frame = |payload: &str| {
        let mut state = SseParseState::default();
        let events =
            parse_sysom_sse_events(&format!("event: Failed\ndata: {payload}\n\n"), &mut state)
                .expect("failure frames are reported as events");
        match events.as_slice() {
            [GenerateEvent::Error(message)] => message.clone(),
            other => panic!("expected a single error event, got {other:?}"),
        }
    };

    let known = code_frame(r#"{"code":"Throttling.User","message":"slow down user alice"}"#);
    assert!(known.contains("Throttling.User"), "{known}");
    assert!(!known.contains("alice"), "{known}");

    let nested = code_frame(r#"{"error":{"code":"InvalidApiKey","message":"key sk-abc"}}"#);
    assert!(nested.contains("InvalidApiKey"), "{nested}");
    assert!(!nested.contains("sk-abc"), "{nested}");

    // Values that pass any syntax rule but are not provider codes.
    for payload in [
        r#"{"code":"sk-super-secret-token"}"#,
        r#"{"code":"10.0.0.7"}"#,
        r#"{"code":"/home/alice/.cosh/config.toml"}"#,
        r#"{"error":{"code":"AKIA1234567890ABCD"}}"#,
        r#"{"code":"request from 10.0.0.7 rejected: bad token sk-xyz"}"#,
    ] {
        let summary = code_frame(payload);
        assert!(
            summary.contains("no recognizable provider error code"),
            "payload {payload} must not be forwarded: {summary}"
        );
        for secret in [
            "sk-super-secret-token",
            "10.0.0.7",
            "alice",
            "AKIA",
            "sk-xyz",
        ] {
            assert!(
                !summary.contains(secret),
                "payload {payload} leaked {secret}: {summary}"
            );
        }
    }

    // A long payload stays bounded regardless of shape.
    let long = code_frame(&"x".repeat(4096));
    assert!(long.len() < 200, "detail must stay bounded: {long}");
    assert!(long.contains("4096 byte payload"), "{long}");
}

/// Core binds a tool result to the id from the first frame, so a slot must never
/// open without an id and name, and a later frame must not rename it.
#[test]
fn tool_identity_must_be_present_once_and_stay_stable() {
    let mut state = SseParseState::default();
    let no_name = frame(serde_json::json!({
        "choices": [{ "message": { "tool_use": [{
            "index": 0,
            "id": "call-1",
            "function": { "arguments": ASK_ARGS },
        }] } }]
    }));
    assert_eq!(
        parse_err(&no_name, &mut state),
        SysomStreamError::ToolIdentityMissing { index: 0 }
    );

    let mut state = SseParseState::default();
    let no_id = frame(serde_json::json!({
        "choices": [{ "message": { "tool_use": [{
            "index": 0,
            "function": { "name": "ask_user_question", "arguments": ASK_ARGS },
        }] } }]
    }));
    assert_eq!(
        parse_err(&no_id, &mut state),
        SysomStreamError::ToolIdentityMissing { index: 0 }
    );

    let mut state = SseParseState::default();
    let numeric_id = frame(serde_json::json!({
        "choices": [{ "message": { "tool_use": [{
            "index": 0,
            "id": 7,
            "function": { "name": "ask_user_question" },
        }] } }]
    }));
    assert_eq!(
        parse_err(&numeric_id, &mut state),
        SysomStreamError::ToolIdentityWrongType { index: 0 }
    );

    let mut state = SseParseState::default();
    assert_eq!(
        parse(
            &frame(message_frame(
                None,
                vec![tool_entry(0, "call-1", "ask_user_question", r#"{"que"#)],
            )),
            &mut state
        )
        .len(),
        2
    );
    assert_eq!(
        parse_err(
            &frame(message_frame(
                None,
                vec![tool_entry(0, "call-2", "ask_user_question", ASK_ARGS)],
            )),
            &mut state
        ),
        SysomStreamError::ToolIdentityChanged { index: 0 }
    );
    assert_eq!(
        parse_err(
            &frame(message_frame(
                None,
                vec![tool_entry(0, "call-1", "shell", ASK_ARGS)],
            )),
            &mut state
        ),
        SysomStreamError::ToolIdentityChanged { index: 0 }
    );
    // Omitting the identity in a later frame is allowed: the slot keeps its own.
    let identity_free = frame(serde_json::json!({
        "choices": [{ "message": { "tool_use": [{
            "index": 0,
            "function": { "arguments": ASK_ARGS },
        }] } }]
    }));
    assert_eq!(
        parse(&identity_free, &mut state),
        vec![r#"delta:0:stion":"How should local changes be handled?"}"#.to_string()]
    );
}

/// Core sizes its pending-call vector from this index, so a sparse or maximal
/// index must be refused here instead of becoming a huge allocation downstream.
#[test]
fn out_of_range_index_is_refused_at_the_protocol_limit() {
    let entry_at = |index: Value| {
        frame(serde_json::json!({
            "choices": [{ "message": { "tool_use": [{
                "index": index,
                "id": "call-1",
                "function": { "name": "ask_user_question", "arguments": ASK_ARGS },
            }] } }]
        }))
    };

    let mut state = SseParseState::default();
    assert_eq!(
        parse(
            &entry_at(serde_json::json!(MAX_TOOL_CALL_INDEX)),
            &mut state
        )
        .len(),
        2,
        "the limit itself is a valid index"
    );

    for out_of_range in [
        serde_json::json!(MAX_TOOL_CALL_INDEX + 1),
        serde_json::json!(4_294_967_295_u64),
        serde_json::json!(u64::MAX),
    ] {
        let mut state = SseParseState::default();
        assert_eq!(
            parse_err(&entry_at(out_of_range.clone()), &mut state),
            SysomStreamError::ToolIndexInvalid { position: 0 },
            "index {out_of_range} must be rejected"
        );
    }

    // An array longer than the protocol allows is out of range even without
    // explicit indices, because the position becomes the index.
    let mut state = SseParseState::default();
    let long_array: Vec<Value> = (0..=MAX_TOOL_CALL_INDEX + 1)
        .map(|_| {
            serde_json::json!({
                "id": "call-1",
                "function": { "name": "ask_user_question", "arguments": ASK_ARGS },
            })
        })
        .collect();
    assert_eq!(
        parse_err(&frame(message_frame(None, long_array)), &mut state),
        SysomStreamError::ToolIndexInvalid {
            position: MAX_TOOL_CALL_INDEX as usize + 1,
        }
    );
}

/// An unusable index must fail rather than silently fall back to the array
/// position, which would attach the arguments to a slot the provider never named.
#[test]
fn unusable_index_does_not_fall_back_to_array_position() {
    for bad_index in [
        serde_json::json!("0"),
        serde_json::json!(-1),
        serde_json::json!(1.5),
        serde_json::json!(u32::MAX as u64 + 1),
        serde_json::json!(null),
    ] {
        let mut state = SseParseState::default();
        let block = frame(serde_json::json!({
            "choices": [{ "message": { "tool_use": [{
                "index": bad_index,
                "id": "call-1",
                "function": { "name": "ask_user_question", "arguments": ASK_ARGS },
            }] } }]
        }));
        assert_eq!(
            parse_err(&block, &mut state),
            SysomStreamError::ToolIndexInvalid { position: 0 },
            "index {bad_index} must be rejected"
        );
    }
}

/// Providers may stop repeating text once they switch to tool calls. An empty
/// snapshot carries no information, so it must not be mistaken for a rewrite.
#[test]
fn empty_content_or_arguments_snapshot_is_treated_as_no_change() {
    let mut state = SseParseState::default();
    let text_then_tool = frame(message_frame(
        Some("Checking."),
        vec![tool_entry(0, "call-1", "ask_user_question", ASK_ARGS)],
    ));
    assert_eq!(parse(&text_then_tool, &mut state).len(), 3);

    let dropped_snapshots = frame(message_frame(
        Some(""),
        vec![tool_entry(0, "call-1", "ask_user_question", "")],
    ));
    assert!(
        parse(&dropped_snapshots, &mut state).is_empty(),
        "empty snapshots must neither re-emit nor fail"
    );

    // A later real snapshot must still extend what was already seen.
    let grown = frame(message_frame(
        Some("Checking. Done."),
        vec![tool_entry(0, "call-1", "ask_user_question", ASK_ARGS)],
    ));
    assert_eq!(parse(&grown, &mut state), vec!["text: Done.".to_string()]);
}
