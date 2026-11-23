use super::*;

#[test]
fn encoder_rejects_oversized_fact_while_serializing() {
    let payload = Payload::Progress {
        task_id: 1,
        progress: HealthProgress::Host("x".repeat(FRAME_LIMIT + 1)),
    };
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
    for version in [1, 3] {
        let body = serde_json::to_vec(&serde_json::json!({
            "version": version, "nonce": "0123456789abcdef0123456789abcdef",
            "sequence": 1, "emitted_ns": 1,
            "payload": {"type":"Progress", "data":{"task_id":1,"progress":{"Host":"fixture"}}}
        }))
        .unwrap();
        let mut decoder = Decoder::default();
        decoder
            .buffer
            .extend_from_slice(&(body.len() as u32).to_be_bytes());
        decoder.buffer.extend_from_slice(&body);
        assert!(matches!(decoder.next(), Err(ProtocolError::Invalid)));
    }
}

#[test]
fn decoder_accepts_current_version_and_task_identity() {
    let mut decoder = Decoder {
        buffer: encode(
            "0123456789abcdef0123456789abcdef",
            1,
            Payload::Progress {
                task_id: 7,
                progress: HealthProgress::Host("fixture".into()),
            },
        )
        .unwrap(),
        ..Decoder::default()
    };
    assert!(matches!(
        decoder.next().unwrap().unwrap().payload,
        Payload::Progress { task_id: 7, .. }
    ));
}

#[test]
fn actual_worker_request_round_trips_the_same_model() {
    let request = WorkerRequest {
        scan_id: "health-1".to_string(),
        started_at_ms: 1,
        now_epoch_seconds: 100,
        deadline: HealthDeadline::after(std::time::Duration::from_secs(1)).expect("deadline"),
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
