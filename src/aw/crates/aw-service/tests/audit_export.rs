//! Consumer fixtures verify public reads without granting execution access.

use aw_contracts::{canonical, Registry};
use aw_core::{journal::FileJournal, ports::Journal};
use aw_service::audit_export::{query, VERSION};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

const SOURCE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SECRET: &str = "private-tool-prompt-environment-body";
struct Fixture {
    directory: tempfile::TempDir,
    journal: FileJournal,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let journal = FileJournal::new(directory.path()).unwrap();
        let fixture = Self { directory, journal };
        fixture.source(SOURCE);
        fixture
    }
    fn source(&self, id: &str) {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(self.directory.path().join(".aw-audit-source.json"))
            .unwrap();
        file.write_all(
            serde_json::to_vec(
                &json!({"apiVersion":"audit.aw/v1alpha1","kind":"AuditSource","sourceId":id}),
            )
            .unwrap()
            .as_slice(),
        )
        .unwrap();
    }
    fn event(&mut self, n: u8, generation: &str) -> String {
        let fixtures: Value = serde_json::from_slice(include_bytes!(
            "../../aw-contracts/tests/fixtures/contracts.json"
        ))
        .unwrap();
        let mut event = fixtures["execution-event-v1alpha1"].clone();
        let key = format!("{n:064x}");
        event["metadata"]["uid"] = json!(key);
        event["metadata"]["producer"]["scopeId"] = json!(SOURCE);
        event["metadata"]["producer"]["generation"] = json!(generation);
        self.journal
            .claim(&key, &json!({"executionEvent":event,"private":SECRET}))
            .unwrap();
        key
    }
    fn call(&self, operation: Value) -> Value {
        let bytes =
            serde_json::to_vec(&json!({"api_version":VERSION,"operation":operation})).unwrap();
        let result = query(
            self.directory.path(),
            &bytes,
            Instant::now() + Duration::from_secs(5),
            &AtomicBool::new(false),
        );
        Registry::new()
            .unwrap()
            .validate("audit-export-response-v1alpha1", &result)
            .unwrap();
        assert!(!result.to_string().contains(SECRET));
        result
    }
    fn read(&self, key: &str, cursor: Value, limit: usize) -> Value {
        self.call(json!({"type":"read","chain_id":key,"cursor":cursor,"limit":limit}))
    }
}

#[test]
fn source_paging_advances_over_private_records_and_preserves_replay() {
    let mut f = Fixture::new();
    let key = f.event(1, "generation-one");
    f.journal
        .append(&key, &json!({"phase":"started","private":SECRET}))
        .unwrap();
    f.journal.append(&key,&json!({"phase":"closed","status":"observed","createdAt":"2026-11-07T00:00:00.000Z","private":SECRET})).unwrap();
    let first = f.read(&key, Value::Null, 1);
    assert!(first["error"].is_null(), "{first}");
    assert_eq!(first["result"]["records"].as_array().unwrap().len(), 1);
    assert_eq!(first["result"]["cursor"]["consumed"]["sequence"], 0);
    assert_eq!(first["result"]["tip"]["sequence"], 2);
    assert_eq!(first["result"]["terminal"], true);
    assert_eq!(first["result"]["caught_up"], false);
    assert_eq!(first, f.read(&key, Value::Null, 1));
    let next = f.read(&key, first["result"]["cursor"].clone(), 1);
    assert_eq!(next["result"]["records"], json!([]));
    assert_eq!(next["result"]["cursor"]["consumed"]["sequence"], 1);
    let last = f.read(&key, next["result"]["cursor"].clone(), 1);
    assert_eq!(last["result"]["records"][0]["data"]["kind"], "event.closed");
    assert_eq!(last["result"]["caught_up"], true);
    assert_eq!(last["result"]["event"], first["result"]["event"]);
}

#[test]
fn late_append_open_revisit_and_historical_generations_share_one_source() {
    let mut f = Fixture::new();
    let first = f.event(1, "generation-one");
    let cursor = f.read(&first, Value::Null, 64)["result"]["cursor"].clone();
    f.journal
        .append(
            &first,
            &json!({"phase":"closed","status":"observed","createdAt":"2026-11-07T00:00:00.000Z"}),
        )
        .unwrap();
    let second = f.event(2, "generation-two");
    let one = f.read(&first, cursor, 64);
    let two = f.read(&second, Value::Null, 64);
    assert_eq!(
        one["result"]["event"]["metadata"]["producer"]["generation"],
        "generation-one"
    );
    assert_eq!(
        two["result"]["event"]["metadata"]["producer"]["generation"],
        "generation-two"
    );
    assert_eq!(one["source_id"], two["source_id"]);
    assert_eq!(one["result"]["terminal"], true);
    assert_eq!(two["result"]["terminal"], false);
}

