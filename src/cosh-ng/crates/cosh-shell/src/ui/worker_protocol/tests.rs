//! Framing, limit, and message round-trip tests for the UI wire protocol.

use crate::ui::worker_protocol::{
    decode_header, decode_host_message, decode_worker_message, encode_header, encode_host_message,
    encode_worker_message, validate_draw_op, validate_frame_echo, validate_header_identity,
    validate_prepared_frame, DrawOp, DrawOpError, EmptyRenderView, FrameGeometry, FrameKey, Hello,
    HostMessage, MessageType, PreparedFrame, ProtocolError, ProtocolHeader, ProtocolLimits,
    PublishedAck, Ready, RenderColor, RenderRequest, RenderStyle, RenderView, Shutdown, Viewport,
    WorkerMessage, FRAME_LENGTH_PREFIX_LEN, PROTOCOL_HEADER_LEN, PROTOCOL_MAGIC, SCHEMA_VERSION,
};

const NONCE: [u8; 16] = [7u8; 16];
const LIMITS: ProtocolLimits = ProtocolLimits {
    max_payload_bytes: 4096,
    max_draw_ops: 64,
    max_text_bytes: 256,
};
const VIEWPORT: Viewport = Viewport {
    columns: 80,
    rows: 24,
};

/// Build an encoded fixed header that advertises `payload_len` payload bytes.
///
/// Every other field is valid so the decoder's only possible complaint is the
/// advertised length; this keeps the oversize test focused on the length gate.
fn header_with_payload_len(payload_len: usize) -> Vec<u8> {
    let header = ProtocolHeader {
        magic: PROTOCOL_MAGIC,
        schema_version: SCHEMA_VERSION,
        message_type: MessageType::Render,
        payload_len: payload_len as u32,
        session_nonce: NONCE,
        worker_generation: 11,
        frame_id: 42,
    };
    encode_header(&header).to_vec()
}

fn sample_prepared_frame() -> PreparedFrame {
    PreparedFrame {
        worker_generation: 11,
        frame_id: 42,
        capture_id: Some(9),
        draw_ops: vec![
            DrawOp::ClearRect {
                x: 0,
                y: 0,
                width: 4,
                height: 1,
            },
            DrawOp::MoveTo { x: 0, y: 0 },
            DrawOp::SetStyle(RenderStyle::Foreground(RenderColor::Green)),
            DrawOp::Text("ok".to_owned()),
            DrawOp::CursorVisible(true),
        ],
        geometry: FrameGeometry {
            columns: 80,
            rows: 24,
            cursor_x: 2,
            cursor_y: 0,
        },
        full_repaint: true,
    }
}

#[test]
fn decoder_rejects_oversized_payload_before_allocation() {
    // Positive control: exactly at the injected limit decodes.
    let ok = header_with_payload_len(LIMITS.max_payload_bytes);
    assert!(decode_header(&ok, &LIMITS).is_ok());
    // Mutation: one byte over the injected limit fails closed.
    let header = header_with_payload_len(LIMITS.max_payload_bytes + 1);
    assert_eq!(
        decode_header(&header, &LIMITS),
        Err(ProtocolError::FrameTooLarge)
    );
}

#[test]
fn encoder_applies_injected_payload_limit() {
    let message = HostMessage::Shutdown(Shutdown {
        reason: "finished".to_owned(),
    });
    assert!(encode_host_message(NONCE, 1, 2, &message, &LIMITS).is_ok());

    let zero_payload_limit = ProtocolLimits {
        max_payload_bytes: 0,
        ..LIMITS
    };
    assert_eq!(
        encode_host_message(NONCE, 1, 2, &message, &zero_payload_limit),
        Err(ProtocolError::FrameTooLarge)
    );
}

#[test]
fn header_round_trip_preserves_fields() {
    let header = ProtocolHeader {
        magic: PROTOCOL_MAGIC,
        schema_version: SCHEMA_VERSION,
        message_type: MessageType::PreparedFrame,
        payload_len: 123,
        session_nonce: NONCE,
        worker_generation: 5,
        frame_id: 99,
    };
    let bytes = encode_header(&header);
    assert_eq!(bytes.len(), PROTOCOL_HEADER_LEN);
    assert_eq!(decode_header(&bytes, &LIMITS), Ok(header));
}

#[test]
fn decoder_rejects_truncated_header() {
    let full = header_with_payload_len(0);
    // Positive control: full header decodes.
    assert!(decode_header(&full, &LIMITS).is_ok());
    // Mutation: drop the final byte.
    assert_eq!(
        decode_header(&full[..PROTOCOL_HEADER_LEN - 1], &LIMITS),
        Err(ProtocolError::Truncated)
    );
}

