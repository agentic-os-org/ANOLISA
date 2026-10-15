//! JSON payload framing and Host/worker direction routing.

use serde::{Deserialize, Serialize};

use super::{
    decode_header, encode_header, FrameId, FrameKey, HostMessage, MessageType, PreparedFrame,
    ProtocolError, ProtocolHeader, ProtocolLimits, SessionNonce, WorkerGeneration, WorkerMessage,
    FRAME_LENGTH_PREFIX_LEN, PROTOCOL_HEADER_LEN, PROTOCOL_MAGIC, SCHEMA_VERSION,
};

/// Serialize `payload` into a full frame (length prefix + header + JSON body).
fn encode_message<T: Serialize>(
    message_type: MessageType,
    session_nonce: SessionNonce,
    worker_generation: WorkerGeneration,
    frame_id: FrameId,
    payload: &T,
    limits: &ProtocolLimits,
) -> Result<Vec<u8>, ProtocolError> {
    let body =
        serde_json::to_vec(payload).map_err(|e| ProtocolError::MalformedPayload(e.to_string()))?;
    if body.len() > limits.max_payload_bytes {
        return Err(ProtocolError::FrameTooLarge);
    }
    let payload_len = u32::try_from(body.len()).map_err(|_| ProtocolError::FrameTooLarge)?;
    let frame_len = PROTOCOL_HEADER_LEN
        .checked_add(body.len())
        .ok_or(ProtocolError::FrameTooLarge)?;
    let frame_len = u32::try_from(frame_len).map_err(|_| ProtocolError::FrameTooLarge)?;
    let header = ProtocolHeader {
        magic: PROTOCOL_MAGIC,
        schema_version: SCHEMA_VERSION,
        message_type,
        payload_len,
        session_nonce,
        worker_generation,
        frame_id,
    };
    let capacity = FRAME_LENGTH_PREFIX_LEN
        .checked_add(frame_len as usize)
        .ok_or(ProtocolError::FrameTooLarge)?;
    let mut buf = Vec::with_capacity(capacity);
    buf.extend_from_slice(&frame_len.to_le_bytes());
    buf.extend_from_slice(&encode_header(&header));
    buf.extend_from_slice(&body);
    Ok(buf)
}

/// Validate a complete single-frame input and return its header and payload.
fn frame_parts<'a>(
    bytes: &'a [u8],
    limits: &ProtocolLimits,
) -> Result<(ProtocolHeader, &'a [u8]), ProtocolError> {
    if bytes.len() < FRAME_LENGTH_PREFIX_LEN {
        return Err(ProtocolError::Truncated);
    }
    let frame_len = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    if frame_len < PROTOCOL_HEADER_LEN {
        return Err(ProtocolError::FrameTooSmall);
    }
    let max_frame_len = PROTOCOL_HEADER_LEN
        .checked_add(limits.max_payload_bytes)
        .ok_or(ProtocolError::FrameTooLarge)?;
    if frame_len > max_frame_len {
        return Err(ProtocolError::FrameTooLarge);
    }
    let total_len = FRAME_LENGTH_PREFIX_LEN
        .checked_add(frame_len)
        .ok_or(ProtocolError::FrameTooLarge)?;
    if bytes.len() < total_len {
        return Err(ProtocolError::Truncated);
    }
    if bytes.len() > total_len {
        return Err(ProtocolError::TrailingBytes);
    }

    let header_start = FRAME_LENGTH_PREFIX_LEN;
    let header_end = header_start + PROTOCOL_HEADER_LEN;
    let header = decode_header(&bytes[header_start..header_end], limits)?;
    let expected_frame_len = PROTOCOL_HEADER_LEN
        .checked_add(header.payload_len as usize)
        .ok_or(ProtocolError::FrameTooLarge)?;
    if frame_len != expected_frame_len {
        return Err(ProtocolError::FrameLengthMismatch);
    }
    Ok((header, &bytes[header_end..total_len]))
}

/// Encode a Host message into a full frame.
///
/// # Errors
/// Returns [`ProtocolError`] if the JSON body cannot be produced or exceeds the
/// payload limit.
pub fn encode_host_message(
    session_nonce: SessionNonce,
    worker_generation: WorkerGeneration,
    frame_id: FrameId,
    message: &HostMessage,
    limits: &ProtocolLimits,
) -> Result<Vec<u8>, ProtocolError> {
    match message {
        HostMessage::Hello(m) => encode_message(
            MessageType::Hello,
            session_nonce,
            worker_generation,
            frame_id,
            m,
            limits,
        ),
        HostMessage::Render(m) => encode_message(
            MessageType::Render,
            session_nonce,
            worker_generation,
            frame_id,
            m,
            limits,
        ),
        HostMessage::PublishedAck(m) => encode_message(
            MessageType::PublishedAck,
            session_nonce,
            worker_generation,
            frame_id,
            m,
            limits,
        ),
        HostMessage::Shutdown(m) => encode_message(
            MessageType::Shutdown,
            session_nonce,
            worker_generation,
            frame_id,
            m,
            limits,
        ),
    }
}