#[test]
fn inventory_change_keeps_progress_and_chain_cursor() {
    let mut f = Fixture::new();
    let key = f.event(2, "g");
    f.event(3, "g");
    let chain = f.read(&key, Value::Null, 64)["result"]["cursor"].clone();
    let inventory = f.call(json!({"type":"enumerate","cursor":null,"limit":1}));
    assert!(inventory["error"].is_null());
    f.event(1, "g");
    let changed =
        f.call(json!({"type":"enumerate","cursor":inventory["result"]["next_cursor"],"limit":1}));
    assert!(changed["error"].is_null(), "{changed}");
    assert_eq!(changed["result"]["changed"], true);
    assert_eq!(changed["result"]["keys"], json!([format!("{:064x}", 3)]));
    assert!(f.read(&key, chain, 64)["error"].is_null());
}

#[test]
fn source_replacement_missing_history_and_prefix_loss_are_explicit() {
    let mut f = Fixture::new();
    let key = f.event(1, "g");
    let prefix = fs::read(f.directory.path().join(format!("{key}.jsonl"))).unwrap();
    f.journal.append(&key, &json!({"phase":"started"})).unwrap();
    let cursor = f.read(&key, Value::Null, 1)["result"]["cursor"].clone();
    fs::write(f.directory.path().join(format!("{key}.jsonl")), prefix).unwrap();
    assert_eq!(f.read(&key, cursor.clone(), 1)["error"], "history_changed");
    f.source(&"b".repeat(64));
    assert_eq!(f.read(&key, cursor.clone(), 1)["error"], "source_changed");
    f.source(SOURCE);
    fs::remove_file(f.directory.path().join(format!("{key}.jsonl"))).unwrap();
    assert_eq!(f.read(&key, cursor, 1)["error"], "history_missing");
}

#[test]
fn legacy_and_malformed_public_history_never_leak_private_records() {
    let mut f = Fixture::new();
    let key = "b".repeat(64);
    f.journal.claim(&key, &json!({"private":SECRET})).unwrap();
    assert_eq!(
        f.read(&key, Value::Null, 64)["error"],
        "unsupported_history"
    );
    let gap = "d".repeat(64);
    f.journal
        .claim(
            &gap,
            &json!({"publicProjectionGap":"aw.native_metadata_unavailable","private":SECRET}),
        )
        .unwrap();
    assert_eq!(
        f.read(&gap, Value::Null, 64)["error"],
        "projection_unavailable"
    );
    let unknown_gap = "e".repeat(64);
    f.journal
        .claim(&unknown_gap, &json!({"publicProjectionGap":SECRET}))
        .unwrap();
    assert_eq!(
        f.read(&unknown_gap, Value::Null, 64)["error"],
        "projection_invalid"
    );
    let key = "c".repeat(64);
    f.journal
        .claim(&key, &json!({"executionEvent":{"private":SECRET}}))
        .unwrap();
    assert_eq!(f.read(&key, Value::Null, 64)["error"], "projection_invalid");
    let key = f.event(1, "g");
    f.journal
        .append(
            &key,
            &json!({"kind":"provider","phase":"completed","private":SECRET}),
        )
        .unwrap();
    assert_eq!(f.read(&key, Value::Null, 64)["error"], "projection_invalid");
}

#[test]
fn missing_descriptor_modes_and_cancellation_do_not_create_state() {
    let f = Fixture::new();
    fs::remove_file(f.directory.path().join(".aw-audit-source.json")).unwrap();
    assert_eq!(
        f.call(json!({"type":"enumerate","cursor":null,"limit":1}))["error"],
        "source_missing"
    );
    assert_eq!(fs::read_dir(f.directory.path()).unwrap().count(), 0);
    f.source(SOURCE);
    fs::set_permissions(f.directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        f.call(json!({"type":"enumerate","cursor":null,"limit":1}))["error"],
        "access_denied"
    );
    let result = query(
        Path::new("/does-not-exist"),
        b"{}",
        Instant::now(),
        &AtomicBool::new(true),
    );
    assert_eq!(result["error"], "cancelled");
}

#[test]
fn request_schema_and_strict_dto_reject_unknown_fields_and_control_commands() {
    let f = Fixture::new();
    for operation in [
        json!({"type":"stop"}),
        json!({"type":"enumerate","cursor":null,"limit":0}),
        json!({"type":"enumerate","cursor":null,"limit":65}),
        json!({"type":"enumerate","cursor":null,"limit":1,"private":SECRET}),
    ] {
        assert_eq!(f.call(operation)["error"], "invalid_request");
    }
    let duplicate=br#"{"api_version":"audit-export.aw/v1alpha1","operation":{"type":"enumerate","cursor":null,"limit":1,"limit":2}}"#;
    let response = query(
        f.directory.path(),
        duplicate,
        Instant::now() + Duration::from_secs(1),
        &AtomicBool::new(false),
    );
    assert_eq!(response["error"], "invalid_request");
    assert!(canonical::bytes(&response).unwrap().len() < 1024);
}