#[test]
fn frame_has_independent_little_endian_length_prefix() {
    let message = HostMessage::Shutdown(Shutdown {
        reason: "finished".to_owned(),
    });
    let frame = encode_host_message(NONCE, 1, 2, &message, &LIMITS).expect("encode");
    let declared = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;

    assert_eq!(declared, frame.len() - FRAME_LENGTH_PREFIX_LEN);
    assert_eq!(
        &frame[FRAME_LENGTH_PREFIX_LEN..FRAME_LENGTH_PREFIX_LEN + PROTOCOL_MAGIC.len()],
        PROTOCOL_MAGIC.as_slice()
    );
    assert_eq!(
        decode_host_message(&frame, &LIMITS).map(|pair| pair.1),
        Ok(message)
    );
}

#[test]
fn frame_decoder_rejects_oversized_length_prefix_before_payload() {
    let mut frame = vec![0u8; FRAME_LENGTH_PREFIX_LEN];
    let oversized = PROTOCOL_HEADER_LEN + LIMITS.max_payload_bytes + 1;
    frame[..FRAME_LENGTH_PREFIX_LEN].copy_from_slice(
        &u32::try_from(oversized)
            .expect("test limit fits u32")
            .to_le_bytes(),
    );

    assert_eq!(
        decode_host_message(&frame, &LIMITS),
        Err(ProtocolError::FrameTooLarge)
    );
}

#[test]
fn frame_decoder_rejects_undersized_length_prefix() {
    let message = HostMessage::Shutdown(Shutdown {
        reason: "finished".to_owned(),
    });
    let mut frame = encode_host_message(NONCE, 1, 2, &message, &LIMITS).expect("encode");
    frame[..FRAME_LENGTH_PREFIX_LEN].copy_from_slice(
        &u32::try_from(PROTOCOL_HEADER_LEN - 1)
            .expect("header length fits u32")
            .to_le_bytes(),
    );

    assert_eq!(
        decode_host_message(&frame, &LIMITS),
        Err(ProtocolError::FrameTooSmall)
    );
}

#[test]
fn frame_decoder_rejects_prefix_and_payload_len_inconsistency() {
    let message = HostMessage::Shutdown(Shutdown {
        reason: "finished".to_owned(),
    });
    let mut frame = encode_host_message(NONCE, 1, 2, &message, &LIMITS).expect("encode");
    let declared = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]);
    frame.extend_from_slice(&[0]);
    frame[..FRAME_LENGTH_PREFIX_LEN].copy_from_slice(&(declared + 1).to_le_bytes());

    assert_eq!(
        decode_host_message(&frame, &LIMITS),
        Err(ProtocolError::FrameLengthMismatch)
    );
}

#[test]
fn frame_decoder_rejects_split_and_truncated_frame() {
    let message = HostMessage::Shutdown(Shutdown {
        reason: "finished".to_owned(),
    });
    let frame = encode_host_message(NONCE, 1, 2, &message, &LIMITS).expect("encode");
    // Positive control: complete frame decodes.
    assert!(decode_host_message(&frame, &LIMITS).is_ok());
    // Mutations: split prefix and an incomplete advertised frame both fail closed.
    assert_eq!(
        decode_host_message(&frame[..FRAME_LENGTH_PREFIX_LEN - 1], &LIMITS),
        Err(ProtocolError::Truncated)
    );
    assert_eq!(
        decode_host_message(&frame[..frame.len() - 1], &LIMITS),
        Err(ProtocolError::Truncated)
    );
}

#[test]
fn full_frame_decoder_rejects_trailing_garbage() {
    let message = HostMessage::Shutdown(Shutdown {
        reason: "finished".to_owned(),
    });
    let mut frame = encode_host_message(NONCE, 1, 2, &message, &LIMITS).expect("encode");
    assert!(decode_host_message(&frame, &LIMITS).is_ok());
    frame.push(0);

    assert_eq!(
        decode_host_message(&frame, &LIMITS),
        Err(ProtocolError::TrailingBytes)
    );
}