/// Encode a worker message into a full frame.
///
/// # Errors
/// See [`encode_host_message`].
pub fn encode_worker_message(
    session_nonce: SessionNonce,
    worker_generation: WorkerGeneration,
    frame_id: FrameId,
    message: &WorkerMessage,
    limits: &ProtocolLimits,
) -> Result<Vec<u8>, ProtocolError> {
    match message {
        WorkerMessage::Ready(m) => encode_message(
            MessageType::Ready,
            session_nonce,
            worker_generation,
            frame_id,
            m,
            limits,
        ),
        WorkerMessage::PreparedFrame(m) => encode_message(
            MessageType::PreparedFrame,
            session_nonce,
            worker_generation,
            frame_id,
            m,
            limits,
        ),
        WorkerMessage::Heartbeat(m) => encode_message(
            MessageType::Heartbeat,
            session_nonce,
            worker_generation,
            frame_id,
            m,
            limits,
        ),
        WorkerMessage::WorkerError(m) => encode_message(
            MessageType::WorkerError,
            session_nonce,
            worker_generation,
            frame_id,
            m,
            limits,
        ),
    }
}

fn from_json<T: for<'de> Deserialize<'de>>(payload: &[u8]) -> Result<T, ProtocolError> {
    serde_json::from_slice(payload).map_err(|e| ProtocolError::MalformedPayload(e.to_string()))
}

/// Decode exactly one complete Host frame into its header and typed message.
///
/// # Errors
/// Fails closed on invalid framing or header fields, trailing bytes, a worker
/// message type arriving on the Host channel ([`ProtocolError::UnexpectedMessageType`]),
/// or a malformed JSON payload.
pub fn decode_host_message(
    bytes: &[u8],
    limits: &ProtocolLimits,
) -> Result<(ProtocolHeader, HostMessage), ProtocolError> {
    let (header, payload) = frame_parts(bytes, limits)?;
    let message = match header.message_type {
        MessageType::Hello => HostMessage::Hello(from_json(payload)?),
        MessageType::Render => HostMessage::Render(from_json(payload)?),
        MessageType::PublishedAck => HostMessage::PublishedAck(from_json(payload)?),
        MessageType::Shutdown => HostMessage::Shutdown(from_json(payload)?),
        MessageType::Ready
        | MessageType::PreparedFrame
        | MessageType::Heartbeat
        | MessageType::WorkerError => return Err(ProtocolError::UnexpectedMessageType),
    };
    Ok((header, message))
}

/// Decode exactly one complete worker frame into its header and typed message.
///
/// # Errors
/// Fails closed on invalid framing or header fields, trailing bytes, a Host
/// message type arriving on the worker channel ([`ProtocolError::UnexpectedMessageType`]),
/// or a malformed JSON payload.
pub fn decode_worker_message(
    bytes: &[u8],
    limits: &ProtocolLimits,
) -> Result<(ProtocolHeader, WorkerMessage), ProtocolError> {
    let (header, payload) = frame_parts(bytes, limits)?;
    let message = match header.message_type {
        MessageType::Ready => WorkerMessage::Ready(from_json(payload)?),
        MessageType::PreparedFrame => WorkerMessage::PreparedFrame(from_json(payload)?),
        MessageType::Heartbeat => WorkerMessage::Heartbeat(from_json(payload)?),
        MessageType::WorkerError => WorkerMessage::WorkerError(from_json(payload)?),
        MessageType::Hello
        | MessageType::Render
        | MessageType::PublishedAck
        | MessageType::Shutdown => return Err(ProtocolError::UnexpectedMessageType),
    };
    Ok((header, message))
}

/// Validate a received header against the identity assigned by the Host.
///
/// Call this after decoding every worker frame. A worker only echoes these
/// fields; it never chooses them.
///
/// # Errors
/// Returns the corresponding mismatch error for nonce, generation, or frame id.
pub fn validate_header_identity(
    header: &ProtocolHeader,
    expected_nonce: SessionNonce,
    expected_key: FrameKey,
) -> Result<(), ProtocolError> {
    if header.session_nonce != expected_nonce {
        return Err(ProtocolError::SessionNonceMismatch);
    }
    if header.worker_generation != expected_key.worker_generation {
        return Err(ProtocolError::WorkerGenerationMismatch);
    }
    if header.frame_id != expected_key.frame_id {
        return Err(ProtocolError::FrameIdMismatch);
    }
    Ok(())
}

/// Verify a worker's [`PreparedFrame`] echoes the complete Host-assigned
/// [`FrameKey`] and agrees with the frame header.
///
/// # Errors
/// Returns [`ProtocolError::UnexpectedMessageType`] if the header does not
/// carry a [`MessageType::PreparedFrame`], and
/// [`ProtocolError::MalformedPayload`] if the header or body identity disagrees
/// with the Host's expected key.
pub fn validate_frame_echo(
    header: &ProtocolHeader,
    frame: &PreparedFrame,
    expected_key: FrameKey,
) -> Result<(), ProtocolError> {
    if header.message_type != MessageType::PreparedFrame {
        return Err(ProtocolError::UnexpectedMessageType);
    }
    let header_matches = header.worker_generation == expected_key.worker_generation
        && header.frame_id == expected_key.frame_id;
    if !header_matches || frame.frame_key() != expected_key {
        return Err(ProtocolError::MalformedPayload(format!(
            "prepared frame echo mismatch: expected={expected_key:?} header=({}, {}) body={:?}",
            header.worker_generation,
            header.frame_id,
            frame.frame_key()
        )));
    }
    Ok(())
}
