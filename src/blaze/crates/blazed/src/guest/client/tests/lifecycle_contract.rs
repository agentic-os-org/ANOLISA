// SPDX-License-Identifier: Apache-2.0

use super::*;

async fn response_client(
    temp: &tempfile::TempDir,
    operation: &'static str,
    response: serde_json::Value,
) -> GuestClient {
    let socket = temp.path().join("guest.sock");
    spawn_server(
        socket.clone(),
        Arc::new(move |request| {
            assert_eq!(request["op"], operation);
            let mut response = response.clone();
            response["id"] = request["id"].clone();
            response
        }),
    )
    .await;
    GuestClient::new(socket, Duration::from_secs(2), 1024)
}

#[tokio::test]
async fn negotiation_rejects_malformed_success_responses() {
    let valid = json!({"ok": true, "proto_version": GUEST_PROTOCOL_VERSION, "ops": ["hello"]});
    let mut cases = Vec::new();
    for field in ["proto_version", "ops"] {
        let mut response = valid.clone();
        response
            .as_object_mut()
            .expect("response object")
            .remove(field);
        cases.push(response);
    }
    let mut wrong_version = valid.clone();
    wrong_version["proto_version"] = json!(GUEST_PROTOCOL_VERSION + 1);
    cases.push(wrong_version);
    for operations in [
        vec!["hello".to_string(); MAX_ADVERTISED_GUEST_OPERATIONS + 1],
        vec![String::new()],
        vec!["bad operation".to_string()],
        vec!["bad\noperation".to_string()],
        vec!["x".repeat(MAX_GUEST_OPERATION_NAME_BYTES + 1)],
    ] {
        let mut response = valid.clone();
        response["ops"] = json!(operations);
        cases.push(response);
    }
    for response in cases {
        let temp = tempfile::tempdir().expect("test directory");
        let client = response_client(&temp, "hello", response.clone()).await;
        let error = client
            .negotiate(&[GuestOp::Hello])
            .await
            .expect_err("invalid hello");
        assert!(
            matches!(error, GuestError::Protocol(_)),
            "{response}: {error:?}"
        );
    }
}

#[tokio::test]
async fn lifecycle_missing_evidence_is_an_unknown_outcome_after_delivery() {
    for (operation, valid, fields) in [
        (
            "prepare_hibernate",
            json!({"ok": true, "synced": true}),
            vec!["synced"],
        ),
        (
            "reseed_rng",
            json!({"ok": true, "seed_bytes": RESTORE_ENTROPY_BYTES, "reseed": true}),
            vec!["seed_bytes", "reseed"],
        ),
        (
            "post_restore",
            json!({"ok": true, "ts_ms": 100000, "delta_ms": 0, "clock_stepped": false}),
            vec!["ts_ms", "delta_ms", "clock_stepped"],
        ),
    ] {
        for field in fields {
            let temp = tempfile::tempdir().expect("test directory");
            let mut response = valid.clone();
            response
                .as_object_mut()
                .expect("response object")
                .remove(field);
            let client = response_client(&temp, operation, response).await;
            let result = match operation {
                "prepare_hibernate" => client
                    .prepare_suspend(Duration::from_secs(2))
                    .await
                    .map(|_| ()),
                "reseed_rng" => client.reseed_rng(&[3; RESTORE_ENTROPY_BYTES]).await,
                "post_restore" => client.sync_realtime_clock_at(100000).await.map(|_| ()),
                _ => unreachable!(),
            };
            let error = result.expect_err("missing lifecycle evidence");
            assert!(
                matches!(error, GuestError::OutcomeUnknown(_)),
                "{operation}/{field}: {error:?}"
            );
        }
    }
}

#[tokio::test]
async fn lifecycle_rejects_negative_or_inconsistent_success_evidence() {
    for (operation, response) in [
        ("prepare_hibernate", json!({"ok": true, "synced": false})),
        (
            "reseed_rng",
            json!({"ok": true, "seed_bytes": RESTORE_ENTROPY_BYTES - 1, "reseed": true}),
        ),
        (
            "reseed_rng",
            json!({"ok": true, "seed_bytes": RESTORE_ENTROPY_BYTES, "reseed": false}),
        ),
        (
            "post_restore",
            json!({"ok": true, "ts_ms": 100000, "delta_ms": 501, "clock_stepped": false}),
        ),
        (
            "post_restore",
            json!({"ok": true, "ts_ms": 100000, "delta_ms": -501, "clock_stepped": false}),
        ),
        (
            "post_restore",
            json!({"ok": true, "ts_ms": 1, "delta_ms": 99999, "clock_stepped": true}),
        ),
    ] {
        let temp = tempfile::tempdir().expect("test directory");
        let client = response_client(&temp, operation, response.clone()).await;
        let result = match operation {
            "prepare_hibernate" => client
                .prepare_suspend(Duration::from_secs(2))
                .await
                .map(|_| ()),
            "reseed_rng" => client.reseed_rng(&[3; RESTORE_ENTROPY_BYTES]).await,
            "post_restore" => client.sync_realtime_clock_at(100000).await.map(|_| ()),
            _ => unreachable!(),
        };
        let error = result.expect_err("invalid lifecycle evidence");
        assert!(
            matches!(error, GuestError::Rejected(_)),
            "{response}: {error:?}"
        );
    }
}

#[tokio::test]
async fn clock_sync_accepts_confirmed_correction_and_threshold_boundary() {
    for (delta, stepped) in [(500, false), (-500, false), (60000, true), (-60000, true)] {
        let temp = tempfile::tempdir().expect("test directory");
        let client = response_client(
            &temp,
            "post_restore",
            json!({
                "ok": true, "ts_ms": 100000, "delta_ms": delta, "clock_stepped": stepped
            }),
        )
        .await;
        let result = client
            .sync_realtime_clock_at(100000)
            .await
            .expect("confirmed clock");
        assert_eq!(result.delta_ms, delta);
        assert_eq!(result.clock_stepped, stepped);
        assert_eq!(result.guest_ts_ms, 100000);
    }
}

#[tokio::test]
async fn invalid_lifecycle_inputs_are_rejected_before_transport() {
    let temp = tempfile::tempdir().expect("test directory");
    let client = GuestClient::new(
        temp.path().join("absent.sock"),
        Duration::from_secs(2),
        1024,
    );
    for length in [0, RESTORE_ENTROPY_BYTES - 1, RESTORE_ENTROPY_BYTES + 1] {
        assert!(matches!(
            client.reseed_rng(&vec![0; length]).await,
            Err(GuestError::InvalidArgument(_))
        ));
    }
    for timestamp in [0, -1] {
        assert!(matches!(
            client.sync_realtime_clock_at(timestamp).await,
            Err(GuestError::InvalidArgument(_))
        ));
    }
}