#[test]
fn full_frame_decoder_rejects_concatenated_coalesced_frames() {
    let first = encode_host_message(
        NONCE,
        1,
        2,
        &HostMessage::PublishedAck(PublishedAck { frame_id: 2 }),
        &LIMITS,
    )
    .expect("encode first");
    let second = encode_host_message(
        NONCE,
        1,
        3,
        &HostMessage::Shutdown(Shutdown {
            reason: "finished".to_owned(),
        }),
        &LIMITS,
    )
    .expect("encode second");
    assert!(decode_host_message(&first, &LIMITS).is_ok());
    assert!(decode_host_message(&second, &LIMITS).is_ok());

    let mut coalesced = first;
    coalesced.extend_from_slice(&second);
    assert_eq!(
        decode_host_message(&coalesced, &LIMITS),
        Err(ProtocolError::TrailingBytes)
    );
}

#[test]
fn decoder_rejects_unknown_magic() {
    let mut bytes = header_with_payload_len(0);
    assert!(decode_header(&bytes, &LIMITS).is_ok());
    // Mutation: corrupt the magic.
    bytes[0] = b'X';
    assert_eq!(
        decode_header(&bytes, &LIMITS),
        Err(ProtocolError::UnknownMagic)
    );
}

#[test]
fn decoder_rejects_unsupported_version() {
    let mut bytes = header_with_payload_len(0);
    assert!(decode_header(&bytes, &LIMITS).is_ok());
    // Mutation: bump the schema version past what we support.
    bytes[4..6].copy_from_slice(&(SCHEMA_VERSION + 1).to_le_bytes());
    assert_eq!(
        decode_header(&bytes, &LIMITS),
        Err(ProtocolError::UnsupportedVersion)
    );
}

#[test]
fn decoder_rejects_unknown_message_type() {
    let mut bytes = header_with_payload_len(0);
    assert!(decode_header(&bytes, &LIMITS).is_ok());
    // Mutation: set an out-of-range message-type discriminant.
    bytes[6] = 200;
    assert_eq!(
        decode_header(&bytes, &LIMITS),
        Err(ProtocolError::UnknownMessageType)
    );
}

#[test]
fn host_message_round_trip() {
    let messages = [
        HostMessage::Hello(Hello {
            protocol_version: SCHEMA_VERSION,
            candidate_version: "host-1".to_owned(),
            capabilities: vec!["render".to_owned()],
        }),
        HostMessage::Render(RenderRequest {
            capture_id: Some(3),
            viewport: Viewport {
                columns: 80,
                rows: 24,
            },
            render_view: RenderView::Empty(EmptyRenderView { revision: 1 }),
        }),
        HostMessage::PublishedAck(PublishedAck { frame_id: 7 }),
        HostMessage::Shutdown(Shutdown {
            reason: "bye".to_owned(),
        }),
    ];
    for message in messages {
        let frame = encode_host_message(NONCE, 1, 2, &message, &LIMITS).expect("encode");
        let (header, decoded) = decode_host_message(&frame, &LIMITS).expect("decode");
        assert_eq!(header.message_type, message.message_type());
        assert_eq!(header.session_nonce, NONCE);
        assert_eq!(decoded, message);
    }
}

#[test]
fn worker_message_round_trip() {
    let messages = [
        WorkerMessage::Ready(Ready {
            protocol_version: SCHEMA_VERSION,
            candidate_version: "worker-1".to_owned(),
            capabilities: vec!["draw".to_owned()],
        }),
        WorkerMessage::PreparedFrame(sample_prepared_frame()),
        WorkerMessage::Heartbeat(crate::ui::worker_protocol::Heartbeat { monotonic_seq: 12 }),
        WorkerMessage::WorkerError(crate::ui::worker_protocol::WorkerError {
            frame_id: Some(42),
            code: "render_failed".to_owned(),
        }),
    ];
    for message in messages {
        let frame = encode_worker_message(NONCE, 11, 42, &message, &LIMITS).expect("encode");
        let (header, decoded) = decode_worker_message(&frame, &LIMITS).expect("decode");
        assert_eq!(header.message_type, message.message_type());
        assert_eq!(decoded, message);
    }
}

#[test]
fn decode_rejects_wrong_direction() {
    // A worker message must not decode on the Host channel and vice versa.
    let worker =
        WorkerMessage::Heartbeat(crate::ui::worker_protocol::Heartbeat { monotonic_seq: 1 });
    let worker_frame = encode_worker_message(NONCE, 1, 1, &worker, &LIMITS).expect("encode");
    // Positive control: it decodes as a worker message.
    assert!(decode_worker_message(&worker_frame, &LIMITS).is_ok());
    // Mutation: same bytes on the Host channel fail closed.
    assert_eq!(
        decode_host_message(&worker_frame, &LIMITS).map(|_| ()),
        Err(ProtocolError::UnexpectedMessageType)
    );
}

