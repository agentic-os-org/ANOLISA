use super::*;

#[test]
fn encoder_rejects_oversized_fact_while_serializing() {
    let payload = Payload::Progress(HealthProgress::Host("x".repeat(FRAME_LIMIT + 1)));
    assert!(matches!(
        encode("0123456789abcdef0123456789abcdef", 1, payload),
        Err(ProtocolError::TooLarge)
    ));
}

#[test]
fn decoder_rejects_oversized_length_without_waiting_for_body() {
    let mut decoder = Decoder::default();
    decoder
        .buffer
        .extend_from_slice(&((FRAME_LIMIT + 1) as u32).to_be_bytes());
    assert!(matches!(decoder.next(), Err(ProtocolError::TooLarge)));
}

#[test]
fn decoder_rejects_unknown_version() {
    let body = br#"{"version":2,"nonce":"0123456789abcdef0123456789abcdef","sequence":1,"emitted_ns":1,"payload":{"type":"Progress","data":{"Host":"fixture"}}}"#;
    let mut decoder = Decoder::default();
    decoder
        .buffer
        .extend_from_slice(&(body.len() as u32).to_be_bytes());
    decoder.buffer.extend_from_slice(body);
    assert!(matches!(decoder.next(), Err(ProtocolError::Invalid)));
}

#[test]
fn actual_worker_request_round_trips_the_same_model() {
    let request = WorkerRequest {
        scan_id: "health-1".to_string(),
        started_at_ms: 1,
        now_epoch_seconds: 100,
        deadline: JobDeadline::after(std::time::Duration::from_secs(1)).expect("deadline"),
        context: super::super::plan::ScanContext::Resources,
        config: crate::config::HealthConfig::default(),
        environment: None,
        disabled: Vec::new(),
        config_source: crate::diagnostics::health::runtime::plan::ConfigSource::CallerSnapshot,
        fixture_name: None,
    };
    let bytes = encode(
        "0123456789abcdef0123456789abcdef",
        0,
        Payload::Start(request),
    )
    .expect("request encoding");
    serde_json::from_slice::<Frame>(&bytes[4..]).expect("actual request model must decode");
}