#[test]
fn decoder_rejects_unknown_render_style() {
    let frame = encode_worker_message(
        NONCE,
        11,
        42,
        &WorkerMessage::PreparedFrame(sample_prepared_frame()),
        &LIMITS,
    )
    .expect("encode");
    // Positive control: the untampered frame decodes.
    assert!(decode_worker_message(&frame, &LIMITS).is_ok());

    // Mutation: forge an unknown render style tag in the JSON payload. serde's
    // externally tagged enum rejects the unknown variant, so decode fails
    // closed instead of coercing it to a known style.
    let payload_start = FRAME_LENGTH_PREFIX_LEN + PROTOCOL_HEADER_LEN;
    let text = String::from_utf8(frame[payload_start..].to_vec()).expect("JSON payload is UTF-8");
    let forged = text.replace("\"Green\"", "\"Chartreuse\"");
    assert_ne!(
        forged, text,
        "expected the sample frame to contain the Green style"
    );
    let mut framed = frame[..payload_start].to_vec();
    framed.extend_from_slice(forged.as_bytes());
    let payload_len = u32::try_from(forged.len()).expect("test payload fits u32");
    let header_payload_offset = FRAME_LENGTH_PREFIX_LEN + 7;
    framed[header_payload_offset..header_payload_offset + 4]
        .copy_from_slice(&payload_len.to_le_bytes());
    let frame_len =
        u32::try_from(framed.len() - FRAME_LENGTH_PREFIX_LEN).expect("test frame fits u32");
    framed[..FRAME_LENGTH_PREFIX_LEN].copy_from_slice(&frame_len.to_le_bytes());
    assert!(matches!(
        decode_worker_message(&framed, &LIMITS),
        Err(ProtocolError::MalformedPayload(_))
    ));
}

#[test]
fn host_rejects_forged_header_identity() {
    let mut header = ProtocolHeader {
        magic: PROTOCOL_MAGIC,
        schema_version: SCHEMA_VERSION,
        message_type: MessageType::PreparedFrame,
        payload_len: 0,
        session_nonce: NONCE,
        worker_generation: 11,
        frame_id: 42,
    };
    let expected_key = FrameKey {
        worker_generation: 11,
        frame_id: 42,
        capture_id: Some(9),
    };
    // Positive control: the Host-assigned identity validates.
    assert_eq!(
        validate_header_identity(&header, NONCE, expected_key),
        Ok(())
    );

    // Named mutations: each Host-owned header field fails independently.
    header.session_nonce = [8u8; 16];
    assert_eq!(
        validate_header_identity(&header, NONCE, expected_key),
        Err(ProtocolError::SessionNonceMismatch)
    );
    header.session_nonce = NONCE;
    header.worker_generation = 12;
    assert_eq!(
        validate_header_identity(&header, NONCE, expected_key),
        Err(ProtocolError::WorkerGenerationMismatch)
    );
    header.worker_generation = 11;
    header.frame_id = 43;
    assert_eq!(
        validate_header_identity(&header, NONCE, expected_key),
        Err(ProtocolError::FrameIdMismatch)
    );
}

#[test]
fn prepared_frame_echo_detects_forged_generation_and_frame_id() {
    let header = ProtocolHeader {
        magic: PROTOCOL_MAGIC,
        schema_version: SCHEMA_VERSION,
        message_type: MessageType::PreparedFrame,
        payload_len: 0,
        session_nonce: NONCE,
        worker_generation: 11,
        frame_id: 42,
    };
    let frame = sample_prepared_frame();
    let expected_key = frame.frame_key();
    // Positive control: honest echo validates against the complete Host key.
    assert_eq!(validate_frame_echo(&header, &frame, expected_key), Ok(()));

    // Named mutations: generation and frame id are checked independently.
    let mut forged_generation = frame.clone();
    forged_generation.worker_generation = 12;
    assert!(matches!(
        validate_frame_echo(&header, &forged_generation, expected_key),
        Err(ProtocolError::MalformedPayload(_))
    ));
    let mut forged_frame_id = frame;
    forged_frame_id.frame_id = 43;
    assert!(matches!(
        validate_frame_echo(&header, &forged_frame_id, expected_key),
        Err(ProtocolError::MalformedPayload(_))
    ));
}

#[test]
fn prepared_frame_echo_rejects_capture_id_only_mutation() {
    let header = ProtocolHeader {
        magic: PROTOCOL_MAGIC,
        schema_version: SCHEMA_VERSION,
        message_type: MessageType::PreparedFrame,
        payload_len: 0,
        session_nonce: NONCE,
        worker_generation: 11,
        frame_id: 42,
    };
    let frame = sample_prepared_frame();
    let expected_key = FrameKey {
        worker_generation: 11,
        frame_id: 42,
        capture_id: Some(9),
    };
    assert_eq!(validate_frame_echo(&header, &frame, expected_key), Ok(()));

    let mut forged_capture = frame;
    forged_capture.capture_id = Some(10);
    assert!(matches!(
        validate_frame_echo(&header, &forged_capture, expected_key),
        Err(ProtocolError::MalformedPayload(_))
    ));
}

// ----- DrawOp safety -----

#[test]
fn draw_ops_reject_escape_and_terminal_queries() {
    for text in ["\u{1b}]8;;x", "\u{1b}[6n", "ok\u{0}bad"] {
        assert_eq!(
            validate_draw_op(&DrawOp::Text(text.into()), VIEWPORT, &LIMITS),
            Err(DrawOpError::ControlCharacter)
        );
    }
}

#[test]
fn draw_ops_accept_plain_whitelisted_ops() {
    // Positive control for the safety gate: plain, in-bounds ops validate.
    let ops = [
        DrawOp::ClearRect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        },
        DrawOp::MoveTo { x: 79, y: 23 },
        DrawOp::SetStyle(RenderStyle::Foreground(RenderColor::Cyan)),
        DrawOp::Text("hello 世界".to_owned()),
        DrawOp::CursorVisible(false),
    ];
    for op in ops {
        assert_eq!(validate_draw_op(&op, VIEWPORT, &LIMITS), Ok(()));
    }
}

#[test]
fn draw_ops_reject_out_of_bounds_coordinates() {
    // Positive control: the last addressable cell is in bounds.
    assert_eq!(
        validate_draw_op(&DrawOp::MoveTo { x: 79, y: 23 }, VIEWPORT, &LIMITS),
        Ok(())
    );
    // Mutation: one column past the viewport width.
    assert!(matches!(
        validate_draw_op(&DrawOp::MoveTo { x: 80, y: 0 }, VIEWPORT, &LIMITS),
        Err(DrawOpError::OutOfBounds { .. })
    ));
    // Mutation: a rectangle that runs off the bottom edge.
    assert!(matches!(
        validate_draw_op(
            &DrawOp::ClearRect {
                x: 0,
                y: 20,
                width: 10,
                height: 5
            },
            VIEWPORT,
            &LIMITS
        ),
        Err(DrawOpError::OutOfBounds { .. })
    ));
}

#[test]
fn draw_ops_apply_injected_text_limit() {
    let max = LIMITS.max_text_bytes;
    // Positive control: text exactly at the injected limit validates.
    assert_eq!(
        validate_draw_op(&DrawOp::Text("a".repeat(max)), VIEWPORT, &LIMITS),
        Ok(())
    );
    // Mutation: one byte over the injected limit fails closed.
    assert!(matches!(
        validate_draw_op(&DrawOp::Text("a".repeat(max + 1)), VIEWPORT, &LIMITS),
        Err(DrawOpError::TextTooLong { .. })
    ));
}

#[test]
fn prepared_frame_applies_injected_draw_op_limit() {
    let frame = sample_prepared_frame();
    let exact_limits = ProtocolLimits {
        max_draw_ops: frame.draw_ops.len(),
        ..LIMITS
    };
    assert_eq!(
        validate_prepared_frame(&frame, VIEWPORT, &exact_limits),
        Ok(())
    );

    let undersized_limits = ProtocolLimits {
        max_draw_ops: frame.draw_ops.len() - 1,
        ..LIMITS
    };
    assert!(matches!(
        validate_prepared_frame(&frame, VIEWPORT, &undersized_limits),
        Err(DrawOpError::TooManyDrawOps { .. })
    ));
}

#[test]
fn prepared_frame_rejects_forged_geometry() {
    let mut frame = sample_prepared_frame();
    // Positive control: honest geometry matching the viewport validates.
    frame.geometry = FrameGeometry {
        columns: 80,
        rows: 24,
        cursor_x: 2,
        cursor_y: 0,
    };
    assert_eq!(validate_prepared_frame(&frame, VIEWPORT, &LIMITS), Ok(()));
    // Mutation: worker forges a wider geometry than the requested viewport.
    frame.geometry.columns = 200;
    assert!(matches!(
        validate_prepared_frame(&frame, VIEWPORT, &LIMITS),
        Err(DrawOpError::GeometryMismatch { .. })
    ));
}
